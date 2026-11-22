//! 内置工具箱：Tool trait + 注册表 + 公共辅助函数

mod bash;
mod edit;
mod glob;
mod grep;
mod ls;
mod read;
mod todo;
mod write;

pub use bash::Shell;

use crate::ai::ToolDef;
use crate::session::{FileTracker, Todo};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    /// 只读，可并发，所有模式可用
    Read,
    /// 修改文件，Plan 模式不可用
    Edit,
    /// 执行命令
    Exec,
    /// 元能力（todo 等），无副作用于工作区
    Meta,
}

pub struct ToolCtx {
    pub cwd: PathBuf,
    pub tracker: Arc<Mutex<FileTracker>>,
    pub todos: Arc<Mutex<Vec<Todo>>>,
    pub shell: Shell,
}

pub struct ToolOutput {
    /// 返回给模型的内容
    pub content: String,
    /// 给用户看的附加展示（如 diff），不发送给模型
    pub display: Option<String>,
}

impl ToolOutput {
    pub fn text(s: impl Into<String>) -> Self {
        Self { content: s.into(), display: None }
    }
}

/// 执行前的描述信息：UI 标题 + 权限判断所需信息
#[derive(Default)]
pub struct Invocation {
    pub title: String,
    /// 审批时展示的预览（diff / 命令）
    pub preview: Option<String>,
    /// bash 命令原文，用于规则匹配
    pub command: Option<String>,
    /// 涉及修改的路径
    pub paths: Vec<PathBuf>,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn schema(&self) -> Value;
    fn kind(&self) -> ToolKind;
    fn describe(&self, input: &Value, ctx: &ToolCtx) -> Invocation;
    async fn run(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput>;
}

pub struct Registry {
    tools: Vec<Arc<dyn Tool>>,
}

impl Registry {
    pub fn builtin() -> Self {
        let tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(read::Read),
            Arc::new(glob::Glob),
            Arc::new(grep::Grep),
            Arc::new(ls::Ls),
            Arc::new(edit::Edit),
            Arc::new(edit::MultiEdit),
            Arc::new(write::Write),
            Arc::new(bash::Bash),
            Arc::new(todo::TodoWrite),
            Arc::new(crate::growth::memory::MemoryTool),
            Arc::new(crate::growth::skills::SkillListTool),
            Arc::new(crate::growth::skills::SkillViewTool),
            Arc::new(crate::growth::skills::SkillManageTool),
            Arc::new(crate::growth::history::SessionSearchTool),
        ];
        Self { tools }
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.iter().find(|t| t.name() == name).cloned()
    }

    /// Plan 模式下隐藏所有修改类工具
    pub fn defs(&self, plan_mode: bool) -> Vec<ToolDef> {
        self.tools
            .iter()
            .filter(|t| !(plan_mode && t.kind() == ToolKind::Edit))
            .map(|t| ToolDef {
                name: t.name().into(),
                description: t.description().into(),
                schema: t.schema(),
            })
            .collect()
    }
}

// ---------------- 公共辅助 ----------------

/// 解析路径：相对路径基于 cwd，并做纯词法规范化（不访问文件系统）
pub fn resolve_path(cwd: &Path, p: &str) -> PathBuf {
    let p = p.trim();
    let raw = Path::new(p);
    let joined = if raw.is_absolute() || p.starts_with('/') && cfg!(not(windows)) {
        raw.to_path_buf()
    } else {
        cwd.join(raw)
    };
    let mut out = PathBuf::new();
    for c in joined.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

pub fn display_path(cwd: &Path, p: &Path) -> String {
    p.strip_prefix(cwd)
        .map(|r| r.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| p.to_string_lossy().to_string())
}

pub fn arg_str<'a>(input: &'a Value, key: &str) -> Result<&'a str> {
    input[key]
        .as_str()
        .ok_or_else(|| anyhow!("缺少必需参数 `{}`", key))
}

pub fn opt_str<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input[key].as_str().filter(|s| !s.is_empty())
}

/// 截断过长输出：保留头尾
pub fn truncate_middle(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let half = max / 2;
    let mut head_end = half;
    while !s.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = s.len() - half;
    while !s.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    format!(
        "{}\n\n... [已截断 {} 字节] ...\n\n{}",
        &s[..head_end],
        tail_start - head_end,
        &s[tail_start..]
    )
}

/// 生成统一 diff（用于展示与审批）
pub fn unified_diff(path: &str, old: &str, new: &str) -> String {
    similar::TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header(&format!("a/{}", path), &format!("b/{}", path))
        .to_string()
}

pub fn diff_stats(old: &str, new: &str) -> (usize, usize) {
    let diff = similar::TextDiff::from_lines(old, new);
    let (mut add, mut del) = (0, 0);
    for c in diff.iter_all_changes() {
        match c.tag() {
            similar::ChangeTag::Insert => add += 1,
            similar::ChangeTag::Delete => del += 1,
            _ => {}
        }
    }
    (add, del)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn read_before_edit_and_mtime_guard() {
        let dir = std::env::temp_dir().join(format!("ferro_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("sample.rs");
        std::fs::write(&file, "fn main() {\n    println!(\"v1\");\n}\n").unwrap();

        let ctx = ToolCtx {
            cwd: dir.clone(),
            tracker: Arc::new(Mutex::new(FileTracker::default())),
            todos: Arc::new(Mutex::new(Vec::new())),
            shell: Shell::detect(None),
        };
        let reg = Registry::builtin();

        // 1. 未读取前直接 edit 应该报错
        let err = reg
            .get("edit")
            .unwrap()
            .run(json!({"file_path": "sample.rs", "old_string": "v1", "new_string": "v2"}), &ctx)
            .await;
        assert!(err.is_err());

        // 2. 先 read 后 edit 应该成功
        let r = reg.get("read").unwrap().run(json!({"file_path": "sample.rs"}), &ctx).await.unwrap();
        assert!(r.content.contains("v1"));

        reg.get("edit")
            .unwrap()
            .run(json!({"file_path": "sample.rs", "old_string": "v1", "new_string": "v2"}), &ctx)
            .await
            .unwrap();
        assert!(std::fs::read_to_string(&file).unwrap().contains("v2"));

        // 3. multi_edit 原子性：若第二处失败则第一处也不应写入
        let bad = reg
            .get("multi_edit")
            .unwrap()
            .run(
                json!({
                    "file_path": "sample.rs",
                    "edits": [
                        {"old_string": "v2", "new_string": "v3"},
                        {"old_string": "not_found", "new_string": "v4"}
                    ]
                }),
                &ctx,
            )
            .await;
        assert!(bad.is_err());
        assert!(std::fs::read_to_string(&file).unwrap().contains("v2"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}

