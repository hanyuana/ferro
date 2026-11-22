//! Phase 3：Hermes Agent 风格自我成长层（持久记忆、程序性技能 SKILL.md、跨会话历史检索）

pub mod history;
pub mod memory;
pub mod skills;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::FileTracker;
    use crate::tools::{Shell, Tool, ToolCtx};
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    #[tokio::test]
    async fn memory_and_skills_lifecycle() {
        let dir = std::env::temp_dir().join(format!("ferro_growth_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let ctx = ToolCtx {
            cwd: dir.clone(),
            tracker: Arc::new(Mutex::new(FileTracker::default())),
            todos: Arc::new(Mutex::new(Vec::new())),
            shell: Shell::detect(None),
        };

        // 1. memory add -> view -> replace -> remove
        let mem = memory::MemoryTool;
        mem.run(
            json!({"action": "add", "scope": "project", "content": "构建命令使用 cargo build --release"}),
            &ctx,
        )
        .await
        .unwrap();

        let prompt_mem = memory::load_memory_prompt(&dir);
        assert!(prompt_mem.contains("cargo build --release"));

        mem.run(
            json!({
                "action": "replace",
                "scope": "project",
                "old_text": "cargo build",
                "content": "构建与测试命令：cargo test && cargo build --release"
            }),
            &ctx,
        )
        .await
        .unwrap();

        let viewed = mem.run(json!({"action": "view", "scope": "project"}), &ctx).await.unwrap();
        assert!(viewed.content.contains("cargo test && cargo build --release"));

        // 2. skill_manage write -> discover_skills -> skill_view -> delete
        let sm = skills::SkillManageTool;
        sm.run(
            json!({
                "action": "write",
                "name": "rust-release-check",
                "description": "执行 Rust 项目的单元测试与 Release 体积检查",
                "instructions": "1. 运行 cargo test\n2. 运行 cargo build --release"
            }),
            &ctx,
        )
        .await
        .unwrap();

        let found = skills::discover_skills(&dir);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "rust-release-check");

        let sv = skills::SkillViewTool;
        let full = sv.run(json!({"name": "rust-release-check"}), &ctx).await.unwrap();
        assert!(full.content.contains("cargo build --release"));

        sm.run(json!({"action": "delete", "name": "rust-release-check"}), &ctx)
            .await
            .unwrap();
        assert!(skills::discover_skills(&dir).is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
