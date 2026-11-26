use super::*;
use crate::agent::{permission::Permissions, prompt::build_system_prompt};
use crate::config::{global_dir, ProviderConfig, ProviderKind};
use crate::session::FileTracker;
use crate::tools::{Shell, ToolCtx};
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessMode {
    Ask,
    Smart,
    Full,
    Plan,
}

pub enum Command {
    Turn {
        id: u64,
        prompt: String,
        images: Vec<(String, String)>,
        access: AccessMode,
        cancel: Arc<Notify>,
    },
    Workspace(PathBuf),
    Reopen(String),
    DeleteSession(String),
    RenameSession(String, String),
    TogglePinSession(String),
    New,
    Undo,
    SelectModel(String),
    Setup { model: String, url: String, key: String, anthropic: bool, persist: bool },
    FetchModels { provider: String, url: String, key: String },
    Search(String),
}
pub struct Snapshot {
    pub cwd: PathBuf,
    pub session_id: String,
    pub model: String,
    pub config: Arc<Config>,
    pub transcript: Option<Vec<super::Row>>,
    pub sessions: Vec<history::SessionInfo>,
    pub recent: Vec<PathBuf>,
    pub todos: String,
    pub usage: String,
}
pub enum Event {
    State(Snapshot),
    Agent(u64, AgentEvent),
    Done(u64),
    Notice(String),
    Sessions(Vec<history::SessionInfo>),
    FetchedModels { models: Vec<String>, status: String },
}

fn snapshot(agent: &Agent, config: &Arc<Config>, reset_transcript: bool) -> Snapshot {
    Snapshot {
        cwd: agent.ctx.cwd.clone(),
        session_id: agent.session_id.clone(),
        model: agent.model_ref.clone(),
        config: Arc::clone(config),
        transcript: reset_transcript.then(|| super::transcript(&agent.messages)),
        sessions: history::list_sessions(&agent.ctx.cwd),
        recent: crate::workspace::load_recent_projects(),
        todos: crate::session::render_todos(&agent.ctx.todos.lock().unwrap()),
        usage: format!("词元：输入 {} / 输出 {}", agent.usage.input, agent.usage.output),
    }
}

pub fn spawn(
    agent: Agent,
    config: Config,
    repaint: egui::Context,
) -> (mpsc::UnboundedSender<Command>, std::sync::mpsc::Receiver<Event>) {
    spawn_with_notify(agent, config, move || repaint.request_repaint())
}

pub fn spawn_with_notify<F>(
    mut agent: Agent,
    config: Config,
    notify: F,
) -> (mpsc::UnboundedSender<Command>, std::sync::mpsc::Receiver<Event>)
where
    F: Fn() + Send + Sync + 'static,
{
    let mut config = Arc::new(config);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (events, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut last_notify = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(1))
            .unwrap_or_else(std::time::Instant::now);
        let mut send = |event: Event| {
            let is_delta = matches!(&event, Event::Agent(_, AgentEvent::Delta(_)));
            let _ = events.send(event);
            let now = std::time::Instant::now();
            if !is_delta || now.duration_since(last_notify) >= std::time::Duration::from_millis(16) {
                last_notify = now;
                notify();
            }
        };
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("desktop worker runtime");
        runtime.block_on(async {
            crate::workspace::record_recent_project(&agent.ctx.cwd);
            send(Event::State(snapshot(&agent, &config, true)));
            while let Some(command) = rx.recv().await {
                if let Command::Search(query) = &command {
                    send(Event::Sessions(history::search_sessions(&agent.ctx.cwd, query)));
                    continue;
                }
                if let Command::FetchModels { provider, url, key } = command {
                    let (models, status) = fetch_remote_models(&provider, &url, &key).await;
                    send(Event::FetchedModels { models, status });
                    continue;
                }
                let mut reset = matches!(&command, Command::Workspace(_) | Command::New | Command::Reopen(_));
                let result: anyhow::Result<()> = async {
                    match command {
                        Command::Turn { id, prompt, images, access, cancel } => {
                            agent.mode = if access == AccessMode::Plan { Mode::Plan } else { Mode::Build };
                            // Temporary grants belong to this turn, never the next one.
                            agent.perms = match access {
                                AccessMode::Ask => Permissions::ask_all(),
                                AccessMode::Smart | AccessMode::Plan => Permissions::new(&config.permission),
                                AccessMode::Full => Permissions::allow_all(),
                            };
                            if !images.is_empty() {
                                agent.push_user(
                                    images
                                        .into_iter()
                                        .map(|(media_type, data)| Block::Image { media_type, data })
                                        .collect(),
                                );
                            }
                            let expanded = crate::tui::app::expand_at_mentions(&agent.ctx.cwd, &prompt);
                            let (tx, mut ev) = mpsc::unbounded_channel();
                            let turn = agent.run_turn(expanded, tx, cancel);
                            tokio::pin!(turn);
                            let result = loop {
                                tokio::select! {
                                    result = &mut turn => break result,
                                    Some(event) = ev.recv() => send(Event::Agent(id, event)),
                                }
                            };
                            // Drain before Done; no approval sender survives completion.
                            while let Ok(event) = ev.try_recv() { send(Event::Agent(id, event)); }
                            drop(turn);
                            if let Err(err) = result { send(Event::Notice(format!("{err:#}"))); }
                            send(Event::Done(id));
                        }
                        Command::Workspace(path) => switch(&mut agent, &mut config, path)?,
                        Command::Reopen(id) => {
                            anyhow::ensure!(history::search_sessions(&agent.ctx.cwd, "").iter().any(|s| s.id == id), "该历史会话不属于当前工作区");
                            let messages = history::load_session(&id).ok_or_else(|| anyhow::anyhow!("无法加载该历史会话"))?;
                            agent.clear();
                            agent.ctx.tracker.lock().unwrap().clear_backups();
                            agent.session_id = id;
                            agent.messages = messages;
                        }
                        Command::DeleteSession(id) => {
                            if history::delete_session(&id) && agent.session_id == id {
                                agent.clear();
                                agent.ctx.tracker.lock().unwrap().clear_backups();
                                reset = true;
                            }
                        }
                        Command::RenameSession(id, title) => {
                            let _ = history::rename_session(&id, &title);
                        }
                        Command::TogglePinSession(id) => {
                            let _ = history::toggle_pin_session(&id);
                        }
                        Command::New => {
                            agent.clear();
                            agent.ctx.tracker.lock().unwrap().clear_backups();
                        }
                        Command::Undo => {
                            let paths = agent.ctx.tracker.lock().unwrap().undo_all();
                            send(Event::Notice(format!("已撤销本轮 {} 个文件的修改", paths.len())));
                        }
                        Command::SelectModel(model) => {
                            let (provider, name) = config.build_provider(&model)?;
                            agent.provider = provider;
                            agent.model = name;
                            agent.model_ref = model;
                        }
                        Command::Setup { model, url, key, anthropic, persist } => {
                            let (pid, name) = model.split_once('/').ok_or_else(|| anyhow::anyhow!("模型格式应为 服务商/模型名"))?;
                            anyhow::ensure!(!pid.is_empty() && !name.is_empty(), "服务商和模型名称不能为空");
                            anyhow::ensure!(url.starts_with("https://") || url.starts_with("http://"), "接口地址必须以 http:// 或 https:// 开头");
                            let mut next = (*config).clone();
                            let mut pc = next.provider.get(pid).cloned().unwrap_or(ProviderConfig {
                                kind: ProviderKind::Openai, base_url: None, api_key: None,
                                max_tokens: None, thinking_budget: 0, reasoning_passback: None,
                            });
                            pc.kind = if anthropic { ProviderKind::Anthropic } else { ProviderKind::Openai };
                            pc.base_url = Some(url);
                            pc.api_key = if key.is_empty() { None } else { Some(key) };
                            next.provider.insert(pid.into(), pc.clone());
                            next.model = Some(model.clone());
                            let (provider, name) = next.build_provider(&model)?;
                            if persist { persist_setup(&model, &pc)?; }
                            agent.provider = provider;
                            agent.model = name;
                            agent.model_ref = model;
                            config = Arc::new(next);
                        }
                        Command::FetchModels { .. } => return Ok(()),
                        Command::Search(query) => {
                            send(Event::Sessions(history::search_sessions(&agent.ctx.cwd, &query)));
                            return Ok(());
                        }
                    }
                    Ok(())
                }.await;
                if let Err(error) = result { send(Event::Notice(format!("{error:#}"))); }
                send(Event::State(snapshot(&agent, &config, reset)));
            }
        });
    });
    (tx, receiver)
}

fn switch(agent: &mut Agent, config: &mut Arc<Config>, path: PathBuf) -> anyhow::Result<()> {
    let path = std::fs::canonicalize(path)?;
    let path = crate::strip_unc(path);
    anyhow::ensure!(path.is_dir() && !crate::workspace::is_system_dir(&path), "请选择有效的项目工作目录");
    let next = Config::load(&path)?;
    let model = next.resolve_model(None);
    let (provider, model_name) = next.build_provider(&model)?;
    let shell = Shell::detect(next.shell.as_deref());
    let ctx = Arc::new(ToolCtx { cwd: path.clone(), shell: shell.clone(),
        tracker: Arc::new(Mutex::new(FileTracker::default())), todos: Arc::new(Mutex::new(Vec::new())) });
    *agent = Agent::new(provider, model_name, model, build_system_prompt(&path, &shell), ctx,
        Permissions::new(&next.permission), agent.mode, next.max_steps.unwrap_or(50));
    *config = Arc::new(next);
    crate::workspace::record_recent_project(&path);
    Ok(())
}

async fn fetch_remote_models(provider: &str, url: &str, key: &str) -> (Vec<String>, String) {
    let pid = provider.trim();
    let pid = if pid.is_empty() { "custom" } else { pid };
    let base = url.trim().trim_end_matches('/');
    if base.is_empty() {
        return (Vec::new(), "请先填写接口地址".into());
    }
    let is_local = base.contains("//127.0.0.1")
        || base.contains("//localhost")
        || base.contains("//[::1]");
    let mut builder = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3));
    if is_local {
        builder = builder.no_proxy();
    }
    let client = match builder.build() {
        Ok(c) => c,
        Err(e) => return (Vec::new(), format!("无法初始化网络请求：{e}")),
    };

    let is_ollama = pid.eq_ignore_ascii_case("ollama") || base.contains("11434");
    if is_ollama {
        let origin = base.strip_suffix("/v1").unwrap_or(base);
        let tags_url = format!("{origin}/api/tags");
        match client.get(&tags_url).send().await {
            Ok(resp) if resp.status().is_success() => {
                if let Ok(val) = resp.json::<serde_json::Value>().await {
                    let mut models: Vec<String> = val
                        .get("models")
                        .and_then(|m| m.as_array())
                        .into_iter()
                        .flatten()
                        .filter_map(|item| item.get("name").and_then(|n| n.as_str()))
                        .map(|name| format!("{pid}/{name}"))
                        .collect();
                    models.sort();
                    models.dedup();
                    if models.is_empty() {
                        return (models, "已连接本机 Ollama，但尚未安装任何模型（可先运行 ollama pull）".into());
                    }
                    let msg = format!("已自动发现 {} 个本机 Ollama 模型，可直接下拉选择", models.len());
                    return (models, msg);
                }
                return (Vec::new(), "无法解析 Ollama 返回的模型列表".into());
            }
            Ok(resp) => {
                return (Vec::new(), format!("Ollama 接口返回状态码 {}，请确认服务正常", resp.status().as_u16()));
            }
            Err(_) => {
                return (Vec::new(), "未检测到运行中的本机 Ollama，请先启动 Ollama 或手动输入模型名".into());
            }
        }
    }

    let resolved_key = if let Some(var) = key.trim().strip_prefix("env:") {
        std::env::var(var.trim()).unwrap_or_default()
    } else {
        key.trim().to_string()
    };
    let models_url = format!("{base}/models");
    let mut req = client.get(&models_url);
    if !resolved_key.is_empty() {
        req = req.bearer_auth(&resolved_key);
    }
    match req.send().await {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(val) = resp.json::<serde_json::Value>().await {
                let mut models: Vec<String> = val
                    .get("data")
                    .and_then(|d| d.as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(|item| item.get("id").and_then(|n| n.as_str()))
                    .map(|id| format!("{pid}/{id}"))
                    .collect();
                models.sort();
                models.dedup();
                if models.is_empty() {
                    return (models, "接口连接成功，但未返回可用模型列表，可手动填写模型名".into());
                }
                let msg = format!("已获取 {} 个可用模型，可直接下拉选择", models.len());
                (models, msg)
            } else {
                (Vec::new(), "接口响应格式无法解析，请手动填写模型名".into())
            }
        }
        Ok(resp) => (Vec::new(), format!("获取模型列表失败（状态码 {}），请检查接口地址与密钥", resp.status().as_u16())),
        Err(_) => (Vec::new(), "无法连接到该接口，请检查接口地址与网络".into()),
    }
}

fn persist_setup(model: &str, pc: &ProviderConfig) -> anyhow::Result<()> {
    let path = global_dir().join("config.toml");
    let mut doc: toml::Value = if path.exists() { toml::from_str(&std::fs::read_to_string(&path)?)? }
        else { toml::Value::Table(Default::default()) };
    doc["model"] = toml::Value::String(model.into());
    let root = doc.as_table_mut().ok_or_else(|| anyhow::anyhow!("配置文件格式无效"))?;
    let providers = root.entry("provider").or_insert(toml::Value::Table(Default::default()))
        .as_table_mut().ok_or_else(|| anyhow::anyhow!("服务商配置格式无效"))?;
    let pid = model.split_once('/').unwrap().0;
    let entry = providers.entry(pid).or_insert(toml::Value::Table(Default::default()))
        .as_table_mut().ok_or_else(|| anyhow::anyhow!("服务商条目格式无效"))?;
    entry.insert("type".into(), toml::Value::String(if matches!(pc.kind, ProviderKind::Anthropic) { "anthropic" } else { "openai" }.into()));
    entry.insert("base_url".into(), toml::Value::String(pc.base_url.clone().unwrap_or_default()));
    entry.insert("api_key".into(), toml::Value::String(pc.api_key.clone().unwrap_or_default()));
    std::fs::create_dir_all(global_dir())?;
    std::fs::write(path, toml::to_string_pretty(&doc)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::{ChatRequest, ChatResponse, Provider, StopReason, Usage};
    use async_trait::async_trait;
    use serde_json::json;
    use std::time::Duration;

    struct ScriptedProvider;

    #[async_trait]
    impl Provider for ScriptedProvider {
        async fn chat(
            &self,
            req: ChatRequest<'_>,
            sink: &(dyn Fn(Delta) + Send + Sync),
        ) -> anyhow::Result<ChatResponse> {
            let last_user = req
                .messages
                .iter()
                .rev()
                .find(|m| m.role == Role::User)
                .and_then(|m| {
                    m.content.iter().find_map(|b| match b {
                        Block::Text { text } if !text.starts_with("<system-reminder>") => {
                            Some(text.as_str())
                        }
                        _ => None,
                    })
                })
                .unwrap_or("");
            let has_tool_result = req
                .messages
                .last()
                .map(|m| {
                    m.content
                        .iter()
                        .any(|b| matches!(b, Block::ToolResult { .. }))
                })
                .unwrap_or(false);

            if last_user.contains("SLOW_TURN") {
                sink(Delta::Text("stream-part-1 ".into()));
                tokio::time::sleep(Duration::from_secs(5)).await;
                return Ok(ChatResponse {
                    message: Message {
                        role: Role::Assistant,
                        content: vec![Block::Text {
                            text: "stream-part-1 done".into(),
                        }],
                    },
                    stop: StopReason::EndTurn,
                    usage: Usage { input: 10, output: 5, ..Default::default() },
                });
            }

            if last_user.contains("ASK_BASH") && !has_tool_result {
                return Ok(ChatResponse {
                    message: Message {
                        role: Role::Assistant,
                        content: vec![Block::ToolUse {
                            id: "call_bash_1".into(),
                            name: "bash".into(),
                            input: json!({ "command": "echo perm_verified" }),
                        }],
                    },
                    stop: StopReason::ToolUse,
                    usage: Usage { input: 12, output: 8, ..Default::default() },
                });
            }

            let reply = if has_tool_result {
                "Tool finished: perm_verified"
            } else {
                "Hello from Ferro native desktop!"
            };
            sink(Delta::Text(reply.into()));
            Ok(ChatResponse {
                message: Message {
                    role: Role::Assistant,
                    content: vec![Block::Text {
                        text: reply.into(),
                    }],
                },
                stop: StopReason::EndTurn,
                usage: Usage { input: 8, output: 6, ..Default::default() },
            })
        }
    }

    #[test]
    fn worker_handles_first_turn_permission_and_stop() {
        let cwd = std::env::temp_dir().join(format!("ferro_desktop_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&cwd);
        let cfg = Config::default();
        let shell = Shell::detect(None);
        let ctx = Arc::new(ToolCtx {
            cwd: cwd.clone(),
            tracker: Arc::new(Mutex::new(FileTracker::default())),
            todos: Arc::new(Mutex::new(Vec::new())),
            shell: shell.clone(),
        });
        let agent = Agent::new(
            Arc::new(ScriptedProvider),
            "mock-model".into(),
            "mock/mock-model".into(),
            "system".into(),
            ctx,
            Permissions::new(&cfg.permission),
            Mode::Build,
            10,
        );

        let (tx, rx) = spawn_with_notify(agent, cfg, || {});

        // 0. Initial state snapshot
        let first = rx.recv_timeout(Duration::from_secs(3)).expect("initial state");
        assert!(matches!(first, Event::State(_)));

        // 1. First-turn chat verification
        let cancel1 = Arc::new(Notify::new());
        tx.send(Command::Turn {
            id: 1,
            prompt: "Hello".into(),
            images: Vec::new(),
            access: AccessMode::Smart,
            cancel: cancel1,
        })
        .unwrap();

        let mut got_text = String::new();
        loop {
            match rx.recv_timeout(Duration::from_secs(3)).expect("turn 1 event") {
                Event::Agent(1, AgentEvent::Delta(Delta::Text(t))) => got_text.push_str(&t),
                Event::Done(1) => break,
                _ => {}
            }
        }
        assert_eq!(got_text, "Hello from Ferro native desktop!");
        let _ = rx.recv_timeout(Duration::from_secs(3)).expect("post turn 1 state");

        // 2. Permission approval in Plan mode (bash requires approval in Plan mode)
        let cancel2 = Arc::new(Notify::new());
        tx.send(Command::Turn {
            id: 2,
            prompt: "ASK_BASH".into(),
            images: Vec::new(),
            access: AccessMode::Plan,
            cancel: cancel2,
        })
        .unwrap();

        let mut approved = false;
        let mut tool_ok = false;
        loop {
            match rx.recv_timeout(Duration::from_secs(5)).expect("turn 2 event") {
                Event::Agent(2, AgentEvent::Permission { name, reply, .. }) => {
                    assert_eq!(name, "bash");
                    approved = true;
                    reply.send(Decision::Once).unwrap();
                }
                Event::Agent(2, AgentEvent::ToolResult { ok, .. }) => {
                    tool_ok = ok;
                }
                Event::Done(2) => break,
                _ => {}
            }
        }
        assert!(approved, "permission prompt must be triggered");
        assert!(tool_ok, "approved tool must succeed");
        let _ = rx.recv_timeout(Duration::from_secs(3)).expect("post turn 2 state");

        // 3. Stop / Cancel verification on an in-flight turn
        let cancel3 = Arc::new(Notify::new());
        tx.send(Command::Turn {
            id: 3,
            prompt: "SLOW_TURN".into(),
            images: Vec::new(),
            access: AccessMode::Smart,
            cancel: cancel3.clone(),
        })
        .unwrap();

        let mut interrupted = false;
        loop {
            match rx.recv_timeout(Duration::from_secs(3)).expect("turn 3 event") {
                Event::Agent(3, AgentEvent::Delta(Delta::Text(_))) => {
                    cancel3.notify_one();
                }
                Event::Agent(3, AgentEvent::Notice(n)) if n.contains("已中断") => {
                    interrupted = true;
                }
                Event::Done(3) => break,
                _ => {}
            }
        }
        assert!(interrupted, "turn 3 must report interruption after cancel");
        let _ = std::fs::remove_dir_all(&cwd);
    }
}
