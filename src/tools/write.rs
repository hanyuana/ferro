use super::*;
use serde_json::json;

pub struct Write;

#[async_trait]
impl Tool for Write {
    fn name(&self) -> &'static str {
        "write"
    }
    fn description(&self) -> &'static str {
        "将内容完整写入文件（不存在则创建，存在则覆盖）。\n\
         - 覆盖已有文件前必须先用 read 读取\n\
         - 修改已有文件优先使用 edit / multi_edit，只在新建文件或整体重写时使用 write\n\
         - 会自动创建不存在的父目录"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": { "type": "string", "description": "文件路径" },
                "content": { "type": "string", "description": "完整的文件内容" }
            },
            "required": ["file_path", "content"]
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Edit
    }
    fn describe(&self, input: &Value, ctx: &ToolCtx) -> Invocation {
        let path = resolve_path(&ctx.cwd, input["file_path"].as_str().unwrap_or(""));
        let rel = display_path(&ctx.cwd, &path);
        let old = std::fs::read_to_string(&path).unwrap_or_default();
        let preview = Some(unified_diff(&rel, &old, input["content"].as_str().unwrap_or("")));
        Invocation { title: rel, preview, paths: vec![path], ..Default::default() }
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput> {
        let path = resolve_path(&ctx.cwd, arg_str(&input, "file_path")?);
        let content = arg_str(&input, "content")?;
        ctx.tracker
            .lock()
            .unwrap()
            .check_writable(&path)
            .map_err(|e| anyhow!(e))?;
        let existed = path.exists();
        let old = if existed { std::fs::read_to_string(&path).unwrap_or_default() } else { String::new() };
        ctx.tracker
            .lock()
            .unwrap()
            .save_backup(&path, if existed { Some(old.clone()) } else { None });
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, content)?;
        ctx.tracker.lock().unwrap().record(&path);
        let rel = display_path(&ctx.cwd, &path);
        let (a, d) = diff_stats(&old, content);
        Ok(ToolOutput {
            content: if existed {
                format!("已覆盖 {}（+{} -{}）", rel, a, d)
            } else {
                format!("已创建 {}（{} 行）", rel, content.lines().count())
            },
            display: Some(unified_diff(&rel, &old, content)),
        })
    }
}
