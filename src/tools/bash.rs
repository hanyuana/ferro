use super::*;
use serde_json::json;
use std::process::Stdio;
use std::time::Duration;

const DEFAULT_TIMEOUT_MS: u64 = 120_000;
const MAX_TIMEOUT_MS: u64 = 600_000;
const MAX_OUTPUT: usize = 30_000;

#[derive(Debug, Clone)]
pub struct Shell {
    pub program: PathBuf,
    pub kind: ShellKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    Bash,
    PowerShell,
}

impl Shell {
    pub fn detect(configured: Option<&str>) -> Self {
        if let Some(s) = configured {
            let lower = s.to_lowercase();
            let kind = if lower.contains("powershell") || lower.contains("pwsh") {
                ShellKind::PowerShell
            } else {
                ShellKind::Bash
            };
            return Self { program: PathBuf::from(s), kind };
        }
        if cfg!(windows) {
            if let Some(bash) = find_git_bash() {
                return Self { program: bash, kind: ShellKind::Bash };
            }
            return Self { program: PathBuf::from("powershell.exe"), kind: ShellKind::PowerShell };
        }
        let program = std::env::var("SHELL")
            .ok()
            .filter(|s| s.ends_with("bash") || s.ends_with("zsh"))
            .unwrap_or_else(|| if Path::new("/bin/bash").exists() { "/bin/bash".into() } else { "/bin/sh".into() });
        Self { program: PathBuf::from(program), kind: ShellKind::Bash }
    }

    pub fn describe(&self) -> String {
        match self.kind {
            ShellKind::Bash => format!("bash ({})", self.program.display()),
            ShellKind::PowerShell => "PowerShell".into(),
        }
    }

    fn command(&self, script: &str) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new(&self.program);
        #[cfg(windows)]
        {
            #[allow(unused_imports)]
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW：防止桌面客户端执行命令时闪烁黑框
        }
        match self.kind {
            ShellKind::Bash => {
                cmd.arg("-c").arg(script);
            }
            ShellKind::PowerShell => {
                let wrapped = format!(
                    "[Console]::OutputEncoding=[System.Text.Encoding]::UTF8; $ErrorActionPreference='Continue'; {}",
                    script
                );
                cmd.args(["-NoProfile", "-NonInteractive", "-Command", &wrapped]);
            }
        }
        cmd
    }
}

fn find_git_bash() -> Option<PathBuf> {
    for p in [r"C:\Program Files\Git\bin\bash.exe", r"C:\Program Files (x86)\Git\bin\bash.exe"] {
        if Path::new(p).exists() {
            return Some(PathBuf::from(p));
        }
    }
    // 通过 PATH 中的 git.exe 反推 Git 安装目录
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        if dir.join("git.exe").exists() {
            if let Some(root) = dir.parent() {
                let bash = root.join("bin").join("bash.exe");
                if bash.exists() {
                    return Some(bash);
                }
            }
        }
    }
    None
}

pub struct Bash;

#[async_trait]
impl Tool for Bash {
    fn name(&self) -> &'static str {
        "bash"
    }
    fn description(&self) -> &'static str {
        "在 shell 中执行命令并返回输出（stdout 与 stderr 合并）。\n\
         - 用于运行构建、测试、git、包管理器等命令\n\
         - 不要用它来读/写/搜索文件，请使用 read / edit / write / glob / grep\n\
         - 每次调用相互独立，工作目录不会保留；需要切换目录时请使用 workdir 参数\n\
         - 默认超时 120 秒，最长 600 秒；不要运行需要交互输入或永不退出的命令（如 dev server）\n\
         - 多个独立命令可以并行调用；有依赖关系的命令用 && 连接"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "要执行的命令" },
                "description": { "type": "string", "description": "用 5-10 个字简述命令的作用" },
                "workdir": { "type": "string", "description": "执行目录，默认为工作目录" },
                "timeout": { "type": "integer", "description": "超时毫秒数" }
            },
            "required": ["command"]
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Exec
    }
    fn describe(&self, input: &Value, _ctx: &ToolCtx) -> Invocation {
        let cmd = input["command"].as_str().unwrap_or("").to_string();
        Invocation { title: cmd.clone(), preview: Some(cmd.clone()), command: Some(cmd), ..Default::default() }
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput> {
        let script = arg_str(&input, "command")?;
        let workdir = opt_str(&input, "workdir")
            .map(|p| resolve_path(&ctx.cwd, p))
            .unwrap_or_else(|| ctx.cwd.clone());
        let timeout = input["timeout"].as_u64().unwrap_or(DEFAULT_TIMEOUT_MS).min(MAX_TIMEOUT_MS);

        let mut cmd = ctx.shell.command(script);
        cmd.current_dir(&workdir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let child = cmd
            .spawn()
            .map_err(|e| anyhow!("无法启动 shell {}: {}", ctx.shell.program.display(), e))?;

        match tokio::time::timeout(Duration::from_millis(timeout), child.wait_with_output()).await {
            Ok(out) => {
                let out = out?;
                let mut text = String::from_utf8_lossy(&out.stdout).to_string();
                let err = String::from_utf8_lossy(&out.stderr);
                if !err.trim().is_empty() {
                    if !text.is_empty() && !text.ends_with('\n') {
                        text.push('\n');
                    }
                    text.push_str(&err);
                }
                let code = out.status.code().unwrap_or(-1);
                let mut content = truncate_middle(text.trim_end(), MAX_OUTPUT);
                if content.is_empty() {
                    content = "(无输出)".into();
                }
                if code != 0 {
                    content.push_str(&format!("\n\n[退出码: {}]", code));
                }
                Ok(ToolOutput::text(content))
            }
            Err(_) => anyhow::bail!("命令超时（{} 秒）已终止: {}", timeout / 1000, script),
        }
    }
}
