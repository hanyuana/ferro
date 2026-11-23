//! Native desktop: all model/tool/filesystem work is owned by one background worker.
mod markdown;
mod theme;
mod worker;
use theme::{Palette, tool_label_zh};
use crate::agent::{permission::Decision, Agent, AgentEvent, Mode};
use crate::ai::{Block, Delta, Message, Role};
use crate::config::Config;
use crate::growth::history;
use eframe::egui;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, Notify};
use worker::{AccessMode, Command, Event, Snapshot};

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
            .with_title("Ferro")
            .with_decorations(false)
            .with_icon(Arc::new(app_icon()))
            .with_inner_size([1220.0, 820.0])
            .with_min_inner_size([760.0, 520.0]),
        ..Default::default()
    };
    eframe::run_native("Ferro · 工作区", options, Box::new(move |cc| {
        install_fonts(&cc.egui_ctx);
        let (tx, rx) = worker::spawn(agent, config, cc.egui_ctx.clone());
        Ok(Box::new(Desktop { tx, rx, state: None, rows: Vec::new(), input: String::new(),
            search: String::new(), search_open: false, active: None, next_turn: 0, pending: None, loading: true,
            settings: false, model: String::new(), url: String::new(), key: String::new(),
            anthropic: false, persist: false, access: if initial_plan { AccessMode::Plan } else { AccessMode::Smart }, notice: String::new(), dark: false, themed: None,
            sidebar: true, last_narrow: false, transcript_width: 0.0,
            ime_composing: false, folder_rx: None, autorun, state_file, last_state_dump: String::new(),
            pending_screenshot: None, open_model_menu_req: false,
            fetched_models: Vec::new(), fetch_status: String::new(), fetching_models: false }))
    })).map_err(|e| anyhow::anyhow!("无法打开桌面窗口：{e}"))
}

struct Autorun {
    report_path: PathBuf,
    stage: u8,
    first_turn_reply: String,
    perm_modal_title: String,
    perm_tool_output: String,
    stop_notice: String,
}
pub struct Row {
    pub label: String,
    pub text: String,
    pub tool_id: Option<String>,
    blocks: Option<Vec<markdown::MdBlock>>,
    height: f32,
}

impl Row {
    pub fn new(label: impl Into<String>, text: impl Into<String>, tool_id: Option<String>) -> Self {
        Self { label: label.into(), text: text.into(), tool_id, blocks: None, height: 0.0 }
    }

    fn set_text(&mut self, label: String, text: String) {
        self.label = label;
        self.text = text;
        self.blocks = None;
        self.height = 0.0;
    }

    fn push_delta(&mut self, delta: &str) {
        self.text.push_str(delta);
        self.blocks = None;
        self.height = 0.0;
    }
}

struct Approval { title: String, preview: String, reply: oneshot::Sender<Decision> }
struct Desktop {
    tx: mpsc::UnboundedSender<Command>, rx: std::sync::mpsc::Receiver<Event>, state: Option<Snapshot>,
    rows: Vec<Row>, input: String, search: String, search_open: bool, active: Option<(u64, Arc<Notify>)>, next_turn: u64,
    pending: Option<Approval>, loading: bool, settings: bool, model: String, url: String, key: String,
    anthropic: bool, persist: bool, access: AccessMode, notice: String, dark: bool, themed: Option<bool>,
    sidebar: bool, last_narrow: bool, transcript_width: f32,
    ime_composing: bool, folder_rx: Option<std::sync::mpsc::Receiver<Option<PathBuf>>>,
    autorun: Option<Autorun>,
    state_file: Option<PathBuf>, last_state_dump: String,
    pending_screenshot: Option<PathBuf>, open_model_menu_req: bool,
    fetched_models: Vec<String>, fetch_status: String, fetching_models: bool,
}
impl Desktop {
    fn command(&mut self, command: Command) {
        if self.active.is_none() && !self.loading {
            self.loading = true;
            self.notice.clear();
            if self.tx.send(command).is_err() { self.notice = "后台服务已断开".into(); self.loading = false; }
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
                            self.rows.push(Row::new(format!("{} · 执行中", tool_label_zh(&name)), title, Some(id)));
                        }
                        AgentEvent::ToolResult { id, name, ok, summary, display } => {
                            non_delta += 1;
                            let label = format!("{} · {}", tool_label_zh(&name), if ok { "完成" } else { "失败" });
                            let text = display.map(|d| format!("{summary}\n\n{d}")).unwrap_or(summary);
                            if let Some(row) = self.rows.iter_mut().find(|r| r.tool_id.as_ref() == Some(&id)) {
                                row.set_text(label, text);
                            } else {
                                self.rows.push(Row::new(label, text, Some(id)));
                            }
                        }
                        AgentEvent::Permission { name, title, preview, reply } => {
                            non_delta += 1;
                            self.pending = Some(Approval { title: format!("{} · {title}", tool_label_zh(&name)), preview: preview.unwrap_or_default(), reply });
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
                Event::FetchedModels { models, status } => {
                    non_delta += 1;
                    self.fetching_models = false;
                    self.fetch_status = status;
                    if self.model.trim().ends_with('/') {
                        if let Some(first) = models.first() {
                            self.model = first.clone();
                        }
                    }
                    self.fetched_models = models;
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
        if self.tx.send(Command::Turn { id, prompt, cancel, access: self.access }).is_err() {
            self.active = None; self.notice = "后台服务已断开".into();
        }
    }
    fn open_model_settings(&mut self) {
        self.model = self.state.as_ref().map(|state| state.model.clone()).unwrap_or_default();
        self.load_model_fields();
        self.fetched_models.clear();
        self.fetch_status.clear();
        self.fetching_models = false;
        self.settings = true;
    }

    fn trigger_fetch_models(&mut self) {
        let provider = self
            .model
            .split_once('/')
            .map(|(p, _)| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| if self.url.contains("11434") { "ollama".into() } else { "custom".into() });
        self.fetching_models = true;
        self.fetch_status = "正在获取可用模型列表…".into();
        let _ = self.tx.send(Command::FetchModels {
            provider,
            url: self.url.trim().to_string(),
            key: self.key.trim().to_string(),
        });
    }

    fn model_picker(&mut self, ui: &mut egui::Ui, palette: &Palette, idle: bool) {
        let Some(state) = &self.state else { return };
        let current = &state.model;
        let selected = state.config.discovered_models.iter().find(|option| &option.id == current)
            .map_or(current.as_str(), |option| option.label.as_str());
        if !idle || self.active.is_some() || self.loading {
            ui.add(egui::Label::new(egui::RichText::new(selected).small()
                .color(palette.text_secondary)).truncate());
            return;
        }

        let mut selection = None;
        let mut settings = false;
        let combo_w = ui.available_width().clamp(120.0, 218.0);
        let combo = egui::ComboBox::from_id_salt("composer_model")
            .selected_text(egui::RichText::new(selected).small().color(palette.text_secondary))
            .width(combo_w)
            .height(320.0)
            .truncate()
            .show_ui(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                let configured = state.config.model.as_ref().filter(|model| *model != current);
                for model in std::iter::once(current).chain(configured) {
                    if !state.config.discovered_models.iter().any(|option| &option.id == model) {
                        let is_cur = model == current;
                        let text = if is_cur {
                            egui::RichText::new(model.as_str()).color(palette.accent).strong()
                        } else {
                            egui::RichText::new(model.as_str()).color(palette.text_primary)
                        };
                        if ui.selectable_label(is_cur, text).clicked() && !is_cur {
                            selection = Some(model.clone());
                        }
                    }
                }
                for option in &state.config.discovered_models {
                    if !option.id.split_once('/').is_some_and(|(provider, model)| {
                        !model.is_empty() && state.config.provider.contains_key(provider)
                    }) { continue; }
                    let is_cur = &option.id == current;
                    let text = if is_cur {
                        egui::RichText::new(&option.label).color(palette.accent).strong()
                    } else {
                        egui::RichText::new(&option.label).color(palette.text_primary)
                    };
                    if ui.selectable_label(is_cur, text)
                        .on_hover_text(&option.id).clicked() && !is_cur {
                        selection = Some(option.id.clone());
                    }
                }
                ui.separator();
                let btn = egui::Button::new(egui::RichText::new("模型设置…").color(palette.text_secondary))
                    .min_size(egui::vec2(ui.available_width(), 28.0));
                settings = ui.add(btn).clicked();
            });
        if self.open_model_menu_req {
            self.open_model_menu_req = false;
            ui.memory_mut(|m| m.open_popup(combo.response.id.with("popup")));
            ui.ctx().request_repaint();
        }
        combo.response.on_hover_text(format!("切换模型 · {current}"));

        if settings {
            ui.memory_mut(|memory| memory.close_popup());
            self.open_model_settings();
        }
        if let Some(model) = selection {
            self.command(Command::SelectModel(model));
        }
    }

    fn render_composer(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        palette: &Palette,
        idle: bool,
        empty_mode: bool,
    ) -> egui::Rect {
        if !self.notice.is_empty() {
            let (summary, details) = theme::friendly_error_zh(&self.notice);
            egui::Frame::none()
                .fill(palette.bg_subtle)
                .stroke(egui::Stroke::new(1.0_f32, palette.border))
                .rounding(10.0)
                .inner_margin(egui::Margin::symmetric(12.0, 8.0))
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.colored_label(palette.orange, summary);
                    if let Some(details) = details {
                        ui.collapsing("原始详情", |ui| {
                            egui::ScrollArea::vertical().max_height(110.0).show(ui, |ui| {
                                ui.add(egui::Label::new(egui::RichText::new(details).small().color(palette.text_secondary)).wrap().selectable(true));
                            });
                        });
                    }
                });
            ui.add_space(8.0);
        }

        let composer_id = egui::Id::new("composer_input");
        let focused = ctx.memory(|m| m.has_focus(composer_id));
        let shortcut = ctx.input_mut(|i| {
            consume_send_shortcut(&mut i.events, &mut self.ime_composing, focused)
        });

        let border_color = if focused { palette.accent } else { palette.border };
        let mut composer_rect = egui::Rect::NOTHING;
        let desired_rows = if empty_mode { 3 } else { 2 };

        egui::Frame::none()
            .fill(palette.bg_card)
            .stroke(egui::Stroke::new(if focused { 1.25_f32 } else { 1.0_f32 }, border_color))
            .rounding(16.0)
            .inner_margin(egui::Margin::symmetric(14.0, 12.0))
            .show(ui, |ui| {
                let edit = ui.add(
                    egui::TextEdit::multiline(&mut self.input)
                        .id(composer_id)
                        .return_key(egui::KeyboardShortcut::new(egui::Modifiers::SHIFT, egui::Key::Enter))
                        .desired_rows(desired_rows)
                        .desired_width(f32::INFINITY)
                        .frame(false)
                        .hint_text(
                            egui::RichText::new("描述任务或提问，支持 @路径 引用文件（回车发送，Shift + 回车换行）")
                                .color(palette.text_muted),
                        ),
                );
                composer_rect = edit.rect;
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let (mode_label, mode_color) = match self.access {
                        AccessMode::Ask => ("请求批准", palette.text_secondary),
                        AccessMode::Smart => ("帮我批准", palette.text_secondary),
                        AccessMode::Full => ("完全访问", palette.orange),
                        AccessMode::Plan => ("只读规划", palette.accent),
                    };
                    ui.menu_button(
                        egui::RichText::new(mode_label).small().color(mode_color),
                        |ui| {
                            ui.set_min_width(276.0);
                            ui.spacing_mut().item_spacing.y = 4.0;
                            ui.label(egui::RichText::new("应如何批准 Ferro 操作？").small().color(palette.text_muted));
                            ui.add_space(2.0);
                            let options: [(AccessMode, &str, &str, egui::Color32); 4] = [
                                (AccessMode::Ask, "请求批准", "编辑文件和执行命令时始终询问", palette.text_primary),
                                (AccessMode::Smart, "帮我批准", "仅对外部文件与高危命令请求批准", palette.text_primary),
                                (AccessMode::Full, "完全访问权限", "直接修改文件并执行命令，无需反复确认", palette.orange),
                                (AccessMode::Plan, "只读规划", "仅分析代码与制定方案，禁止修改文件", palette.accent),
                            ];
                            for (mode_opt, title, desc, title_col) in options {
                                let selected = self.access == mode_opt;
                                let (rect, resp) = ui.allocate_exact_size(
                                    egui::vec2(ui.available_width().max(264.0), 46.0),
                                    if self.active.is_none() { egui::Sense::click() } else { egui::Sense::hover() },
                                );
                                if resp.hovered() {
                                    ui.painter().rect_filled(rect, 8.0, palette.bg_hover);
                                } else if selected {
                                    ui.painter().rect_filled(rect, 8.0, palette.bg_subtle);
                                }
                                ui.allocate_new_ui(
                                    egui::UiBuilder::new().max_rect(rect.shrink2(egui::vec2(10.0, 6.0))),
                                    |ui| {
                                        ui.horizontal(|ui| {
                                            ui.vertical(|ui| {
                                                ui.spacing_mut().item_spacing.y = 1.0;
                                                ui.label(egui::RichText::new(title).size(13.0).strong().color(title_col));
                                                ui.label(egui::RichText::new(desc).size(11.5).color(palette.text_muted));
                                            });
                                            if selected {
                                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                                    ui.label(egui::RichText::new("✓").size(14.0).strong().color(title_col));
                                                });
                                            }
                                        });
                                    },
                                );
                                if self.active.is_none() && resp.clicked() {
                                    self.access = mode_opt;
                                    ui.close_menu();
                                }
                            }
                            ui.separator();
                            if ui
                                .add_enabled(
                                    idle && !self.rows.is_empty(),
                                    egui::Button::new("撤销本轮修改").min_size(egui::vec2(ui.available_width(), 28.0)),
                                )
                                .clicked()
                            {
                                self.command(Command::Undo);
                                ui.close_menu();
                            }
                            if let Some(state) = &self.state {
                                ui.label(egui::RichText::new(&state.usage).small().color(palette.text_muted));
                                if state.todos != "(空)" {
                                    ui.collapsing("待办事项", |ui| {
                                        ui.label(&state.todos);
                                    });
                                }
                            }
                        },
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.active.is_some() {
                            let stop = egui::Button::new(
                                egui::RichText::new("停止").strong().color(palette.danger),
                            )
                            .min_size(egui::vec2(64.0, 32.0))
                            .fill(palette.danger_soft)
                            .stroke(egui::Stroke::new(1.0_f32, palette.danger))
                            .rounding(10.0);
                            if ui.add(stop).clicked() {
                                self.stop();
                            }
                        } else {
                            let ready = idle && !self.input.trim().is_empty();
                            let color = if ready { egui::Color32::WHITE } else { palette.text_muted };
                            let fill = if ready { palette.accent } else { palette.bg_hover };
                            let send = egui::Button::new(egui::RichText::new("发送").strong().color(color))
                                .min_size(egui::vec2(64.0, 32.0))
                                .fill(fill)
                                .stroke(egui::Stroke::NONE)
                                .rounding(10.0)
                                .sense(if ready { egui::Sense::click() } else { egui::Sense::hover() });
                            let clicked = ui.add(send).on_hover_text("回车发送，Shift + 回车换行").clicked();
                            if ready && (clicked || shortcut) {
                                self.send();
                            }
                        }
                        self.model_picker(ui, palette, idle);
                    });
                });
            });

        composer_rect
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
    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        visuals.panel_fill.to_normalized_gamma_f32()
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.events();
        // 处理 GPU 帧缓冲截图回调
        if let Some(target_path) = self.pending_screenshot.clone() {
            let mut captured: Option<Arc<egui::ColorImage>> = None;
            ctx.input(|i| {
                for ev in &i.events {
                    if let egui::Event::Screenshot { image, .. } = ev {
                        captured = Some(Arc::clone(image));
                    }
                }
            });
            if let Some(img) = captured {
                self.pending_screenshot = None;
                let mut rgba = Vec::with_capacity(img.pixels.len() * 4);
                for c in &img.pixels {
                    rgba.extend_from_slice(&c.to_srgba_unmultiplied());
                }
                let _ = markdown::write_rgba_png(&target_path, img.size[0] as u32, img.size[1] as u32, &rgba);
            }
        }
        // 窄窗口（<= 860px）自动收起侧栏，宽窗口自动恢复，用户仍可随时手动切换
        let screen_w = ctx.screen_rect().width();
        let is_narrow = screen_w > 0.0 && screen_w <= 860.0;
        if is_narrow != self.last_narrow {
            self.last_narrow = is_narrow;
            self.sidebar = !is_narrow;
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::B)) {
            self.sidebar = !self.sidebar;
        }

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
        if self.active.is_some() || self.loading || self.folder_rx.is_some() || self.autorun.is_some() || self.pending_screenshot.is_some() || self.fetching_models {
            ctx.request_repaint_after(std::time::Duration::from_millis(80));
        }
        let palette = if self.themed != Some(self.dark) {
            self.themed = Some(self.dark);
            theme::apply_theme(ctx, self.dark)
        } else if self.dark { Palette::dark() } else { Palette::light() };
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
                } else if cmd == "shift_enter" {
                    ctx.memory_mut(|m| m.request_focus(composer_id));
                    ctx.input_mut(|i| {
                        i.events.push(egui::Event::Key {
                            key: egui::Key::Enter,
                            physical_key: Some(egui::Key::Enter),
                            pressed: true,
                            repeat: false,
                            modifiers: egui::Modifiers::SHIFT,
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
                            modifiers: egui::Modifiers::NONE,
                        });
                    });
                } else if let Some(path) = cmd.strip_prefix("screenshot:") {
                    self.pending_screenshot = Some(PathBuf::from(path.trim()));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot);
                } else if let Some(size) = cmd.strip_prefix("resize:") {
                    if let Some((w, h)) = size.split_once(',') {
                        if let (Ok(w), Ok(h)) = (w.trim().parse::<f32>(), h.trim().parse::<f32>()) {
                            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(w, h)));
                        }
                    }
                } else if cmd == "theme:dark" {
                    self.dark = true;
                } else if cmd == "theme:light" {
                    self.dark = false;
                } else if cmd == "sidebar:open" {
                    self.sidebar = true;
                } else if cmd == "sidebar:close" {
                    self.sidebar = false;
                } else if cmd == "new_session" && idle {
                    self.command(Command::New);
                } else if let Some(id) = cmd.strip_prefix("reopen:") {
                    if idle { self.command(Command::Reopen(id.trim().to_string())); }
                } else if cmd == "open_model_menu" {
                    self.open_model_menu_req = true;
                } else if cmd == "close_popup" {
                    ctx.memory_mut(|m| m.close_popup());
                } else if cmd == "open_settings" {
                    self.open_model_settings();
                } else if cmd == "close_settings" {
                    self.settings = false;
                }
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(80));
        }

        egui::TopBottomPanel::top("masthead")
            .frame(
                egui::Frame::none()
                    .fill(palette.bg_canvas)
                    .stroke(egui::Stroke::NONE)
                    .inner_margin(egui::Margin::symmetric(12.0, 6.0)),
            )
            .show(ctx, |ui| {
                // 顶部空白区支持按住拖拽移动窗口、双击切换最大化
                let bar_rect = ui.max_rect();
                let drag_resp = ui.interact(bar_rect, egui::Id::new("titlebar_drag"), egui::Sense::click_and_drag());
                if drag_resp.drag_started_by(egui::PointerButton::Primary) {
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
                if drag_resp.double_clicked() {
                    let is_max = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!is_max));
                }

                ui.horizontal(|ui| {
                    let sidebar_label = if self.sidebar { "收起侧栏" } else { "展开侧栏" };
                    let sb_btn = egui::Button::new(
                        egui::RichText::new(sidebar_label).small().color(palette.text_secondary),
                    )
                    .fill(egui::Color32::TRANSPARENT)
                    .stroke(egui::Stroke::NONE)
                    .min_size(egui::vec2(0.0, 28.0));
                    if ui.add(sb_btn).on_hover_text("切换左侧会话栏 (Ctrl+B)").clicked() {
                        self.sidebar = !self.sidebar;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // 一体化矢量窗口控制按钮：关闭 / 最大化 / 最小化（零字体依赖，永不缺字）
                        let (close_rect, close_resp) = ui.allocate_exact_size(egui::vec2(32.0, 26.0), egui::Sense::click());
                        if close_resp.hovered() {
                            ui.painter().rect_filled(close_rect, 6.0, palette.danger);
                        }
                        let close_col = if close_resp.hovered() { egui::Color32::WHITE } else { palette.text_secondary };
                        let cc = close_rect.center();
                        ui.painter().line_segment([egui::pos2(cc.x - 4.5, cc.y - 4.5), egui::pos2(cc.x + 4.5, cc.y + 4.5)], egui::Stroke::new(1.3_f32, close_col));
                        ui.painter().line_segment([egui::pos2(cc.x + 4.5, cc.y - 4.5), egui::pos2(cc.x - 4.5, cc.y + 4.5)], egui::Stroke::new(1.3_f32, close_col));
                        if close_resp.on_hover_text("关闭").clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }

                        let is_max = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
                        let (max_rect, max_resp) = ui.allocate_exact_size(egui::vec2(32.0, 26.0), egui::Sense::click());
                        if max_resp.hovered() {
                            ui.painter().rect_filled(max_rect, 6.0, palette.bg_hover);
                        }
                        let mc = max_rect.center();
                        let stroke = egui::Stroke::new(1.2_f32, palette.text_secondary);
                        if is_max {
                            ui.painter().rect_stroke(egui::Rect::from_center_size(egui::pos2(mc.x - 1.0, mc.y + 1.0), egui::vec2(8.0, 8.0)), 1.5, stroke);
                            ui.painter().line_segment([egui::pos2(mc.x - 2.0, mc.y - 5.0), egui::pos2(mc.x + 5.0, mc.y - 5.0)], stroke);
                            ui.painter().line_segment([egui::pos2(mc.x + 5.0, mc.y - 5.0), egui::pos2(mc.x + 5.0, mc.y + 2.0)], stroke);
                        } else {
                            ui.painter().rect_stroke(egui::Rect::from_center_size(mc, egui::vec2(9.0, 9.0)), 1.5, stroke);
                        }
                        if max_resp.on_hover_text(if is_max { "还原" } else { "最大化" }).clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!is_max));
                        }

                        let (min_rect, min_resp) = ui.allocate_exact_size(egui::vec2(32.0, 26.0), egui::Sense::click());
                        if min_resp.hovered() {
                            ui.painter().rect_filled(min_rect, 6.0, palette.bg_hover);
                        }
                        let minc = min_rect.center();
                        ui.painter().line_segment([egui::pos2(minc.x - 5.0, minc.y), egui::pos2(minc.x + 5.0, minc.y)], egui::Stroke::new(1.3_f32, palette.text_secondary));
                        if min_resp.on_hover_text("最小化").clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                        }

                        ui.add_space(6.0);
                        ui.menu_button(egui::RichText::new("设置").color(palette.text_secondary), |ui| {
                            ui.set_min_width(148.0);
                            if ui.add_enabled(idle, egui::Button::new("模型设置").min_size(egui::vec2(ui.available_width(), 28.0))).clicked() {
                                self.open_model_settings();
                                ui.close_menu();
                            }
                            if ui.add(egui::Button::new(if self.dark { "切换浅色主题" } else { "切换深色主题" }).min_size(egui::vec2(ui.available_width(), 28.0))).clicked() {
                                self.dark = !self.dark;
                                ctx.request_repaint();
                                ui.close_menu();
                            }
                        });
                        let workspace = self.state.as_ref().map(|state| {
                            state.cwd.file_name().unwrap_or(state.cwd.as_os_str()).to_string_lossy().into_owned()
                        }).unwrap_or_else(|| "项目".into());
                        let project = ui.menu_button(
                            egui::RichText::new(format!("项目 · {workspace}")).color(palette.text_secondary),
                            |ui| {
                                ui.set_max_width(280.0);
                                if let Some(state) = &self.state {
                                    ui.add(egui::Label::new(egui::RichText::new(workspace.as_str()).strong()).truncate());
                                    ui.add(egui::Label::new(egui::RichText::new(state.cwd.display().to_string()).small().color(palette.text_muted)).wrap().selectable(true));
                                    ui.add_space(4.0);
                                }
                                let open = ui.add_enabled(
                                    idle,
                                    egui::Button::new(egui::RichText::new("打开项目…").color(palette.accent).strong())
                                        .fill(palette.accent_soft)
                                        .stroke(egui::Stroke::new(1.0_f32, palette.accent_border))
                                        .min_size(egui::vec2(ui.available_width(), 30.0)),
                                );
                                open_btn_rect = open.rect;
                                if open.clicked() {
                                    open_project_clicked = true;
                                    ui.close_menu();
                                }
                                let mut recent = None;
                                if let Some(state) = &self.state {
                                    if !state.recent.is_empty() {
                                        ui.separator();
                                        ui.label(egui::RichText::new("最近项目").small().color(palette.text_muted));
                                        egui::ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
                                            for path in &state.recent {
                                                let name = path.file_name().unwrap_or(path.as_os_str()).to_string_lossy();
                                                if ui.add_enabled(idle, egui::Button::new(name).truncate().min_size(egui::vec2(ui.available_width(), 28.0)))
                                                    .on_hover_text(path.display().to_string()).clicked() {
                                                    recent = Some(path.clone());
                                                }
                                            }
                                        });
                                    }
                                }
                                if let Some(path) = recent {
                                    self.search.clear();
                                    self.command(Command::Workspace(path));
                                    ui.close_menu();
                                }
                            },
                        );
                        project.response.on_hover_text(
                            self.state.as_ref().map(|s| s.cwd.display().to_string()).unwrap_or(workspace),
                        );
                        if self.active.is_some() || self.loading || self.folder_rx.is_some() {
                            let status = if self.active.is_some() { "处理中" } else if self.loading { "正在加载" } else { "请选择目录" };
                            ui.label(egui::RichText::new(status).small().color(palette.accent));
                            ui.spinner();
                        }
                    });
                });
                // 底部极细中性分割线，清晰界定顶栏与主区
                let rect = ui.max_rect();
                ui.painter().line_segment(
                    [egui::pos2(rect.left() - 12.0, rect.bottom() + 6.0), egui::pos2(rect.right() + 12.0, rect.bottom() + 6.0)],
                    egui::Stroke::new(1.0_f32, palette.border_soft),
                );
            });
        if self.sidebar {
            const SIDEBAR_W: f32 = 244.0;
            const SIDEBAR_PAD_X: f32 = 12.0;
            const INNER_W: f32 = SIDEBAR_W - SIDEBAR_PAD_X * 2.0;
            const MORE_W: f32 = 26.0;

            egui::SidePanel::left("workspace")
                .exact_width(SIDEBAR_W)
                .resizable(false)
                .show_separator_line(true)
                .frame(
                    egui::Frame::none()
                        .fill(palette.bg_sidebar)
                        .inner_margin(egui::Margin::symmetric(SIDEBAR_PAD_X, 14.0)),
                )
                .show(ctx, |ui| {
                    ui.set_width(INNER_W);
                    let busy_tip = "正在处理中，请稍候或先点击停止";
                    let new_btn = egui::Button::new(
                        egui::RichText::new("+  新建会话").strong().color(egui::Color32::WHITE),
                    )
                    .min_size(egui::vec2(INNER_W, 36.0))
                    .fill(if idle { palette.accent } else { palette.accent.gamma_multiply(0.65) })
                    .stroke(egui::Stroke::NONE)
                    .rounding(10.0)
                    .sense(if idle { egui::Sense::click() } else { egui::Sense::hover() });
                    let new_resp = ui.add(new_btn);
                    if !idle { new_resp.clone().on_hover_text(busy_tip); }
                    if idle && new_resp.clicked() {
                        self.search.clear();
                        self.search_open = false;
                        self.command(Command::New);
                    }
                    ui.add_space(12.0);
                    let mut focus_search = false;
                    ui.horizontal(|ui| {
                        ui.set_width(INNER_W);
                        ui.label(egui::RichText::new("会话记录").small().strong().color(palette.text_muted));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let srch_btn = egui::Button::new(
                                egui::RichText::new(if self.search_open { "清除" } else { "搜索" })
                                    .small()
                                    .color(palette.text_secondary),
                            )
                            .fill(egui::Color32::TRANSPARENT)
                            .stroke(egui::Stroke::NONE)
                            .sense(if idle { egui::Sense::click() } else { egui::Sense::hover() });
                            if ui.add(srch_btn).clicked() && idle {
                                self.search_open = !self.search_open;
                                focus_search = self.search_open;
                                if !self.search_open {
                                    self.search.clear();
                                    self.command(Command::Search(String::new()));
                                }
                            }
                        });
                    });
                    if self.search_open {
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            let search = ui.add(
                                egui::TextEdit::singleline(&mut self.search)
                                    .desired_width((INNER_W - 56.0).max(60.0))
                                    .hint_text("输入关键词"),
                            );
                            if focus_search { search.request_focus(); }
                            let find_btn = egui::Button::new("查找")
                                .sense(if idle { egui::Sense::click() } else { egui::Sense::hover() });
                            if (idle && ui.add(find_btn).clicked())
                                || (idle && search.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                                self.command(Command::Search(self.search.clone()));
                            }
                        });
                    }
                    ui.add_space(6.0);
                    let mut reopen = None;
                    let mut delete = None;
                    egui::ScrollArea::vertical()
                        .id_salt("history")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.set_width(INNER_W);
                            ui.spacing_mut().item_spacing = egui::vec2(4.0, 3.0);
                            if let Some(state) = &self.state {
                                if state.sessions.is_empty() {
                                    ui.add_space(8.0);
                                    let hint = if self.search_open && !self.search.is_empty() { "没有匹配的会话" } else { "暂无会话" };
                                    ui.label(egui::RichText::new(hint).small().color(palette.text_muted));
                                }
                                let active_id = if self.rows.is_empty() { "" } else { state.session_id.as_str() };
                                for session in &state.sessions {
                                    let is_selected = !active_id.is_empty() && session.id == active_id;
                                    ui.push_id(&session.id, |ui| {
                                        // 整行严格锁死 INNER_W x 34.0，左右两区用绝对坐标切分，确保短/长标题的「···」像素级垂直对齐
                                        let (row_rect, _) = ui.allocate_exact_size(egui::vec2(INNER_W, 34.0), egui::Sense::hover());
                                        let title_rect = egui::Rect::from_min_max(
                                            egui::pos2(row_rect.left(), row_rect.top() + 1.0),
                                            egui::pos2(row_rect.right() - MORE_W - 2.0, row_rect.bottom() - 1.0),
                                        );
                                        let more_rect = egui::Rect::from_min_max(
                                            egui::pos2(row_rect.right() - MORE_W, row_rect.top() + 3.0),
                                            egui::pos2(row_rect.right(), row_rect.bottom() - 3.0),
                                        );

                                        let sense = if idle { egui::Sense::click() } else { egui::Sense::hover() };
                                        let resp = ui.interact(title_rect, ui.id().with("row_click"), sense);
                                        if is_selected {
                                            ui.painter().rect(
                                                title_rect,
                                                8.0,
                                                palette.accent_soft,
                                                egui::Stroke::new(1.0_f32, palette.accent_border),
                                            );
                                            let bar = egui::Rect::from_min_max(
                                                egui::pos2(title_rect.left() + 1.0, title_rect.top() + 7.0),
                                                egui::pos2(title_rect.left() + 4.0, title_rect.bottom() - 7.0),
                                            );
                                            ui.painter().rect_filled(bar, 2.0, palette.accent);
                                        } else if resp.hovered() {
                                            ui.painter().rect_filled(title_rect, 8.0, palette.bg_hover);
                                        }

                                        let text_rect = title_rect.shrink2(egui::vec2(10.0, 0.0));
                                        let label_text = if is_selected {
                                            egui::RichText::new(&session.title).size(13.0).color(palette.accent).strong()
                                        } else {
                                            egui::RichText::new(&session.title).size(13.0).color(palette.text_primary)
                                        };
                                        ui.allocate_new_ui(
                                            egui::UiBuilder::new()
                                                .max_rect(text_rect)
                                                .layout(egui::Layout::left_to_right(egui::Align::Center)),
                                            |ui| {
                                                ui.add(egui::Label::new(label_text).truncate().selectable(false));
                                            },
                                        );

                                        let tip = if idle {
                                            format!("{}\n{} 条消息", session.title, session.count)
                                        } else {
                                            format!("{}\n（{}）", session.title, busy_tip)
                                        };
                                        let resp = resp.on_hover_text_at_pointer(tip);
                                        if idle && resp.clicked() {
                                            reopen = Some(session.id.clone());
                                        }

                                        ui.allocate_new_ui(
                                            egui::UiBuilder::new()
                                                .max_rect(more_rect)
                                                .layout(egui::Layout::centered_and_justified(egui::Direction::LeftToRight)),
                                            |ui| {
                                                ui.spacing_mut().button_padding = egui::vec2(2.0, 2.0);
                                                let v = ui.visuals_mut();
                                                v.widgets.inactive.bg_fill = egui::Color32::TRANSPARENT;
                                                v.widgets.inactive.weak_bg_fill = egui::Color32::TRANSPARENT;
                                                v.widgets.inactive.bg_stroke = egui::Stroke::NONE;
                                                v.widgets.hovered.bg_stroke = egui::Stroke::NONE;
                                                v.widgets.open.bg_stroke = egui::Stroke::NONE;
                                                ui.menu_button(
                                                    egui::RichText::new("···").size(13.0).color(palette.text_muted),
                                                    |ui| {
                                                        if ui.add_enabled(
                                                            idle,
                                                            egui::Button::new(egui::RichText::new("删除会话").color(palette.danger)),
                                                        ).clicked() {
                                                            delete = Some(session.id.clone());
                                                            ui.close_menu();
                                                        }
                                                    },
                                                ).response.on_hover_text("会话操作");
                                            },
                                        );
                                    });
                                }
                            }
                        });
                    if let Some(id) = reopen { self.command(Command::Reopen(id)); }
                    if let Some(id) = delete { self.command(Command::DeleteSession(id)); }
                });
        }
        // 已有会话时，将输入区固定在底部；空会话时，将欢迎标题与输入区组合在主区中部
        if !self.rows.is_empty() {
            egui::TopBottomPanel::bottom("composer")
                .frame(egui::Frame::none().fill(palette.bg_canvas).inner_margin(egui::Margin::symmetric(18.0, 12.0)))
                .show(ctx, |ui| {
                    centered_column(ui, |ui| {
                        composer_rect = self.render_composer(ctx, ui, &palette, idle, false);
                    });
                });
        }
        egui::CentralPanel::default().frame(egui::Frame::none().fill(palette.bg_canvas)
            .inner_margin(egui::Margin::symmetric(18.0, 0.0))).show(ctx, |ui| {
            if self.rows.is_empty() {
                let avail_h = ui.available_height();
                let top_pad = ((avail_h - 196.0) * 0.42).max(28.0);
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    centered_column(ui, |ui| {
                        ui.add_space(top_pad);
                        ui.vertical_centered(|ui| {
                            ui.label(egui::RichText::new("想从哪里开始？").size(28.0).strong().color(palette.text_primary));
                        });
                        ui.add_space(22.0);
                        composer_rect = self.render_composer(ctx, ui, &palette, idle, true);
                    });
                });
                return;
            }
            egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
                centered_column(ui, |ui| {
                    ui.add_space(20.0);
                    let width = ui.available_width();
                    if (self.transcript_width - width).abs() > 1.0 {
                        self.transcript_width = width;
                        for row in &mut self.rows { row.height = 0.0; }
                    }
                    let total = self.rows.len();
                    let clip = ui.clip_rect();
                    for (index, row) in self.rows.iter_mut().enumerate() {
                        if row.height > 0.0 && index + 2 < total {
                            let top = ui.cursor().min.y;
                            if top + row.height < clip.min.y - 400.0 || top > clip.max.y + 400.0 {
                                ui.allocate_space(egui::vec2(width, row.height));
                                continue;
                            }
                        }
                        row.height = ui.push_id(index, |ui| render_row(ui, row, &palette)).response.rect.height();
                    }
                });
            });
        });
        if self.pending.as_ref().is_some_and(|p| p.reply.is_closed()) { self.pending.take(); }
        if let Some(p) = &self.pending {
            let mut choice = None;
            egui::Window::new("工具授权").collapsible(false).resizable(true).show(ctx, |ui| {
                ui.label(&p.title);
                egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| { markdown::render_diff_or_output(ui, &p.preview, &palette); });
                ui.horizontal(|ui| {
                    if ui.button("本次允许").clicked() { choice = Some(Decision::Once); }
                    if ui.button("本轮允许").clicked() { choice = Some(Decision::Always); }
                    if ui.button("拒绝并停止").clicked() { choice = Some(Decision::Reject(None)); }
                });
            });
            if let Some(choice) = choice { if let Some(p) = self.pending.take() { let _ = p.reply.send(choice); } }
        }
        if self.settings {
            let mut open = true;
            egui::Window::new("模型设置").open(&mut open).default_width(480.0)
                .default_pos(ctx.screen_rect().center() - egui::vec2(240.0, 230.0))
                .collapsible(false).show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                ui.label(egui::RichText::new("快捷服务商预设（不额外修改时默认使用已导入/Magpie 模型）").small().color(palette.text_muted));
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Ollama（本地）").clicked() {
                        self.model = "ollama/".into();
                        self.url = "http://localhost:11434/v1".into();
                        self.key.clear();
                        self.anthropic = false;
                        self.trigger_fetch_models();
                    }
                    if ui.button("OpenAI").clicked() {
                        self.model = "openai/gpt-4o".into();
                        self.url = "https://api.openai.com/v1".into();
                        self.anthropic = false;
                        self.fetched_models.clear();
                        self.fetch_status = "请填写 OpenAI 接口密钥（或 env:OPENAI_API_KEY），也可点「获取模型列表」".into();
                    }
                    if ui.button("DeepSeek").clicked() {
                        self.model = "deepseek/deepseek-chat".into();
                        self.url = "https://api.deepseek.com/v1".into();
                        self.anthropic = false;
                        self.fetched_models.clear();
                        self.fetch_status = "请填写 DeepSeek 接口密钥（或 env:DEEPSEEK_API_KEY）".into();
                    }
                    if ui.button("Anthropic").clicked() {
                        self.model = "anthropic/claude-sonnet-4-5".into();
                        self.url = "https://api.anthropic.com".into();
                        self.anthropic = true;
                        self.fetched_models.clear();
                        self.fetch_status = "请填写 Anthropic 接口密钥（或 env:ANTHROPIC_API_KEY）".into();
                    }
                    if ui.button("载入已有配置").clicked() {
                        self.load_model_fields();
                    }
                });

                ui.separator();
                ui.label("提供方／模型（格式：提供方/模型名，支持自定义提供方）");
                ui.add(egui::TextEdit::singleline(&mut self.model).desired_width(f32::INFINITY));

                ui.horizontal(|ui| {
                    let fetch_label = if self.fetching_models { "正在获取模型…" } else { "获取接口模型列表" };
                    if ui.add_enabled(!self.fetching_models, egui::Button::new(fetch_label)).clicked() {
                        self.trigger_fetch_models();
                    }
                    if !self.fetched_models.is_empty() {
                        let cur_show = self.fetched_models.iter().find(|m| *m == &self.model)
                            .map(|s| s.as_str()).unwrap_or("点此选择已拉取模型…");
                        egui::ComboBox::from_id_salt("fetched_models_combo")
                            .selected_text(cur_show)
                            .width(240.0)
                            .show_ui(ui, |ui| {
                                for m in &self.fetched_models {
                                    ui.selectable_value(&mut self.model, m.clone(), m);
                                }
                            });
                    }
                });
                if !self.fetch_status.is_empty() {
                    let color = if !self.fetched_models.is_empty() { palette.accent } else { palette.text_secondary };
                    ui.label(egui::RichText::new(&self.fetch_status).small().color(color));
                }

                let mut picked_imported = false;
                if let Some(s) = &self.state {
                    if !s.config.discovered_models.is_empty() {
                        egui::ComboBox::from_label("已导入模型（Magpie / 本地配置）")
                            .selected_text("选择…")
                            .width(220.0)
                            .show_ui(ui, |ui| {
                                for model in &s.config.discovered_models {
                                    if ui.selectable_value(&mut self.model, model.id.clone(), &model.label).clicked() {
                                        picked_imported = true;
                                    }
                                }
                            });
                    }
                }
                if picked_imported {
                    self.load_model_fields();
                }

                ui.label("接口地址（OpenAI 兼容服务一般以 /v1 结尾）");
                ui.add(egui::TextEdit::singleline(&mut self.url).desired_width(f32::INFINITY));

                ui.label("接口密钥（直接粘贴 sk-... 或填 env:环境变量名；本机 Ollama 可留空）");
                ui.add(egui::TextEdit::singleline(&mut self.key).password(true).desired_width(f32::INFINITY));

                ui.checkbox(&mut self.anthropic, "使用 Anthropic 协议（取消勾选时使用 OpenAI 兼容协议）");
                ui.checkbox(&mut self.persist, "保存到全局配置（写入 %APPDATA%\\ferro\\config.toml，下次启动继续生效）");

                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let apply_btn = egui::Button::new(egui::RichText::new("应用").strong().color(egui::Color32::WHITE))
                        .fill(palette.accent)
                        .stroke(egui::Stroke::NONE)
                        .min_size(egui::vec2(96.0, 32.0))
                        .rounding(8.0);
                    if ui.add_enabled(idle, apply_btn).clicked() {
                        self.command(Command::Setup {
                            model: self.model.trim().into(),
                            url: self.url.trim().trim_end_matches('/').into(),
                            key: self.key.trim().into(),
                            anthropic: self.anthropic,
                            persist: self.persist,
                        });
                        self.key.clear();
                        self.settings = false;
                    }
                    if ui.add(egui::Button::new("取消").min_size(egui::vec2(72.0, 32.0))).clicked() {
                        self.settings = false;
                    }
                });
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
                    let mut dlg = rfd::FileDialog::new().set_title("选择项目目录");
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
                let mut dlg = rfd::FileDialog::new().set_title("选择项目目录").set_parent(frame);
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
        // 无边框窗口四边/四角拖拽缩放 + 极细外轮廓描边
        let is_max = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        if !is_max {
            let sr = ctx.screen_rect();
            ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("win_border")))
                .rect_stroke(sr, 0.0, egui::Stroke::new(1.0_f32, palette.border));
            if let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) {
                let m = 5.0;
                let (l, r, t, b) = (pos.x <= sr.left() + m, pos.x >= sr.right() - m, pos.y <= sr.top() + m, pos.y >= sr.bottom() - m);
                let dir = match (l, r, t, b) {
                    (true, _, true, _) => Some((egui::ResizeDirection::NorthWest, egui::CursorIcon::ResizeNwSe)),
                    (_, true, true, _) => Some((egui::ResizeDirection::NorthEast, egui::CursorIcon::ResizeNeSw)),
                    (true, _, _, true) => Some((egui::ResizeDirection::SouthWest, egui::CursorIcon::ResizeNeSw)),
                    (_, true, _, true) => Some((egui::ResizeDirection::SouthEast, egui::CursorIcon::ResizeNwSe)),
                    (true, _, _, _) => Some((egui::ResizeDirection::West, egui::CursorIcon::ResizeHorizontal)),
                    (_, true, _, _) => Some((egui::ResizeDirection::East, egui::CursorIcon::ResizeHorizontal)),
                    (_, _, true, _) => Some((egui::ResizeDirection::North, egui::CursorIcon::ResizeVertical)),
                    (_, _, _, true) => Some((egui::ResizeDirection::South, egui::CursorIcon::ResizeVertical)),
                    _ => None,
                };
                if let Some((resize_dir, cursor)) = dir {
                    ctx.set_cursor_icon(cursor);
                    if ctx.input(|i| i.pointer.primary_pressed()) {
                        ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(resize_dir));
                    }
                }
            }
        }
    }
}
impl Desktop {
    fn drive_autorun(&mut self, ctx: &egui::Context) {
        let Some(mut auto) = self.autorun.take() else { return };
        let idle = self.active.is_none() && !self.loading;
        match auto.stage {
            0 if idle && self.state.is_some() => {
                // Stage 1: send first-turn conversation
                self.access = AccessMode::Smart;
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
                self.access = AccessMode::Plan;
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
                self.access = AccessMode::Smart;
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
            let (label, text, id): (String, String, Option<String>) = match block {
                Block::Text { text } if !text.starts_with("<system-reminder>") => {
                    let clean = if message.role == Role::User {
                        text.split("\n\n<file").next().unwrap_or(text).to_string()
                    } else {
                        text.clone()
                    };
                    (if message.role == Role::User { "You" } else { "Ferro" }.into(), clean, None)
                }
                Block::Reasoning { text, .. } => ("Thinking".into(), text.clone(), None),
                Block::ToolUse { id, name, input } => (tool_label_zh(name).into(), serde_json::to_string_pretty(input).unwrap_or_default(), Some(id.clone())),
                Block::ToolResult { tool_use_id, content, is_error } => (if *is_error { "工具失败" } else { "工具结果" }.into(), content.clone(), Some(tool_use_id.clone())),
                _ => continue,
            };
            rows.push(Row::new(label, text, id));
        }
    }
    rows
}

fn centered_column(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui)) {
    let width = ui.available_width().min(760.0);
    ui.horizontal_top(|ui| {
        let gutter = (ui.available_width() - width) / 2.0;
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.add_space(gutter);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.set_width(width);
            contents(ui);
        });
    });
}

fn render_row(ui: &mut egui::Ui, row: &mut Row, palette: &Palette) {
    if row.tool_id.is_some() || row.label == "Thinking" {
        let label = if row.label == "Thinking" { "思考过程" } else { &row.label };
        egui::CollapsingHeader::new(egui::RichText::new(label).small().color(palette.text_secondary))
            .show(ui, |ui| markdown::render_diff_or_output(ui, &row.text, palette));
    } else if row.label == "You" {
        let max_width = ui.available_width() * 0.78;
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                egui::Frame::none().fill(palette.bg_user_bubble).rounding(14.0).inner_margin(12.0)
                    .show(ui, |ui| {
                        ui.set_max_width(max_width);
                        ui.add(egui::Label::new(egui::RichText::new(&row.text).color(palette.text_user_bubble))
                            .wrap().selectable(true));
                    });
            });
        });
    } else {
        let blocks = row.blocks.get_or_insert_with(|| markdown::parse_markdown(&row.text));
        markdown::render_blocks(ui, blocks, palette);
        if ui.small_button("复制回复").clicked() { ui.ctx().copy_text(row.text.clone()); }
    }
    ui.add_space(22.0);
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let paths = if cfg!(windows) {
        // 优先加载微软雅黑（清晰度与中英文基线表现远优于黑体），回退到黑体
        vec![
            PathBuf::from("C:/Windows/Fonts/msyh.ttc"),
            PathBuf::from("C:/Windows/Fonts/msyh.ttf"),
            PathBuf::from("C:/Windows/Fonts/simhei.ttf"),
        ]
    } else if cfg!(target_os = "macos") {
        vec![PathBuf::from("/System/Library/Fonts/PingFang.ttc"), PathBuf::from("/System/Library/Fonts/STHeiti Medium.ttc")]
    } else {
        vec![PathBuf::from("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc")]
    };
    let custom = std::env::var_os("FERRO_CJK_FONT").map(PathBuf::from);
    for path in custom.into_iter().chain(paths) {
        if let Ok(bytes) = std::fs::read(path) {
            fonts.font_data.insert("cjk".into(), egui::FontData::from_owned(bytes));
            // 正文字体首位使用微软雅黑/苹方，确保中文与数字、英文视觉字重和基线一致；等宽字体保留代码等宽在前
            fonts.families.entry(egui::FontFamily::Proportional).or_default().insert(0, "cjk".into());
            fonts.families.entry(egui::FontFamily::Monospace).or_default().push("cjk".into());
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

/// 先让输入法处理候选词。输入框有焦点时，回车发送，Shift + 回车留给文本框换行。
fn consume_send_shortcut(
    events: &mut Vec<egui::Event>,
    ime_composing: &mut bool,
    composer_focused: bool,
) -> bool {
    let mut ime_activity_this_frame = *ime_composing;
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
    events.retain(|event| {
        if let egui::Event::Key { key, physical_key, pressed: true, repeat, modifiers } = event {
            let enter = *key == egui::Key::Enter || *physical_key == Some(egui::Key::Enter);
            if enter && !modifiers.shift && !modifiers.alt {
                triggered |= !repeat;
                return false;
            }
        }
        true
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
    fn plain_enter_sends_and_shift_enter_does_not() {
        let mut ime = false;
        let mut events = vec![key_ev(egui::Key::Enter, Some(egui::Key::Enter), egui::Modifiers::NONE)];
        assert!(consume_send_shortcut(&mut events, &mut ime, true));
        assert!(events.is_empty());

        events.push(key_ev(egui::Key::Enter, Some(egui::Key::Enter), egui::Modifiers::SHIFT));
        assert!(!consume_send_shortcut(&mut events, &mut ime, true));
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn repeated_enter_does_not_send() {
        let mut ime = false;
        let mut events = vec![egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: Some(egui::Key::Enter),
            pressed: true,
            repeat: true,
            modifiers: egui::Modifiers::NONE,
        }];
        assert!(!consume_send_shortcut(&mut events, &mut ime, true));
        assert!(events.is_empty());
    }

    #[test]
    fn ime_preedit_and_commit_prevent_misfire() {
        let mut ime = false;
        let mods = egui::Modifiers::NONE;

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

        // Next frame after IME commit finishes, plain Enter sends normally
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
