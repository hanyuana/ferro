use super::*;
use crate::agent::{permission::Permissions, prompt::build_system_prompt};
use crate::config::{global_dir, ProviderConfig, ProviderKind};
use crate::session::FileTracker;
use crate::tools::{Shell, ToolCtx};
use std::sync::Mutex;

pub enum Command {
    Turn { id: u64, prompt: String, mode: Mode, cancel: Arc<Notify> },
    Workspace(PathBuf),
    Reopen(String),
    DeleteSession(String),
    New,
    Undo,
    Setup { model: String, url: String, key: String, anthropic: bool, persist: bool },
    Search(String),
}
pub struct Snapshot {
    pub cwd: PathBuf,
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
}

fn snapshot(agent: &Agent, config: &Arc<Config>, reset_transcript: bool) -> Snapshot {
    Snapshot {
        cwd: agent.ctx.cwd.clone(),
        model: agent.model_ref.clone(),
        config: Arc::clone(config),
        transcript: reset_transcript.then(|| super::transcript(&agent.messages)),
        sessions: history::list_sessions(&agent.ctx.cwd),
        recent: crate::workspace::load_recent_projects(),
        todos: crate::session::render_todos(&agent.ctx.todos.lock().unwrap()),
        usage: format!("Tokens  {} in / {} out", agent.usage.input, agent.usage.output),
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
                let mut reset = matches!(&command, Command::Workspace(_) | Command::New | Command::Reopen(_));
                let result: anyhow::Result<()> = async {
                    match command {
                        Command::Turn { id, prompt, mode, cancel } => {
                            agent.mode = mode;
                            // Temporary grants belong to this turn, never the next one.
                            agent.perms = Permissions::new(&config.permission);
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
                            anyhow::ensure!(history::search_sessions(&agent.ctx.cwd, "").iter().any(|s| s.id == id), "Session is not in this workspace");
                            let messages = history::load_session(&id).ok_or_else(|| anyhow::anyhow!("Unable to load session"))?;
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
                        Command::New => {
                            agent.clear();
                            agent.ctx.tracker.lock().unwrap().clear_backups();
                        }
                        Command::Undo => {
                            let paths = agent.ctx.tracker.lock().unwrap().undo_all();
                            send(Event::Notice(format!("Restored {} files (current turn snapshots)", paths.len())));
                        }
                        Command::Setup { model, url, key, anthropic, persist } => {
                            let (pid, name) = model.split_once('/').ok_or_else(|| anyhow::anyhow!("Use provider/model"))?;
                            anyhow::ensure!(!pid.is_empty() && !name.is_empty(), "Provider and model are required");
                            anyhow::ensure!(url.starts_with("https://") || url.starts_with("http://"), "Base URL must use http:// or https://");
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
    anyhow::ensure!(path.is_dir() && !crate::workspace::is_system_dir(&path), "Choose a project directory");
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

fn persist_setup(model: &str, pc: &ProviderConfig) -> anyhow::Result<()> {
    let path = global_dir().join("config.toml");
    let mut doc: toml::Value = if path.exists() { toml::from_str(&std::fs::read_to_string(&path)?)? }
        else { toml::Value::Table(Default::default()) };
    doc["model"] = toml::Value::String(model.into());
    let root = doc.as_table_mut().ok_or_else(|| anyhow::anyhow!("Invalid configuration"))?;
    let providers = root.entry("provider").or_insert(toml::Value::Table(Default::default()))
        .as_table_mut().ok_or_else(|| anyhow::anyhow!("Invalid providers"))?;
    let pid = model.split_once('/').unwrap().0;
    let entry = providers.entry(pid).or_insert(toml::Value::Table(Default::default()))
        .as_table_mut().ok_or_else(|| anyhow::anyhow!("Invalid provider"))?;
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
            mode: Mode::Build,
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
            mode: Mode::Plan,
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
            mode: Mode::Build,
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
