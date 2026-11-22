//! Native desktop: all model/tool/filesystem work is owned by one background worker.
mod worker;
use crate::agent::{permission::Decision, Agent, AgentEvent, Mode};
use crate::ai::{Block, Delta, Message, Role};
use crate::config::Config;
use crate::growth::history;
use eframe::egui;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, Notify};
use worker::{Command, Event, Snapshot};

pub fn run(agent: Agent, config: Config) -> anyhow::Result<()> {
    let initial_plan = agent.mode == Mode::Plan;
    let autorun = std::env::var_os("FERRO_DESKTOP_AUTORUN").map(|p| Autorun {
        report_path: PathBuf::from(p),
        stage: 0,
        first_turn_reply: String::new(),
        perm_modal_title: String::new(),
        perm_tool_output: String::new(),
        stop_notice: String::new(),
    });
    let state_file = std::env::var_os("FERRO_DESKTOP_STATE_FILE").map(PathBuf::from);
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        viewport: egui::ViewportBuilder::default()
            .with_title("Ferro · workspace")
            .with_icon(Arc::new(app_icon()))
            .with_inner_size([1220.0, 820.0])
            .with_min_inner_size([760.0, 520.0]),
        ..Default::default()
    };
    eframe::run_native("Ferro · workspace", options, Box::new(move |cc| {
        install_fonts(&cc.egui_ctx);
        let (tx, rx) = worker::spawn(agent, config, cc.egui_ctx.clone());
        Ok(Box::new(Desktop { tx, rx, state: None, rows: Vec::new(), input: String::new(),
            search: String::new(), active: None, next_turn: 0, pending: None, loading: true,
            settings: false, model: String::new(), url: String::new(), key: String::new(),
            anthropic: false, persist: false, plan: initial_plan, notice: String::new(), dark: true,
            ime_composing: false, folder_rx: None, autorun, state_file, last_state_dump: String::new() }))
    })).map_err(|e| anyhow::anyhow!("Native window: {e}"))
}

struct Autorun {
    report_path: PathBuf,
    stage: u8,
    first_turn_reply: String,
    perm_modal_title: String,
    perm_tool_output: String,
    stop_notice: String,
}
enum Segment {
    Prose(String),
    Code { lang: String, code: String },
}

pub struct Row {
    pub label: String,
    pub text: String,
    pub tool_id: Option<String>,
    segments: Vec<Segment>,
    height: f32,
}

impl Row {
    pub fn new(label: impl Into<String>, text: impl Into<String>, tool_id: Option<String>) -> Self {
        let label = label.into();
        let text = text.into();
        let segments = if tool_id.is_some() || label == "Thinking" {
            Vec::new()
        } else {
            parse_segments(&text)
        };
        Self { label, text, tool_id, segments, height: 0.0 }
    }

    fn set_text(&mut self, label: String, text: String) {
        self.label = label;
        self.text = text;
        self.segments = if self.tool_id.is_some() || self.label == "Thinking" {
            Vec::new()
        } else {
            parse_segments(&self.text)
        };
        self.height = 0.0;
    }

    fn push_delta(&mut self, delta: &str) {
        self.text.push_str(delta);
        if self.tool_id.is_none() && self.label != "Thinking" {
            self.segments = parse_segments(&self.text);
        }
        self.height = 0.0;
    }
}

fn parse_segments(text: &str) -> Vec<Segment> {
    let mut out = Vec::new();
    for (index, part) in text.split("```").enumerate() {
        if index % 2 == 1 {
            let (lang, code) = part.split_once('\n').unwrap_or(("", part));
            out.push(Segment::Code {
                lang: lang.trim().to_string(),
                code: code.to_string(),
            });
        } else if !part.is_empty() {
            out.push(Segment::Prose(part.to_string()));
        }
    }
    out
}

struct Approval { title: String, preview: String, reply: oneshot::Sender<Decision> }
struct Desktop {
    tx: mpsc::UnboundedSender<Command>, rx: std::sync::mpsc::Receiver<Event>, state: Option<Snapshot>,
    rows: Vec<Row>, input: String, search: String, active: Option<(u64, Arc<Notify>)>, next_turn: u64,
    pending: Option<Approval>, loading: bool, settings: bool, model: String, url: String, key: String,
    anthropic: bool, persist: bool, plan: bool, notice: String, dark: bool,
    ime_composing: bool, folder_rx: Option<std::sync::mpsc::Receiver<Option<PathBuf>>>,
    autorun: Option<Autorun>,
    state_file: Option<PathBuf>, last_state_dump: String,
}
impl Desktop {
    fn command(&mut self, command: Command) {
        if self.active.is_none() && !self.loading {
            self.loading = true;
            if self.tx.send(command).is_err() { self.notice = "Worker disconnected".into(); self.loading = false; }
        }
    }
    fn stop(&mut self) {
        if let Some((_, cancel)) = &self.active { cancel.notify_one(); }
        self.pending.take();
    }
    fn events(&mut self) {
        // Coalesce all pending delta chunks while bounding non-delta transitions per frame.
        let mut non_delta = 0usize;
        while non_delta < 256 {
            let Ok(event) = self.rx.try_recv() else { break };
            match event {
                Event::State(mut state) => {
                    non_delta += 1;
                    if let Some(rows) = state.transcript.take() {
                        self.rows = rows;
                    }
                    self.loading = false;
                    self.state = Some(state);
                }
                Event::Done(id) if self.active.as_ref().map(|a| a.0) == Some(id) => {
                    non_delta += 1;
                    self.pending.take(); self.active = None; self.loading = true;
                }
                Event::Agent(id, event) if self.active.as_ref().map(|a| a.0) == Some(id) => {
                    match event {
                        AgentEvent::Delta(Delta::Text(text)) => self.append("Ferro", &text),
                        AgentEvent::Delta(Delta::Reasoning(text)) => self.append("Thinking", &text),
                        AgentEvent::ToolCall { id, name, title } => {
                            non_delta += 1;
                            self.rows.push(Row::new(format!("Running · {name}"), title, Some(id)));
                        }
                        AgentEvent::ToolResult { id, name, ok, summary, display } => {
                            non_delta += 1;
                            let label = format!("{} · {name}", if ok { "Done" } else { "Failed" });
                            let text = display.map(|d| format!("{summary}\n\n{d}")).unwrap_or(summary);
                            if let Some(row) = self.rows.iter_mut().find(|r| r.tool_id.as_ref() == Some(&id)) {
                                row.set_text(label, text);
                            } else {
                                self.rows.push(Row::new(label, text, Some(id)));
                            }
                        }
                        AgentEvent::Permission { name, title, preview, reply } => {
                            non_delta += 1;
                            self.pending = Some(Approval { title: format!("{name}: {title}"), preview: preview.unwrap_or_default(), reply });
                        }
                        AgentEvent::Notice(text) => {
                            non_delta += 1;
                            self.notice = text;
                        }
                    }
                }
                Event::Notice(text) => {
                    non_delta += 1;
                    self.notice = text;
                }
                Event::Sessions(sessions) => {
                    non_delta += 1;
                    if let Some(s) = &mut self.state { s.sessions = sessions; }
                    self.loading = false;
                }
                _ => {
                    non_delta += 1;
                }
            }
        }
    }
    fn append(&mut self, label: &str, text: &str) {
        if let Some(row) = self.rows.last_mut().filter(|r| r.label == label && r.tool_id.is_none()) {
            row.push_delta(text);
        } else {
            self.rows.push(Row::new(label, text, None));
        }
    }
    fn send(&mut self) {
        if self.input.trim().is_empty() || self.active.is_some() || self.loading { return; }
        self.next_turn += 1;
        let id = self.next_turn;
        let cancel = Arc::new(Notify::new());
        let prompt = std::mem::take(&mut self.input);
        self.rows.push(Row::new("You", prompt.clone(), None));
        self.active = Some((id, cancel.clone()));
        self.notice.clear();
        if self.tx.send(Command::Turn { id, prompt, cancel, mode: if self.plan { Mode::Plan } else { Mode::Build } }).is_err() {
            self.active = None; self.notice = "Worker disconnected".into();
        }
    }
    fn load_model_fields(&mut self) {
        if let Some(s) = &self.state {
            if let Some((pid, _)) = self.model.split_once('/') {
                if let Some(pc) = s.config.provider.get(pid) {
                    self.url = pc.base_url.clone().unwrap_or_default();
                    self.key = pc.api_key.clone().unwrap_or_default();
                    self.anthropic = matches!(pc.kind, crate::config::ProviderKind::Anthropic);
                }
            }
        }
    }
}
impl eframe::App for Desktop {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.events();
        if let Some(rx) = &self.folder_rx {
            match rx.try_recv() {
                Ok(Some(chosen)) => {
                    self.folder_rx = None;
                    self.search.clear();
                    self.command(Command::Workspace(chosen));
                }
                Ok(None) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.folder_rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if self.active.is_some() || self.loading || self.folder_rx.is_some() || self.autorun.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(80));
        }
        ctx.set_visuals(if self.dark { egui::Visuals::dark() } else { egui::Visuals::light() });
        let idle = self.active.is_none() && !self.loading && self.folder_rx.is_none();
        let mut open_project_clicked = false;
        let mut open_btn_rect = egui::Rect::NOTHING;
        let mut composer_rect = egui::Rect::NOTHING;
        if let Some(sf) = &self.state_file {
            let trig = sf.with_extension("trigger");
            if let Ok(cmd) = std::fs::read_to_string(&trig) {
                let _ = std::fs::remove_file(&trig);
                let cmd = cmd.trim();
                let composer_id = egui::Id::new("composer_input");
                if cmd == "open_project" && idle {
                    open_project_clicked = true;
                } else if cmd == "focus_composer" {
                    ctx.memory_mut(|m| m.request_focus(composer_id));
                } else if cmd == "ctrl_enter" {
                    ctx.memory_mut(|m| m.request_focus(composer_id));
                    ctx.input_mut(|i| {
                        i.events.push(egui::Event::Key {
                            key: egui::Key::Enter,
                            physical_key: Some(egui::Key::Enter),
                            pressed: true,
                            repeat: false,
                            modifiers: egui::Modifiers { ctrl: true, command: true, ..Default::default() },
                        });
                    });
                } else if cmd == "plain_enter" {
                    ctx.memory_mut(|m| m.request_focus(composer_id));
                    ctx.input_mut(|i| {
                        i.events.push(egui::Event::Key {
                            key: egui::Key::Enter,
                            physical_key: Some(egui::Key::Enter),
                            pressed: true,
                            repeat: false,
                            modifiers: egui::Modifiers::NONE,
                        });
                    });
                } else if let Some(txt) = cmd.strip_prefix("text:") {
                    ctx.memory_mut(|m| m.request_focus(composer_id));
                    ctx.input_mut(|i| i.events.push(egui::Event::Text(txt.to_string())));
                } else if let Some(txt) = cmd.strip_prefix("ime_commit:") {
                    ctx.memory_mut(|m| m.request_focus(composer_id));
                    ctx.input_mut(|i| {
                        i.events.push(egui::Event::Ime(egui::ImeEvent::Commit(txt.to_string())));
                        i.events.push(egui::Event::Key {
                            key: egui::Key::Enter,
                            physical_key: Some(egui::Key::Enter),
                            pressed: true,
                            repeat: false,
                            modifiers: egui::Modifiers { ctrl: true, command: true, ..Default::default() },
                        });
                    });
                }
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(80));
        }

        egui::TopBottomPanel::top("masthead").show(ctx, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.heading(egui::RichText::new("FERRO").color(egui::Color32::from_rgb(79, 184, 162)));
                ui.label("/  local workspace");
                if ui.add_enabled(idle, egui::Button::new("Model setup")).clicked() {
                    self.model = self.state.as_ref().map(|s| s.model.clone()).unwrap_or_default();
                    self.load_model_fields(); self.settings = true;
                }
                ui.checkbox(&mut self.dark, "Dark");
                if self.active.is_some() { ui.spinner(); ui.label("Working"); }
                else if self.loading { ui.label("Loading…"); }
                else { ui.label("Ready"); }
            });
            ui.add_space(8.0);
        });
        egui::SidePanel::left("workspace").default_width(250.0).resizable(true).show(ctx, |ui| {
            ui.add_space(12.0); ui.heading("Workspace");
            if let Some(s) = &self.state { ui.label(s.cwd.display().to_string()); }
            let open_resp = ui.add_enabled(idle, egui::Button::new("Open project…"));
            open_btn_rect = open_resp.rect;
            if open_resp.clicked() { open_project_clicked = true; }
            ui.separator();
            let recent = self.state.as_ref().map(|s| s.recent.clone()).unwrap_or_default();
            egui::CollapsingHeader::new("Recent workspaces").default_open(true).show(ui, |ui| {
                for path in recent {
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    if ui.add_enabled(idle, egui::Button::new(name)).on_hover_text(path.display().to_string()).clicked() {
                        self.search.clear(); self.command(Command::Workspace(path));
                    }
                }
            });
            ui.separator();
            ui.horizontal(|ui| {
                ui.heading("History");
                if ui.add_enabled(idle, egui::Button::new("New")).clicked() { self.search.clear(); self.command(Command::New); }
            });
            ui.add(egui::TextEdit::singleline(&mut self.search).hint_text("Search conversation text"));
            if ui.add_enabled(idle, egui::Button::new("Search")).clicked() { self.command(Command::Search(self.search.clone())); }
            let mut reopen = None;
            let mut delete = None;
            egui::ScrollArea::vertical().id_salt("history_scroll").show(ui, |ui| {
                if let Some(s) = &self.state {
                    for session in &s.sessions {
                        ui.horizontal(|ui| {
                            if ui.add_enabled(idle, egui::Button::new(&session.title).wrap()).on_hover_text(format!("{} messages · {}", session.count, session.id)).clicked() {
                                reopen = Some(session.id.clone());
                            }
                            if ui.add_enabled(idle, egui::Button::new("×").small()).on_hover_text("Delete session").clicked() {
                                delete = Some(session.id.clone());
                            }
                        });
                    }
                }
            });
            if let Some(id) = reopen { self.command(Command::Reopen(id)); }
            if let Some(id) = delete { self.command(Command::DeleteSession(id)); }
        });
        egui::TopBottomPanel::bottom("composer").show(ctx, |ui| {
            if !self.notice.is_empty() { ui.colored_label(egui::Color32::from_rgb(220, 160, 80), &self.notice); }
            if let Some(s) = &self.state { ui.small(format!("{} · {}", s.model, s.usage)); }
            ui.horizontal(|ui| {
                ui.add_enabled(self.active.is_none(), egui::Checkbox::new(&mut self.plan, "PLAN (read only)"));
                if ui.add_enabled(idle, egui::Button::new("Undo last turn edits")).clicked() { self.command(Command::Undo); }
                if let Some(s) = &self.state {
                    if s.todos != "(空)" { ui.collapsing("Tasks", |ui| { ui.label(&s.todos); }); }
                }
            });
            let composer_id = egui::Id::new("composer_input");
            let composer_focused = ctx.memory(|m| m.has_focus(composer_id));
            let shortcut = ctx.input_mut(|i| {
                consume_send_shortcut(&mut i.events, &mut self.ime_composing, composer_focused)
            });
            let edit = ui.add(
                egui::TextEdit::multiline(&mut self.input)
                    .id(composer_id)
                    .desired_rows(3)
                    .desired_width(f32::INFINITY)
                    .hint_text("Describe a task · @path adds a file · Ctrl/Cmd+Enter sends"),
            );
            composer_rect = edit.rect;
            ui.horizontal(|ui| {
                if ui.add_enabled(idle, egui::Button::new("Send")).clicked() || (idle && shortcut) { self.send(); }
                if ui.add_enabled(self.active.is_some(), egui::Button::new("Stop")).clicked() { self.stop(); }
            });
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
                if self.rows.is_empty() {
                    ui.add_space(60.0); ui.heading("Build something with your code.");
                    ui.label("Choose a project, configure a model, then start a conversation.");
                    ui.label("Ferro needs no account. Use Ollama or an existing API.");
                }
                let total = self.rows.len();
                let clip = ui.clip_rect();
                for (index, row) in self.rows.iter_mut().enumerate() {
                    // Viewport culling for already-measured off-screen rows (keeps 100% history intact).
                    if row.height > 0.0 && index + 2 < total {
                        let top = ui.cursor().min.y;
                        let bottom = top + row.height;
                        if bottom < clip.min.y - 400.0 || top > clip.max.y + 400.0 {
                            ui.allocate_space(egui::vec2(ui.available_width(), row.height));
                            ui.add_space(10.0);
                            continue;
                        }
                    }
                    ui.push_id(index, |ui| {
                        let resp = egui::Frame::group(ui.style()).inner_margin(14.0).show(ui, |ui| {
                            ui.set_min_width((ui.available_width() - 8.0).max(0.0));
                            ui.horizontal(|ui| {
                                ui.strong(&row.label);
                                if ui.small_button("Copy").clicked() { ui.ctx().copy_text(row.text.clone()); }
                            });
                            if row.tool_id.is_some() || row.label == "Thinking" {
                                ui.collapsing("Details / diff", |ui| { render_text(ui, &row.text); });
                            } else {
                                render_segments(ui, &row.segments);
                            }
                        });
                        row.height = resp.response.rect.height();
                        ui.add_space(10.0);
                    });
                }
            });
        });
        if self.pending.as_ref().is_some_and(|p| p.reply.is_closed()) { self.pending.take(); }
        if let Some(p) = &self.pending {
            let mut choice = None;
            egui::Window::new("Tool approval").collapsible(false).resizable(true).show(ctx, |ui| {
                ui.label(&p.title);
                egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| { render_text(ui, &p.preview); });
                ui.horizontal(|ui| {
                    if ui.button("Allow once").clicked() { choice = Some(Decision::Once); }
                    if ui.button("Allow this turn").clicked() { choice = Some(Decision::Always); }
                    if ui.button("Reject & stop").clicked() { choice = Some(Decision::Reject(None)); }
                });
            });
            if let Some(choice) = choice { if let Some(p) = self.pending.take() { let _ = p.reply.send(choice); } }
        }
        if self.settings {
            let mut open = true;
            egui::Window::new("Model setup · no Ferro login").open(&mut open).default_width(500.0).show(ctx, |ui| {
                ui.label("Provider/model (custom providers supported)");
                ui.text_edit_singleline(&mut self.model);
                ui.horizontal(|ui| {
                    if ui.button("Load provider values").clicked() { self.load_model_fields(); }
                    if ui.button("Ollama").clicked() { self.model = "ollama/".into(); self.url = "http://localhost:11434/v1".into(); self.key.clear(); self.anthropic = false; }
                });
                if let Some(s) = &self.state {
                    egui::ComboBox::from_label("Imported models").selected_text("Select…").show_ui(ui, |ui| {
                        for model in &s.config.discovered_models {
                            ui.selectable_value(&mut self.model, model.id.clone(), &model.label);
                        }
                    });
                }
                ui.label("Base URL (include /v1 where required)"); ui.text_edit_singleline(&mut self.url);
                ui.label("API Key, env:VARIABLE, or empty for no authentication");
                ui.add(egui::TextEdit::singleline(&mut self.key).password(true));
                ui.checkbox(&mut self.anthropic, "Anthropic protocol (otherwise OpenAI compatible)");
                ui.checkbox(&mut self.persist, "Save to global config (key stored as entered)");
                ui.label("Ollama: enter an already installed model name after ollama/. No model download or login is performed.");
                if ui.add_enabled(idle, egui::Button::new("Apply")).clicked() {
                    self.command(Command::Setup { model: self.model.trim().into(), url: self.url.trim().trim_end_matches('/').into(), key: self.key.trim().into(), anthropic: self.anthropic, persist: self.persist });
                    self.key.clear(); self.settings = false;
                }
            });
            self.settings &= open;
        }
        if open_project_clicked && self.folder_rx.is_none() {
            let initial = self.state.as_ref().map(|s| s.cwd.clone());
            #[cfg(windows)]
            {
                let _ = frame;
                let (ftx, frx) = std::sync::mpsc::channel();
                let repaint = ctx.clone();
                self.folder_rx = Some(frx);
                std::thread::spawn(move || {
                    let mut dlg = rfd::FileDialog::new();
                    if let Some(dir) = initial.filter(|d| d.is_dir()) {
                        dlg = dlg.set_directory(dir);
                    }
                    let chosen = dlg.pick_folder();
                    let _ = ftx.send(chosen);
                    repaint.request_repaint();
                });
            }
            #[cfg(not(windows))]
            {
                let mut dlg = rfd::FileDialog::new().set_parent(frame);
                if let Some(dir) = initial.filter(|d| d.is_dir()) {
                    dlg = dlg.set_directory(dir);
                }
                if let Some(chosen) = dlg.pick_folder() {
                    self.search.clear();
                    self.command(Command::Workspace(chosen));
                }
                ctx.request_repaint();
            }
        }
        if let Some(path) = &self.state_file {
            let ppp = ctx.pixels_per_point();
            let dump = serde_json::json!({
                "cwd": self.state.as_ref().map(|s| s.cwd.display().to_string()).unwrap_or_default(),
                "loading": self.loading,
                "active": self.active.is_some(),
                "input": self.input,
                "notice": self.notice,
                "rows_count": self.rows.len(),
                "last_user_row": self.rows.iter().rev().find(|r| r.label == "You").map(|r| r.text.clone()).unwrap_or_default(),
                "last_ferro_row": self.rows.iter().rev().find(|r| r.label == "Ferro").map(|r| r.text.clone()).unwrap_or_default(),
                "ppp": ppp,
                "open_btn_px": [
                    (open_btn_rect.center().x * ppp) as i32,
                    (open_btn_rect.center().y * ppp) as i32
                ],
                "composer_px": [
                    (composer_rect.center().x * ppp) as i32,
                    (composer_rect.center().y * ppp) as i32
                ],
            })
            .to_string();
            if dump != self.last_state_dump {
                let _ = std::fs::write(path, &dump);
                self.last_state_dump = dump;
            }
        }
        self.drive_autorun(ctx);
    }
}
impl Desktop {
    fn drive_autorun(&mut self, ctx: &egui::Context) {
        let Some(mut auto) = self.autorun.take() else { return };
        let idle = self.active.is_none() && !self.loading;
        match auto.stage {
            0 if idle && self.state.is_some() => {
                // Stage 1: send first-turn conversation
                self.plan = false;
                self.input = "FIRST_TURN_HELLO: Reply with the single word HELLO_NATIVE and do not use tools.".into();
                self.send();
                auto.stage = 1;
            }
            1 if idle => {
                auto.first_turn_reply = self
                    .rows
                    .iter()
                    .rev()
                    .find(|r| r.label == "Ferro")
                    .map(|r| r.text.clone())
                    .unwrap_or_default();
                // Stage 2: trigger permission dialog in PLAN mode
                self.plan = true;
                self.input = "ASK_PERM_BASH: Call the bash tool with command: echo gui_perm_approved_ok".into();
                self.send();
                auto.stage = 2;
            }
            2 => {
                if let Some(p) = self.pending.take() {
                    auto.perm_modal_title = p.title;
                    let _ = p.reply.send(Decision::Once);
                    auto.stage = 3;
                }
            }
            3 if idle => {
                auto.perm_tool_output = self
                    .rows
                    .iter()
                    .rev()
                    .find(|r| r.tool_id.is_some())
                    .map(|r| format!("{}: {}", r.label, r.text))
                    .unwrap_or_default();
                // Stage 3: trigger streaming turn and click Stop mid-stream
                self.plan = false;
                self.input = "SLOW_STREAM_STOP: Write a long detailed 300-word explanation of Rust ownership.".into();
                self.send();
                auto.stage = 4;
            }
            4 => {
                let has_streamed = self
                    .rows
                    .last()
                    .map(|r| r.label == "Ferro" && !r.text.is_empty())
                    .unwrap_or(false);
                if self.active.is_some() && has_streamed {
                    self.stop();
                    auto.stage = 5;
                }
            }
            5 if idle => {
                auto.stop_notice = self.notice.clone();
                let report = serde_json::json!({
                    "window_renderer": "eframe::Renderer::Glow",
                    "first_turn_reply": auto.first_turn_reply,
                    "perm_modal_title": auto.perm_modal_title,
                    "perm_tool_output": auto.perm_tool_output,
                    "stop_notice": auto.stop_notice,
                    "rows_count": self.rows.len(),
                });
                let _ = std::fs::write(
                    &auto.report_path,
                    serde_json::to_string_pretty(&report).unwrap_or_default(),
                );
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            _ => {}
        }
        self.autorun = Some(auto);
    }
}
impl Drop for Desktop { fn drop(&mut self) { self.stop(); } }

pub fn transcript(messages: &[Message]) -> Vec<Row> {
    let mut rows = Vec::with_capacity(messages.len());
    for message in messages {
        for block in &message.content {
            let (label, text, id) = match block {
                Block::Text { text } if !text.starts_with("<system-reminder>") => {
                    let clean = if message.role == Role::User {
                        text.split("\n\n<file").next().unwrap_or(text).to_string()
                    } else {
                        text.clone()
                    };
                    (if message.role == Role::User { "You" } else { "Ferro" }.into(), clean, None)
                }
                Block::Reasoning { text, .. } => ("Thinking".into(), text.clone(), None),
                Block::ToolUse { id, name, input } => (format!("Tool · {name}"), serde_json::to_string_pretty(input).unwrap_or_default(), Some(id.clone())),
                Block::ToolResult { tool_use_id, content, is_error } => (if *is_error { "Failed" } else { "Result" }.into(), content.clone(), Some(tool_use_id.clone())),
                _ => continue,
            };
            rows.push(Row::new(label, text, id));
        }
    }
    rows
}

fn render_segments(ui: &mut egui::Ui, segments: &[Segment]) {
    for seg in segments {
        match seg {
            Segment::Prose(part) => {
                ui.add(egui::Label::new(part).wrap().selectable(true));
            }
            Segment::Code { lang, code } => {
                ui.horizontal(|ui| {
                    ui.small(lang);
                    if ui.small_button("Copy code").clicked() {
                        ui.ctx().copy_text(code.clone());
                    }
                });
                egui::ScrollArea::horizontal().id_salt(ui.next_auto_id()).show(ui, |ui| {
                    ui.add(egui::Label::new(egui::RichText::new(code).monospace()).selectable(true));
                });
            }
        }
    }
}

fn render_text(ui: &mut egui::Ui, text: &str) {
    // Fenced code remains selectable and copyable, including an unfinished streamed fence.
    for (index, part) in text.split("```").enumerate() {
        if index % 2 == 1 {
            let (language, code) = part.split_once('\n').unwrap_or(("", part));
            ui.horizontal(|ui| { ui.small(language); if ui.small_button("Copy code").clicked() { ui.ctx().copy_text(code.into()); } });
            egui::ScrollArea::horizontal().id_salt(ui.next_auto_id()).show(ui, |ui| {
                ui.add(egui::Label::new(egui::RichText::new(code).monospace()).selectable(true));
            });
        } else {
            ui.add(egui::Label::new(part).wrap().selectable(true));
        }
    }
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let paths = if cfg!(windows) {
        // Prefer single-face TTF (simhei.ttf ~9.7 MB) over multi-face TTC (msyh.ttc ~19.6 MB) to cut font atlas RAM in half.
        vec![PathBuf::from("C:/Windows/Fonts/simhei.ttf"), PathBuf::from("C:/Windows/Fonts/msyh.ttc")]
    } else if cfg!(target_os = "macos") {
        vec![PathBuf::from("/System/Library/Fonts/PingFang.ttc"), PathBuf::from("/System/Library/Fonts/STHeiti Medium.ttc")]
    } else {
        vec![PathBuf::from("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc")]
    };
    let custom = std::env::var_os("FERRO_CJK_FONT").map(PathBuf::from);
    for path in custom.into_iter().chain(paths) {
        if let Ok(bytes) = std::fs::read(path) {
            fonts.font_data.insert("cjk".into(), egui::FontData::from_owned(bytes));
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts.families.entry(family).or_default().push("cjk".into());
            }
            break;
        }
    }
    ctx.set_fonts(fonts);
}

pub fn app_icon() -> egui::IconData {
    const RGBA: &[u8] = include_bytes!("../../assets/icon_64_rgba.bin");
    egui::IconData {
        rgba: RGBA.to_vec(),
        width: 64,
        height: 64,
    }
}

/// Inspect raw input events before `TextEdit` runs:
/// - Tracks active IME composition (`Preedit` / `Commit` / `Disabled`) so confirming Chinese IME
///   candidates never misfires a send.
/// - Matches `Ctrl+Enter` / `Cmd+Enter` directly on each `Event::Key`'s own `modifiers` (avoiding
///   stale end-of-frame `InputState::modifiers` when `Ctrl` is released in the same frame) and
///   checks both logical `key` and `physical_key` for Windows IME/Numpad Enter compatibility.
/// - Leaves plain `Enter` untouched so `TextEdit::multiline` inserts a newline normally.
fn consume_send_shortcut(
    events: &mut Vec<egui::Event>,
    ime_composing: &mut bool,
    composer_focused: bool,
) -> bool {
    let mut ime_activity_this_frame = false;
    for ev in events.iter() {
        if let egui::Event::Ime(ime) = ev {
            match ime {
                egui::ImeEvent::Preedit(text) => {
                    *ime_composing = !text.is_empty();
                    if !text.is_empty() {
                        ime_activity_this_frame = true;
                    }
                }
                egui::ImeEvent::Commit(_) => {
                    *ime_composing = false;
                    ime_activity_this_frame = true;
                }
                egui::ImeEvent::Disabled => {
                    *ime_composing = false;
                }
                egui::ImeEvent::Enabled => {}
            }
        }
    }

    if !composer_focused || *ime_composing || ime_activity_this_frame {
        return false;
    }

    let mut triggered = false;
    events.retain(|ev| {
        let is_send = matches!(
            ev,
            egui::Event::Key {
                key,
                physical_key,
                pressed: true,
                modifiers,
                ..
            } if (*key == egui::Key::Enter || *physical_key == Some(egui::Key::Enter))
                && (modifiers.command || modifiers.ctrl)
                && !modifiers.alt
        );
        if is_send {
            triggered = true;
            false // consume Ctrl/Cmd+Enter so TextEdit does not process it
        } else {
            true
        }
    });
    triggered
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_ev(key: egui::Key, physical_key: Option<egui::Key>, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    #[test]
    fn ctrl_enter_sends_and_consumes_when_focused() {
        let mut ime = false;
        let mods = egui::Modifiers { ctrl: true, command: true, ..Default::default() };
        let mut events = vec![key_ev(egui::Key::Enter, Some(egui::Key::Enter), mods)];
        assert!(consume_send_shortcut(&mut events, &mut ime, true));
        assert!(events.is_empty(), "Ctrl+Enter event should be consumed");
    }

    #[test]
    fn plain_enter_does_not_send_and_stays_for_newline() {
        let mut ime = false;
        let mut events = vec![key_ev(egui::Key::Enter, Some(egui::Key::Enter), egui::Modifiers::NONE)];
        assert!(!consume_send_shortcut(&mut events, &mut ime, true));
        assert_eq!(events.len(), 1, "plain Enter must pass through to TextEdit for newline");
    }

    #[test]
    fn ime_preedit_and_commit_prevent_misfire() {
        let mut ime = false;
        let mods = egui::Modifiers { ctrl: true, command: true, ..Default::default() };

        // During Chinese IME preedit, neither Enter nor Ctrl+Enter should misfire send
        let mut ev1 = vec![
            egui::Event::Ime(egui::ImeEvent::Preedit("nihao".into())),
            key_ev(egui::Key::Enter, Some(egui::Key::Enter), mods),
        ];
        assert!(!consume_send_shortcut(&mut ev1, &mut ime, true));
        assert!(ime);

        // Same-frame IME Commit (confirming Chinese candidate) must not trigger send
        let mut ev2 = vec![
            egui::Event::Ime(egui::ImeEvent::Commit("你好".into())),
            key_ev(egui::Key::Enter, Some(egui::Key::Enter), mods),
        ];
        assert!(!consume_send_shortcut(&mut ev2, &mut ime, true));
        assert!(!ime);

        // Next frame after IME commit finishes, Ctrl+Enter sends normally
        let mut ev3 = vec![key_ev(egui::Key::Enter, Some(egui::Key::Enter), mods)];
        assert!(consume_send_shortcut(&mut ev3, &mut ime, true));
    }

    #[test]
    fn unfocused_composer_ignores_ctrl_enter() {
        let mut ime = false;
        let mods = egui::Modifiers { ctrl: true, command: true, ..Default::default() };
        let mut events = vec![key_ev(egui::Key::Enter, Some(egui::Key::Enter), mods)];
        assert!(!consume_send_shortcut(&mut events, &mut ime, false));
        assert_eq!(events.len(), 1);
    }
}
