use super::*;
use serde_json::json;
use std::collections::BTreeMap;

pub struct Ls;

const LIMIT: usize = 200;

#[derive(Default)]
struct Node {
    children: BTreeMap<String, Node>,
    is_dir: bool,
}

fn render(node: &Node, indent: usize, out: &mut String) {
    // 目录在前
    let mut entries: Vec<(&String, &Node)> = node.children.iter().collect();
    entries.sort_by_key(|(name, n)| (!n.is_dir, name.to_lowercase()));
    for (name, child) in entries {
        out.push_str(&"  ".repeat(indent));
        out.push_str(name);
        if child.is_dir {
            out.push('/');
        }
        out.push('\n');
        render(child, indent + 1, out);
    }
}

#[async_trait]
impl Tool for Ls {
    fn name(&self) -> &'static str {
        "ls"
    }
    fn description(&self) -> &'static str {
        "以树形结构列出目录内容（自动遵循 .gitignore）。用于快速了解项目结构。"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "目录路径，默认为工作目录" },
                "depth": { "type": "integer", "description": "递归深度，默认 2" }
            }
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    fn describe(&self, input: &Value, ctx: &ToolCtx) -> Invocation {
        let p = input["path"].as_str().unwrap_or(".");
        Invocation { title: display_path(&ctx.cwd, &resolve_path(&ctx.cwd, p)), ..Default::default() }
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput> {
        let root = opt_str(&input, "path")
            .map(|p| resolve_path(&ctx.cwd, p))
            .unwrap_or_else(|| ctx.cwd.clone());
        let depth = input["depth"].as_u64().unwrap_or(2).clamp(1, 6) as usize;
        if !root.is_dir() {
            anyhow::bail!("{} 不是目录", root.display());
        }
        tokio::task::spawn_blocking(move || -> Result<ToolOutput> {
            let mut tree = Node { is_dir: true, ..Default::default() };
            let mut n = 0;
            let mut truncated = false;
            for entry in ignore::WalkBuilder::new(&root).max_depth(Some(depth)).build().flatten() {
                if entry.depth() == 0 {
                    continue;
                }
                if n >= LIMIT {
                    truncated = true;
                    break;
                }
                n += 1;
                let rel = entry.path().strip_prefix(&root).unwrap_or(entry.path());
                let mut cur = &mut tree;
                for c in rel.components() {
                    cur = cur.children.entry(c.as_os_str().to_string_lossy().to_string()).or_default();
                }
                cur.is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            }
            let mut out = format!("{}/\n", root.display());
            render(&tree, 1, &mut out);
            if truncated {
                out.push_str(&format!("\n(仅显示前 {} 项)\n", LIMIT));
            }
            Ok(ToolOutput::text(out))
        })
        .await?
    }
}
