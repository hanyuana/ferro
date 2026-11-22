//! ratatui 视图渲染：顶部栏、消息流、侧边 Todo 面板、自适应输入框、底栏与悬浮弹窗

use super::app::{AppState, ChatEntry, PendingPermission};
use super::markdown::{render_diff, render_markdown};
use crate::agent::Mode;
use crate::session::{Todo, TodoStatus};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn draw(f: &mut Frame, state: &mut AppState, todos: &[Todo]) {
    let area = f.area();
    let input_lines = state.input.lines().count().max(1).clamp(1, 6) as u16;
    let input_height = input_lines + 2;

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),            // 顶部标题栏
            Constraint::Min(5),               // 中部对话流 (+ 可选 Todo 侧栏)
            Constraint::Length(input_height), // 底部多行输入框
            Constraint::Length(1),            // 底部状态栏
        ])
        .split(area);

    draw_header(f, chunks[0], state, todos);

    // 如果有 Todo 且终端宽度充裕，右侧分栏显示实时 Todo 进度卡片
    if !todos.is_empty() && chunks[1].width >= 85 {
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(73), Constraint::Percentage(27)])
            .split(chunks[1]);
        draw_chat(f, body[0], state);
        draw_todo_pane(f, body[1], todos);
    } else {
        draw_chat(f, chunks[1], state);
    }

    draw_input(f, chunks[2], state);
    draw_status_bar(f, chunks[3], state);

    // 悬浮补全菜单（@文件 或 /命令）
    if state.pending_perm.is_none() {
        draw_autocomplete_popup(f, chunks[2], state);
    }

    // 权限审批模态弹窗
    if let Some(perm) = &state.pending_perm {
        draw_permission_modal(f, area, perm);
    }
}

fn draw_header(f: &mut Frame, area: Rect, state: &AppState, todos: &[Todo]) {
    let done = todos.iter().filter(|t| t.status == TodoStatus::Completed).count();
    let todo_badge = if todos.is_empty() {
        String::new()
    } else {
        format!("  ✓ 待办 {}/{} ", done, todos.len())
    };

    let spans = vec![
        Span::styled(
            " ◆ Ferro ",
            Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" v{} ", env!("CARGO_PKG_VERSION")),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled("│ ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            state.model_ref.clone(),
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            state.cwd.display().to_string(),
            Style::default().fg(Color::Gray),
        ),
        Span::styled(todo_badge, Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
    ];
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_chat(f: &mut Frame, area: Rect, state: &mut AppState) {
    let mut lines: Vec<Line<'static>> = Vec::new();

    for entry in &state.entries {
        match entry {
            ChatEntry::User(text) => {
                lines.push(Line::raw(""));
                for (i, l) in text.lines().enumerate() {
                    let prefix = if i == 0 { "❯ " } else { "  " };
                    lines.push(Line::from(vec![
                        Span::styled(
                            prefix.to_string(),
                            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            l.to_string(),
                            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
                        ),
                    ]));
                }
            }
            ChatEntry::Reasoning(text) => {
                let count = text.lines().count().max(1);
                if state.show_reasoning {
                    lines.push(Line::from(Span::styled(
                        "✻ 思考过程 (Ctrl+T 折叠)".to_string(),
                        Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
                    )));
                    for l in text.lines() {
                        lines.push(Line::from(Span::styled(
                            format!("  {}", l),
                            Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
                        )));
                    }
                } else {
                    lines.push(Line::from(Span::styled(
                        format!("✻ 已思考 ({} 行，按 Ctrl+T 展开)", count),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
            }
            ChatEntry::Assistant(text) => {
                lines.push(Line::raw(""));
                lines.extend(render_markdown(text));
            }
            ChatEntry::ToolCall { name, title } => {
                lines.push(Line::from(vec![
                    Span::styled("● ".to_string(), Style::default().fg(Color::Cyan)),
                    Span::styled(
                        tool_label(name),
                        Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!("({})", title), Style::default().fg(Color::DarkGray)),
                ]));
            }
            ChatEntry::ToolResult { name, ok, summary, display } => {
                let summary_style = if *ok {
                    Style::default().fg(Color::DarkGray)
                } else {
                    Style::default().fg(Color::Red)
                };
                lines.push(Line::from(vec![
                    Span::styled("  ⎿ ".to_string(), Style::default().fg(Color::DarkGray)),
                    Span::styled(summary.clone(), summary_style),
                ]));
                if *ok {
                    if let Some(disp) = display {
                        if name != "todo_write" {
                            lines.extend(render_diff(disp, 40));
                        }
                    }
                }
            }
            ChatEntry::System(msg) => {
                lines.push(Line::from(Span::styled(
                    format!("  • {}", msg),
                    Style::default().fg(Color::Yellow),
                )));
            }
        }
    }

    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);

    // 估算自动换行的总视觉行数，实现平滑自动滚底
    let width = inner.width.max(1) as usize;
    let total_visual_lines: usize = lines
        .iter()
        .map(|l| {
            let w: usize = l.spans.iter().map(|s| UnicodeWidthStr::width(s.content.as_ref())).sum();
            if w == 0 { 1 } else { (w + width - 1) / width }
        })
        .sum();
    let max_scroll = total_visual_lines.saturating_sub(inner.height as usize) as u16;
    if state.auto_scroll || state.scroll > max_scroll {
        state.scroll = max_scroll;
    }

    let p = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((state.scroll, 0));
    f.render_widget(p, area);
}

fn draw_todo_pane(f: &mut Frame, area: Rect, todos: &[Todo]) {
    let mut lines = Vec::new();
    for t in todos {
        let (icon, style) = match t.status {
            TodoStatus::Completed => (
                "✓ ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::CROSSED_OUT),
            ),
            TodoStatus::InProgress => (
                "▶ ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            TodoStatus::Pending => ("○ ", Style::default().fg(Color::Gray)),
            TodoStatus::Cancelled => ("✕ ", Style::default().fg(Color::DarkGray)),
        };
        lines.push(Line::from(vec![
            Span::styled(icon.to_string(), style),
            Span::styled(t.content.clone(), style),
        ]));
    }
    let block = Block::default()
        .title(" 待办进度 (Todo) ")
        .borders(Borders::TOP | Borders::LEFT)
        .border_style(Style::default().fg(Color::DarkGray));
    f.render_widget(Paragraph::new(lines).block(block).wrap(Wrap { trim: false }), area);
}

fn draw_input(f: &mut Frame, area: Rect, state: &AppState) {
    let (border_color, title) = match state.mode {
        Mode::Build => (
            Color::Cyan,
            " BUILD 模式 — Enter 发送 · Shift+Enter 换行 · Tab 切换 PLAN ",
        ),
        Mode::Plan => (
            Color::Yellow,
            " PLAN 只读模式 — Enter 发送 · Shift+Enter 换行 · Tab 切换 BUILD ",
        ),
    };

    let block = Block::default()
        .title(Span::styled(
            title,
            Style::default().fg(border_color).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));
    let inner = block.inner(area);

    let display_text = if state.input.is_empty() && !state.busy {
        Line::from(Span::styled(
            "输入任务描述，@ 引用文件，或输入 / 查看命令…",
            Style::default().fg(Color::DarkGray),
        ))
        .into()
    } else {
        ratatui::text::Text::from(state.input.as_str())
    };

    f.render_widget(Paragraph::new(display_text).block(block), area);

    // 仅在没有审批弹窗时将终端光标定位到输入框
    if state.pending_perm.is_none() && inner.width > 0 && inner.height > 0 {
        let before = &state.input[..state.cursor];
        let row = before.chars().filter(|&c| c == '\n').count();
        let last_line = before.rsplit('\n').next().unwrap_or("");
        let col = UnicodeWidthStr::width(last_line) as u16;
        let visible_row = (row as u16).min(inner.height.saturating_sub(1));
        let visible_col = col.min(inner.width.saturating_sub(1));
        f.set_cursor_position((inner.x + visible_col, inner.y + visible_row));
    }
}

fn draw_status_bar(f: &mut Frame, area: Rect, state: &AppState) {
    let mode_span = match state.mode {
        Mode::Build => Span::styled(
            " BUILD ",
            Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        Mode::Plan => Span::styled(
            " PLAN ",
            Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD),
        ),
    };

    let status_span = if state.busy {
        let s = SPINNER[state.tick % SPINNER.len()];
        Span::styled(
            format!(" {} {} ", s, state.status_text),
            Style::default().fg(Color::Cyan),
        )
    } else {
        Span::styled(" ● 就绪 ", Style::default().fg(Color::Green))
    };

    let mut usage_str = format!("输入 {} · 输出 {}", state.usage.input, state.usage.output);
    if state.usage.cache_read > 0 {
        usage_str.push_str(&format!(" · 缓存 {}", state.usage.cache_read));
    }

    let spans = vec![
        mode_span,
        status_span,
        Span::styled(
            "│ Tab 切模式 · @ 文件 · / 命令 · PgUp/PgDn 滚动 · Ctrl+C 中断/退出 │ ",
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(usage_str, Style::default().fg(Color::Gray)),
    ];
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_autocomplete_popup(f: &mut Frame, input_area: Rect, state: &AppState) {
    if let Some((_, query, files)) = state.active_file_query() {
        let height = (files.len() as u16 + 2).min(12);
        let y = input_area.y.saturating_sub(height);
        let rect = Rect::new(input_area.x + 1, y, input_area.width.min(64), height);
        f.render_widget(Clear, rect);

        let selected = state.popup_selected.min(files.len() - 1);
        let lines: Vec<Line> = files
            .iter()
            .enumerate()
            .map(|(i, file)| {
                let style = if i == selected {
                    Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };
                Line::from(Span::styled(format!(" @{} ", file), style))
            })
            .collect();

        let block = Block::default()
            .title(format!(" 引用文件 @{} (↑↓ 选择 · Tab/Enter 确认) ", query))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan));
        f.render_widget(Paragraph::new(lines).block(block), rect);
        return;
    }

    if let Some(cmds) = state.active_slash_query() {
        let height = (cmds.len() as u16 + 2).min(12);
        let y = input_area.y.saturating_sub(height);
        let rect = Rect::new(input_area.x + 1, y, input_area.width.min(68), height);
        f.render_widget(Clear, rect);

        let selected = state.popup_selected.min(cmds.len() - 1);
        let lines: Vec<Line> = cmds
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let style = if i == selected {
                    Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Cyan)
                };
                Line::from(vec![
                    Span::styled(format!(" {:<10} ", c.cmd), style),
                    Span::styled(c.desc.to_string(), Style::default().fg(Color::Gray)),
                ])
            })
            .collect();

        let block = Block::default()
            .title(" 命令补全 (↑↓ 选择 · Tab 补全 · Enter 执行) ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan));
        f.render_widget(Paragraph::new(lines).block(block), rect);
    }
}

fn draw_permission_modal(f: &mut Frame, area: Rect, perm: &PendingPermission) {
    let w = (area.width * 4 / 5).clamp(40, 100).min(area.width);
    let h = (area.height * 3 / 5).clamp(10, 26).min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let rect = Rect::new(x, y, w, h);
    f.render_widget(Clear, rect);

    let mut lines: Vec<Line> = vec![
        Line::from(vec![
            Span::styled(" 操作: ", Style::default().fg(Color::Gray)),
            Span::styled(
                format!("{}({})", tool_label(&perm.name), perm.title),
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::raw(""),
    ];

    if let Some(preview) = &perm.preview {
        if perm.name == "bash" {
            lines.push(Line::from(Span::styled(
                format!("   $ {}", preview),
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            )));
        } else {
            lines.extend(render_diff(preview, 15));
        }
        lines.push(Line::raw(""));
    }

    if perm.typing_feedback {
        lines.push(Line::from(vec![
            Span::styled(" 拒绝并反馈修改意见: ", Style::default().fg(Color::Yellow)),
            Span::styled(perm.feedback.clone(), Style::default().fg(Color::White)),
        ]));
        lines.push(Line::from(Span::styled(
            " 按 Enter 发送反馈，按 Esc 返回选项",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        lines.push(Line::from(vec![
            Span::styled(" [Y / Enter] ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
            Span::raw("允许本次   "),
            Span::styled("[A] ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::raw("本会话始终允许   "),
            Span::styled("[N / Esc] ", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
            Span::raw("拒绝   "),
            Span::styled("[F] ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::raw("输入修改意见"),
        ]));
    }

    let block = Block::default()
        .title(Span::styled(
            " ⚠ 需要安全授权 ",
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));
    f.render_widget(Paragraph::new(lines).block(block).wrap(Wrap { trim: false }), rect);
}

fn tool_label(name: &str) -> String {
    match name {
        "read" => "Read".into(),
        "write" => "Write".into(),
        "edit" => "Edit".into(),
        "multi_edit" => "MultiEdit".into(),
        "glob" => "Glob".into(),
        "grep" => "Grep".into(),
        "ls" => "List".into(),
        "bash" => "Bash".into(),
        "todo_write" => "Todo".into(),
        "memory" => "Memory".into(),
        "skill_list" => "SkillList".into(),
        "skill_view" => "SkillView".into(),
        "skill_manage" => "SkillManage".into(),
        "session_search" => "SessionSearch".into(),
        other => other.to_string(),
    }
}
