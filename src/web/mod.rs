//! 独立桌面客户端宿主（Windows GUI 子系统：双击无黑框，自带原生文件夹选择框与独立应用窗口生命周期）

use crate::agent::permission::Decision;
use crate::agent::{Agent, AgentEvent, Mode};
use crate::ai::{Block, Delta, Role};
use crate::config::{global_dir, Config};
use crate::tui::app::expand_at_mentions;
use anyhow::Result;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, oneshot, Mutex, Notify};

const UI_HTML: &str = include_str!("ui.html");

struct WebState {
    agent: Mutex<Agent>,
    config: Mutex<Config>,
    sse_tx: broadcast::Sender<String>,
    active_cancel: Mutex<Option<Arc<Notify>>>,
    pending_perms: Mutex<HashMap<u64, oneshot::Sender<Decision>>>,
    next_perm_id: AtomicU64,
    shutdown: Arc<Notify>,
    auto_exit_on_close: bool,
}

pub async fn run_gui(agent: Agent, config: Config, port: Option<u16>) -> Result<()> {
    let auto_exit_on_close = port.is_none();
    let bind_addr = format!("127.0.0.1:{}", port.unwrap_or(0));
    let listener = match TcpListener::bind(&bind_addr).await {
        Ok(l) => l,
        Err(_) => TcpListener::bind("127.0.0.1:0").await?,
    };
    let addr = listener.local_addr()?;
    let url = format!("http://127.0.0.1:{}", addr.port());

    let (sse_tx, _) = broadcast::channel::<String>(512);
    let shutdown = Arc::new(Notify::new());
    let shared = Arc::new(WebState {
        agent: Mutex::new(agent),
        config: Mutex::new(config),
        sse_tx,
        active_cancel: Mutex::new(None),
        pending_perms: Mutex::new(HashMap::new()),
        next_perm_id: AtomicU64::new(1),
        shutdown: shutdown.clone(),
        auto_exit_on_close,
    });

    let url_for_win = url.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let _ = open_app_window(&url_for_win);
    });

    loop {
        tokio::select! {
            Ok((stream, _)) = listener.accept() => {
                let st = shared.clone();
                tokio::spawn(async move {
                    let _ = handle_conn(stream, st).await;
                });
            }
            _ = shutdown.notified() => {
                break;
            }
        }
    }
    Ok(())
}

fn open_app_window(url: &str) -> Option<tokio::process::Child> {
    if cfg!(windows) {
        let candidates = [
            r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
            r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
        ];
        for bin in candidates {
            if Path::new(bin).exists() {
                // 使用按进程隔离的独立用户目录，彻底避免与已打开的浏览器冲突，保证每次双击都能弹出独立原生窗口
                let profile = std::env::temp_dir()
                    .join("ferro_client_window")
                    .join(format!("pid_{}", std::process::id()));
                let default_dir = profile.join("Default");
                let _ = std::fs::create_dir_all(&default_dir);
                let _ = std::fs::write(
                    default_dir.join("Preferences"),
                    r#"{"browser":{"has_seen_welcome_page":true},"profile":{"exit_type":"Normal"}}"#,
                );
                let mut cmd = tokio::process::Command::new(bin);
                cmd.arg(format!("--app={}", url))
                    .arg(format!("--user-data-dir={}", profile.display()))
                    .arg("--window-size=1320,860")
                    .arg("--no-first-run")
                    .arg("--no-default-browser-check")
                    .arg("--disable-extensions")
                    .arg("--disable-sync")
                    .arg("--disable-features=Translate,msEdgeSidebarV2");
                if let Ok(child) = cmd.spawn() {
                    return Some(child);
                }
            }
        }
    }
    None
}

/// 调用 Windows 原生文件夹选择对话框
async fn pick_folder_dialog(initial: &Path) -> Option<PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    let script = format!(
        "Add-Type -AssemblyName System.Windows.Forms; \
         $d = New-Object System.Windows.Forms.FolderBrowserDialog; \
         $d.Description = '选择项目工作区文件夹'; \
         $d.SelectedPath = '{}'; \
         if ($d.ShowDialog() -eq 'OK') {{ [Console]::OutputEncoding=[System.Text.Encoding]::UTF8; Write-Output $d.SelectedPath }}",
        initial.display().to_string().replace('\'', "''")
    );
    let mut cmd = tokio::process::Command::new("powershell.exe");
    #[cfg(windows)]
    {
        #[allow(unused_imports)]
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let out = cmd
        .args(["-NoProfile", "-Sta", "-Command", &script])
        .output()
        .await
        .ok()?;
    let chosen = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if chosen.is_empty() {
        None
    } else {
        Some(PathBuf::from(chosen))
    }
}

async fn handle_conn(mut stream: TcpStream, state: Arc<WebState>) -> Result<()> {
    let mut buf = vec![0u8; 65536];
    let mut len = 0usize;
    let header_end = loop {
        let n = stream.read(&mut buf[len..]).await?;
        if n == 0 {
            return Ok(());
        }
        len += n;
        if let Some(pos) = buf[..len].windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
        if len == buf.len() {
            buf.resize(buf.len() * 2, 0);
        }
    };

    let header_str = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = header_str.lines();
    let req_line = lines.next().unwrap_or("");
    let mut parts = req_line.split_whitespace();
    let method = parts.next().unwrap_or("GET");
    let full_path = parts.next().unwrap_or("/");
    let (path, query) = full_path.split_once('?').unwrap_or((full_path, ""));

    let mut content_len = 0usize;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                content_len = v.trim().parse().unwrap_or(0);
            }
        }
    }

    while len < header_end + content_len {
        if buf.len() < header_end + content_len {
            buf.resize(header_end + content_len, 0);
        }
        let n = stream.read(&mut buf[len..header_end + content_len]).await?;
        if n == 0 {
            break;
        }
        len += n;
    }
    let body_bytes = &buf[header_end..len.min(header_end + content_len)];
    let body_json: Value = serde_json::from_slice(body_bytes).unwrap_or(Value::Null);

    if method == "GET" && path == "/" {
        return send_response(&mut stream, "200 OK", "text/html; charset=utf-8", UI_HTML.as_bytes()).await;
    }

    if method == "GET" && path == "/api/events" {
        let hdr = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n";
        stream.write_all(hdr.as_bytes()).await?;
        let mut rx = state.sse_tx.subscribe();
        while let Ok(msg) = rx.recv().await {
            let packet = format!("data: {}\n\n", msg);
            if stream.write_all(packet.as_bytes()).await.is_err() {
                break;
            }
        }
        return Ok(());
    }

    let resp = match (method, path) {
        ("GET", "/api/state") => build_state_json(&state).await,
        ("GET", "/api/file") => {
            let rel = parse_query_param(query, "path").unwrap_or_default();
            let a = state.agent.lock().await;
            let full = crate::tools::resolve_path(&a.ctx.cwd, &rel);
            let content = std::fs::read_to_string(&full).unwrap_or_else(|e| format!("无法读取文件: {}", e));
            json!({ "content": crate::tools::truncate_middle(&content, 100_000) })
        }
        ("GET", "/api/skill") => {
            let name = parse_query_param(query, "name").unwrap_or_default();
            let a = state.agent.lock().await;
            let skills = crate::growth::skills::discover_skills(&a.ctx.cwd);
            let content = skills
                .into_iter()
                .find(|s| s.name == name)
                .and_then(|s| std::fs::read_to_string(s.path).ok())
                .unwrap_or_else(|| "未找到技能内容".into());
            json!({ "content": content })
        }
        ("POST", "/api/mode") => {
            let m = body_json["mode"].as_str().unwrap_or("build");
            let mut a = state.agent.lock().await;
            a.mode = if m == "plan" { Mode::Plan } else { Mode::Build };
            json!({ "ok": true, "mode": m })
        }
        ("POST", "/api/config") => {
            {
                let mut cfg = state.config.lock().await;
                let mut a = state.agent.lock().await;
                let target_model = body_json["model"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .unwrap_or_else(|| a.model_ref.clone());
                if let Some(url) = body_json["base_url"].as_str().filter(|s| !s.is_empty()) {
                    if let Some((pid, _)) = target_model.split_once('/') {
                        if let Some(pc) = cfg.provider.get_mut(pid) {
                            pc.base_url = Some(url.to_string());
                        }
                    }
                }
                if let Some(key) = body_json["api_key"].as_str().filter(|s| !s.is_empty()) {
                    let _ = cfg.set_api_key(&target_model, key);
                    let _ = persist_key_to_disk(&target_model, key, body_json["base_url"].as_str());
                }
                if let Ok((p, m)) = cfg.build_provider(&target_model) {
                    a.provider = p;
                    a.model = m;
                    a.model_ref = target_model;
                }
            }
            build_state_json(&state).await
        }
        ("POST", "/api/memory") => {
            if let Some(content) = body_json["content"].as_str().filter(|s| !s.is_empty()) {
                let cwd = state.agent.lock().await.ctx.cwd.clone();
                let mem_path = crate::growth::memory::Scope::Project.path(&cwd);
                if let Some(p) = mem_path.parent() {
                    let _ = std::fs::create_dir_all(p);
                }
                let existing = std::fs::read_to_string(&mem_path).unwrap_or_default();
                let updated = if existing.trim().is_empty() {
                    content.to_string()
                } else {
                    format!("{}\n---\n{}", existing.trim(), content)
                };
                let _ = std::fs::write(&mem_path, updated);
            }
            build_state_json(&state).await
        }
        ("POST", "/api/clear") => {
            state.agent.lock().await.clear();
            build_state_json(&state).await
        }
        ("POST", "/api/session/load") => {
            let id = body_json["id"].as_str().unwrap_or("").trim();
            let mut ui_msgs = Vec::new();
            if let Some(msgs) = crate::growth::history::load_session(id) {
                let mut a = state.agent.lock().await;
                a.session_id = id.to_string();
                for m in &msgs {
                    match m.role {
                        Role::User => {
                            for b in &m.content {
                                if let Block::Text { text } = b {
                                    if !text.starts_with("<system-reminder>") {
                                        let clean = text.split("\n\n<file").next().unwrap_or(text);
                                        ui_msgs.push(json!({ "type": "user", "text": clean }));
                                    }
                                }
                            }
                        }
                        Role::Assistant => {
                            for b in &m.content {
                                match b {
                                    Block::Reasoning { text, .. } => {
                                        ui_msgs.push(json!({ "type": "reasoning", "text": text }));
                                    }
                                    Block::Text { text } => {
                                        ui_msgs.push(json!({ "type": "text", "text": text }));
                                    }
                                    Block::ToolUse { name, input, .. } => {
                                        let title = input
                                            .get("file_path")
                                            .or_else(|| input.get("command"))
                                            .or_else(|| input.get("pattern"))
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("")
                                            .to_string();
                                        ui_msgs.push(json!({ "type": "tool_call", "name": name, "title": title }));
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                a.messages = msgs;
            }
            json!({ "ok": true, "messages": ui_msgs })
        }
        ("POST", "/api/session/delete") => {
            let id = body_json["id"].as_str().unwrap_or("").trim();
            if crate::growth::history::delete_session(id) {
                let mut a = state.agent.lock().await;
                if a.session_id == id {
                    a.clear();
                }
            }
            build_state_json(&state).await
        }
        ("POST", "/api/project/remove") => {
            if let Some(p) = body_json["path"].as_str().filter(|s| !s.is_empty()) {
                remove_recent_project(p);
            }
            build_state_json(&state).await
        }
        ("POST", "/api/project") => {
            if let Some(p) = body_json["path"].as_str().filter(|s| !s.is_empty()) {
                switch_workspace(&state, PathBuf::from(p)).await;
            }
            build_state_json(&state).await
        }
        ("POST", "/api/browse_folder") => {
            let cur = state.agent.lock().await.ctx.cwd.clone();
            if let Some(chosen) = pick_folder_dialog(&cur).await {
                switch_workspace(&state, chosen).await;
            }
            build_state_json(&state).await
        }
        ("POST", "/api/stop") => {
            if let Some(c) = state.active_cancel.lock().await.as_ref() {
                c.notify_one();
            }
            json!({ "ok": true })
        }
        ("POST", "/api/quit") => {
            if state.auto_exit_on_close {
                let st = state.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                    if st.sse_tx.receiver_count() == 0 {
                        st.shutdown.notify_one();
                    }
                });
            }
            json!({ "ok": true })
        }
        ("POST", "/api/permission") => {
            let id = body_json["id"].as_u64().unwrap_or(0);
            let dec = match body_json["decision"].as_str().unwrap_or("reject") {
                "once" => Decision::Once,
                "always" => Decision::Always,
                _ => Decision::Reject(None),
            };
            if let Some(tx) = state.pending_perms.lock().await.remove(&id) {
                let _ = tx.send(dec);
            }
            json!({ "ok": true })
        }
        ("POST", "/api/chat") => {
            let raw = body_json["prompt"].as_str().unwrap_or("").trim().to_string();
            if !raw.is_empty() {
                spawn_chat_turn(state.clone(), raw).await;
            }
            json!({ "ok": true })
        }
        _ => json!({ "error": "not found" }),
    };

    let bytes = serde_json::to_vec(&resp)?;
    send_response(&mut stream, "200 OK", "application/json; charset=utf-8", &bytes).await
}

async fn switch_workspace(state: &WebState, target: PathBuf) {
    if target.is_dir() && !is_system_dir(&target) {
        record_recent_project(&target);
        let mut a = state.agent.lock().await;
        let shell = a.ctx.shell.clone();
        a.system = crate::agent::prompt::build_system_prompt(&target, &shell);
        a.ctx = Arc::new(crate::tools::ToolCtx {
            cwd: target,
            tracker: Arc::new(std::sync::Mutex::new(crate::session::FileTracker::default())),
            todos: Arc::new(std::sync::Mutex::new(Vec::new())),
            shell,
        });
        a.clear();
    }
}

pub fn is_system_dir(p: &Path) -> bool {
    let s = p.to_string_lossy().to_lowercase().replace('/', "\\");
    s.starts_with("c:\\windows")
        || s.starts_with("c:\\program files")
        || s == "c:\\"
}

fn recent_projects_file() -> PathBuf {
    global_dir().join("recent_projects.json")
}

pub fn load_recent_projects() -> Vec<PathBuf> {
    let p = recent_projects_file();
    let mut out: Vec<PathBuf> = if let Ok(text) = std::fs::read_to_string(&p) {
        let list: Vec<String> = serde_json::from_str(&text).unwrap_or_default();
        list.into_iter()
            .map(PathBuf::from)
            .filter(|d| d.is_dir() && !is_system_dir(d))
            .collect()
    } else {
        Vec::new()
    };
    // 若列表较少，自动合并本机 ~/.codex/config.toml 中的项目工作区
    if out.len() <= 1 {
        if let Some(home) = dirs::home_dir() {
            let codex_cfg = home.join(".codex").join("config.toml");
            if let Ok(text) = std::fs::read_to_string(codex_cfg) {
                if let Ok(v) = toml::from_str::<toml::Value>(&text) {
                    if let Some(projs) = v.get("projects").and_then(|p| p.as_table()) {
                        for (k, _) in projs {
                            let pb = PathBuf::from(k);
                            if pb.is_dir()
                                && !is_system_dir(&pb)
                                && !out.iter().any(|x| x.to_string_lossy().eq_ignore_ascii_case(&pb.to_string_lossy()))
                            {
                                out.push(pb);
                                if out.len() >= 12 {
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

pub fn record_recent_project(cwd: &Path) {
    if is_system_dir(cwd) || !cwd.is_dir() {
        return;
    }
    let mut list = load_recent_projects();
    list.retain(|p| !p.to_string_lossy().eq_ignore_ascii_case(&cwd.to_string_lossy()));
    list.insert(0, cwd.to_path_buf());
    list.truncate(12);
    let strings: Vec<String> = list.into_iter().map(|p| p.display().to_string()).collect();
    let file = recent_projects_file();
    if let Some(parent) = file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(&strings) {
        let _ = std::fs::write(file, json);
    }
}

pub fn remove_recent_project(target: &str) {
    let mut list = load_recent_projects();
    list.retain(|p| !p.to_string_lossy().eq_ignore_ascii_case(target));
    let strings: Vec<String> = list.into_iter().map(|p| p.display().to_string()).collect();
    if let Ok(json) = serde_json::to_string_pretty(&strings) {
        let _ = std::fs::write(recent_projects_file(), json);
    }
}

async fn spawn_chat_turn(state: Arc<WebState>, raw: String) {
    let _ = state.sse_tx.send(json!({ "type": "user", "text": raw }).to_string());
    let cancel = Arc::new(Notify::new());
    *state.active_cancel.lock().await = Some(cancel.clone());

    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<AgentEvent>();
    let state_for_events = state.clone();

    tokio::spawn(async move {
        while let Some(ev) = ev_rx.recv().await {
            let payload = match ev {
                AgentEvent::Delta(Delta::Text(t)) => json!({ "type": "text", "text": t }),
                AgentEvent::Delta(Delta::Reasoning(t)) => json!({ "type": "reasoning", "text": t }),
                AgentEvent::ToolCall { name, title } => json!({ "type": "tool_call", "name": name, "title": title }),
                AgentEvent::ToolResult { name, ok, summary, display } => json!({
                    "type": "tool_result",
                    "name": name,
                    "ok": ok,
                    "summary": summary,
                    "display": display
                }),
                AgentEvent::Permission { name, title, preview, reply } => {
                    let id = state_for_events.next_perm_id.fetch_add(1, Ordering::SeqCst);
                    state_for_events.pending_perms.lock().await.insert(id, reply);
                    json!({ "type": "permission", "id": id, "name": name, "title": title, "preview": preview })
                }
                AgentEvent::Notice(n) => json!({ "type": "notice", "text": n }),
            };
            let _ = state_for_events.sse_tx.send(payload.to_string());
        }
    });

    tokio::spawn(async move {
        {
            let cfg = state.config.lock().await;
            cfg.ensure_local_magpie_running();
        }
        let mut a = state.agent.lock().await;
        let expanded = expand_at_mentions(&a.ctx.cwd, &raw);
        let res = a.run_turn(expanded, ev_tx, cancel).await;
        if let Err(e) = res {
            let _ = state
                .sse_tx
                .send(json!({ "type": "notice", "text": format!("错误: {:#}", e) }).to_string());
        }
        let u = &a.usage;
        let _ = state.sse_tx.send(
            json!({
                "type": "done",
                "usage": { "input": u.input, "output": u.output, "cache_read": u.cache_read }
            })
            .to_string(),
        );
    });
}

async fn build_state_json(state: &WebState) -> Value {
    let a = state.agent.lock().await;
    let cfg = state.config.lock().await;
    let cwd = &a.ctx.cwd;

    let mut files = Vec::new();
    for entry in ignore::WalkBuilder::new(cwd).max_depth(Some(5)).build().flatten() {
        if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            if let Ok(rel) = entry.path().strip_prefix(cwd) {
                files.push(rel.to_string_lossy().replace('\\', "/"));
                if files.len() >= 500 {
                    break;
                }
            }
        }
    }
    files.sort();

    let mem_path = crate::growth::memory::Scope::Project.path(cwd);
    let memories: Vec<String> = std::fs::read_to_string(&mem_path)
        .unwrap_or_default()
        .split("\n---\n")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();

    let skills: Vec<Value> = crate::growth::skills::discover_skills(cwd)
        .into_iter()
        .map(|s| json!({ "name": s.name, "description": s.description, "source": s.source }))
        .collect();

    let todos = a.ctx.todos.lock().unwrap().clone();

    record_recent_project(cwd);
    let recent = load_recent_projects();
    let projects: Vec<Value> = recent
        .into_iter()
        .map(|p| {
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| p.display().to_string());
            let active = p.to_string_lossy().eq_ignore_ascii_case(&cwd.to_string_lossy());
            json!({
                "name": name,
                "path": p.display().to_string(),
                "active": active,
            })
        })
        .collect();

    let sessions: Vec<Value> = crate::growth::history::list_sessions(cwd)
        .into_iter()
        .map(|s| {
            json!({
                "id": s.id,
                "title": s.title,
                "count": s.count,
                "active": s.id == a.session_id,
            })
        })
        .collect();

    json!({
        "session_id": a.session_id,
        "mode": if a.mode == Mode::Plan { "plan" } else { "build" },
        "model": a.model_ref,
        "models": cfg.discovered_models,
        "has_key": cfg.has_api_key(&a.model_ref),
        "cwd": cwd.display().to_string(),
        "projects": projects,
        "sessions": sessions,
        "files": files,
        "memories": memories,
        "skills": skills,
        "todos": todos,
    })
}

fn persist_key_to_disk(model_ref: &str, api_key: &str, base_url: Option<&str>) -> Result<()> {
    let Some((pid, _)) = model_ref.split_once('/') else {
        return Ok(());
    };
    let path = global_dir().join("config.toml");
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let mut doc: toml::Value = if path.exists() {
        toml::from_str(&std::fs::read_to_string(&path).unwrap_or_default())
            .unwrap_or_else(|_| toml::Value::Table(Default::default()))
    } else {
        toml::Value::Table(Default::default())
    };
    if let toml::Value::Table(root) = &mut doc {
        root.insert("model".into(), toml::Value::String(model_ref.to_string()));
        let provs = root
            .entry("provider")
            .or_insert_with(|| toml::Value::Table(Default::default()));
        if let toml::Value::Table(pt) = provs {
            let entry = pt
                .entry(pid.to_string())
                .or_insert_with(|| toml::Value::Table(Default::default()));
            if let toml::Value::Table(t) = entry {
                let kind = if pid == "anthropic" { "anthropic" } else { "openai" };
                t.entry("type")
                    .or_insert_with(|| toml::Value::String(kind.into()));
                t.insert("api_key".into(), toml::Value::String(api_key.to_string()));
                if let Some(u) = base_url.filter(|s| !s.is_empty()) {
                    t.insert("base_url".into(), toml::Value::String(u.to_string()));
                }
            }
        }
    }
    std::fs::write(&path, toml::to_string_pretty(&doc)?)?;
    Ok(())
}

fn parse_query_param(query: &str, key: &str) -> Option<String> {
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=')?;
        if k == key {
            return Some(url_decode(v));
        }
    }
    None
}

fn url_decode(s: &str) -> String {
    let mut bytes = Vec::with_capacity(s.len());
    let raw = s.as_bytes();
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'%' && i + 2 < raw.len() {
            if let Ok(hex) = std::str::from_utf8(&raw[i + 1..i + 3]) {
                if let Ok(b) = u8::from_str_radix(hex, 16) {
                    bytes.push(b);
                    i += 3;
                    continue;
                }
            }
        } else if raw[i] == b'+' {
            bytes.push(b' ');
            i += 1;
            continue;
        }
        bytes.push(raw[i]);
        i += 1;
    }
    String::from_utf8_lossy(&bytes).to_string()
}

async fn send_response(stream: &mut TcpStream, status: &str, content_type: &str, body: &[u8]) -> Result<()> {
    let hdr = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        status,
        content_type,
        body.len()
    );
    stream.write_all(hdr.as_bytes()).await?;
    stream.write_all(body).await?;
    Ok(())
}
