//! Phase 1 交互界面：流式 REPL（Phase 2 将替换为 ratatui 全屏 TUI）

use crate::agent::permission::Decision;
use crate::agent::{Agent, AgentEvent, Mode};
use crate::ai::{Delta, Usage};
use crate::session::render_todos;
use crossterm::style::Stylize;
use std::io::{IsTerminal, Write};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Mutex, Notify};

enum Input {
    Line(String),
    Eof,
}

#[derive(PartialEq)]
enum Out {
    None,
    Text,
    Reasoning,
}

struct Renderer {
    last: Out,
    /// 是否显示思考过程
    show_reasoning: bool,
}

impl Renderer {
    fn ensure_newline(&mut self) {
        if self.last != Out::None {
            println!();
            self.last = Out::None;
        }
    }

    fn event(&mut self, ev: AgentEvent) -> Option<(String, String, Option<String>, tokio::sync::oneshot::Sender<Decision>)> {
        match ev {
            AgentEvent::Delta(Delta::Text(t)) => {
                if self.last == Out::Reasoning {
                    println!("\n");
                }
                print!("{}", t);
                self.last = Out::Text;
            }
            AgentEvent::Delta(Delta::Reasoning(t)) => {
                if self.show_reasoning {
                    if self.last != Out::Reasoning {
                        self.ensure_newline();
                        print!("{}", "✻ 思考中…\n".dark_grey());
                    }
                    print!("{}", t.as_str().dark_grey().italic());
                    self.last = Out::Reasoning;
                }
            }
            AgentEvent::ToolCall { name, title } => {
                self.ensure_newline();
                println!("{} {}{}", "●".cyan(), tool_label(&name).bold(), format!("({})", title).dark_grey());
            }
            AgentEvent::ToolResult { name, ok, summary, display } => {
                if ok {
                    match (name.as_str(), display) {
                        ("todo_write", Some(d)) => {
                            for l in d.lines() {
                                println!("  {} {}", "⎿".dark_grey(), colorize_todo(l));
                            }
                        }
                        (_, Some(diff)) => {
                            println!("  {} {}", "⎿".dark_grey(), summary.dark_grey());
                            print_diff(&diff, 80);
                        }
                        _ => println!("  {} {}", "⎿".dark_grey(), summary.dark_grey()),
                    }
                } else {
                    println!("  {} {}", "⎿".dark_grey(), summary.red());
                }
            }
            AgentEvent::Permission { name, title, preview, reply } => {
                self.ensure_newline();
                return Some((name, title, preview, reply));
            }
            AgentEvent::Notice(n) => {
                self.ensure_newline();
                println!("{}", format!("⚠ {}", n).yellow());
            }
        }
        let _ = std::io::stdout().flush();
        None
    }
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

fn colorize_todo(l: &str) -> String {
    if l.starts_with("[x]") {
        l.dark_green().crossed_out().to_string()
    } else if l.starts_with("[~]") {
        l.yellow().bold().to_string()
    } else {
        l.to_string()
    }
}

fn print_diff(diff: &str, max_lines: usize) {
    let lines: Vec<&str> = diff.lines().filter(|l| !l.starts_with("---") && !l.starts_with("+++")).collect();
    for l in lines.iter().take(max_lines) {
        let s = if l.starts_with('+') {
            l.green().to_string()
        } else if l.starts_with('-') {
            l.red().to_string()
        } else if l.starts_with("@@") {
            l.cyan().to_string()
        } else {
            l.dark_grey().to_string()
        };
        println!("    {}", s);
    }
    if lines.len() > max_lines {
        println!("    {}", format!("... 还有 {} 行", lines.len() - max_lines).dark_grey());
    }
}

fn fmt_usage(u: &Usage) -> String {
    let mut s = format!("输入 {} · 输出 {}", u.input, u.output);
    if u.cache_read > 0 || u.cache_write > 0 {
        s.push_str(&format!(" · 缓存命中 {} · 缓存写入 {}", u.cache_read, u.cache_write));
    }
    s
}

fn spawn_stdin() -> mpsc::UnboundedReceiver<Input> {
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let interactive = stdin.is_terminal();
        loop {
            let mut buf = String::new();
            match stdin.read_line(&mut buf) {
                Ok(0) => {
                    if tx.send(Input::Eof).is_err() || !interactive {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Ok(_) => {
                    if tx.send(Input::Line(buf.trim_end_matches(['\r', '\n']).to_string())).is_err() {
                        break;
                    }
                }
                Err(_) => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    });
    rx
}

fn spawn_ctrlc() -> mpsc::UnboundedReceiver<()> {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while tokio::signal::ctrl_c().await.is_ok() {
            if tx.send(()).is_err() {
                break;
            }
        }
    });
    rx
}

fn print_help() {
    println!("{}", "命令：".bold());
    println!("  /plan     切换到 PLAN 模式（只读分析）");
    println!("  /build    切换到 BUILD 模式（可修改文件）");
    println!("  /clear    清空当前对话");
    println!("  /model    查看或切换模型，如 /model deepseek/deepseek-chat");
    println!("  /todo     查看待办清单");
    println!("  /usage    查看累计 token 用量");
    println!("  /think    显示/隐藏思考过程");
    println!("  /exit     退出");
    println!("  行尾输入 \\ 可换行继续输入；运行中按 Ctrl+C 中断");
}

fn prompt(mode: Mode) {
    let tag = match mode {
        Mode::Build => " BUILD ".on_dark_cyan().black().bold(),
        Mode::Plan => " PLAN ".on_dark_yellow().black().bold(),
    };
    print!("\n{} {} ", tag, "❯".bold());
    let _ = std::io::stdout().flush();
}

pub async fn run_interactive(agent: Agent, config: crate::config::Config) -> anyhow::Result<()> {
    let agent = Arc::new(Mutex::new(agent));
    let mut lines = spawn_stdin();
    let mut ctrlc = spawn_ctrlc();
    let mut renderer = Renderer { last: Out::None, show_reasoning: true };
    let mut last_ctrlc: Option<Instant> = None;

    {
        let a = agent.lock().await;
        println!(
            "{} {}  {}",
            "◆ Ferro".bold().cyan(),
            env!("CARGO_PKG_VERSION").dark_grey(),
            format!("模型 {} · {}", a.model_ref, a.ctx.cwd.display()).dark_grey()
        );
        println!("{}", "输入 /help 查看命令，/plan 与 /build 切换模式".dark_grey());
    }

    loop {
        let mode = agent.lock().await.mode;
        prompt(mode);

        // ---- 读取用户输入（支持行尾 \ 续行） ----
        let mut text = String::new();
        let input = loop {
            tokio::select! {
                l = lines.recv() => match l {
                    Some(Input::Line(l)) => {
                        if let Some(stripped) = l.strip_suffix('\\') {
                            text.push_str(stripped);
                            text.push('\n');
                            print!("{} ", "…".dark_grey());
                            let _ = std::io::stdout().flush();
                            continue;
                        }
                        text.push_str(&l);
                        break Some(std::mem::take(&mut text));
                    }
                    Some(Input::Eof) => {
                        // Windows 下 Ctrl+C 也可能导致读到 EOF，此时忽略
                        if last_ctrlc.map(|t| t.elapsed() < Duration::from_secs(1)).unwrap_or(false) {
                            continue;
                        }
                        break None;
                    }
                    None => break None,
                },
                _ = ctrlc.recv() => {
                    if last_ctrlc.map(|t| t.elapsed() < Duration::from_secs(2)).unwrap_or(false) {
                        break None;
                    }
                    last_ctrlc = Some(Instant::now());
                    println!("\n{}", "再按一次 Ctrl+C 退出".dark_grey());
                    prompt(mode);
                }
            }
        };
        let Some(input) = input else {
            println!();
            return Ok(());
        };
        let input = input.trim().to_string();
        if input.is_empty() {
            continue;
        }

        // ---- 斜杠命令 ----
        if input.starts_with('/') {
            let mut a = agent.lock().await;
            let (cmd, arg) = input.split_once(' ').unwrap_or((input.as_str(), ""));
            match cmd {
                "/exit" | "/quit" | "/q" => return Ok(()),
                "/help" | "/?" => print_help(),
                "/plan" => {
                    a.mode = Mode::Plan;
                    println!("{}", "已切换到 PLAN 模式：只读分析，不会修改文件".yellow());
                }
                "/build" => {
                    a.mode = Mode::Build;
                    println!("{}", "已切换到 BUILD 模式：可以修改文件和执行命令".cyan());
                }
                "/clear" | "/new" => {
                    a.clear();
                    println!("{}", "对话已清空".dark_grey());
                }
                "/todo" => println!("{}", render_todos(&a.ctx.todos.lock().unwrap())),
                "/usage" => println!("{}", fmt_usage(&a.usage).dark_grey()),
                "/think" => {
                    renderer.show_reasoning = !renderer.show_reasoning;
                    println!("思考过程显示：{}", if renderer.show_reasoning { "开" } else { "关" });
                }
                "/model" => {
                    let arg = arg.trim();
                    if arg.is_empty() {
                        println!("当前模型：{}", a.model_ref);
                    } else {
                        match config.build_provider(arg) {
                            Ok((p, m)) => {
                                a.provider = p;
                                a.model = m;
                                a.model_ref = arg.to_string();
                                println!("{}", format!("已切换到 {}", arg).cyan());
                            }
                            Err(e) => println!("{}", format!("切换失败：{:#}", e).red()),
                        }
                    }
                }
                _ => println!("{}", format!("未知命令 {}，输入 /help 查看", cmd).red()),
            }
            continue;
        }

        // ---- 执行一轮 Agent ----
        let (tx, mut rx) = mpsc::unbounded_channel();
        let cancel = Arc::new(Notify::new());
        let agent2 = agent.clone();
        let cancel2 = cancel.clone();
        let mut handle = tokio::spawn(async move {
            let mut a = agent2.lock().await;
            let before = a.usage.clone();
            let r = a.run_turn(input, tx, cancel2).await;
            let mut turn = a.usage.clone();
            turn.input -= before.input;
            turn.output -= before.output;
            turn.cache_read -= before.cache_read;
            turn.cache_write -= before.cache_write;
            (r, turn)
        });
        println!();

        let outcome = loop {
            tokio::select! {
                biased;
                ev = rx.recv() => {
                    let Some(ev) = ev else { continue };
                    if let Some((name, title, preview, reply)) = renderer.event(ev) {
                        let d = ask_permission(&name, &title, preview.as_deref(), &mut lines, &mut ctrlc).await;
                        let _ = reply.send(d);
                    }
                }
                _ = ctrlc.recv() => {
                    cancel.notify_one();
                    last_ctrlc = Some(Instant::now());
                }
                r = &mut handle => break r,
            }
        };
        while let Ok(ev) = rx.try_recv() {
            if let Some((_, _, _, reply)) = renderer.event(ev) {
                let _ = reply.send(Decision::Reject(None));
            }
        }
        renderer.ensure_newline();
        match outcome {
            Ok((Ok(()), turn)) => println!("{}", format!("  {}", fmt_usage(&turn)).dark_grey()),
            Ok((Err(e), _)) => println!("{}", format!("✗ {:#}", e).red()),
            Err(e) => println!("{}", format!("✗ 内部错误: {}", e).red()),
        }
    }
}

async fn ask_permission(
    name: &str,
    title: &str,
    preview: Option<&str>,
    lines: &mut mpsc::UnboundedReceiver<Input>,
    ctrlc: &mut mpsc::UnboundedReceiver<()>,
) -> Decision {
    println!("{} {} {}", "?".yellow().bold(), "需要授权".yellow().bold(), format!("{}({})", tool_label(name), title).bold());
    if let Some(p) = preview {
        if name == "bash" {
            println!("    {}", p.white());
        } else {
            print_diff(p, 60);
        }
    }
    print!(
        "  {} 允许  {} 始终允许  {} 拒绝  {} ",
        "[y/回车]".green(),
        "[a]".cyan(),
        "[n]".red(),
        "(或直接输入修改意见) ❯".dark_grey()
    );
    let _ = std::io::stdout().flush();
    tokio::select! {
        l = lines.recv() => match l {
            Some(Input::Line(l)) => match l.trim() {
                "" | "y" | "Y" | "yes" => Decision::Once,
                "a" | "A" | "always" => Decision::Always,
                "n" | "N" | "no" => Decision::Reject(None),
                other => Decision::Reject(Some(other.to_string())),
            },
            _ => Decision::Reject(None),
        },
        _ = ctrlc.recv() => {
            println!();
            Decision::Reject(None)
        }
    }
}

/// 非交互模式：`ferro -p "..."`
pub async fn run_once(mut agent: Agent, input: String, auto_yes: bool) -> anyhow::Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let cancel = Arc::new(Notify::new());
    let printer = tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            match ev {
                AgentEvent::Delta(Delta::Text(t)) => {
                    print!("{}", t);
                    let _ = std::io::stdout().flush();
                }
                AgentEvent::ToolCall { name, title } => eprintln!("● {}({})", tool_label(&name), title),
                AgentEvent::ToolResult { ok: false, summary, .. } => eprintln!("  ⎿ {}", summary),
                AgentEvent::Permission { name, title, reply, .. } => {
                    if auto_yes {
                        let _ = reply.send(Decision::Once);
                    } else {
                        eprintln!("  ⎿ 已拒绝 {}({})（非交互模式，使用 --yes 自动允许）", name, title);
                        let _ = reply.send(Decision::Reject(Some("非交互模式下无法获得授权，请换一种方式完成任务".into())));
                    }
                }
                AgentEvent::Notice(n) => eprintln!("⚠ {}", n),
                _ => {}
            }
        }
    });
    let r = agent.run_turn(input, tx, cancel).await;
    let _ = printer.await;
    println!();
    r
}
