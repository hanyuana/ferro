//! 系统提示词构建：静态部分保持稳定以最大化 Prompt Cache 命中；
//! 易变信息（模式切换等）通过 <system-reminder> 附加在用户消息中。

use crate::tools::Shell;
use std::path::{Path, PathBuf};

const BASE: &str = r#"You are Ferro, a fast, native AI coding agent running in the user's terminal. You help with software engineering tasks: writing code, fixing bugs, refactoring, explaining code, running commands.

# Tone and style
- Respond in the same language the user writes in.
- Be concise and direct. Output is shown in a terminal; use GitHub-flavored Markdown sparingly.
- Do not add preamble or postamble ("Sure!", "Here is..."). After finishing a task, give a short summary of what changed.
- Never invent URLs, file paths or APIs. If unsure, inspect the code first.

# Doing tasks
1. Understand: use glob / grep / ls / read to explore the codebase before changing it. Read files before editing them.
2. Plan: for multi-step tasks (3+ steps), use todo_write to track progress; keep exactly one item in_progress and mark items completed immediately.
3. Implement: follow the existing code style, conventions, libraries and patterns of the project. Prefer editing existing files over creating new ones. Make minimal, focused changes.
4. Verify: when possible run the project's build / lint / tests with bash and fix the problems you introduced.

# Tool usage
- Call multiple independent read-only tools (read, glob, grep, ls) in a single response; they run in parallel.
- Use edit / multi_edit for changes to existing files; use write only for new files or full rewrites.
- Use bash for builds, tests, git and package managers — never for reading, searching or editing files.
- Do not commit, push, or perform destructive git operations unless the user explicitly asks.
- If the user rejects a tool call, do not retry the same action; adjust based on their feedback or ask.
- Reference code locations as `path:line` so the user can navigate to them.
- Memory & Skills: use `memory` to record durable project conventions or user preferences; use `skill_view` / `skill_manage` to read or save reusable workflows (`SKILL.md`); use `session_search` when the user references past conversations.

# Modes
- BUILD mode: you may modify files and run commands.
- PLAN mode: read-only. Explore and analyze, then present a concrete plan. Do NOT modify files (edit tools are unavailable). Only run commands that do not change state."#;

const PROJECT_FILES: [&str; 3] = ["AGENTS.md", "CLAUDE.md", "FERRO.md"];

pub fn build_system_prompt(cwd: &Path, shell: &Shell) -> String {
    let mut s = String::from(BASE);
    let git_root = find_git_root(cwd);
    s.push_str("\n\n# Environment\n");
    s.push_str(&format!("- Working directory: {}\n", cwd.display()));
    s.push_str(&format!("- Platform: {} ({})\n", std::env::consts::OS, std::env::consts::ARCH));
    s.push_str(&format!("- Shell used by the bash tool: {}\n", shell.describe()));
    s.push_str(&format!("- Git repository: {}\n", if git_root.is_some() { "yes" } else { "no" }));
    s.push_str(&format!("- Today's date: {}\n", today()));

    let instructions = load_instructions(cwd, git_root.as_deref());
    if !instructions.is_empty() {
        s.push_str("\n# Project instructions\nThe following instruction files were provided by the user. Follow them.\n");
        for (path, text) in instructions {
            s.push_str(&format!("\n<instructions path=\"{}\">\n{}\n</instructions>\n", path.display(), text.trim()));
        }
    }
    s.push_str(&crate::growth::skills::load_skills_prompt(cwd));
    s.push_str(&crate::growth::memory::load_memory_prompt(cwd));
    s
}

pub fn mode_reminder(plan: bool) -> String {
    if plan {
        "<system-reminder>PLAN mode is active. You are in read-only mode: do not modify any files and do not run commands that change state. Investigate and present a plan.</system-reminder>".into()
    } else {
        "<system-reminder>BUILD mode is active. You may now modify files and run commands.</system-reminder>".into()
    }
}

fn find_git_root(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors().find(|p| p.join(".git").exists()).map(Path::to_path_buf)
}

/// 全局指令 + 从 git 根目录到 cwd 路径上的所有指令文件（每个目录取第一个存在的）
fn load_instructions(cwd: &Path, git_root: Option<&Path>) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let global = crate::config::global_dir();
    for name in PROJECT_FILES {
        let p = global.join(name);
        if let Ok(t) = std::fs::read_to_string(&p) {
            out.push((p, t));
            break;
        }
    }
    let stop = git_root.unwrap_or(cwd);
    let mut dirs: Vec<&Path> = cwd.ancestors().take_while(|p| p.starts_with(stop)).collect();
    dirs.reverse();
    for d in dirs {
        for name in PROJECT_FILES {
            let p = d.join(name);
            if let Ok(t) = std::fs::read_to_string(&p) {
                out.push((p, t));
                break;
            }
        }
    }
    out
}

fn today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    // civil_from_days（Howard Hinnant 算法）
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{:04}-{:02}-{:02}", y, m, d)
}
