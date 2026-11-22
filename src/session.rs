//! 会话级状态：文件已读追踪（先读后写 + mtime 冲突锁）与 Todo 列表

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Default)]
pub struct FileTracker {
    read: HashMap<PathBuf, Option<SystemTime>>,
    /// 记录本轮被修改前的文件原内容（None 表示文件原本不存在，撤销时应删除）
    backups: HashMap<PathBuf, Option<String>>,
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

impl FileTracker {
    pub fn record(&mut self, p: &Path) {
        self.read.insert(p.to_path_buf(), mtime(p));
    }

    /// 在修改文件前记录原始快照（同一轮多次修改同一文件仅保留最初状态）
    pub fn save_backup(&mut self, p: &Path, original: Option<String>) {
        self.backups.entry(p.to_path_buf()).or_insert(original);
    }

    pub fn clear_backups(&mut self) {
        self.backups.clear();
    }

    /// 撤销本轮所有文件修改，返回已恢复的文件列表
    pub fn undo_all(&mut self) -> Vec<PathBuf> {
        let mut restored = Vec::new();
        for (path, orig) in self.backups.drain() {
            let ok = match orig {
                Some(content) => std::fs::write(&path, content).is_ok(),
                None => std::fs::remove_file(&path).is_ok(),
            };
            if ok {
                self.read.insert(path.clone(), mtime(&path));
                restored.push(path);
            }
        }
        restored
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
