//! Hermes 风格技能系统（程序性记忆 `SKILL.md`）：
//! - 兼容目录：`<cwd>/.ferro/skills/*/SKILL.md`、`<cwd>/.claude/skills/*/SKILL.md`、`<config_dir>/ferro/skills/*/SKILL.md`
//! - 渐进式披露：System Prompt 仅注入技能名与一句话描述，按需通过 `skill_view` 读取全文
//! - 自我沉淀：Agent 完成复杂任务后可通过 `skill_manage` 创建或更新可复用的 `SKILL.md`

use crate::config::global_dir;
use crate::tools::{arg_str, opt_str, unified_diff, Invocation, Tool, ToolCtx, ToolKind, ToolOutput};
use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct SkillMeta {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    pub source: &'static str,
}

/// 扫描全局与项目技能目录（项目级同名技能覆盖全局）
pub fn discover_skills(cwd: &Path) -> Vec<SkillMeta> {
    let mut map: BTreeMap<String, SkillMeta> = BTreeMap::new();
    let roots: [(PathBuf, &'static str); 3] = [
        (global_dir().join("skills"), "global"),
        (cwd.join(".claude").join("skills"), ".claude"),
        (cwd.join(".ferro").join("skills"), ".ferro"),
    ];

    for (root, source) in roots {
        let Ok(rd) = std::fs::read_dir(&root) else { continue };
        for entry in rd.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let skill_md = dir.join("SKILL.md");
            if !skill_md.is_file() {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&skill_md) else { continue };
            let fallback = dir
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "unnamed".into());
            let (name, description) = parse_frontmatter(&text, &fallback);
            map.insert(
                name.clone(),
                SkillMeta {
                    name,
                    description,
                    path: skill_md,
                    source,
                },
            );
        }
    }
    map.into_values().collect()
}

/// 解析 YAML frontmatter 中的 name 与 description
fn parse_frontmatter(text: &str, fallback_name: &str) -> (String, String) {
    let mut name = fallback_name.to_string();
    let mut desc = String::new();
    let trimmed = text.trim_start();
    if let Some(after_open) = trimmed.strip_prefix("---") {
        if let Some(end) = after_open.find("\n---") {
            let fm = &after_open[..end];
            for line in fm.lines() {
                let line = line.trim();
                if let Some(v) = line.strip_prefix("name:") {
                    let v = v.trim().trim_matches(['"', '\'']);
                    if !v.is_empty() {
                        name = v.to_string();
                    }
                } else if let Some(v) = line.strip_prefix("description:") {
                    let v = v.trim().trim_matches(['"', '\'']);
                    if !v.is_empty() {
                        desc = v.to_string();
                    }
                }
            }
        }
    }
    if desc.is_empty() {
        desc = text
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with("---") && !l.starts_with('#'))
            .unwrap_or("可复用技能工作流")
            .chars()
            .take(120)
            .collect();
    }
    (name, desc)
}

/// 生成注入 System Prompt 的轻量技能索引（渐进式披露）
pub fn load_skills_prompt(cwd: &Path) -> String {
    let skills = discover_skills(cwd);
    if skills.is_empty() {
        return String::new();
    }
    let mut s = String::from(
        "\n# Available Skills (Progressive Disclosure)\n\
         Call `skill_view` with a skill's name to read its full instructions before executing a matching workflow:\n",
    );
    for sk in skills {
        s.push_str(&format!("- `{}` ({}): {}\n", sk.name, sk.source, sk.description));
    }
    s
}

pub struct SkillListTool;

#[async_trait]
impl Tool for SkillListTool {
    fn name(&self) -> &'static str {
        "skill_list"
    }
    fn description(&self) -> &'static str {
        "列出所有可用的技能（包括全局技能、项目 .ferro/skills 与 .claude/skills）。"
    }
    fn schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    fn describe(&self, _input: &Value, _ctx: &ToolCtx) -> Invocation {
        Invocation { title: "all".into(), ..Default::default() }
    }
    async fn run(&self, _input: Value, ctx: &ToolCtx) -> Result<ToolOutput> {
        let skills = discover_skills(&ctx.cwd);
        if skills.is_empty() {
            return Ok(ToolOutput::text("当前没有已安装的技能。可使用 skill_manage 创建新技能。"));
        }
        let mut out = format!("共发现 {} 个技能：\n", skills.len());
        for sk in skills {
            out.push_str(&format!(
                "- {} [{}]: {}\n  路径: {}\n",
                sk.name,
                sk.source,
                sk.description,
                sk.path.display()
            ));
        }
        Ok(ToolOutput::text(out))
    }
}

pub struct SkillViewTool;

#[async_trait]
impl Tool for SkillViewTool {
    fn name(&self) -> &'static str {
        "skill_view"
    }
    fn description(&self) -> &'static str {
        "读取指定技能的完整 SKILL.md 指令内容及同目录下的辅助文件列表。"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "技能名称" }
            },
            "required": ["name"]
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    fn describe(&self, input: &Value, _ctx: &ToolCtx) -> Invocation {
        Invocation {
            title: input["name"].as_str().unwrap_or("").to_string(),
            ..Default::default()
        }
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput> {
        let name = arg_str(&input, "name")?.trim();
        let skills = discover_skills(&ctx.cwd);
        let sk = skills
            .into_iter()
            .find(|s| s.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| anyhow!("未找到名为 `{}` 的技能，请先用 skill_list 查看", name))?;
        let content = std::fs::read_to_string(&sk.path)?;
        Ok(ToolOutput::text(format!(
            "# Skill: {} ({})\n\n{}",
            sk.name,
            sk.path.display(),
            content
        )))
    }
}

pub struct SkillManageTool;

#[async_trait]
impl Tool for SkillManageTool {
    fn name(&self) -> &'static str {
        "skill_manage"
    }
    fn description(&self) -> &'static str {
        "创建、更新或删除可复用技能（SKILL.md，灵感源自 Hermes Agent 自我成长闭环）。\n\
         - 当你成功解决了一个复杂问题或总结出标准化的项目工作流时，可将其沉淀为技能\n\
         - action: `write`（创建或覆盖）| `delete`（删除）\n\
         - scope: `project`（默认，保存在 .ferro/skills/<name>/SKILL.md）| `global`"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["write", "delete"] },
                "name": { "type": "string", "description": "技能标识符（仅限字母、数字、连字符，如 rust-ci-check）" },
                "description": { "type": "string", "description": "一句话描述该技能的用途与触发场景" },
                "instructions": { "type": "string", "description": "技能的 Markdown 详细执行步骤（无需包含 frontmatter，会自动生成）" },
                "scope": { "type": "string", "enum": ["project", "global"] }
            },
            "required": ["action", "name"]
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Edit
    }
    fn describe(&self, input: &Value, _ctx: &ToolCtx) -> Invocation {
        let act = input["action"].as_str().unwrap_or("write");
        let name = input["name"].as_str().unwrap_or("");
        Invocation { title: format!("{}:{}", act, name), ..Default::default() }
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput> {
        let action = arg_str(&input, "action")?;
        let name = arg_str(&input, "name")?.trim();
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            bail!("技能名称 `{}` 不合法，请仅使用 ASCII 字母、数字、`-` 或 `_`", name);
        }
        let base = match opt_str(&input, "scope").unwrap_or("project") {
            "global" => global_dir().join("skills"),
            _ => ctx.cwd.join(".ferro").join("skills"),
        };
        let skill_dir = base.join(name);
        let skill_file = skill_dir.join("SKILL.md");

        match action {
            "write" => {
                let desc = arg_str(&input, "description")?.trim();
                let body = arg_str(&input, "instructions")?.trim();
                let full = format!("---\nname: {}\ndescription: {}\n---\n\n{}\n", name, desc, body);
                let old = std::fs::read_to_string(&skill_file).unwrap_or_default();
                std::fs::create_dir_all(&skill_dir)?;
                std::fs::write(&skill_file, &full)?;
                Ok(ToolOutput {
                    content: format!("已保存技能 `{}` 到 {}", name, skill_file.display()),
                    display: Some(unified_diff(&format!("skills/{}/SKILL.md", name), &old, &full)),
                })
            }
            "delete" => {
                if !skill_dir.exists() {
                    bail!("技能目录不存在: {}", skill_dir.display());
                }
                std::fs::remove_dir_all(&skill_dir)?;
                Ok(ToolOutput::text(format!("已删除技能 `{}`", name)))
            }
            other => bail!("未知的 action `{}`", other),
        }
    }
}
