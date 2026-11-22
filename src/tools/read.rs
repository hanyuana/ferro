use super::*;
use serde_json::json;

pub struct Read;

const DEFAULT_LIMIT: usize = 2000;
const MAX_LINE: usize = 2000;
const MAX_BYTES: usize = 250 * 1024;

#[async_trait]
impl Tool for Read {
    fn name(&self) -> &'static str {
        "read"
    }
    fn description(&self) -> &'static str {
        "读取文件内容，返回带行号的文本（格式：行号<TAB>内容）。\n\
         - 默认从第 1 行读取最多 2000 行，可用 offset/limit 分段读取大文件\n\
         - 修改已有文件之前必须先读取它\n\
         - 可以在一次回复中并行调用多次 read 读取多个文件\n\
         - 读取目录请使用 ls"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": { "type": "string", "description": "文件路径（绝对路径或相对于工作目录）" },
                "offset": { "type": "integer", "description": "起始行号（从 1 开始）" },
                "limit": { "type": "integer", "description": "读取的最大行数" }
            },
            "required": ["file_path"]
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    fn describe(&self, input: &Value, ctx: &ToolCtx) -> Invocation {
        let p = input["file_path"].as_str().unwrap_or("?");
        Invocation { title: display_path(&ctx.cwd, &resolve_path(&ctx.cwd, p)), ..Default::default() }
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput> {
        let path = resolve_path(&ctx.cwd, arg_str(&input, "file_path")?);
        let meta = tokio::fs::metadata(&path)
            .await
            .map_err(|_| anyhow!("文件不存在: {}", path.display()))?;
        if meta.is_dir() {
            anyhow::bail!("{} 是目录，请使用 ls 工具", path.display());
        }
        let bytes = tokio::fs::read(&path).await?;
        if bytes.iter().take(8192).any(|&b| b == 0) {
            anyhow::bail!("{} 是二进制文件，无法以文本读取", path.display());
        }
        let text = String::from_utf8_lossy(&bytes);
        let offset = input["offset"].as_u64().unwrap_or(1).max(1) as usize;
        let limit = input["limit"].as_u64().map(|v| v as usize).unwrap_or(DEFAULT_LIMIT);

        let total = text.lines().count();
        let mut out = String::new();
        let mut last = offset.saturating_sub(1);
        for (i, line) in text.lines().enumerate().skip(offset - 1).take(limit) {
            let line = line.strip_suffix('\r').unwrap_or(line);
            let shown: String = if line.chars().count() > MAX_LINE {
                line.chars().take(MAX_LINE).collect::<String>() + "...[行已截断]"
            } else {
                line.to_string()
            };
            out.push_str(&format!("{:>6}\t{}\n", i + 1, shown));
            last = i + 1;
            if out.len() > MAX_BYTES {
                break;
            }
        }
        if total == 0 {
            out.push_str("(空文件)\n");
        } else if last < total {
            out.push_str(&format!(
                "\n(文件共 {} 行，已显示到第 {} 行。使用 offset={} 继续读取)\n",
                total,
                last,
                last + 1
            ));
        }
        ctx.tracker.lock().unwrap().record(&path);
        Ok(ToolOutput::text(out))
    }
}
