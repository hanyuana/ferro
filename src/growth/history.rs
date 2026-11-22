//! 跨会话持久化与全文检索（`session_search` 工具 + `--continue` 会话恢复）：
//! 使用零外部依赖的追加式 `.jsonl` 存储于 `<config_dir>/ferro/sessions/`，
//! 支持关键词打分检索历史会话决策与上下文。

use crate::ai::{Block, Message, Role};
use crate::config::global_dir;
use crate::tools::{arg_str, Invocation, Tool, ToolCtx, ToolKind, ToolOutput};
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize)]
struct SessionHeader {
    id: String,
    cwd: String,
    created_at: u64,
}

pub fn sessions_dir() -> PathBuf {
    global_dir().join("sessions")
}

pub fn new_session_id() -> String {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!("{}_{}_{}", ts, std::process::id(), SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
}

pub fn save_session(id: &str, cwd: &Path, messages: &[Message]) -> Result<()> {
    if messages.is_empty() {
        return Ok(());
    }
    let dir = sessions_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.jsonl", id));
    let created_at = id
        .split('_')
        .next()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    let header = SessionHeader {
        id: id.to_string(),
        cwd: cwd.to_string_lossy().to_string(),
        created_at,
    };
    let mut out = serde_json::to_string(&header)? + "\n";
    for m in messages {
        out.push_str(&serde_json::to_string(m)?);
        out.push('\n');
    }
    std::fs::write(path, out)?;
    Ok(())
}

/// 会话列表项（用于客户端侧栏）
pub struct SessionInfo {
    pub id: String,
    pub title: String,
    pub created_at: u64,
    pub count: usize,
}

/// 从用户消息中提取会话标题（跳过系统提醒与 @文件 附件内容）
fn title_of(messages: &[Message]) -> String {
    for m in messages {
        if m.role != Role::User {
            continue;
        }
        for b in &m.content {
            if let Block::Text { text } = b {
                if text.starts_with("<system-reminder>") {
                    continue;
                }
                let main = text.split("\n\n<file").next().unwrap_or(text);
                let first = main.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
                if !first.is_empty() {
                    return first.chars().take(40).collect();
                }
            }
        }
    }
    "新会话".into()
}

fn read_session_file(path: &Path) -> Option<(SessionHeader, Vec<Message>)> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    let hdr: SessionHeader = serde_json::from_str(lines.next()?).ok()?;
    let msgs = lines.filter_map(|l| serde_json::from_str::<Message>(l).ok()).collect();
    Some((hdr, msgs))
}

/// 列出某个工作区下的历史会话（按时间倒序）
pub fn list_sessions(cwd: &Path) -> Vec<SessionInfo> {
    let Ok(rd) = std::fs::read_dir(sessions_dir()) else {
        return Vec::new();
    };
    let cwd_str = cwd.to_string_lossy();
    let mut out: Vec<SessionInfo> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("jsonl"))
        .filter_map(|p| read_session_file(&p))
        .filter(|(h, msgs)| h.cwd.eq_ignore_ascii_case(&cwd_str) && !msgs.is_empty())
        .map(|(h, msgs)| SessionInfo {
            title: title_of(&msgs),
            count: msgs.len(),
            id: h.id,
            created_at: h.created_at,
        })
        .collect();
    out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    out.truncate(50);
    out
}

/// Workspace-scoped full text search for the desktop history picker.
pub fn search_sessions(cwd: &Path, query: &str) -> Vec<SessionInfo> {
    let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let Ok(entries) = std::fs::read_dir(sessions_dir()) else { return Vec::new() };
    let mut matches = Vec::new();
    for entry in entries.flatten() {
        if entry.path().extension().and_then(|s| s.to_str()) != Some("jsonl") { continue; }
        let Some((header, messages)) = read_session_file(&entry.path()) else { continue };
        if !header.cwd.eq_ignore_ascii_case(&cwd.to_string_lossy()) { continue; }
        let text = serde_json::to_string(&messages).unwrap_or_default().to_lowercase();
        if terms.iter().all(|term| text.contains(term)) && !messages.is_empty() {
            matches.push(SessionInfo { id: header.id, title: title_of(&messages), count: messages.len(), created_at: header.created_at });
        }
    }
    matches.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    matches.truncate(50);
    matches
}

/// 按 ID 加载会话消息
pub fn load_session(id: &str) -> Option<Vec<Message>> {
    if id.contains(['/', '\\', '.']) {
        return None;
    }
    read_session_file(&sessions_dir().join(format!("{}.jsonl", id))).map(|(_, m)| m)
}

/// 按 ID 删除历史会话文件
pub fn delete_session(id: &str) -> bool {
    if id.is_empty() || id.contains(['/', '\\', '.']) {
        return false;
    }
    std::fs::remove_file(sessions_dir().join(format!("{}.jsonl", id))).is_ok()
}

/// 加载当前工作目录下最近一次会话
pub fn load_latest_session(cwd: &Path) -> Option<(String, Vec<Message>)> {
    let dir = sessions_dir();
    let rd = std::fs::read_dir(&dir).ok()?;
    let cwd_str = cwd.to_string_lossy();
    let mut candidates: Vec<(u64, PathBuf)> = Vec::new();

    for entry in rd.flatten() {
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&p) else { continue };
        let Some(first_line) = text.lines().next() else { continue };
        let Ok(hdr) = serde_json::from_str::<SessionHeader>(first_line) else { continue };
        if hdr.cwd.eq_ignore_ascii_case(&cwd_str) {
            candidates.push((hdr.created_at, p));
        }
    }
    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    let (_, latest_path) = candidates.into_iter().next()?;
    let text = std::fs::read_to_string(&latest_path).ok()?;
    let mut lines = text.lines();
    let hdr: SessionHeader = serde_json::from_str(lines.next()?).ok()?;
    let mut msgs = Vec::new();
    for line in lines {
        if let Ok(m) = serde_json::from_str::<Message>(line) {
            msgs.push(m);
        }
    }
    if msgs.is_empty() {
        None
    } else {
        Some((hdr.id, msgs))
    }
}

pub struct SessionSearchTool;

#[async_trait]
impl Tool for SessionSearchTool {
    fn name(&self) -> &'static str {
        "session_search"
    }
    fn description(&self) -> &'static str {
        "在历史会话库中全文搜索过去的对话、决策与代码讨论（灵感源自 Hermes Agent 跨会话回忆）。\n\
         - query: 空格分隔的关键词，按匹配度倒序返回最相关的历史片段"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "搜索关键词（多个词以空格分隔）" },
                "limit": { "type": "integer", "description": "最大返回条数，默认 5" }
            },
            "required": ["query"]
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    fn describe(&self, input: &Value, _ctx: &ToolCtx) -> Invocation {
        Invocation {
            title: input["query"].as_str().unwrap_or("").to_string(),
            ..Default::default()
        }
    }
    async fn run(&self, input: Value, _ctx: &ToolCtx) -> Result<ToolOutput> {
        let query = arg_str(&input, "query")?.trim().to_lowercase();
        let limit = input["limit"].as_u64().unwrap_or(5).clamp(1, 20) as usize;
        let terms: Vec<&str> = query.split_whitespace().filter(|s| !s.is_empty()).collect();
        if terms.is_empty() {
            return Ok(ToolOutput::text("查询词不能为空"));
        }

        let dir = sessions_dir();
        let Ok(rd) = std::fs::read_dir(&dir) else {
            return Ok(ToolOutput::text("暂无历史会话记录"));
        };

        let mut hits: Vec<(usize, u64, String, String, String)> = Vec::new();

        for entry in rd.flatten() {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&p) else { continue };
            let mut lines = text.lines();
            let Some(first) = lines.next() else { continue };
            let Ok(hdr) = serde_json::from_str::<SessionHeader>(first) else { continue };

            for line in lines {
                let Ok(msg) = serde_json::from_str::<Message>(line) else { continue };
                for b in msg.content {
                    let Block::Text { text: body } = b else { continue };
                    if body.starts_with("<system-reminder>") {
                        continue;
                    }
                    let lower = body.to_lowercase();
                    let score: usize = terms.iter().map(|t| lower.matches(t).count()).sum();
                    if score > 0 {
                        let role = match msg.role {
                            Role::User => "User",
                            Role::Assistant => "Assistant",
                        };
                        let snippet: String = body.chars().take(400).collect();
                        hits.push((score, hdr.created_at, hdr.id.clone(), hdr.cwd.clone(), format!("[{}] {}", role, snippet)));
                    }
                }
            }
        }

        if hits.is_empty() {
            return Ok(ToolOutput::text("未在历史会话中找到匹配内容"));
        }

        hits.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
        let mut out = format!("找到 {} 条相关历史会话片段：\n", hits.len().min(limit));
        for (score, _, id, cwd, snippet) in hits.into_iter().take(limit) {
            out.push_str(&format!(
                "\n--- 会话 {} (工作区: {}, 匹配度: {}) ---\n{}\n",
                id, cwd, score, snippet
            ));
        }
        Ok(ToolOutput::text(out))
    }
}
