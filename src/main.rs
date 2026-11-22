#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

mod agent;
mod ai;
mod config;
mod growth;
mod repl;
mod session;
mod tools;
mod tui;
mod desktop;
mod workspace;

use agent::{permission::Permissions, prompt::build_system_prompt, Agent, Mode};
use anyhow::Result;
use clap::{Parser, Subcommand};
use config::{global_dir, Config, SAMPLE_CONFIG};
use session::FileTracker;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tools::{Shell, ToolCtx};

#[derive(Parser)]
#[command(name = "ferro", version, about = "Ferro — 极简原生 Rust AI 编程 Agent")]
struct Cli {
    /// 单次非交互执行提示词
    #[arg(short, long)]
    prompt: Option<String>,

    /// 模型（格式：provider/model，如 deepseek/deepseek-chat、anthropic/claude-sonnet-4-5）
    #[arg(short, long)]
    model: Option<String>,

    /// 工作目录
    #[arg(short = 'C', long)]
    cwd: Option<PathBuf>,

    /// 以只读 PLAN 模式启动
    #[arg(long)]
    plan: bool,

    /// 恢复当前工作目录下最近一次历史会话
    #[arg(short = 'c', long = "continue")]
    resume: bool,

    /// 非交互模式下自动同意需要授权的操作
    #[arg(short = 'y', long)]
    yes: bool,

    /// 使用终端 TUI 界面（默认启动图形化 Agent Studio 桌面窗口）
    #[arg(long)]
    tui: bool,

    /// 使用纯文本流式 REPL
    #[arg(long)]
    no_tui: bool,

    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// 生成默认配置文件（默认写入全局配置目录，--local 写入当前目录 .ferro/config.toml）
    Init {
        #[arg(long)]
        local: bool,
    },
}

fn main() -> Result<()> {
    #[cfg(windows)]
    if std::env::args_os().len() > 1 {
        extern "system" {
            fn AttachConsole(dwProcessId: u32) -> i32;
            fn GetStdHandle(nStdHandle: u32) -> isize;
        }
        unsafe {
            const STD_OUTPUT_HANDLE: u32 = (-11i32) as u32;
            let h = GetStdHandle(STD_OUTPUT_HANDLE);
            if h == 0 || h == -1 {
                AttachConsole(u32::MAX);
            }
        }
    }

    let cli = Cli::parse();
    let cwd = resolve_initial_cwd(cli.cwd);

    if let Some(Cmd::Init { local }) = cli.cmd {
        let target = if local {
            cwd.join(".ferro").join("config.toml")
        } else {
            global_dir().join("config.toml")
        };
        if target.exists() {
            println!("配置文件已存在: {}", target.display());
            return Ok(());
        }
        if let Some(p) = target.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(&target, SAMPLE_CONFIG)?;
        println!("已生成配置文件: {}", target.display());
        return Ok(());
    }

    let cfg = Config::load(&cwd)?;
    let model_ref = cfg.resolve_model(cli.model.as_deref());
    let (provider, model) = cfg.build_provider(&model_ref)?;
    let shell = Shell::detect(cfg.shell.as_deref());
    let system = build_system_prompt(&cwd, &shell);

    let ctx = Arc::new(ToolCtx {
        cwd,
        tracker: Arc::new(Mutex::new(FileTracker::default())),
        todos: Arc::new(Mutex::new(Vec::new())),
        shell,
    });
    let perms = Permissions::new(&cfg.permission);
    let mode = if cli.plan { Mode::Plan } else { Mode::Build };
    let mut agent = Agent::new(
        provider,
        model,
        model_ref,
        system,
        ctx,
        perms,
        mode,
        cfg.max_steps.unwrap_or(50),
    );
    if cli.resume {
        if let Some((sid, msgs)) = growth::history::load_latest_session(&agent.ctx.cwd) {
            agent.session_id = sid;
            agent.messages = msgs;
        }
    }

    if cli.prompt.is_none() && !cli.no_tui && !cli.tui {
        return desktop::run(agent, cfg);
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    rt.block_on(async move {
        match cli.prompt {
            Some(p) => repl::run_once(agent, p, cli.yes).await,
            None if cli.no_tui => repl::run_interactive(agent, cfg).await,
            _ => tui::run_tui(agent, cfg).await,
        }
    })
}

fn resolve_initial_cwd(cli_cwd: Option<PathBuf>) -> PathBuf {
    if let Some(p) = cli_cwd {
        return strip_unc(std::fs::canonicalize(&p).unwrap_or(p));
    }
    if let Ok(cur) = std::env::current_dir() {
        let cur = strip_unc(std::fs::canonicalize(&cur).unwrap_or(cur));
        if !workspace::is_system_dir(&cur) {
            return cur;
        }
    }
    if let Some(recent) = workspace::load_recent_projects().into_iter().next() {
        return recent;
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let dir = strip_unc(dir.to_path_buf());
            if !workspace::is_system_dir(&dir) {
                return dir;
            }
        }
    }
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

fn strip_unc(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(stripped) = s.strip_prefix(r"\\?\") {
        PathBuf::from(stripped)
    } else {
        p
    }
}
