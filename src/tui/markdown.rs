//! 轻量级流式 Markdown 与 Unified Diff 终端渲染器（转为 ratatui Line/Span）

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// 将 Markdown 文本渲染为带样式的 ratatui 行列表
pub fn render_markdown(text: &str) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut in_code = false;
    let mut code_lang = String::new();

    for raw in text.lines() {
        let trimmed = raw.trim_start();
        if let Some(rest) = trimmed.strip_prefix("```") {
            if !in_code {
                in_code = true;
                code_lang = rest.trim().to_string();
                let header = if code_lang.is_empty() {
                    "  ┌── code ──".to_string()
                } else {
                    format!("  ┌── {} ──", code_lang)
                };
                lines.push(Line::from(Span::styled(header, Style::default().fg(Color::DarkGray))));
            } else {
                in_code = false;
                code_lang.clear();
                lines.push(Line::from(Span::styled(
                    "  └──────────".to_string(),
                    Style::default().fg(Color::DarkGray),
                )));
            }
            continue;
        }

        if in_code {
            let mut spans = vec![Span::styled("  │ ".to_string(), Style::default().fg(Color::DarkGray))];
            spans.extend(highlight_code_line(raw));
            lines.push(Line::from(spans));
            continue;
        }

        // 标题
        if let Some(h) = raw.strip_prefix("### ") {
            lines.push(Line::from(vec![
                Span::styled("▍ ".to_string(), Style::default().fg(Color::Cyan)),
                Span::styled(
                    h.to_string(),
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ),
            ]));
            continue;
        }
        if let Some(h) = raw.strip_prefix("## ") {
            lines.push(Line::from(vec![
                Span::styled("▌ ".to_string(), Style::default().fg(Color::Cyan)),
                Span::styled(
                    h.to_string(),
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                ),
            ]));
            continue;
        }
        if let Some(h) = raw.strip_prefix("# ") {
            lines.push(Line::from(vec![
                Span::styled("█ ".to_string(), Style::default().fg(Color::Cyan)),
                Span::styled(
                    h.to_string(),
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                ),
            ]));
            continue;
        }

        // 引用块
        if let Some(q) = trimmed.strip_prefix("> ") {
            let mut spans = vec![Span::styled("  ┃ ".to_string(), Style::default().fg(Color::DarkGray))];
            spans.extend(parse_inline(q, Style::default().fg(Color::Gray).add_modifier(Modifier::ITALIC)));
            lines.push(Line::from(spans));
            continue;
        }

        // 列表项
        if let Some(item) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")) {
            let indent = raw.len() - trimmed.len();
            let mut spans = vec![Span::styled(
                format!("{}• ", " ".repeat(indent)),
                Style::default().fg(Color::Cyan),
            )];
            spans.extend(parse_inline(item, Style::default()));
            lines.push(Line::from(spans));
            continue;
        }

        lines.push(Line::from(parse_inline(raw, Style::default())));
    }
    lines
}

/// 解析行内 `**粗体**` 与 `` `行内代码` ``
fn parse_inline(line: &str, base: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut rest = line;

    while !rest.is_empty() {
        let next_code = rest.find('`');
        let next_bold = rest.find("**");

        match (next_code, next_bold) {
            (Some(c), b) if b.map(|b| c <= b).unwrap_or(true) => {
                if c > 0 {
                    spans.push(Span::styled(rest[..c].to_string(), base));
                }
                let after = &rest[c + 1..];
                if let Some(end) = after.find('`') {
                    spans.push(Span::styled(
                        after[..end].to_string(),
                        Style::default().fg(Color::Yellow),
                    ));
                    rest = &after[end + 1..];
                } else {
                    spans.push(Span::styled(rest[c..].to_string(), base));
                    break;
                }
            }
            (_, Some(b)) => {
                if b > 0 {
                    spans.push(Span::styled(rest[..b].to_string(), base));
                }
                let after = &rest[b + 2..];
                if let Some(end) = after.find("**") {
                    spans.push(Span::styled(
                        after[..end].to_string(),
                        base.add_modifier(Modifier::BOLD),
                    ));
                    rest = &after[end + 2..];
                } else {
                    spans.push(Span::styled(rest[b..].to_string(), base));
                    break;
                }
            }
            _ => {
                spans.push(Span::styled(rest.to_string(), base));
                break;
            }
        }
    }
    if spans.is_empty() {
        spans.push(Span::raw(String::new()));
    }
    spans
}

/// 极简代码行语法着色（注释 / 字符串 / 关键字）
fn highlight_code_line(line: &str) -> Vec<Span<'static>> {
    let trimmed = line.trim_start();
    if trimmed.starts_with("//") || trimmed.starts_with('#') {
        return vec![Span::styled(line.to_string(), Style::default().fg(Color::DarkGray))];
    }
    vec![Span::styled(line.to_string(), Style::default().fg(Color::White))]
}

/// 将 Unified Diff 渲染为彩色行列表
pub fn render_diff(diff: &str, max_lines: usize) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let body: Vec<&str> = diff
        .lines()
        .filter(|l| !l.starts_with("---") && !l.starts_with("+++"))
        .collect();

    for l in body.iter().take(max_lines) {
        let style = if l.starts_with('+') {
            Style::default().fg(Color::Green)
        } else if l.starts_with('-') {
            Style::default().fg(Color::Red)
        } else if l.starts_with("@@") {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        out.push(Line::from(vec![
            Span::raw("    ".to_string()),
            Span::styled((*l).to_string(), style),
        ]));
    }
    if body.len() > max_lines {
        out.push(Line::from(Span::styled(
            format!("    ... 还有 {} 行变更", body.len() - max_lines),
            Style::default().fg(Color::DarkGray),
        )));
    }
    out
}
