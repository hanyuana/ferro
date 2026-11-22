//! 会话级状态：文件已读追踪（先读后写 + mtime 冲突锁）与 Todo 列表

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Default)]
pub struct FileTracker {
    read: HashMap<PathBuf, Option<SystemTime>>,
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

impl FileTracker {
    pub fn record(&mut self, p: &Path) {
        self.read.insert(p.to_path_buf(), mtime(p));
    }

    /// 修改已存在文件之前调用：必须先读过，且读后未被外部修改
    pub fn check_writable(&self, p: &Path) -> Result<(), String> {
        if !p.exists() {
            return Ok(());
        }
        match self.read.get(p) {
            None => Err(format!(
                "文件 {} 尚未读取。修改已存在的文件前必须先用 read 工具读取它。",
                p.display()
            )),
            Some(recorded) if *recorded != mtime(p) => Err(format!(
                "文件 {} 在上次读取后被修改过（可能是用户或其他程序）。请重新 read 后再修改。",
                p.display()
            )),
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Todo {
    pub content: String,
    pub status: TodoStatus,
}

pub fn render_todos(todos: &[Todo]) -> String {
    if todos.is_empty() {
        return "(空)".into();
    }
    todos
        .iter()
        .map(|t| {
            let mark = match t.status {
                TodoStatus::Pending => "[ ]",
                TodoStatus::InProgress => "[~]",
                TodoStatus::Completed => "[x]",
                TodoStatus::Cancelled => "[-]",
            };
            format!("{} {}", mark, t.content)
        })
        .collect::<Vec<_>>()
        .join("\n")
}
