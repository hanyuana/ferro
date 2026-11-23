//! 权限引擎：Plan/Build 模式锁 + 最长匹配优先的 bash 规则 + 会话级"始终允许"

use crate::config::{Action, PermissionConfig};
use crate::tools::{Invocation, ToolKind};
use std::path::Path;

#[derive(Debug)]
pub enum Decision {
    Once,
    Always,
    /// 拒绝；附带用户反馈时 Agent 会继续，否则结束本轮
    Reject(Option<String>),
}

struct Rule {
    pattern: String,
    matcher: globset::GlobMatcher,
    action: Action,
}

impl Rule {
    fn new(pattern: &str, action: Action) -> Option<Self> {
        let matcher = globset::GlobBuilder::new(pattern)
            .literal_separator(false)
            .backslash_escape(false)
            .build()
            .ok()?
            .compile_matcher();
        Some(Self { pattern: pattern.to_string(), matcher, action })
    }

    fn specificity(&self) -> usize {
        self.pattern.chars().filter(|c| !matches!(c, '*' | '?')).count()
    }
}

pub struct Permissions {
    edit: Action,
    rules: Vec<Rule>,
    session_edit_allow: bool,
    full_access: bool,
}

fn severity(a: Action) -> u8 {
    match a {
        Action::Allow => 0,
        Action::Ask => 1,
        Action::Deny => 2,
    }
}

impl Permissions {
    pub fn new(cfg: &PermissionConfig) -> Self {
        let rules = cfg.bash.iter().filter_map(|(p, a)| Rule::new(p, *a)).collect();
        Self { edit: cfg.edit, rules, session_edit_allow: false, full_access: false }
    }

    /// 请求批准模式：修改文件与执行命令时始终询问
    pub fn ask_all() -> Self {
        let rules = Rule::new("*", Action::Ask).into_iter().collect();
        Self { edit: Action::Ask, rules, session_edit_allow: false, full_access: false }
    }

    /// 完全访问模式：直接放行文件编辑与命令执行，不弹窗打断
    pub fn allow_all() -> Self {
        Self { edit: Action::Allow, rules: Vec::new(), session_edit_allow: true, full_access: true }
    }

    pub fn check(&self, kind: ToolKind, inv: &Invocation, plan_mode: bool, cwd: &Path) -> Action {
        if self.full_access && !plan_mode {
            return Action::Allow;
        }
        match kind {
            ToolKind::Read | ToolKind::Meta => Action::Allow,
            ToolKind::Edit => {
                if plan_mode {
                    return Action::Deny;
                }
                if inv.paths.iter().any(|p| !p.starts_with(cwd)) {
                    return Action::Ask; // 修改工作目录之外的文件总是询问
                }
                if self.session_edit_allow {
                    Action::Allow
                } else {
                    self.edit
                }
            }
            ToolKind::Exec => {
                let cmd = inv.command.as_deref().unwrap_or("");
                let mut worst = Action::Allow;
                for part in split_commands(cmd) {
                    let (action, is_fallback) = self.match_bash(&part);
                    // Plan 模式下，只有被显式规则允许的命令才免询问
                    let action = if plan_mode && is_fallback && action == Action::Allow { Action::Ask } else { action };
                    if severity(action) > severity(worst) {
                        worst = action;
                    }
                }
                worst
            }
        }
    }

    /// 返回 (动作, 是否仅匹配到兜底规则)
    fn match_bash(&self, cmd: &str) -> (Action, bool) {
        let mut best: Option<&Rule> = None;
        for r in self.rules.iter().filter(|r| r.matcher.is_match(cmd)) {
            best = match best {
                None => Some(r),
                Some(b) => {
                    let (rs, bs) = (r.specificity(), b.specificity());
                    if rs > bs || (rs == bs && severity(r.action) > severity(b.action)) {
                        Some(r)
                    } else {
                        Some(b)
                    }
                }
            };
        }
        match best {
            Some(r) => (r.action, r.specificity() == 0),
            None => (Action::Ask, true),
        }
    }

    /// 用户选择"始终允许"后记录到会话规则
    pub fn remember(&mut self, kind: ToolKind, inv: &Invocation) {
        match kind {
            ToolKind::Edit => self.session_edit_allow = true,
            ToolKind::Exec => {
                for part in split_commands(inv.command.as_deref().unwrap_or("")) {
                    let pattern = always_pattern(&part);
                    if let Some(r) = Rule::new(&pattern, Action::Allow) {
                        self.rules.push(r);
                    }
                }
            }
            _ => {}
        }
    }
}

/// 生成"始终允许"的规则，如 `cargo test --lib` -> `cargo test*`
pub fn always_pattern(cmd: &str) -> String {
    let words: Vec<&str> = cmd.split_whitespace().collect();
    match words.as_slice() {
        [] => cmd.to_string(),
        [one] => format!("{}*", one),
        [a, b, ..] if !b.starts_with('-') => format!("{} {}*", a, b),
        [a, ..] => format!("{}*", a),
    }
}

/// 按 `&&` `||` `;` `|` 换行 拆分复合命令（识别引号）
pub fn split_commands(cmd: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let chars: Vec<char> = cmd.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if let Some(q) = quote {
            cur.push(c);
            if c == q {
                quote = None;
            } else if c == '\\' && q == '"' && i + 1 < chars.len() {
                cur.push(chars[i + 1]);
                i += 1;
            }
        } else {
            match c {
                '\'' | '"' => {
                    quote = Some(c);
                    cur.push(c);
                }
                ';' | '\n' | '|' | '&' => {
                    let is_sep = c != '&' || chars.get(i + 1) == Some(&'&');
                    if is_sep {
                        if (c == '|' || c == '&') && chars.get(i + 1) == Some(&c) {
                            i += 1;
                        }
                        if !cur.trim().is_empty() {
                            parts.push(cur.trim().to_string());
                        }
                        cur.clear();
                    } else {
                        cur.push(c);
                    }
                }
                _ => cur.push(c),
            }
        }
        i += 1;
    }
    if !cur.trim().is_empty() {
        parts.push(cur.trim().to_string());
    }
    if parts.is_empty() {
        parts.push(String::new());
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn perms(rules: &[(&str, Action)]) -> Permissions {
        let mut bash = BTreeMap::new();
        for (p, a) in rules {
            bash.insert(p.to_string(), *a);
        }
        Permissions::new(&PermissionConfig { edit: Action::Allow, bash })
    }

    fn bash(cmd: &str) -> Invocation {
        Invocation { command: Some(cmd.into()), ..Default::default() }
    }

    #[test]
    fn split() {
        assert_eq!(split_commands("a && b | c; d"), vec!["a", "b", "c", "d"]);
        assert_eq!(split_commands("echo 'a && b'"), vec!["echo 'a && b'"]);
        assert_eq!(split_commands("cmd 2>&1"), vec!["cmd 2>&1"]);
    }

    #[test]
    fn longest_match_wins() {
        let p = perms(&[("*", Action::Ask), ("git status*", Action::Allow), ("rm -rf *", Action::Deny)]);
        let cwd = Path::new("/");
        assert_eq!(p.check(ToolKind::Exec, &bash("git status"), false, cwd), Action::Allow);
        assert_eq!(p.check(ToolKind::Exec, &bash("ls"), false, cwd), Action::Ask);
        assert_eq!(p.check(ToolKind::Exec, &bash("git status && rm -rf /"), false, cwd), Action::Deny);
    }

    #[test]
    fn plan_mode_asks_on_fallback() {
        let p = perms(&[("*", Action::Allow), ("git diff*", Action::Allow)]);
        let cwd = Path::new("/");
        assert_eq!(p.check(ToolKind::Exec, &bash("cargo build"), true, cwd), Action::Ask);
        assert_eq!(p.check(ToolKind::Exec, &bash("git diff"), true, cwd), Action::Allow);
    }

    #[test]
    fn always() {
        assert_eq!(always_pattern("cargo test --lib"), "cargo test*");
        assert_eq!(always_pattern("ls -la"), "ls*");
    }
}
