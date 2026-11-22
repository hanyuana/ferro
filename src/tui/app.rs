//! TUI 状态树：消息流、输入框、@文件模糊补全、/斜杠命令补全与权限审批弹窗

use crate::agent::permission::Decision;
use crate::agent::{AgentEvent, Mode};
use crate::ai::{Delta, Usage};
use std::path::{Path, PathBuf};
use tokio::sync::oneshot;

pub enum ChatEntry {
    User(String),
    Reasoning(String),
    Assistant(String),
    ToolCall {
        name: String,
        title: String,
    },
    ToolResult {
        name: String,
        ok: bool,
        summary: String,
        display: Option<String>,
    },
    System(String),
}

pub struct PendingPermission {
    pub name: String,
    pub title: String,
    pub preview: Option<String>,
    pub reply: oneshot::Sender<Decision>,
    pub typing_feedback: bool,
    pub feedback: String,
}

#[derive(Clone)]
pub struct SlashCmd {
    pub cmd: &'static str,
    pub desc: &'static str,
}

pub const SLASH_COMMANDS: &[SlashCmd] = &[
    SlashCmd { cmd: "/plan", desc: "切换到 PLAN 模式（只读架构探索）" },
    SlashCmd { cmd: "/build", desc: "切换到 BUILD 模式（读写与命令执行）" },
    SlashCmd { cmd: "/model", desc: "查看或切换模型，如 /model deepseek/deepseek-chat" },
    SlashCmd { cmd: "/key", desc: "直接设置当前模型的 API Key，如 /key sk-xxx" },
    SlashCmd { cmd: "/memory", desc: "查看当前项目与全局持久记忆 (MEMORY.md / USER.md)" },
    SlashCmd { cmd: "/skills", desc: "列出所有已发现的技能 (SKILL.md)" },
    SlashCmd { cmd: "/clear", desc: "清空当前对话历史与待办清单" },
    SlashCmd { cmd: "/think", desc: "展开或折叠模型思考过程 (Ctrl+T)" },
    SlashCmd { cmd: "/todo", desc: "在对话流中打印当前待办清单" },
    SlashCmd { cmd: "/usage", desc: "显示累计 Token 与缓存统计" },
    SlashCmd { cmd: "/help", desc: "查看快捷键与命令说明" },
    SlashCmd { cmd: "/exit", desc: "退出 Ferro" },
];

pub struct AppState {
    pub entries: Vec<ChatEntry>,
    pub input: String,
    pub cursor: usize,
    pub history: Vec<String>,
    pub history_idx: Option<usize>,

    pub mode: Mode,
    pub model_ref: String,
    pub cwd: PathBuf,
    pub usage: Usage,
    pub busy: bool,
    pub status_text: String,
    pub tick: usize,

    pub show_reasoning: bool,
    pub scroll: u16,
    pub auto_scroll: bool,

    pub workspace_files: Vec<String>,
    pub popup_selected: usize,

    pub pending_perm: Option<PendingPermission>,
}

impl AppState {
    pub fn new(mode: Mode, model_ref: String, cwd: PathBuf) -> Self {
        let workspace_files = scan_workspace_files(&cwd, 2000);
        Self {
            entries: vec![ChatEntry::System(
                "欢迎使用 Ferro TUI！按 Tab 切换 BUILD/PLAN 模式，输入 @ 引用文件，输入 / 查看命令。".into(),
            )],
            input: String::new(),
            cursor: 0,
            history: Vec::new(),
            history_idx: None,
            mode,
            model_ref,
            cwd,
            usage: Usage::default(),
            busy: false,
            status_text: "就绪".into(),
            tick: 0,
            show_reasoning: true,
            scroll: 0,
            auto_scroll: true,
            workspace_files,
            popup_selected: 0,
            pending_perm: None,
        }
    }

    pub fn refresh_files(&mut self) {
        self.workspace_files = scan_workspace_files(&self.cwd, 2000);
    }

    pub fn handle_agent_event(&mut self, ev: AgentEvent) {
        self.auto_scroll = true;
        match ev {
            AgentEvent::Delta(Delta::Text(t)) => {
                self.status_text = "正在生成回复…".into();
                if let Some(ChatEntry::Assistant(cur)) = self.entries.last_mut() {
                    cur.push_str(&t);
                } else {
                    self.entries.push(ChatEntry::Assistant(t));
                }
            }
            AgentEvent::Delta(Delta::Reasoning(t)) => {
                self.status_text = "正在深度思考…".into();
                if let Some(ChatEntry::Reasoning(cur)) = self.entries.last_mut() {
                    cur.push_str(&t);
                } else {
                    self.entries.push(ChatEntry::Reasoning(t));
                }
            }
            AgentEvent::ToolCall { name, title, .. } => {
                self.status_text = format!("正在执行 {}({})…", name, title);
                self.entries.push(ChatEntry::ToolCall { name, title });
            }
            AgentEvent::ToolResult { name, ok, summary, display, .. } => {
                if matches!(name.as_str(), "write" | "edit" | "multi_edit") && ok {
                    self.refresh_files();
                }
                self.entries.push(ChatEntry::ToolResult { name, ok, summary, display });
            }
            AgentEvent::Permission { name, title, preview, reply } => {
                self.status_text = format!("等待授权: {}({})", name, title);
                self.pending_perm = Some(PendingPermission {
                    name,
                    title,
                    preview,
                    reply,
                    typing_feedback: false,
                    feedback: String::new(),
                });
            }
            AgentEvent::Notice(n) => {
                self.entries.push(ChatEntry::System(n));
            }
        }
    }

    // ---------------- 输入框编辑 ----------------

    pub fn insert_char(&mut self, c: char) {
        self.input.insert(self.cursor, c);
        self.cursor += c.len_utf8();
        self.popup_selected = 0;
    }

    #[cfg(test)]
    pub fn insert_str(&mut self, s: &str) {
        self.input.insert_str(self.cursor, s);
        self.cursor += s.len();
        self.popup_selected = 0;
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            let prev = self.input[..self.cursor]
                .char_indices()
                .last()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.input.replace_range(prev..self.cursor, "");
            self.cursor = prev;
            self.popup_selected = 0;
        }
    }

    pub fn delete(&mut self) {
        if self.cursor < self.input.len() {
            let next = self.input[self.cursor..]
                .chars()
                .next()
                .map(|c| self.cursor + c.len_utf8())
                .unwrap_or(self.input.len());
            self.input.replace_range(self.cursor..next, "");
        }
    }

    pub fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.input[..self.cursor]
                .char_indices()
                .last()
                .map(|(i, _)| i)
                .unwrap_or(0);
        }
    }

    pub fn move_right(&mut self) {
        if self.cursor < self.input.len() {
            self.cursor = self.input[self.cursor..]
                .chars()
                .next()
                .map(|c| self.cursor + c.len_utf8())
                .unwrap_or(self.input.len());
        }
    }

    pub fn move_home(&mut self) {
        let before = &self.input[..self.cursor];
        self.cursor = before.rfind('\n').map(|i| i + 1).unwrap_or(0);
    }

    pub fn move_end(&mut self) {
        let after = &self.input[self.cursor..];
        self.cursor = after.find('\n').map(|i| self.cursor + i).unwrap_or(self.input.len());
    }

    pub fn clear_input(&mut self) {
        self.input.clear();
        self.cursor = 0;
        self.popup_selected = 0;
    }

    pub fn history_up(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let next = match self.history_idx {
            None => self.history.len() - 1,
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.history_idx = Some(next);
        self.input = self.history[next].clone();
        self.cursor = self.input.len();
    }

    pub fn history_down(&mut self) {
        let Some(cur) = self.history_idx else { return };
        if cur + 1 < self.history.len() {
            self.history_idx = Some(cur + 1);
            self.input = self.history[cur + 1].clone();
            self.cursor = self.input.len();
        } else {
            self.history_idx = None;
            self.clear_input();
        }
    }

    pub fn take_input(&mut self) -> String {
        let s = self.input.trim().to_string();
        if !s.is_empty() {
            self.history.push(s.clone());
        }
        self.history_idx = None;
        self.clear_input();
        s
    }

    // ---------------- 自动补全（@文件 与 /命令） ----------------

    /// 若光标当前处于 `@xxx` 词内，返回 (`@` 起始字节位置, 查询词, 匹配的文件列表)
    pub fn active_file_query(&self) -> Option<(usize, String, Vec<String>)> {
        let before = &self.input[..self.cursor];
        let at_pos = before.rfind('@')?;
        if at_pos > 0 {
            let prev_char = before[..at_pos].chars().last()?;
            if !prev_char.is_whitespace() {
                return None;
            }
        }
        let query = &before[at_pos + 1..];
        if query.chars().any(char::is_whitespace) {
            return None;
        }
        let q_lower = query.to_lowercase();
        let matches: Vec<String> = self
            .workspace_files
            .iter()
            .filter(|f| f.to_lowercase().contains(&q_lower))
            .take(10)
            .cloned()
            .collect();
        if matches.is_empty() {
            None
        } else {
            Some((at_pos, query.to_string(), matches))
        }
    }

    /// 若输入以 `/` 开头且不含空格，返回匹配的斜杠命令列表
    pub fn active_slash_query(&self) -> Option<Vec<SlashCmd>> {
        if !self.input.starts_with('/') || self.input.contains(' ') || self.input.contains('\n') {
            return None;
        }
        let q = self.input.to_lowercase();
        let matches: Vec<SlashCmd> = SLASH_COMMANDS
            .iter()
            .filter(|c| c.cmd.starts_with(&q))
            .cloned()
            .collect();
        if matches.is_empty() {
            None
        } else {
            Some(matches)
        }
    }

    /// 应用当前选中的补全项；返回 true 表示消费了按键
    pub fn complete_selection(&mut self) -> bool {
        if let Some((at_pos, _, files)) = self.active_file_query() {
            let idx = self.popup_selected.min(files.len() - 1);
            let chosen = format!("@{} ", files[idx]);
            self.input.replace_range(at_pos..self.cursor, &chosen);
            self.cursor = at_pos + chosen.len();
            self.popup_selected = 0;
            return true;
        }
        if let Some(cmds) = self.active_slash_query() {
            let idx = self.popup_selected.min(cmds.len() - 1);
            let chosen = cmds[idx].cmd;
            if chosen == "/model" || chosen == "/key" {
                self.input = format!("{} ", chosen);
                self.cursor = self.input.len();
                self.popup_selected = 0;
                return true;
            }
            if self.input != chosen {
                self.input = chosen.to_string();
                self.cursor = self.input.len();
                self.popup_selected = 0;
                return true;
            }
        }
        false
    }
}

fn scan_workspace_files(cwd: &Path, limit: usize) -> Vec<String> {
    let mut out = Vec::new();
    for entry in ignore::WalkBuilder::new(cwd).max_depth(Some(6)).build().flatten() {
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        if let Ok(rel) = entry.path().strip_prefix(cwd) {
            out.push(rel.to_string_lossy().replace('\\', "/"));
            if out.len() >= limit {
                break;
            }
        }
    }
    out.sort();
    out
}

/// 将用户输入中的 `@path/to/file` 自动展开并附带文件内容块，方便模型直接阅读
pub fn expand_at_mentions(cwd: &Path, raw: &str) -> String {
    let mut attachments = Vec::new();
    for word in raw.split_whitespace() {
        if let Some(rel) = word.strip_prefix('@') {
            let rel = rel.trim_matches(|c: char| matches!(c, ',' | '.' | ';' | ')' | '(' | '"' | '\''));
            if rel.is_empty() {
                continue;
            }
            let path = cwd.join(rel);
            if path.is_file() {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    let snippet = crate::tools::truncate_middle(&content, 20_000);
                    attachments.push(format!("<file path=\"{}\">\n{}\n</file>", rel, snippet));
                }
            }
        }
    }
    if attachments.is_empty() {
        raw.to_string()
    } else {
        format!("{}\n\n{}", raw, attachments.join("\n\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autocomplete_file_and_slash() {
        let mut s = AppState::new(Mode::Build, "deepseek/deepseek-chat".into(), PathBuf::from("."));
        s.workspace_files = vec!["src/main.rs".into(), "src/tui/app.rs".into(), "Cargo.toml".into()];

        s.insert_str("看看 @app");
        let (_, q, matches) = s.active_file_query().unwrap();
        assert_eq!(q, "app");
        assert_eq!(matches, vec!["src/tui/app.rs"]);
        assert!(s.complete_selection());
        assert_eq!(s.input, "看看 @src/tui/app.rs ");

        s.clear_input();
        s.insert_str("/pl");
        let cmds = s.active_slash_query().unwrap();
        assert_eq!(cmds[0].cmd, "/plan");
        assert!(s.complete_selection());
        assert_eq!(s.input, "/plan");
    }
}

