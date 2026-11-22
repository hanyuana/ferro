use super::*;
use serde_json::json;
use std::time::SystemTime;

pub struct Glob;

const LIMIT: usize = 100;

#[async_trait]
impl Tool for Glob {
    fn name(&self) -> &'static str {
        "glob"
    }
    fn description(&self) -> &'static str {
        "按 glob 模式快速查找文件（自动遵循 .gitignore），结果按修改时间倒序排列。\n\
         示例：`**/*.rs`、`src/**/*.ts`、`*.toml`"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "glob 模式" },
                "path": { "type": "string", "description": "搜索目录，默认为工作目录" }
            },
            "required": ["pattern"]
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    fn describe(&self, input: &Value, _ctx: &ToolCtx) -> Invocation {
        Invocation { title: input["pattern"].as_str().unwrap_or("").to_string(), ..Default::default() }
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput> {
        let pattern = arg_str(&input, "pattern")?.to_string();
        let root = opt_str(&input, "path")
            .map(|p| resolve_path(&ctx.cwd, p))
            .unwrap_or_else(|| ctx.cwd.clone());
        let cwd = ctx.cwd.clone();
        tokio::task::spawn_blocking(move || -> Result<ToolOutput> {
            let matcher = globset::GlobBuilder::new(pattern.trim_start_matches("./"))
                .literal_separator(false)
                .build()?
                .compile_matcher();
            let mut hits: Vec<(SystemTime, PathBuf)> = Vec::new();
            for entry in ignore::WalkBuilder::new(&root).build().flatten() {
                if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                    continue;
                }
                let rel = entry.path().strip_prefix(&root).unwrap_or(entry.path());
                let rel_s = rel.to_string_lossy().replace('\\', "/");
                if matcher.is_match(&rel_s) {
                    let mt = entry
                        .metadata()
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .unwrap_or(SystemTime::UNIX_EPOCH);
                    hits.push((mt, entry.into_path()));
                }
            }
            if hits.is_empty() {
                return Ok(ToolOutput::text("未找到匹配的文件"));
            }
            hits.sort_by(|a, b| b.0.cmp(&a.0));
            let total = hits.len();
            let mut out: Vec<String> =
                hits.into_iter().take(LIMIT).map(|(_, p)| display_path(&cwd, &p)).collect();
            if total > LIMIT {
                out.push(format!("\n(共 {} 个结果，仅显示前 {} 个，请使用更精确的模式)", total, LIMIT));
            }
            Ok(ToolOutput::text(out.join("\n")))
        })
        .await?
    }
}
