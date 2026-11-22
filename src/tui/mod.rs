//! 全屏 TUI 交互层（基于 ratatui + crossterm）

pub mod app;
pub mod markdown;
pub mod ui;

use crate::agent::permission::Decision;
use crate::agent::{Agent, AgentEvent, Mode};
use crate::ai::Usage;
use crate::config::Config;
use crate::session::render_todos;
use anyhow::Result;
use app::{ expand_at_mentions, AppState, ChatEntry };
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::{IsTerminal, Stdout};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex, Notify};

struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = std::io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        Ok(Self { terminal })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            self.terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        );
        let _ = self.terminal.show_cursor();
    }
}

pub async fn run_tui(agent: Agent, mut config: Config) -> Result<()> {
    if !std::io::stdout().is_terminal() || !std::io::stdin().is_terminal() {
        return crate::repl::run_interactive(agent, config).await;
    }

    let orig_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(std::io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
        orig_hook(info);
    }));

    let mut guard = TerminalGuard::enter()?;
    let todos_arc = agent.ctx.todos.clone();
    let cwd = agent.ctx.cwd.clone();
    let mut state = AppState::new(agent.mode, agent.model_ref.clone(), cwd.clone());
    if !config.has_api_key(&state.model_ref) {
        state.entries.push(ChatEntry::System(format!(
            "提示：当前模型 ({}) 尚未配置 API Key。可直接输入 `/key 你的API_KEY` 设置，或输入 `/model ollama/qwen2.5-coder` 切换其他模型。",
            state.model_ref
        )));
    }
    let agent = Arc::new(Mutex::new(agent));

    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<Event>();
    std::thread::spawn(move || loop {
        if event::poll(Duration::from_millis(50)).unwrap_or(false) {
            if let Ok(e) = event::read() {
                if ev_tx.send(e).is_err() {
                    break;
                }
            }
        }
    });

    let (agent_tx, mut agent_rx) = mpsc::unbounded_channel::<AgentEvent>();
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<(Result<()>, Usage)>();
    let mut active_cancel: Option<Arc<Notify>> = None;
    let mut tick_interval = tokio::time::interval(Duration::from_millis(80));

    loop {
        {
            let todos = todos_arc.lock().unwrap().clone();
            guard.terminal.draw(|f| ui::draw(f, &mut state, &todos))?;
        }

        tokio::select! {
            biased;
            Some(aev) = agent_rx.recv() => {
                state.handle_agent_event(aev);
            }
            Some((res, total_usage)) = done_rx.recv() => {
                state.busy = false;
                state.status_text = "就绪".into();
                state.usage = total_usage;
                active_cancel = None;
                if let Err(e) = res {
                    state.entries.push(ChatEntry::System(format!("错误: {:#}", e)));
                }
            }
            Some(term_ev) = ev_rx.recv() => {
                if handle_term_event(
                    term_ev,
                    &mut state,
                    &agent,
                    &mut config,
                    &agent_tx,
                    &done_tx,
                    &mut active_cancel,
                ).await? {
                    break;
                }
            }
            _ = tick_interval.tick() => {
                if state.busy {
                    state.tick = state.tick.wrapping_add(1);
                }
            }
        }
    }

    Ok(())
}

async fn handle_term_event(
    ev: Event,
    state: &mut AppState,
    agent: &Arc<Mutex<Agent>>,
    config: &mut Config,
    agent_tx: &mpsc::UnboundedSender<AgentEvent>,
    done_tx: &mpsc::UnboundedSender<(Result<()>, Usage)>,
    active_cancel: &mut Option<Arc<Notify>>,
) -> Result<bool> {
    match ev {
        Event::Mouse(me) => match me.kind {
            MouseEventKind::ScrollUp => {
                state.auto_scroll = false;
                state.scroll = state.scroll.saturating_sub(3);
            }
            MouseEventKind::ScrollDown => {
                state.scroll = state.scroll.saturating_add(3);
            }
            _ => {}
        },
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            // 1. 若当前有权限审批弹窗，优先处理弹窗按键
            if let Some(mut perm) = state.pending_perm.take() {
                if perm.typing_feedback {
                    match key.code {
                        KeyCode::Enter => {
                            let fb = perm.feedback.trim().to_string();
                            let _ = perm.reply.send(Decision::Reject(if fb.is_empty() { None } else { Some(fb) }));
                        }
                        KeyCode::Esc => {
                            perm.typing_feedback = false;
                            state.pending_perm = Some(perm);
                        }
                        KeyCode::Backspace => {
                            perm.feedback.pop();
                            state.pending_perm = Some(perm);
                        }
                        KeyCode::Char(c) => {
                            perm.feedback.push(c);
                            state.pending_perm = Some(perm);
                        }
                        _ => state.pending_perm = Some(perm),
                    }
                } else {
                    match key.code {
                        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                            let _ = perm.reply.send(Decision::Once);
                        }
                        KeyCode::Char('a') | KeyCode::Char('A') => {
                            let _ = perm.reply.send(Decision::Always);
                        }
                        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                            let _ = perm.reply.send(Decision::Reject(None));
                        }
                        KeyCode::Char('f') | KeyCode::Char('F') => {
                            perm.typing_feedback = true;
                            state.pending_perm = Some(perm);
                        }
                        _ => state.pending_perm = Some(perm),
                    }
                }
                return Ok(false);
            }

            // 2. 全局快捷键
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                match key.code {
                    KeyCode::Char('c') => {
                        if let Some(cancel) = active_cancel {
                            cancel.notify_one();
                            return Ok(false);
                        }
                        if !state.input.is_empty() {
                            state.clear_input();
                            return Ok(false);
                        }
                        return Ok(true);
                    }
                    KeyCode::Char('d') if state.input.is_empty() && !state.busy => {
                        return Ok(true);
                    }
                    KeyCode::Char('t') => {
                        state.show_reasoning = !state.show_reasoning;
                        return Ok(false);
                    }
                    KeyCode::Char('u') => {
                        state.auto_scroll = false;
                        state.scroll = state.scroll.saturating_sub(10);
                        return Ok(false);
                    }
                    _ => {}
                }
            }

            match key.code {
                KeyCode::PageUp => {
                    state.auto_scroll = false;
                    state.scroll = state.scroll.saturating_sub(10);
                }
                KeyCode::PageDown => {
                    state.scroll = state.scroll.saturating_add(10);
                }
                KeyCode::Tab => {
                    if !state.complete_selection() && !state.busy {
                        state.mode = match state.mode {
                            Mode::Build => Mode::Plan,
                            Mode::Plan => Mode::Build,
                        };
                        agent.lock().await.mode = state.mode;
                    }
                }
                KeyCode::Up => {
                    if state.active_file_query().is_some() || state.active_slash_query().is_some() {
                        state.popup_selected = state.popup_selected.saturating_sub(1);
                    } else {
                        state.history_up();
                    }
                }
                KeyCode::Down => {
                    if let Some((_, _, files)) = state.active_file_query() {
                        if state.popup_selected + 1 < files.len() {
                            state.popup_selected += 1;
                        }
                    } else if let Some(cmds) = state.active_slash_query() {
                        if state.popup_selected + 1 < cmds.len() {
                            state.popup_selected += 1;
                        }
                    } else {
                        state.history_down();
                    }
                }
                KeyCode::Left => state.move_left(),
                KeyCode::Right => state.move_right(),
                KeyCode::Home => state.move_home(),
                KeyCode::End => state.move_end(),
                KeyCode::Backspace => state.backspace(),
                KeyCode::Delete => state.delete(),
                KeyCode::Esc => {
                    if let Some(cancel) = active_cancel {
                        cancel.notify_one();
                    } else {
                        state.clear_input();
                    }
                }
                KeyCode::Enter => {
                    if key.modifiers.contains(KeyModifiers::SHIFT)
                        || key.modifiers.contains(KeyModifiers::ALT)
                    {
                        state.insert_char('\n');
                        return Ok(false);
                    }
                    // 若处于 @文件 补全菜单，Enter 优先确认文件名
                    if state.active_file_query().is_some() {
                        state.complete_selection();
                        return Ok(false);
                    }
                    // 若处于 /命令 菜单且还未补全完整，先补全
                    if state.active_slash_query().is_some() && state.complete_selection() {
                        if state.input == "/model " || state.input == "/key " {
                            return Ok(false);
                        }
                    }
                    if state.busy {
                        return Ok(false);
                    }
                    let raw = state.take_input();
                    if raw.is_empty() {
                        return Ok(false);
                    }
                    if raw.starts_with('/') {
                        return handle_slash_command(&raw, state, agent, config).await;
                    }
                    state.entries.push(ChatEntry::User(raw.clone()));
                    state.auto_scroll = true;
                    state.busy = true;
                    state.status_text = "正在请求模型…".into();

                    let expanded = expand_at_mentions(&state.cwd, &raw);
                    let cancel = Arc::new(Notify::new());
                    *active_cancel = Some(cancel.clone());
                    let agent_clone = agent.clone();
                    let tx = agent_tx.clone();
                    let dtx = done_tx.clone();
                    tokio::spawn(async move {
                        let mut a = agent_clone.lock().await;
                        let r = a.run_turn(expanded, tx, cancel).await;
                        let u = a.usage.clone();
                        let _ = dtx.send((r, u));
                    });
                }
                KeyCode::Char(c) => {
                    state.insert_char(c);
                }
                _ => {}
            }
        }
        _ => {}
    }
    Ok(false)
}

async fn handle_slash_command(
    raw: &str,
    state: &mut AppState,
    agent: &Arc<Mutex<Agent>>,
    config: &mut Config,
) -> Result<bool> {
    let (cmd, arg) = raw.split_once(' ').unwrap_or((raw, ""));
    let mut a = agent.lock().await;
    match cmd {
        "/exit" | "/quit" | "/q" => return Ok(true),
        "/plan" => {
            a.mode = Mode::Plan;
            state.mode = Mode::Plan;
            state
                .entries
                .push(ChatEntry::System("已切换到 PLAN 模式（只读分析，禁止修改文件）".into()));
        }
        "/build" => {
            a.mode = Mode::Build;
            state.mode = Mode::Build;
            state
                .entries
                .push(ChatEntry::System("已切换到 BUILD 模式（可修改文件与执行命令）".into()));
        }
        "/clear" | "/new" => {
            a.clear();
            state.entries.clear();
            state.entries.push(ChatEntry::System("对话历史与待办已清空".into()));
        }
        "/think" => {
            state.show_reasoning = !state.show_reasoning;
            state.entries.push(ChatEntry::System(format!(
                "思考过程显示：{}",
                if state.show_reasoning { "展开" } else { "折叠" }
            )));
        }
        "/todo" => {
            let rendered = render_todos(&a.ctx.todos.lock().unwrap());
            state.entries.push(ChatEntry::System(format!("待办清单：\n{}", rendered)));
        }
        "/memory" => {
            let mem = crate::growth::memory::load_memory_prompt(&state.cwd);
            if mem.is_empty() {
                state.entries.push(ChatEntry::System(
                    "当前暂无持久记忆条目（Agent 可通过 memory 工具自动记录，或手动编辑 .ferro/memory/MEMORY.md）"
                        .into(),
                ));
            } else {
                state.entries.push(ChatEntry::System(mem));
            }
        }
        "/skills" => {
            let skills = crate::growth::skills::discover_skills(&state.cwd);
            if skills.is_empty() {
                state.entries.push(ChatEntry::System(
                    "当前未发现技能（可将 SKILL.md 放入 .ferro/skills/<name>/ 或 .claude/skills/<name>/）".into(),
                ));
            } else {
                let list = skills
                    .into_iter()
                    .map(|s| format!("• {} [{}]: {}", s.name, s.source, s.description))
                    .collect::<Vec<_>>()
                    .join("\n");
                state.entries.push(ChatEntry::System(format!("已加载技能：\n{}", list)));
            }
        }
        "/usage" => {
            state.entries.push(ChatEntry::System(format!(
                "累计 Token：输入 {} · 输出 {} · 缓存命中 {} · 缓存写入 {}",
                a.usage.input, a.usage.output, a.usage.cache_read, a.usage.cache_write
            )));
        }
        "/model" => {
            let target = arg.trim();
            if target.is_empty() {
                state
                    .entries
                    .push(ChatEntry::System(format!("当前模型：{}", a.model_ref)));
            } else {
                match config.build_provider(target) {
                    Ok((p, m)) => {
                        a.provider = p;
                        a.model = m;
                        a.model_ref = target.to_string();
                        state.model_ref = target.to_string();
                        state
                            .entries
                            .push(ChatEntry::System(format!("已切换模型为：{}", target)));
                    }
                    Err(e) => {
                        state
                            .entries
                            .push(ChatEntry::System(format!("模型切换失败：{:#}", e)));
                    }
                }
            }
        }
        "/key" => {
            let key = arg.trim();
            if key.is_empty() {
                state
                    .entries
                    .push(ChatEntry::System("用法：/key <你的API_KEY>（为当前模型服务商设置密钥）".into()));
            } else {
                let model_ref = a.model_ref.clone();
                match config.set_api_key(&model_ref, key).and_then(|_| config.build_provider(&model_ref)) {
                    Ok((p, m)) => {
                        a.provider = p;
                        a.model = m;
                        state
                            .entries
                            .push(ChatEntry::System(format!("已为 {} 设置 API Key，现在可以直接开始对话！", model_ref)));
                    }
                    Err(e) => {
                        state.entries.push(ChatEntry::System(format!("设置失败：{:#}", e)));
                    }
                }
            }
        }
        "/help" | "/?" => {
            state.entries.push(ChatEntry::System(
                "快捷键：Tab 切换 BUILD/PLAN 模式 | @ 引用文件 | / 斜杠命令 | Shift+Enter 换行 | PgUp/PgDn/滚轮 滚动历史 | Ctrl+T 展开/折叠思考 | Ctrl+C 中断当前生成或退出"
                    .into(),
            ));
        }
        _ => {
            state
                .entries
                .push(ChatEntry::System(format!("未知命令 {}，输入 /help 查看可用命令", cmd)));
        }
    }
    Ok(false)
}
