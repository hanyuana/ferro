//! Agent 核心循环：流式请求 → 读写分离的并发工具调度 → 权限审批 → 循环

pub mod permission;
pub mod prompt;

use crate::ai::{Block, ChatRequest, Delta, Message, Provider, Role, StopReason, Usage};
use crate::config::Action;
use crate::tools::{truncate_middle, Registry, Tool, ToolCtx, ToolKind};
use anyhow::Result;
use futures_util::future::join_all;
use permission::{Decision, Permissions};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::{mpsc::UnboundedSender, oneshot, Notify};

const MAX_TOOL_OUTPUT: usize = 50_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Build,
    Plan,
}

pub enum AgentEvent {
    Delta(Delta),
    ToolCall { id: String, name: String, title: String },
    ToolResult { id: String, name: String, ok: bool, summary: String, display: Option<String> },
    Permission { name: String, title: String, preview: Option<String>, reply: oneshot::Sender<Decision> },
    Notice(String),
}

pub struct Agent {
    pub session_id: String,
    pub provider: Arc<dyn Provider>,
    pub model: String,
    pub model_ref: String,
    pub system: String,
    pub registry: Registry,
    pub ctx: Arc<ToolCtx>,
    pub perms: Permissions,
    pub mode: Mode,
    pub messages: Vec<Message>,
    pub usage: Usage,
    pub max_steps: usize,
    reminded_mode: Option<Mode>,
}

type Tx = UnboundedSender<AgentEvent>;

enum Flow {
    Continue,
    Stop,
}

impl Agent {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider: Arc<dyn Provider>,
        model: String,
        model_ref: String,
        system: String,
        ctx: Arc<ToolCtx>,
        perms: Permissions,
        mode: Mode,
        max_steps: usize,
    ) -> Self {
        Self {
            session_id: crate::growth::history::new_session_id(),
            provider,
            model,
            model_ref,
            system,
            registry: Registry::builtin(),
            ctx,
            perms,
            mode,
            messages: Vec::new(),
            usage: Usage::default(),
            max_steps,
            reminded_mode: None,
        }
    }

    pub fn clear(&mut self) {
        self.session_id = crate::growth::history::new_session_id();
        self.messages.clear();
        self.reminded_mode = None;
        self.ctx.todos.lock().unwrap().clear();
    }

    pub fn save_history(&self) {
        let _ = crate::growth::history::save_session(&self.session_id, &self.ctx.cwd, &self.messages);
    }

    /// 追加用户消息；若末尾已是用户消息则合并（保证角色交替）
    fn push_user(&mut self, blocks: Vec<Block>) {
        if let Some(last) = self.messages.last_mut() {
            if last.role == Role::User {
                last.content.extend(blocks);
                return;
            }
        }
        self.messages.push(Message { role: Role::User, content: blocks });
    }

    pub async fn run_turn(&mut self, input: String, tx: Tx, cancel: Arc<Notify>) -> Result<()> {
        self.ctx.tracker.lock().unwrap().clear_backups();
        let mut blocks = vec![Block::Text { text: input }];
        if self.reminded_mode != Some(self.mode) {
            // 首轮的 Build 模式无需提醒，减少噪音
            if !(self.reminded_mode.is_none() && self.mode == Mode::Build) {
                blocks.push(Block::Text { text: prompt::mode_reminder(self.mode == Mode::Plan) });
            }
            self.reminded_mode = Some(self.mode);
        }
        self.push_user(blocks);

        for _ in 0..self.max_steps {
            let tools = self.registry.defs(self.mode == Mode::Plan);
            let sink_tx = tx.clone();
            let sink = move |d: Delta| {
                let _ = sink_tx.send(AgentEvent::Delta(d));
            };
            let req = ChatRequest {
                model: &self.model,
                system: &self.system,
                messages: &self.messages,
                tools: &tools,
            };
            let resp = tokio::select! {
                r = self.provider.chat(req, &sink) => r?,
                _ = cancel.notified() => {
                    let _ = tx.send(AgentEvent::Notice("已中断".into()));
                    return Ok(());
                }
            };
            self.usage.add(&resp.usage);

            if resp.message.content.is_empty() {
                break;
            }
            let calls = resp.message.tool_uses();
            self.messages.push(resp.message);
            if calls.is_empty() {
                if resp.stop == StopReason::MaxTokens {
                    let _ = tx.send(AgentEvent::Notice("输出达到长度上限，后续内容已截断".into()));
                }
                self.save_history();
                return Ok(());
            }
            let (results, flow) = self.execute_tools(calls, &tx, &cancel).await;
            self.push_user(results);
            self.save_history();
            if let Flow::Stop = flow {
                return Ok(());
            }
        }
        self.save_history();
        let _ = tx.send(AgentEvent::Notice(format!("已达到单轮最大步数 {}，可输入“继续”让我接着做", self.max_steps)));
        Ok(())
    }

    /// 读写分离调度：连续的只读工具并发执行，其余工具按顺序执行并经过权限审批
    async fn execute_tools(
        &mut self,
        calls: Vec<(String, String, Value)>,
        tx: &Tx,
        cancel: &Notify,
    ) -> (Vec<Block>, Flow) {
        let n = calls.len();
        let mut results: Vec<Option<Block>> = (0..n).map(|_| None).collect();
        let mut flow = Flow::Continue;
        let mut i = 0;

        let result = |id: &str, content: String, is_error: bool| Block::ToolResult {
            tool_use_id: id.to_string(),
            content,
            is_error,
        };

        while i < n {
            let tool = self.registry.get(&calls[i].1);
            let concurrent = tool.as_ref().map(|t| t.kind() == ToolKind::Read).unwrap_or(false);

            if concurrent {
                let mut j = i;
                while j < n && self.registry.get(&calls[j].1).map(|t| t.kind() == ToolKind::Read).unwrap_or(false) {
                    j += 1;
                }
                let batch: Vec<_> = (i..j)
                    .map(|k| {
                        let (call_id, name, input) = &calls[k];
                        let tool = self.registry.get(name).unwrap();
                        let inv = tool.describe(input, &self.ctx);
                        let _ = tx.send(AgentEvent::ToolCall {
                            id: call_id.clone(),
                            name: name.clone(),
                            title: inv.title,
                        });
                        run_tool(tool, input.clone(), self.ctx.clone(), call_id.clone(), name.clone(), tx.clone())
                    })
                    .collect();
                tokio::select! {
                    outs = join_all(batch) => {
                        for (k, (content, is_err)) in (i..j).zip(outs) {
                            results[k] = Some(result(&calls[k].0, content, is_err));
                        }
                    }
                    _ = cancel.notified() => {
                        flow = Flow::Stop;
                        break;
                    }
                }
                i = j;
                continue;
            }

            let (id, name, input) = &calls[i];
            let Some(tool) = tool else {
                results[i] = Some(result(id, format!("未知工具: {}", name), true));
                i += 1;
                continue;
            };
            if input.get("__raw").is_some() {
                results[i] = Some(result(id, "工具参数不是合法的 JSON，请重新生成调用".into(), true));
                i += 1;
                continue;
            }

            let inv = tool.describe(input, &self.ctx);
            let plan = self.mode == Mode::Plan;
            match self.perms.check(tool.kind(), &inv, plan, &self.ctx.cwd) {
                Action::Deny => {
                    let msg = if plan && tool.kind() == ToolKind::Edit {
                        "当前处于 PLAN（只读）模式，不能修改文件。".to_string()
                    } else {
                        "该操作被权限规则禁止。".to_string()
                    };
                    let _ = tx.send(AgentEvent::ToolCall { id: id.clone(), name: name.clone(), title: inv.title.clone() });
                    let _ = tx.send(AgentEvent::ToolResult { id: id.clone(), name: name.clone(), ok: false, summary: msg.clone(), display: None });
                    results[i] = Some(result(id, msg, true));
                    i += 1;
                    continue;
                }
                Action::Ask => {
                    let (reply_tx, reply_rx) = oneshot::channel();
                    let _ = tx.send(AgentEvent::Permission {
                        name: name.clone(),
                        title: inv.title.clone(),
                        preview: inv.preview.clone(),
                        reply: reply_tx,
                    });
                    let decision = tokio::select! {
                        d = reply_rx => d.unwrap_or(Decision::Reject(None)),
                        _ = cancel.notified() => Decision::Reject(None),
                    };
                    match decision {
                        Decision::Once => {}
                        Decision::Always => self.perms.remember(tool.kind(), &inv),
                        Decision::Reject(feedback) => {
                            let msg = match &feedback {
                                Some(f) => format!("用户拒绝了此操作，并给出反馈：{}", f),
                                None => "用户拒绝了此操作。请停下来等待用户的进一步指示。".into(),
                            };
                            results[i] = Some(result(id, msg, true));
                            if feedback.is_none() {
                                flow = Flow::Stop;
                                break;
                            }
                            i += 1;
                            continue;
                        }
                    }
                }
                Action::Allow => {}
            }

            let _ = tx.send(AgentEvent::ToolCall { id: id.clone(), name: name.clone(), title: inv.title });
            tokio::select! {
                (content, is_err) = run_tool(tool, input.clone(), self.ctx.clone(), id.clone(), name.clone(), tx.clone()) => {
                    results[i] = Some(result(id, content, is_err));
                }
                _ = cancel.notified() => {
                    flow = Flow::Stop;
                    break;
                }
            }
            i += 1;
        }

        if matches!(flow, Flow::Stop) {
            let _ = tx.send(AgentEvent::Notice("已中断".into()));
        }
        // 未执行的调用也必须有结果，保证消息结构合法
        let blocks = results
            .into_iter()
            .enumerate()
            .map(|(k, r)| r.unwrap_or_else(|| result(&calls[k].0, "已被用户中断，未执行。".into(), true)))
            .collect();
        (blocks, flow)
    }
}

async fn run_tool(
    tool: Arc<dyn Tool>,
    input: Value,
    ctx: Arc<ToolCtx>,
    id: String,
    name: String,
    tx: Tx,
) -> (String, bool) {
    match tool.run(input, &ctx).await {
        Ok(out) => {
            let content = truncate_middle(&out.content, MAX_TOOL_OUTPUT);
            let _ = tx.send(AgentEvent::ToolResult {
                id,
                name,
                ok: true,
                summary: summarize(&content),
                display: out.display,
            });
            (content, false)
        }
        Err(e) => {
            let msg = format!("错误: {:#}", e);
            let _ = tx.send(AgentEvent::ToolResult { id, name, ok: false, summary: msg.clone(), display: None });
            (msg, true)
        }
    }
}

fn summarize(content: &str) -> String {
    let lines = content.lines().count();
    let first = content.lines().next().unwrap_or("").trim();
    let first: String = first.chars().take(100).collect();
    if lines > 1 {
        format!("{}  (+{} 行)", first, lines - 1)
    } else {
        first
    }
}
