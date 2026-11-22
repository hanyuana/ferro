use super::*;
use serde_json::json;

/// 在文本中执行一次替换。依次尝试：精确匹配 → CRLF 归一化 → 逐行去空白匹配
pub fn apply_edit(content: &str, old: &str, new: &str, replace_all: bool) -> Result<String> {
    if old == new {
        anyhow::bail!("old_string 与 new_string 相同，无需修改");
    }
    if old.is_empty() {
        anyhow::bail!("old_string 不能为空（创建新文件请使用 write）");
    }

    let try_exact = |old: &str, new: &str| -> Result<Option<String>> {
        let n = content.matches(old).count();
        match n {
            0 => Ok(None),
            1 => Ok(Some(content.replacen(old, new, 1))),
            _ if replace_all => Ok(Some(content.replace(old, new))),
            _ => anyhow::bail!(
                "old_string 在文件中出现了 {} 次。请提供更多上下文使其唯一，或设置 replace_all=true",
                n
            ),
        }
    };

    if let Some(r) = try_exact(old, new)? {
        return Ok(r);
    }
    if content.contains("\r\n") && !old.contains("\r\n") {
        let (o, n) = (old.replace('\n', "\r\n"), new.replace('\n', "\r\n"));
        if let Some(r) = try_exact(&o, &n)? {
            return Ok(r);
        }
    }

    // 逐行去除首尾空白后匹配（容忍缩进/行尾空格差异）
    let old_lines: Vec<&str> = old.trim_end_matches(['\n', '\r']).lines().map(str::trim).collect();
    if !old_lines.is_empty() {
        let mut spans = Vec::new();
        let mut offsets = Vec::new();
        let mut pos = 0;
        let lines: Vec<&str> = content.split_inclusive('\n').collect();
        for l in &lines {
            offsets.push(pos);
            pos += l.len();
        }
        offsets.push(pos);
        if lines.len() >= old_lines.len() {
            for start in 0..=(lines.len() - old_lines.len()) {
                if (0..old_lines.len()).all(|k| lines[start + k].trim() == old_lines[k]) {
                    let end_line = start + old_lines.len() - 1;
                    let s = offsets[start];
                    // 去掉最后一行的换行符，保留原文件的换行
                    let last = lines[end_line];
                    let trimmed_len = last.trim_end_matches(['\n', '\r']).len();
                    let e = offsets[end_line] + trimmed_len;
                    spans.push((s, e));
                }
            }
        }
        match spans.len() {
            0 => {}
            1 => {
                let (s, e) = spans[0];
                let mut new_text = new.trim_end_matches(['\n', '\r']).to_string();
                if content.contains("\r\n") && !new_text.contains("\r\n") {
                    new_text = new_text.replace('\n', "\r\n");
                }
                return Ok(format!("{}{}{}", &content[..s], new_text, &content[e..]));
            }
            n if !replace_all => anyhow::bail!("old_string（忽略空白后）匹配到 {} 处，请提供更多上下文", n),
            _ => {}
        }
    }
    anyhow::bail!("在文件中找不到 old_string。请先用 read 确认文件当前内容，并确保 old_string 与原文完全一致（包括缩进）")
}

fn load_for_edit(ctx: &ToolCtx, path: &Path) -> Result<String> {
    ctx.tracker
        .lock()
        .unwrap()
        .check_writable(path)
        .map_err(|e| anyhow!(e))?;
    std::fs::read_to_string(path).map_err(|e| anyhow!("无法读取 {}: {}", path.display(), e))
}

fn finish_write(ctx: &ToolCtx, path: &Path, old: &str, new: &str) -> Result<ToolOutput> {
    {
        let mut tr = ctx.tracker.lock().unwrap();
        tr.save_backup(path, Some(old.to_string()));
    }
    std::fs::write(path, new)?;
    ctx.tracker.lock().unwrap().record(path);
    let rel = display_path(&ctx.cwd, path);
    let (a, d) = diff_stats(old, new);
    let snippet = changed_context_snippet(old, new);
    let content = if snippet.is_empty() {
        format!("已修改 {}（+{} -{}）", rel, a, d)
    } else {
        format!("已修改 {}（+{} -{}）。修改处最新行号上下文：\n{}", rel, a, d, snippet)
    };
    Ok(ToolOutput {
        content,
        display: Some(unified_diff(&rel, old, new)),
    })
}

/// 提取修改位置附近带行号的最新代码片段，防止模型连续编辑时行号或上下文漂移
fn changed_context_snippet(old: &str, new: &str) -> String {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let first_diff = old_lines
        .iter()
        .zip(new_lines.iter())
        .position(|(a, b)| a != b)
        .unwrap_or(0);
    let start = first_diff.saturating_sub(2);
    let end = (first_diff + 12).min(new_lines.len());
    let mut out = String::new();
    for (idx, line) in new_lines[start..end].iter().enumerate() {
        out.push_str(&format!("{:>6}\t{}\n", start + idx + 1, line));
    }
    out
}

pub struct Edit;

#[async_trait]
impl Tool for Edit {
    fn name(&self) -> &'static str {
        "edit"
    }
    fn description(&self) -> &'static str {
        "对文件进行精确的字符串替换。\n\
         - 必须先用 read 读取过该文件\n\
         - old_string 必须与文件内容完全一致（包括缩进），且在文件中唯一；不唯一时请扩大上下文或设置 replace_all\n\
         - 不要包含 read 输出中的行号前缀\n\
         - 同一文件多处修改优先使用 multi_edit"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": { "type": "string", "description": "要修改的文件路径" },
                "old_string": { "type": "string", "description": "要被替换的原文" },
                "new_string": { "type": "string", "description": "替换后的新文本" },
                "replace_all": { "type": "boolean", "description": "替换所有匹配（默认 false）" }
            },
            "required": ["file_path", "old_string", "new_string"]
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Edit
    }
    fn describe(&self, input: &Value, ctx: &ToolCtx) -> Invocation {
        let path = resolve_path(&ctx.cwd, input["file_path"].as_str().unwrap_or(""));
        let rel = display_path(&ctx.cwd, &path);
        let preview = std::fs::read_to_string(&path).ok().and_then(|old| {
            apply_edit(
                &old,
                input["old_string"].as_str().unwrap_or(""),
                input["new_string"].as_str().unwrap_or(""),
                input["replace_all"].as_bool().unwrap_or(false),
            )
            .ok()
            .map(|new| unified_diff(&rel, &old, &new))
        });
        Invocation { title: rel, preview, paths: vec![path], ..Default::default() }
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput> {
        let path = resolve_path(&ctx.cwd, arg_str(&input, "file_path")?);
        let old = load_for_edit(ctx, &path)?;
        let new = apply_edit(
            &old,
            arg_str(&input, "old_string")?,
            arg_str(&input, "new_string")?,
            input["replace_all"].as_bool().unwrap_or(false),
        )?;
        finish_write(ctx, &path, &old, &new)
    }
}

pub struct MultiEdit;

impl MultiEdit {
    fn compute(old: &str, input: &Value) -> Result<String> {
        let edits = input["edits"]
            .as_array()
            .filter(|a| !a.is_empty())
            .ok_or_else(|| anyhow!("缺少参数 edits（非空数组）"))?;
        let mut cur = old.to_string();
        for (i, e) in edits.iter().enumerate() {
            cur = apply_edit(
                &cur,
                e["old_string"].as_str().unwrap_or(""),
                e["new_string"].as_str().unwrap_or(""),
                e["replace_all"].as_bool().unwrap_or(false),
            )
            .map_err(|err| anyhow!("第 {} 处修改失败（全部修改均未应用）: {}", i + 1, err))?;
        }
        Ok(cur)
    }
}

#[async_trait]
impl Tool for MultiEdit {
    fn name(&self) -> &'static str {
        "multi_edit"
    }
    fn description(&self) -> &'static str {
        "在同一个文件中一次性执行多处替换（原子操作：任何一处失败则全部不生效）。\n\
         - 各处修改按顺序应用，后一处基于前一处的结果\n\
         - 规则与 edit 相同：必须先 read，old_string 必须精确且唯一"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": { "type": "string" },
                "edits": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "old_string": { "type": "string" },
                            "new_string": { "type": "string" },
                            "replace_all": { "type": "boolean" }
                        },
                        "required": ["old_string", "new_string"]
                    }
                }
            },
            "required": ["file_path", "edits"]
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Edit
    }
    fn describe(&self, input: &Value, ctx: &ToolCtx) -> Invocation {
        let path = resolve_path(&ctx.cwd, input["file_path"].as_str().unwrap_or(""));
        let rel = display_path(&ctx.cwd, &path);
        let preview = std::fs::read_to_string(&path)
            .ok()
            .and_then(|old| Self::compute(&old, input).ok().map(|new| unified_diff(&rel, &old, &new)));
        Invocation { title: rel, preview, paths: vec![path], ..Default::default() }
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput> {
        let path = resolve_path(&ctx.cwd, arg_str(&input, "file_path")?);
        let old = load_for_edit(ctx, &path)?;
        let new = Self::compute(&old, &input)?;
        finish_write(ctx, &path, &old, &new)
    }
}

#[cfg(test)]
mod tests {
    use super::apply_edit;

    #[test]
    fn exact_and_unique() {
        assert_eq!(apply_edit("a b c", "b", "x", false).unwrap(), "a x c");
        assert!(apply_edit("b b", "b", "x", false).is_err());
        assert_eq!(apply_edit("b b", "b", "x", true).unwrap(), "x x");
    }

    #[test]
    fn crlf_normalized() {
        let src = "fn a() {\r\n    1\r\n}\r\n";
        let out = apply_edit(src, "    1\n}", "    2\n}", false).unwrap();
        assert_eq!(out, "fn a() {\r\n    2\r\n}\r\n");
    }

    #[test]
    fn whitespace_tolerant() {
        let src = "fn a() {\n    let x = 1;  \n    x\n}\n";
        let out = apply_edit(src, "let x = 1;\nx", "    let x = 2;\n    x", false).unwrap();
        assert_eq!(out, "fn a() {\n    let x = 2;\n    x\n}\n");
    }
}
