use super::*;
use serde_json::json;
use std::time::SystemTime;

pub struct Grep;

const MAX_MATCHES: usize = 100;
const MAX_FILE: u64 = 2 * 1024 * 1024;

#[async_trait]
impl Tool for Grep {
    fn name(&self) -> &'static str {
        "grep"
    }
    fn description(&self) -> &'static str {
        "用正则表达式搜索文件内容（内置 ripgrep 同源引擎，自动遵循 .gitignore）。\n\
         - pattern 为 Rust 正则语法，如 `fn\\s+main`、`TODO|FIXME`\n\
         - include 用于过滤文件，如 `*.rs`、`*.{ts,tsx}`\n\
         - 返回匹配的文件、行号与内容，按文件修改时间倒序"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "正则表达式" },
                "path": { "type": "string", "description": "搜索的目录或文件，默认为工作目录" },
                "include": { "type": "string", "description": "文件名 glob 过滤" },
                "ignore_case": { "type": "boolean", "description": "忽略大小写" }
            },
            "required": ["pattern"]
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    fn describe(&self, input: &Value, _ctx: &ToolCtx) -> Invocation {
        let mut t = input["pattern"].as_str().unwrap_or("").to_string();
        if let Some(inc) = input["include"].as_str() {
            t.push_str(&format!("  ({})", inc));
        }
        Invocation { title: t, ..Default::default() }
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput> {
        let re = regex::RegexBuilder::new(arg_str(&input, "pattern")?)
            .case_insensitive(input["ignore_case"].as_bool().unwrap_or(false))
            .build()
            .map_err(|e| anyhow!("正则表达式无效: {}", e))?;
        let include = match opt_str(&input, "include") {
            Some(g) => Some(globset::Glob::new(g)?.compile_matcher()),
            None => None,
        };
        let root = opt_str(&input, "path")
            .map(|p| resolve_path(&ctx.cwd, p))
            .unwrap_or_else(|| ctx.cwd.clone());
        let cwd = ctx.cwd.clone();

        tokio::task::spawn_blocking(move || -> Result<ToolOutput> {
            let mut files: Vec<(SystemTime, PathBuf, Vec<(usize, String)>)> = Vec::new();
            let mut count = 0usize;
            let mut truncated = false;
            for entry in ignore::WalkBuilder::new(&root).build().flatten() {
                if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                    continue;
                }
                if let Some(m) = &include {
                    if !m.is_match(entry.file_name()) {
                        continue;
                    }
                }
                let Ok(meta) = entry.metadata() else { continue };
                if meta.len() > MAX_FILE {
                    continue;
                }
                let Ok(bytes) = std::fs::read(entry.path()) else { continue };
                if bytes.iter().take(8192).any(|&b| b == 0) {
                    continue;
                }
                let text = String::from_utf8_lossy(&bytes);
                let mut hits = Vec::new();
                for (i, line) in text.lines().enumerate() {
                    if re.is_match(line) {
                        let l = line.trim_end();
                        let shown: String = if l.chars().count() > 300 {
                            l.chars().take(300).collect::<String>() + "..."
                        } else {
                            l.to_string()
                        };
                        hits.push((i + 1, shown));
                        count += 1;
                        if count >= MAX_MATCHES {
                            truncated = true;
                            break;
                        }
                    }
                }
                if !hits.is_empty() {
                    files.push((meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), entry.into_path(), hits));
                }
                if truncated {
                    break;
                }
            }
            if files.is_empty() {
                return Ok(ToolOutput::text("未找到匹配"));
            }
            files.sort_by(|a, b| b.0.cmp(&a.0));
            let mut out = format!("找到 {} 处匹配\n", count);
            for (_, p, hits) in files {
                out.push_str(&format!("\n{}:\n", display_path(&cwd, &p)));
                for (n, l) in hits {
                    out.push_str(&format!("  {}: {}\n", n, l));
                }
            }
            if truncated {
                out.push_str(&format!("\n(结果已截断为前 {} 处，请缩小搜索范围)\n", MAX_MATCHES));
            }
            Ok(ToolOutput::text(out))
        })
        .await?
    }
}
