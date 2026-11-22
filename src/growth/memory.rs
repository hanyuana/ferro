//! Hermes 风格持久记忆系统：
//! - `project`: `<cwd>/.ferro/memory/MEMORY.md`（项目规范、架构约定、构建测试命令、踩坑记录）
//! - `global`:  `<config_dir>/ferro/memory/MEMORY.md`（跨项目通用工程经验）
//! - `user`:    `<config_dir>/ferro/memory/USER.md`（用户个人偏好、沟通风格、常用工具链）
//!
//! 会话启动时以冻结快照注入 System Prompt（保持前缀稳定，不破坏 Prompt Cache），
//! 运行时 Agent 通过 `memory` 工具自主增删改。

use crate::config::global_dir;
use crate::tools::{arg_str, opt_str, Invocation, Tool, ToolCtx, ToolKind, ToolOutput};
use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const MAX_MEMORY_CHARS: usize = 4000;
const MAX_USER_CHARS: usize = 2500;

#[derive(Debug, Clone, Copy)]
pub enum Scope {
    Project,
    Global,
    User,
}

impl Scope {
    fn parse(s: Option<&str>) -> Result<Self> {
        match s.unwrap_or("project") {
            "project" => Ok(Self::Project),
            "global" => Ok(Self::Global),
            "user" => Ok(Self::User),
            other => bail!("未知的 scope `{}`，可选值：project | global | user", other),
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Self::Project => "project (MEMORY.md)",
            Self::Global => "global (MEMORY.md)",
            Self::User => "user (USER.md)",
        }
    }

    fn limit(&self) -> usize {
        match self {
            Self::Project | Self::Global => MAX_MEMORY_CHARS,
            Self::User => MAX_USER_CHARS,
        }
    }

    pub fn path(&self, cwd: &Path) -> PathBuf {
        match self {
            Self::Project => cwd.join(".ferro").join("memory").join("MEMORY.md"),
            Self::Global => global_dir().join("memory").join("MEMORY.md"),
            Self::User => global_dir().join("memory").join("USER.md"),
        }
    }
}

fn read_entries(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.split("\n---\n")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

fn save_entries(path: &Path, entries: &[String], limit: usize) -> Result<(usize, usize)> {
    let body = entries.join("\n---\n");
    let chars = body.chars().count();
    if chars > limit {
        bail!(
            "记忆内容超出上限（{}/{} 字符）。请先用 replace 或 remove 合并、精简旧条目。",
            chars,
            limit
        );
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, &body)?;
    Ok((chars, limit))
}

/// 会话启动时加载冻结记忆快照，拼入 System Prompt
pub fn load_memory_prompt(cwd: &Path) -> String {
    let mut sections = Vec::new();
    for (scope, tag) in [
        (Scope::User, "user_profile"),
        (Scope::Global, "global_memory"),
        (Scope::Project, "project_memory"),
    ] {
        let p = scope.path(cwd);
        if let Ok(text) = std::fs::read_to_string(&p) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                let used = trimmed.chars().count();
                sections.push(format!(
                    "<{} scope=\"{}\" chars=\"{}/{}\">\n{}\n</{}>",
                    tag,
                    scope.label(),
                    used,
                    scope.limit(),
                    trimmed,
                    tag
                ));
            }
        }
    }
    if sections.is_empty() {
        String::new()
    } else {
        format!(
            "\n# Persistent Memory\n\
             The following memories were saved across sessions. Use the `memory` tool to add, replace, or remove entries when you learn durable project facts, build/test conventions, or explicit user preferences.\n\n{}",
            sections.join("\n\n")
        )
    }
}

pub struct MemoryTool;

#[async_trait]
impl Tool for MemoryTool {
    fn name(&self) -> &'static str {
        "memory"
    }
    fn description(&self) -> &'static str {
        "管理跨会话持久记忆（灵感源自 Hermes Agent）。\n\
         - scope:\n\
           - `project`（默认）：当前项目的架构约定、构建/测试命令、环境事实（存放在 .ferro/memory/MEMORY.md）\n\
           - `global`：跨项目通用的工程经验（存放在全局 MEMORY.md）\n\
           - `user`：用户的偏好、沟通习惯、编码风格要求（存放在全局 USER.md）\n\
         - action:\n\
           - `view`：查看当前记忆条目与字符用量\n\
           - `add`：新增一条精炼的记忆（参数 content）\n\
           - `replace`：将包含 old_text 的条目替换为 content（用于更新或浓缩旧记忆）\n\
           - `remove`：删除包含 old_text 的条目\n\
         - 只记录长期有价值的事实，不要记录临时会话状态或敏感密钥"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["view", "add", "replace", "remove"] },
                "scope": { "type": "string", "enum": ["project", "global", "user"], "description": "默认 project" },
                "content": { "type": "string", "description": "add 或 replace 时的新记忆内容" },
                "old_text": { "type": "string", "description": "replace 或 remove 时用于匹配旧条目的子串" }
            },
            "required": ["action"]
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Meta
    }
    fn describe(&self, input: &Value, _ctx: &ToolCtx) -> Invocation {
        let act = input["action"].as_str().unwrap_or("view");
        let scope = input["scope"].as_str().unwrap_or("project");
        Invocation { title: format!("{}:{}", scope, act), ..Default::default() }
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput> {
        let action = arg_str(&input, "action")?;
        let scope = Scope::parse(opt_str(&input, "scope"))?;
        let path = scope.path(&ctx.cwd);
        let limit = scope.limit();
        let mut entries = read_entries(&path);

        match action {
            "view" => {
                let body = entries.join("\n---\n");
                let used = body.chars().count();
                if entries.is_empty() {
                    Ok(ToolOutput::text(format!("{} 当前为空（0/{} 字符）", scope.label(), limit)))
                } else {
                    Ok(ToolOutput::text(format!(
                        "{}（{}/{} 字符，共 {} 条）：\n\n{}",
                        scope.label(),
                        used,
                        limit,
                        entries.len(),
                        body
                    )))
                }
            }
            "add" => {
                let content = arg_str(&input, "content")?.trim();
                if content.is_empty() {
                    bail!("content 不能为空");
                }
                if !entries.iter().any(|e| e == content) {
                    entries.push(content.to_string());
                }
                let (used, max) = save_entries(&path, &entries, limit)?;
                Ok(ToolOutput::text(format!(
                    "已写入 {}（当前 {}/{} 字符，共 {} 条）",
                    scope.label(),
                    used,
                    max,
                    entries.len()
                )))
            }
            "replace" => {
                let needle = arg_str(&input, "old_text")?.trim();
                let content = arg_str(&input, "content")?.trim();
                let matches: Vec<usize> = entries
                    .iter()
                    .enumerate()
                    .filter_map(|(i, e)| if e.contains(needle) { Some(i) } else { None })
                    .collect();
                match matches.as_slice() {
                    [] => Err(anyhow!("未找到包含 `{}` 的记忆条目", needle)),
                    [idx] => {
                        entries[*idx] = content.to_string();
                        let (used, max) = save_entries(&path, &entries, limit)?;
                        Ok(ToolOutput::text(format!(
                            "已更新 {} 中的条目（当前 {}/{} 字符）",
                            scope.label(),
                            used,
                            max
                        )))
                    }
                    _ => Err(anyhow!("`{}` 匹配到 {} 条记忆，请提供更具体的 old_text", needle, matches.len())),
                }
            }
            "remove" => {
                let needle = arg_str(&input, "old_text")?.trim();
                let before = entries.len();
                entries.retain(|e| !e.contains(needle));
                if entries.len() == before {
                    bail!("未找到包含 `{}` 的记忆条目", needle);
                }
                let (used, max) = save_entries(&path, &entries, limit)?;
                Ok(ToolOutput::text(format!(
                    "已从 {} 删除 {} 条记忆（剩余 {}/{} 字符）",
                    scope.label(),
                    before - entries.len(),
                    used,
                    max
                )))
            }
            other => bail!("未知的 action `{}`", other),
        }
    }
}
