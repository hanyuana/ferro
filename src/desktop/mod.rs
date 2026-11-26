//! Native desktop: all model/tool/filesystem work is owned by one background worker.
mod clipboard_img;
mod markdown;
mod theme;
mod worker;
use clipboard_img::{BuiltinBrowser, PendingImage};
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
            ime_composing: false, folder_rx: None, file_rx: None, open_project_req: false, pick_files_req: false,
            pending_images: Vec::new(), browser: BuiltinBrowser::new(),
            renaming_session: None, rename_buf: String::new(), editing_row: None, edit_row_buf: String::new(),
            autorun, state_file, last_state_dump: String::new(),
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
    pub images: Vec<PendingImage>,
    blocks: Option<Vec<markdown::MdBlock>>,
    height: f32,
}

impl Row {
    pub fn new(label: impl Into<String>, text: impl Into<String>, tool_id: Option<String>) -> Self {
        Self { label: label.into(), text: text.into(), tool_id, images: Vec::new(), blocks: None, height: 0.0 }
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
    ime_composing: bool,
    folder_rx: Option<std::sync::mpsc::Receiver<Option<PathBuf>>>,
    file_rx: Option<std::sync::mpsc::Receiver<Vec<PathBuf>>>,
    open_project_req: bool,
    pick_files_req: bool,
    pending_images: Vec<PendingImage>,
    browser: BuiltinBrowser,
    renaming_session: Option<String>,
    rename_buf: String,
    editing_row: Option<usize>,
    edit_row_buf: String,
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
        if (self.input.trim().is_empty() && self.pending_images.is_empty()) || self.active.is_some() || self.loading {
            return;
        }
        self.next_turn += 1;
        let id = self.next_turn;
        let cancel = Arc::new(Notify::new());
        let mut prompt = std::mem::take(&mut self.input);
        let attached = std::mem::take(&mut self.pending_images);
        if prompt.trim().is_empty() && !attached.is_empty() {
            prompt = "请分析这张图片".into();
        }
        let images: Vec<(String, String)> = attached
            .iter()
            .map(|img| (img.media_type.clone(), img.base64.clone()))
            .collect();
        let mut row = Row::new("You", prompt.clone(), None);
        row.images = attached;
        self.rows.push(row);
        self.active = Some((id, cancel.clone()));
        self.notice.clear();
        if self.tx.send(Command::Turn { id, prompt, images, cancel, access: self.access }).is_err() {
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
        let menu_resp = ui.scope(|ui| {
            let v = ui.visuals_mut();
            v.widgets.inactive.bg_fill = egui::Color32::TRANSPARENT;
            v.widgets.inactive.weak_bg_fill = egui::Color32::TRANSPARENT;
            v.widgets.inactive.bg_stroke = egui::Stroke::NONE;
            v.widgets.hovered.bg_stroke = egui::Stroke::NONE;
            v.widgets.open.bg_stroke = egui::Stroke::NONE;
            v.widgets.hovered.rounding = egui::Rounding::same(14.0);
            v.widgets.open.rounding = egui::Rounding::same(14.0);
            ui.menu_button(
                egui::RichText::new(format!("{selected}  ∨")).size(12.5).color(palette.text_secondary),
                |ui| {
                    ui.set_min_width(220.0);
                    ui.spacing_mut().item_spacing.y = 3.0;
                    egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                        let configured = state.config.model.as_ref().filter(|model| *model != current);
                        for model in std::iter::once(current).chain(configured) {
                            if !state.config.discovered_models.iter().any(|option| &option.id == model) {
                                let is_cur = model == current;
                                let label = if is_cur { format!("✓  {model}") } else { format!("    {model}") };
                                if flat_menu_item(ui, &label, palette, true) && !is_cur {
                                    selection = Some(model.clone());
                                    ui.close_menu();
                                }
                            }
                        }
                        for option in &state.config.discovered_models {
                            if !option.id.split_once('/').is_some_and(|(provider, model)| {
                                !model.is_empty() && state.config.provider.contains_key(provider)
                            }) { continue; }
                            let is_cur = &option.id == current;
                            let label = if is_cur { format!("✓  {}", option.label) } else { format!("    {}", option.label) };
                            if flat_menu_item(ui, &label, palette, true) && !is_cur {
                                selection = Some(option.id.clone());
                                ui.close_menu();
                            }
                        }
                    });
                    ui.separator();
                    if flat_menu_item(ui, "    模型与密钥设置…", palette, true) {
                        settings = true;
                        ui.close_menu();
                    }
                },
            )
            .response
        }).inner;
        if self.open_model_menu_req {
            self.open_model_menu_req = false;
            ui.memory_mut(|m| m.open_popup(menu_resp.id.with("popup")));
            ui.ctx().request_repaint();
        }
        menu_resp.on_hover_text(format!("切换模型 · {current}"));

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
        _empty_mode: bool,
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
        // 检测 Ctrl+V 粘贴截图/复制的图片文件，以及拖拽进窗口的图片或文件
        if clipboard_img::should_paste_clipboard_image(ctx) {
            let imgs = clipboard_img::try_paste_clipboard_images();
            if !imgs.is_empty() {
                self.pending_images.extend(imgs);
                ctx.memory_mut(|m| m.request_focus(composer_id));
            }
        }
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        if !dropped.is_empty() {
            self.attach_picked_files(ctx, dropped);
        }

        let shortcut = ctx.input_mut(|i| {
            consume_send_shortcut(&mut i.events, &mut self.ime_composing, focused)
        });

        let border_color = palette.border;
        let mut composer_rect = egui::Rect::NOTHING;

        egui::Frame::none()
            .fill(palette.bg_card)
            .stroke(egui::Stroke::new(1.0_f32, border_color))
            .rounding(24.0)
            .shadow(egui::Shadow {
                offset: egui::vec2(0.0, 4.0),
                blur: 18.0,
                spread: 0.0,
                color: egui::Color32::from_black_alpha(if self.dark { 70 } else { 15 }),
            })
            .inner_margin(egui::Margin::symmetric(16.0, 11.0))
            .show(ui, |ui| {
                // 已粘贴/添加的图片附件缩略图预览栏（带右上角 × 移除按钮）
                if !self.pending_images.is_empty() {
                    let mut remove_idx = None;
                    ui.horizontal_wrapped(|ui| {
                        for (idx, img) in self.pending_images.iter_mut().enumerate() {
                            if img.tex.is_none() {
                                if let Some((tw, th, rgba)) = &img.thumb_rgba {
                                    let cimg = egui::ColorImage::from_rgba_unmultiplied([*tw as usize, *th as usize], rgba);
                                    img.tex = Some(ui.ctx().load_texture(format!("paste_thumb_{idx}"), cimg, egui::TextureOptions::LINEAR));
                                }
                            }
                            egui::Frame::none()
                                .fill(palette.bg_subtle)
                                .stroke(egui::Stroke::new(1.0_f32, palette.border_soft))
                                .rounding(10.0)
                                .inner_margin(egui::Margin::symmetric(8.0, 6.0))
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        if let Some(tex) = &img.tex {
                                            ui.add(egui::Image::new((tex.id(), egui::vec2(38.0, 38.0))).rounding(6.0));
                                        }
                                        ui.label(egui::RichText::new(&img.label).size(12.0).color(palette.text_primary));
                                        let del = egui::Button::new(egui::RichText::new("×").size(14.0).color(palette.text_secondary))
                                            .fill(egui::Color32::TRANSPARENT)
                                            .stroke(egui::Stroke::NONE)
                                            .min_size(egui::vec2(20.0, 20.0));
                                        if ui.add(del).on_hover_text("移除图片").clicked() {
                                            remove_idx = Some(idx);
                                        }
                                    });
                                });
                        }
                    });
                    if let Some(i) = remove_idx {
                        self.pending_images.remove(i);
                    }
                    ui.add_space(6.0);
                }

                let edit = ui.add(
                    egui::TextEdit::multiline(&mut self.input)
                        .id(composer_id)
                        .return_key(egui::KeyboardShortcut::new(egui::Modifiers::SHIFT, egui::Key::Enter))
                        .desired_rows(1)
                        .desired_width(f32::INFINITY)
                        .frame(false)
                        .hint_text(
                            egui::RichText::new("可向 Ferro 询问任何问题。输入 @ 提及文件")
                                .color(palette.text_muted),
                        ),
                );
                composer_rect = edit.rect;
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    // 左下角「+」文件与项目快捷菜单（1:1 对齐本地 ChatGPT 输入框左下角 + 号）
                    ui.scope(|ui| {
                        let v = ui.visuals_mut();
                        v.widgets.inactive.bg_fill = egui::Color32::TRANSPARENT;
                        v.widgets.inactive.weak_bg_fill = egui::Color32::TRANSPARENT;
                        v.widgets.inactive.bg_stroke = egui::Stroke::NONE;
                        v.widgets.hovered.bg_stroke = egui::Stroke::NONE;
                        v.widgets.open.bg_stroke = egui::Stroke::NONE;
                        v.widgets.hovered.rounding = egui::Rounding::same(14.0);
                        v.widgets.open.rounding = egui::Rounding::same(14.0);
                        ui.menu_button(
                            egui::RichText::new("+").size(18.0).color(palette.text_secondary),
                            |ui| {
                                ui.set_min_width(196.0);
                                ui.spacing_mut().item_spacing.y = 3.0;
                                if flat_menu_item(ui, "粘贴剪贴板图片 (Ctrl+V)", palette, idle) {
                                    let imgs = clipboard_img::try_paste_clipboard_images();
                                    if !imgs.is_empty() {
                                        self.pending_images.extend(imgs);
                                    } else {
                                        self.notice = "剪贴板中暂未检测到截图或图片文件".into();
                                    }
                                    ui.close_menu();
                                }
                                if flat_menu_item(ui, "添加照片和文件", palette, idle) {
                                    self.pick_files_req = true;
                                    ui.close_menu();
                                }
                                if flat_menu_item(ui, "选择文件夹", palette, idle) {
                                    self.open_project_req = true;
                                    ui.close_menu();
                                }
                                if !self.rows.is_empty() {
                                    ui.separator();
                                    if flat_menu_item(ui, "撤销本轮文件修改", palette, idle) {
                                        self.command(Command::Undo);
                                        ui.close_menu();
                                    }
                                }
                            },
                        )
                        .response
                        .on_hover_text("添加照片和文件");
                    });

                    let (mode_label, mode_fg, mode_bg) = match self.access {
                        AccessMode::Ask => ("请求批准", palette.text_secondary, palette.bg_subtle),
                        AccessMode::Smart => ("帮我批准", palette.text_secondary, palette.bg_subtle),
                        AccessMode::Full => ("完全访问", palette.orange, palette.orange_soft),
                        AccessMode::Plan => ("只读规划", palette.blue, palette.bg_subtle),
                    };
                    ui.scope(|ui| {
                        let v = ui.visuals_mut();
                        v.widgets.inactive.bg_fill = mode_bg;
                        v.widgets.inactive.weak_bg_fill = mode_bg;
                        v.widgets.inactive.bg_stroke = egui::Stroke::NONE;
                        v.widgets.hovered.bg_stroke = egui::Stroke::NONE;
                        v.widgets.open.bg_stroke = egui::Stroke::NONE;
                        v.widgets.inactive.rounding = egui::Rounding::same(14.0);
                        v.widgets.hovered.rounding = egui::Rounding::same(14.0);
                        v.widgets.open.rounding = egui::Rounding::same(14.0);
                        ui.menu_button(
                            egui::RichText::new(mode_label).size(12.5).color(mode_fg),
                            |ui| {
                                ui.set_min_width(276.0);
                                ui.spacing_mut().item_spacing.y = 4.0;
                                ui.label(egui::RichText::new("应如何批准 Ferro 操作？").small().color(palette.text_muted));
                                ui.add_space(2.0);
                                let options: [(AccessMode, &str, &str, egui::Color32); 4] = [
                                    (AccessMode::Ask, "请求批准", "编辑文件和执行命令时始终询问", palette.text_primary),
                                    (AccessMode::Smart, "帮我批准", "仅对检测到的风险操作请求批准", palette.text_primary),
                                    (AccessMode::Full, "完全访问权限", "可不受限制地读写项目文件和执行命令", palette.orange),
                                    (AccessMode::Plan, "只读规划", "仅分析代码与制定方案，禁止修改文件", palette.blue),
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
                                if let Some(state) = &self.state {
                                    ui.separator();
                                    ui.label(egui::RichText::new(&state.usage).small().color(palette.text_muted));
                                    if state.todos != "(空)" {
                                        ui.collapsing("待办事项", |ui| {
                                            ui.label(&state.todos);
                                        });
                                    }
                                }
                            },
                        );
                    });

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // 右下角 32x32 实心正圆发送（↑）/ 停止（■）按钮
                        if self.active.is_some() {
                            let (rect, resp) = ui.allocate_exact_size(egui::vec2(32.0, 32.0), egui::Sense::click());
                            ui.painter().circle_filled(rect.center(), 15.0, palette.accent);
                            let sq = egui::Rect::from_center_size(rect.center(), egui::vec2(10.0, 10.0));
                            ui.painter().rect_filled(sq, 2.0, palette.accent_fg);
                            if resp.on_hover_text("停止生成").clicked() {
                                self.stop();
                            }
                        } else {
                            let ready = idle && (!self.input.trim().is_empty() || !self.pending_images.is_empty());
                            let sense = if ready { egui::Sense::click() } else { egui::Sense::hover() };
                            let (rect, resp) = ui.allocate_exact_size(egui::vec2(32.0, 32.0), sense);
                            let bg = if ready {
                                palette.accent
                            } else if self.dark {
                                egui::Color32::from_rgb(68, 68, 68)
                            } else {
                                egui::Color32::from_rgb(214, 214, 214)
                            };
                            let fg = if ready {
                                palette.accent_fg
                            } else {
                                palette.bg_card
                            };
                            ui.painter().circle_filled(rect.center(), 15.0, bg);
                            let c = rect.center();
                            let st = egui::Stroke::new(1.8_f32, fg);
                            ui.painter().line_segment([egui::pos2(c.x, c.y + 5.5), egui::pos2(c.x, c.y - 5.5)], st);
                            ui.painter().line_segment([egui::pos2(c.x - 4.5, c.y - 1.0), egui::pos2(c.x, c.y - 5.5)], st);
                            ui.painter().line_segment([egui::pos2(c.x + 4.5, c.y - 1.0), egui::pos2(c.x, c.y - 5.5)], st);
                            let clicked = resp.on_hover_text("回车发送，Shift + 回车换行").clicked();
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

    fn attach_picked_files(&mut self, ctx: &egui::Context, files: Vec<PathBuf>) {
        if files.is_empty() {
            return;
        }
        let base = self.state.as_ref().map(|s| s.cwd.clone());
        for f in files {
            if clipboard_img::is_image_path(&f) {
                if let Some(img) = clipboard_img::load_image_file(&f) {
                    self.pending_images.push(img);
                    continue;
                }
            }
            let rel = base
                .as_ref()
                .and_then(|b| f.strip_prefix(b).ok().map(PathBuf::from))
                .unwrap_or(f);
            let tag = format!("@{} ", rel.to_string_lossy().replace('\\', "/"));
            if !self.input.is_empty() && !self.input.ends_with(char::is_whitespace) {
                self.input.push(' ');
            }
            self.input.push_str(&tag);
        }
        ctx.memory_mut(|m| m.request_focus(egui::Id::new("composer_input")));
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

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
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
        if let Some(rx) = &self.file_rx {
            match rx.try_recv() {
                Ok(files) => {
                    self.file_rx = None;
                    self.attach_picked_files(ctx, files);
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.file_rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if self.active.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(28));
        } else if self.loading || self.folder_rx.is_some() || self.file_rx.is_some() || self.autorun.is_some() || self.pending_screenshot.is_some() || self.fetching_models {
            ctx.request_repaint_after(std::time::Duration::from_millis(80));
        }
        let palette = if self.themed != Some(self.dark) {
            self.themed = Some(self.dark);
            theme::apply_theme(ctx, self.dark)
        } else if self.dark { Palette::dark() } else { Palette::light() };
        let idle = self.active.is_none() && !self.loading && self.folder_rx.is_none() && self.file_rx.is_none();
        let mut open_project_clicked = std::mem::take(&mut self.open_project_req);
        let pick_files_clicked = std::mem::take(&mut self.pick_files_req);
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
                } else if cmd == "paste_clipboard" {
                    let imgs = clipboard_img::try_paste_clipboard_images();
                    if !imgs.is_empty() {
                        self.pending_images.extend(imgs);
                        ctx.memory_mut(|m| m.request_focus(composer_id));
                    }
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
                    if !self.sidebar {
                        if icon_action_btn(ui, NavIcon::Sidebar, "展开侧栏 (Ctrl+B)", &palette).clicked() {
                            self.sidebar = true;
                        }
                        if icon_action_btn(ui, NavIcon::NewChat, "新聊天", &palette).clicked() && idle {
                            self.search.clear();
                            self.search_open = false;
                            self.command(Command::New);
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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
                    });
                });
            });
        if self.sidebar {
            const SIDEBAR_W: f32 = 252.0;
            const SIDEBAR_PAD_X: f32 = 10.0;
            const INNER_W: f32 = SIDEBAR_W - SIDEBAR_PAD_X * 2.0;
            const MORE_W: f32 = 26.0;

            egui::SidePanel::left("workspace")
                .exact_width(SIDEBAR_W)
                .resizable(false)
                .show_separator_line(false)
                .frame(
                    egui::Frame::none()
                        .fill(palette.bg_sidebar)
                        .inner_margin(egui::Margin::symmetric(SIDEBAR_PAD_X, 10.0)),
                )
                .show(ctx, |ui| {
                    ui.set_width(INNER_W);
                    let busy_tip = "正在处理中，请稍候或先点击停止";
                    let mut focus_search = false;

                    // 1. 顶部图标工具行（左：收起侧栏；右：搜索、新聊天）
                    ui.horizontal(|ui| {
                        ui.set_width(INNER_W);
                        if icon_action_btn(ui, NavIcon::Sidebar, "收起侧栏 (Ctrl+B)", &palette).clicked() {
                            self.sidebar = false;
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if icon_action_btn(ui, NavIcon::NewChat, "新聊天", &palette).clicked() && idle {
                                self.search.clear();
                                self.search_open = false;
                                self.command(Command::New);
                            }
                            if icon_action_btn(ui, NavIcon::Search, "搜索聊天", &palette).clicked() && idle {
                                self.search_open = !self.search_open;
                                focus_search = self.search_open;
                                if !self.search_open {
                                    self.search.clear();
                                    self.command(Command::Search(String::new()));
                                }
                            }
                        });
                    });
                    ui.add_space(4.0);

                    // 2. ChatGPT 风格扁平导航列表项（仅保留「新聊天」，搜索由右上角放大镜触发）
                    if sidebar_flat_row(ui, INNER_W, NavIcon::NewChat, "新聊天", false, idle, &palette)
                        .on_hover_text(if idle { "开始新对话" } else { busy_tip })
                        .clicked() && idle
                    {
                        self.search.clear();
                        self.search_open = false;
                        self.command(Command::New);
                    }
                    if self.search_open {
                        ui.add_space(2.0);
                        ui.horizontal(|ui| {
                            let search = ui.add(
                                egui::TextEdit::singleline(&mut self.search)
                                    .desired_width((INNER_W - 52.0).max(60.0))
                                    .hint_text("搜索聊天…"),
                            );
                            if focus_search { search.request_focus(); }
                            let find_btn = egui::Button::new("查找")
                                .sense(if idle { egui::Sense::click() } else { egui::Sense::hover() });
                            if (idle && ui.add(find_btn).clicked())
                                || (idle && search.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                                self.command(Command::Search(self.search.clone()));
                            }
                        });
                        ui.add_space(2.0);
                    }

                    // 3. 「项目」扁平列表区（标题行右侧直接带 + 按钮「新建项目」，默认精简展示前 3 个常用项目）
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.set_width(INNER_W);
                        ui.add_space(8.0);
                        ui.label(egui::RichText::new("项目").size(11.5).color(palette.text_muted));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let plus_resp = icon_action_btn(ui, NavIcon::Plus, "新建项目", &palette);
                            open_btn_rect = plus_resp.rect;
                            if plus_resp.clicked() && idle {
                                open_project_clicked = true;
                            }
                        });
                    });
                    let mut switch_project = None;
                    if let Some(state) = &self.state {
                        let cur_cwd = &state.cwd;
                        let mut dirs: Vec<PathBuf> = vec![cur_cwd.clone()];
                        for p in &state.recent {
                            if p != cur_cwd && dirs.len() < 3 {
                                dirs.push(p.clone());
                            }
                        }
                        for path in dirs {
                            let is_cur = &path == cur_cwd;
                            let name = path.file_name().unwrap_or(path.as_os_str()).to_string_lossy();
                            let resp = sidebar_flat_row(ui, INNER_W, NavIcon::Folder, &name, is_cur, idle, &palette)
                                .on_hover_text(path.display().to_string());
                            if idle && !is_cur && resp.clicked() {
                                switch_project = Some(path);
                            }
                        }
                    }
                    if let Some(path) = switch_project {
                        self.search.clear();
                        self.command(Command::Workspace(path));
                    }

                    // 4. 「聊天」扁平列表区（支持本地 ChatGPT 的「已置顶」分组、重命名聊天、置顶/取消置顶与删除）
                    ui.add_space(8.0);
                    let mut reopen = None;
                    let mut delete = None;
                    let mut toggle_pin = None;
                    let mut commit_rename: Option<(String, String)> = None;
                    let mut cancel_rename = false;
                    let list_h = (ui.available_height() - 42.0).max(80.0);
                    egui::ScrollArea::vertical()
                        .id_salt("history")
                        .max_height(list_h)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.set_width(INNER_W);
                            ui.spacing_mut().item_spacing = egui::vec2(4.0, 2.0);
                            if let Some(state) = &self.state {
                                if state.sessions.is_empty() {
                                    ui.horizontal(|ui| {
                                        ui.add_space(8.0);
                                        ui.label(egui::RichText::new("聊天").size(11.5).color(palette.text_muted));
                                    });
                                    ui.add_space(6.0);
                                    let hint = if self.search_open && !self.search.is_empty() { "没有匹配的聊天" } else { "暂无聊天" };
                                    ui.horizontal(|ui| {
                                        ui.add_space(8.0);
                                        ui.label(egui::RichText::new(hint).small().color(palette.text_muted));
                                    });
                                }
                                let active_id = if self.rows.is_empty() { "" } else { state.session_id.as_str() };
                                let has_pinned = state.sessions.iter().any(|s| s.pinned);
                                let mut printed_pinned_hdr = false;
                                let mut printed_chat_hdr = false;
                                for session in &state.sessions {
                                    if session.pinned && !printed_pinned_hdr {
                                        printed_pinned_hdr = true;
                                        ui.horizontal(|ui| {
                                            ui.add_space(8.0);
                                            ui.label(egui::RichText::new("已置顶").size(11.5).color(palette.text_muted));
                                        });
                                        ui.add_space(2.0);
                                    } else if !session.pinned && !printed_chat_hdr {
                                        printed_chat_hdr = true;
                                        if has_pinned {
                                            ui.add_space(6.0);
                                        }
                                        ui.horizontal(|ui| {
                                            ui.add_space(8.0);
                                            ui.label(egui::RichText::new("聊天").size(11.5).color(palette.text_muted));
                                        });
                                        ui.add_space(2.0);
                                    }
                                    let is_selected = !active_id.is_empty() && session.id == active_id;
                                    let is_renaming = self.renaming_session.as_deref() == Some(session.id.as_str());
                                    ui.push_id(&session.id, |ui| {
                                        if is_renaming {
                                            ui.horizontal(|ui| {
                                                let te = ui.add(
                                                    egui::TextEdit::singleline(&mut self.rename_buf)
                                                        .desired_width((INNER_W - 44.0).max(80.0)),
                                                );
                                                if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                                    commit_rename = Some((session.id.clone(), self.rename_buf.clone()));
                                                } else if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                                                    cancel_rename = true;
                                                } else if !te.has_focus() {
                                                    te.request_focus();
                                                }
                                                if ui.small_button("✓").on_hover_text("保存名称").clicked() {
                                                    commit_rename = Some((session.id.clone(), self.rename_buf.clone()));
                                                }
                                            });
                                            return;
                                        }
                                        let (row_rect, _) = ui.allocate_exact_size(egui::vec2(INNER_W, 36.0), egui::Sense::hover());
                                        let row_hovered = ui.rect_contains_pointer(row_rect);
                                        let title_rect = egui::Rect::from_min_max(
                                            egui::pos2(row_rect.left(), row_rect.top() + 1.0),
                                            egui::pos2(row_rect.right() - MORE_W - 2.0, row_rect.bottom() - 1.0),
                                        );
                                        let more_rect = egui::Rect::from_min_max(
                                            egui::pos2(row_rect.right() - MORE_W, row_rect.top() + 3.0),
                                            egui::pos2(row_rect.right(), row_rect.bottom() - 3.0),
                                        );

                                        // ChatGPT 扁平选中态：10px 圆角（--radius-token-row: 10px），无边框
                                        let bg_rect = row_rect.shrink2(egui::vec2(0.0, 1.0));
                                        if is_selected {
                                            ui.painter().rect_filled(bg_rect, 10.0, palette.bg_active);
                                        } else if row_hovered {
                                            ui.painter().rect_filled(bg_rect, 10.0, palette.bg_hover);
                                        }

                                        let sense = if idle { egui::Sense::click() } else { egui::Sense::hover() };
                                        let resp = ui.interact(title_rect, ui.id().with("row_click"), sense);

                                        let text_rect = title_rect.shrink2(egui::vec2(10.0, 0.0));
                                        let label_text = if is_selected {
                                            egui::RichText::new(&session.title).size(13.2).color(palette.text_primary).strong()
                                        } else {
                                            egui::RichText::new(&session.title).size(13.2).color(palette.text_primary)
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

                                        // 仅在鼠标悬停或当前选中行时展示右侧「···」按钮（置顶 / 重命名聊天 / 删除）
                                        if row_hovered || is_selected {
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
                                                        egui::RichText::new("···").size(13.0).color(palette.text_secondary),
                                                        |ui| {
                                                            ui.set_min_width(140.0);
                                                            ui.spacing_mut().item_spacing.y = 2.0;
                                                            let pin_label = if session.pinned { "取消置顶" } else { "置顶" };
                                                            if flat_menu_item(ui, pin_label, &palette, idle) {
                                                                toggle_pin = Some(session.id.clone());
                                                                ui.close_menu();
                                                            }
                                                            if flat_menu_item(ui, "重命名聊天", &palette, idle) {
                                                                self.renaming_session = Some(session.id.clone());
                                                                self.rename_buf = session.title.clone();
                                                                ui.close_menu();
                                                            }
                                                            ui.separator();
                                                            if ui.add_enabled(
                                                                idle,
                                                                egui::Button::new(egui::RichText::new("删除").color(palette.danger))
                                                                    .fill(egui::Color32::TRANSPARENT)
                                                                    .stroke(egui::Stroke::NONE),
                                                            ).clicked() {
                                                                delete = Some(session.id.clone());
                                                                ui.close_menu();
                                                            }
                                                        },
                                                    ).response.on_hover_text("聊天操作");
                                                },
                                            );
                                        }
                                    });
                                }
                            }
                        });
                    if cancel_rename {
                        self.renaming_session = None;
                    }
                    if let Some((id, new_title)) = commit_rename {
                        self.renaming_session = None;
                        self.command(Command::RenameSession(id, new_title));
                    }
                    if let Some(id) = toggle_pin { self.command(Command::TogglePinSession(id)); }
                    if let Some(id) = reopen { self.command(Command::Reopen(id)); }
                    if let Some(id) = delete { self.command(Command::DeleteSession(id)); }

                    // 5. 侧栏底部左下角固定「设置」扁平入口（对齐 ChatGPT 左下角习惯）
                    ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                        let resp = sidebar_flat_row(ui, INNER_W, NavIcon::Settings, "设置", self.settings, true, &palette);
                        let popup_id = ui.make_persistent_id("sidebar_settings_popup");
                        if resp.clicked() {
                            ui.memory_mut(|m| m.toggle_popup(popup_id));
                        }
                        egui::popup_above_or_below_widget(
                            ui,
                            popup_id,
                            &resp,
                            egui::AboveOrBelow::Above,
                            egui::PopupCloseBehavior::CloseOnClickOutside,
                            |ui| {
                                ui.set_min_width(210.0);
                                ui.spacing_mut().item_spacing.y = 3.0;
                                if flat_menu_item(ui, "模型与密钥设置…", &palette, idle) {
                                    self.open_model_settings();
                                    ui.memory_mut(|m| m.close_popup());
                                }
                                let theme_switch = if self.dark { "切换为浅色主题" } else { "切换为深色主题" };
                                if flat_menu_item(ui, theme_switch, &palette, true) {
                                    self.dark = !self.dark;
                                    ctx.request_repaint();
                                    ui.memory_mut(|m| m.close_popup());
                                }
                            },
                        );
                    });
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
                            ui.label(egui::RichText::new("我们从哪里开始？").size(28.0).strong().color(palette.text_primary));
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
                    let is_active = self.active.is_some();
                    let mut action = RowAction::None;
                    for (index, row) in self.rows.iter_mut().enumerate() {
                        if row.height > 0.0 && index + 2 < total && self.editing_row != Some(index) {
                            let top = ui.cursor().min.y;
                            if top + row.height < clip.min.y - 400.0 || top > clip.max.y + 400.0 {
                                ui.allocate_space(egui::vec2(width, row.height));
                                continue;
                            }
                        }
                        let streaming_this = is_active && index + 1 == total && row.label == "Ferro";
                        let is_last_ferro = !is_active && row.label == "Ferro" && index + 1 == total;
                        let is_editing = self.editing_row == Some(index);
                        let resp = ui.push_id(index, |ui| {
                            render_row(
                                ui,
                                index,
                                row,
                                &palette,
                                streaming_this,
                                is_last_ferro,
                                idle,
                                is_editing,
                                &mut self.edit_row_buf,
                                &mut action,
                            )
                        });
                        row.height = resp.response.rect.height();
                    }
                    match action {
                        RowAction::None => {}
                        RowAction::StartEdit(idx, txt) => {
                            self.editing_row = Some(idx);
                            self.edit_row_buf = txt;
                        }
                        RowAction::CancelEdit => {
                            self.editing_row = None;
                            self.edit_row_buf.clear();
                        }
                        RowAction::CommitEdit(txt) => {
                            self.editing_row = None;
                            self.input = txt;
                            self.send();
                        }
                        RowAction::RetryLast => {
                            if let Some(last_user) = self.rows.iter().rev().find(|r| r.label == "You").map(|r| r.text.clone()) {
                                self.input = last_user;
                                self.send();
                            }
                        }
                    }
                    // 等待首字或工具执行间隙：在对话流内联展示 ChatGPT 风格呼吸脉冲圆点动画
                    let last_is_ferro = self.rows.last().is_some_and(|r| r.label == "Ferro" && r.tool_id.is_none());
                    if is_active && !last_is_ferro {
                        draw_chatgpt_loading_indicator(ui, &palette);
                        ui.add_space(22.0);
                    }
                });
            });
        });
        if self.pending.as_ref().is_some_and(|p| p.reply.is_closed()) { self.pending.take(); }
        if let Some(p) = &self.pending {
            let mut choice = None;
            let screen = ctx.screen_rect();
            ctx.layer_painter(egui::LayerId::new(egui::Order::Middle, egui::Id::new("perm_backdrop")))
                .rect_filled(screen, 0.0, egui::Color32::from_black_alpha(72));
            egui::Window::new("perm_modal")
                .title_bar(false)
                .collapsible(false)
                .resizable(false)
                .fixed_size(egui::vec2(500.0, 340.0))
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .frame(
                    egui::Frame::none()
                        .fill(palette.bg_card)
                        .stroke(egui::Stroke::new(1.0_f32, palette.border))
                        .rounding(16.0)
                        .shadow(egui::Shadow {
                            offset: egui::vec2(0.0, 10.0),
                            blur: 32.0,
                            spread: 0.0,
                            color: egui::Color32::from_black_alpha(55),
                        })
                        .inner_margin(egui::Margin::symmetric(20.0, 18.0)),
                )
                .show(ctx, |ui| {
                    ui.label(egui::RichText::new("审批选项").size(16.5).strong().color(palette.text_primary));
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(&p.title).size(13.0).color(palette.text_secondary));
                    ui.add_space(8.0);
                    egui::Frame::none()
                        .fill(palette.bg_subtle)
                        .stroke(egui::Stroke::new(1.0_f32, palette.border_soft))
                        .rounding(10.0)
                        .inner_margin(egui::Margin::same(10.0))
                        .show(ui, |ui| {
                            egui::ScrollArea::vertical().max_height(190.0).show(ui, |ui| {
                                markdown::render_diff_or_output(ui, &p.preview, &palette);
                            });
                        });
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let once_btn = egui::Button::new(egui::RichText::new("允许一次").size(13.0).strong().color(palette.accent_fg))
                                .fill(palette.accent)
                                .stroke(egui::Stroke::NONE)
                                .rounding(16.0)
                                .min_size(egui::vec2(88.0, 32.0));
                            if ui.add(once_btn).clicked() { choice = Some(Decision::Once); }
                            let sess_btn = egui::Button::new(egui::RichText::new("允许此对话").size(13.0).color(palette.text_primary))
                                .fill(palette.bg_subtle)
                                .stroke(egui::Stroke::NONE)
                                .rounding(16.0)
                                .min_size(egui::vec2(96.0, 32.0));
                            if ui.add(sess_btn).clicked() { choice = Some(Decision::Always); }
                            let rej_btn = egui::Button::new(egui::RichText::new("拒绝").size(13.0).color(palette.danger))
                                .fill(palette.bg_subtle)
                                .stroke(egui::Stroke::NONE)
                                .rounding(16.0)
                                .min_size(egui::vec2(68.0, 32.0));
                            if ui.add(rej_btn).clicked() { choice = Some(Decision::Reject(None)); }
                        });
                    });
                });
            if let Some(choice) = choice { if let Some(p) = self.pending.take() { let _ = p.reply.send(choice); } }
        }
        if self.settings {
            let mut open = true;
            let screen = ctx.screen_rect();
            ctx.layer_painter(egui::LayerId::new(egui::Order::Middle, egui::Id::new("settings_backdrop")))
                .rect_filled(screen, 0.0, egui::Color32::from_black_alpha(72));
            egui::Window::new("settings_modal")
                .title_bar(false)
                .collapsible(false)
                .resizable(false)
                .fixed_size(egui::vec2(500.0, 420.0))
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .frame(
                    egui::Frame::none()
                        .fill(palette.bg_card)
                        .stroke(egui::Stroke::new(1.0_f32, palette.border))
                        .rounding(16.0)
                        .shadow(egui::Shadow {
                            offset: egui::vec2(0.0, 10.0),
                            blur: 32.0,
                            spread: 0.0,
                            color: egui::Color32::from_black_alpha(55),
                        })
                        .inner_margin(egui::Margin::symmetric(20.0, 18.0)),
                )
                .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("设置").size(16.5).strong().color(palette.text_primary));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let close_btn = egui::Button::new(egui::RichText::new("×").size(17.0).color(palette.text_secondary))
                            .fill(egui::Color32::TRANSPARENT)
                            .stroke(egui::Stroke::NONE)
                            .min_size(egui::vec2(28.0, 28.0));
                        if ui.add(close_btn).on_hover_text("关闭").clicked() {
                            open = false;
                        }
                    });
                });
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
                    let apply_btn = egui::Button::new(egui::RichText::new("应用").strong().color(palette.accent_fg))
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
        if open_project_clicked {
            let initial = self.state.as_ref().map(|s| s.cwd.clone());
            self.browser.open_for(initial, false);
        }
        if pick_files_clicked {
            let initial = self.state.as_ref().map(|s| s.cwd.clone());
            self.browser.open_for(initial, true);
        }
        let (browsed_dir, browsed_files) = self.browser.show(ctx, &palette);
        if let Some(chosen) = browsed_dir {
            self.search.clear();
            self.command(Command::Workspace(chosen));
        }
        if !browsed_files.is_empty() {
            self.attach_picked_files(ctx, browsed_files);
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
    let mut rows: Vec<Row> = Vec::with_capacity(messages.len());
    for message in messages {
        for block in &message.content {
            match block {
                Block::Text { text } if !text.starts_with("<system-reminder>") => {
                    let clean = if message.role == Role::User {
                        text.split("\n\n<file").next().unwrap_or(text).to_string()
                    } else {
                        text.clone()
                    };
                    let label = if message.role == Role::User { "You" } else { "Ferro" };
                    rows.push(Row::new(label, clean, None));
                }
                Block::Reasoning { text, .. } => {
                    rows.push(Row::new("Thinking", text.clone(), None));
                }
                Block::ToolUse { id, name, input } => {
                    let brief = input
                        .get("path")
                        .or_else(|| input.get("command"))
                        .or_else(|| input.get("pattern"))
                        .and_then(|v| v.as_str())
                        .map(|s| format!("{} · {}", tool_label_zh(name), s))
                        .unwrap_or_else(|| tool_label_zh(name).to_string());
                    let body = serde_json::to_string_pretty(input).unwrap_or_default();
                    rows.push(Row::new(brief, body, Some(id.clone())));
                }
                Block::ToolResult { tool_use_id, content, is_error } => {
                    // 将工具结果合并到对应的 ToolUse 行中，避免界面拆出多余的「工具结果」折叠行
                    if let Some(prev) = rows.iter_mut().rev().find(|r| r.tool_id.as_deref() == Some(tool_use_id)) {
                        if *is_error && !prev.label.ends_with("失败") {
                            prev.label = format!("{} · 失败", prev.label);
                        }
                        if !content.trim().is_empty() {
                            let merged = if prev.text.trim().is_empty() {
                                content.clone()
                            } else {
                                format!("{}\n\n{}", prev.text, content)
                            };
                            prev.set_text(prev.label.clone(), merged);
                        }
                    } else {
                        let label = if *is_error { "工具失败" } else { "工具结果" };
                        rows.push(Row::new(label, content.clone(), Some(tool_use_id.clone())));
                    }
                }
                _ => continue,
            }
        }
    }
    rows
}

#[derive(Clone, Copy)]
enum NavIcon {
    Sidebar,
    NewChat,
    Search,
    Plus,
    Folder,
    Settings,
}

fn draw_nav_icon(painter: &egui::Painter, c: egui::Pos2, icon: NavIcon, color: egui::Color32) {
    let st = egui::Stroke::new(1.35_f32, color);
    match icon {
        NavIcon::Sidebar => {
            let r = egui::Rect::from_center_size(c, egui::vec2(14.0, 12.0));
            painter.rect_stroke(r, 2.5, st);
            painter.line_segment([egui::pos2(r.left() + 4.8, r.top() + 0.5), egui::pos2(r.left() + 4.8, r.bottom() - 0.5)], st);
        }
        NavIcon::NewChat => {
            let r = egui::Rect::from_center_size(c, egui::vec2(13.0, 13.0));
            painter.line_segment([egui::pos2(r.left(), r.top() + 2.0), egui::pos2(r.left(), r.bottom())], st);
            painter.line_segment([egui::pos2(r.left(), r.bottom()), egui::pos2(r.right() - 2.0, r.bottom())], st);
            painter.line_segment([egui::pos2(r.left(), r.top() + 2.0), egui::pos2(r.left() + 6.0, r.top() + 2.0)], st);
            painter.line_segment([egui::pos2(r.right() - 2.0, r.bottom()), egui::pos2(r.right() - 2.0, r.bottom() - 6.0)], st);
            painter.line_segment([egui::pos2(c.x - 1.5, c.y + 1.5), egui::pos2(r.right() + 0.5, r.top() - 0.5)], st);
        }
        NavIcon::Search => {
            let cc = egui::pos2(c.x - 1.2, c.y - 1.2);
            painter.circle_stroke(cc, 4.8, st);
            painter.line_segment([egui::pos2(cc.x + 3.4, cc.y + 3.4), egui::pos2(c.x + 6.0, c.y + 6.0)], st);
        }
        NavIcon::Plus => {
            painter.line_segment([egui::pos2(c.x - 5.2, c.y), egui::pos2(c.x + 5.2, c.y)], st);
            painter.line_segment([egui::pos2(c.x, c.y - 5.2), egui::pos2(c.x, c.y + 5.2)], st);
        }
        NavIcon::Folder => {
            let r = egui::Rect::from_center_size(egui::pos2(c.x, c.y + 0.8), egui::vec2(14.0, 10.5));
            painter.rect_stroke(r, 2.0, st);
            painter.line_segment([egui::pos2(r.left() + 1.0, r.top() - 2.0), egui::pos2(r.left() + 6.0, r.top() - 2.0)], st);
            painter.line_segment([egui::pos2(r.left() + 1.0, r.top() - 2.0), egui::pos2(r.left() + 1.0, r.top())], st);
            painter.line_segment([egui::pos2(r.left() + 6.0, r.top() - 2.0), egui::pos2(r.left() + 7.5, r.top())], st);
        }
        NavIcon::Settings => {
            painter.circle_stroke(c, 5.2, st);
            painter.circle_filled(c, 1.8, color);
        }
    }
}

fn icon_action_btn(ui: &mut egui::Ui, icon: NavIcon, tip: &str, palette: &Palette) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(30.0, 30.0), egui::Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, 8.0, palette.bg_hover);
    }
    let col = if resp.hovered() { palette.text_primary } else { palette.text_secondary };
    draw_nav_icon(ui.painter(), rect.center(), icon, col);
    resp.on_hover_text(tip)
}

fn sidebar_flat_row(
    ui: &mut egui::Ui,
    width: f32,
    icon: NavIcon,
    label: &str,
    selected: bool,
    enabled: bool,
    palette: &Palette,
) -> egui::Response {
    let sense = if enabled { egui::Sense::click() } else { egui::Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, 36.0), sense);
    let bg_rect = rect.shrink2(egui::vec2(0.0, 1.0));
    if selected {
        ui.painter().rect_filled(bg_rect, 10.0, palette.bg_active);
    } else if resp.hovered() {
        ui.painter().rect_filled(bg_rect, 10.0, palette.bg_hover);
    }
    let icon_center = egui::pos2(rect.left() + 17.0, rect.center().y);
    let fg = if selected { palette.text_primary } else { palette.text_primary };
    let icon_col = if selected { palette.text_primary } else { palette.text_secondary };
    draw_nav_icon(ui.painter(), icon_center, icon, icon_col);

    let text_rect = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 33.0, rect.top()),
        egui::pos2(rect.right() - 8.0, rect.bottom()),
    );
    let rt = if selected {
        egui::RichText::new(label).size(13.2).color(fg).strong()
    } else {
        egui::RichText::new(label).size(13.2).color(fg)
    };
    ui.allocate_new_ui(
        egui::UiBuilder::new()
            .max_rect(text_rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
        |ui| {
            ui.add(egui::Label::new(rt).truncate().selectable(false));
        },
    );
    resp
}

fn flat_menu_item(ui: &mut egui::Ui, label: &str, palette: &Palette, enabled: bool) -> bool {
    let sense = if enabled { egui::Sense::click() } else { egui::Sense::hover() };
    let w = ui.available_width().max(176.0);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 30.0), sense);
    if enabled && resp.hovered() {
        ui.painter().rect_filled(rect, 7.0, palette.bg_hover);
    }
    let col = if enabled { palette.text_primary } else { palette.text_muted };
    ui.allocate_new_ui(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(egui::vec2(10.0, 0.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
        |ui| {
            ui.add(egui::Label::new(egui::RichText::new(label).size(13.0).color(col)).truncate().selectable(false));
        },
    );
    enabled && resp.clicked()
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

fn draw_chatgpt_loading_indicator(ui: &mut egui::Ui, palette: &Palette) {
    let t = ui.input(|i| i.time) as f32;
    // 平滑正弦呼吸脉冲：半径在 4.6px ~ 6.8px 之间呼吸，伴随轻微外圈光晕
    let phase = (t * 4.6).sin() * 0.5 + 0.5;
    let radius = 4.6 + phase * 2.2;
    let alpha = (0.65 + phase * 0.35).clamp(0.0, 1.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(24.0, 24.0), egui::Sense::hover());
    let center = egui::pos2(rect.left() + 8.0, rect.center().y);
    let halo_alpha = ((1.0 - phase) * 0.18).clamp(0.0, 1.0);
    ui.painter().circle_filled(center, radius + 3.2, palette.text_primary.gamma_multiply(halo_alpha));
    ui.painter().circle_filled(center, radius, palette.text_primary.gamma_multiply(alpha));
}

enum RowAction {
    None,
    StartEdit(usize, String),
    CancelEdit,
    CommitEdit(String),
    RetryLast,
}

fn render_row(
    ui: &mut egui::Ui,
    index: usize,
    row: &mut Row,
    palette: &Palette,
    streaming_this: bool,
    is_last_ferro: bool,
    idle: bool,
    is_editing: bool,
    edit_buf: &mut String,
    action: &mut RowAction,
) {
    if row.tool_id.is_some() || row.label == "Thinking" {
        let label = if row.label == "Thinking" {
            if streaming_this { "正在思考" } else { "已完成思考" }
        } else {
            &row.label
        };
        let is_running = row.label.ends_with("执行中");
        ui.horizontal(|ui| {
            if is_running {
                let t = ui.input(|i| i.time) as f32;
                let phase = (t * 5.2).sin() * 0.5 + 0.5;
                let (dot_rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 16.0), egui::Sense::hover());
                ui.painter().circle_filled(dot_rect.center(), 3.2 + phase * 1.1, palette.text_secondary.gamma_multiply(0.55 + phase * 0.45));
            }
            egui::CollapsingHeader::new(egui::RichText::new(label).small().color(palette.text_secondary))
                .show(ui, |ui| markdown::render_diff_or_output(ui, &row.text, palette));
        });
    } else if row.label == "You" {
        let max_width = ui.available_width() * 0.76;
        if is_editing {
            egui::Frame::none()
                .fill(palette.bg_user_bubble)
                .rounding(18.0)
                .inner_margin(egui::Margin::symmetric(16.0, 12.0))
                .show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(edit_buf)
                            .desired_rows(2)
                            .desired_width(f32::INFINITY),
                    );
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let send_btn = egui::Button::new(egui::RichText::new("发送").size(12.5).strong().color(palette.accent_fg))
                                .fill(palette.accent)
                                .stroke(egui::Stroke::NONE)
                                .rounding(14.0)
                                .min_size(egui::vec2(64.0, 28.0));
                            if ui.add_enabled(idle && !edit_buf.trim().is_empty(), send_btn).clicked() {
                                *action = RowAction::CommitEdit(edit_buf.clone());
                            }
                            let cancel_btn = egui::Button::new(egui::RichText::new("取消").size(12.5).color(palette.text_primary))
                                .fill(palette.bg_card)
                                .stroke(egui::Stroke::new(1.0_f32, palette.border))
                                .rounding(14.0)
                                .min_size(egui::vec2(64.0, 28.0));
                            if ui.add(cancel_btn).clicked() {
                                *action = RowAction::CancelEdit;
                            }
                        });
                    });
                });
        } else {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                        egui::Frame::none()
                            .fill(palette.bg_user_bubble)
                            .rounding(18.0)
                            .inner_margin(egui::Margin::symmetric(16.0, 10.0))
                            .show(ui, |ui| {
                                ui.set_max_width(max_width);
                                ui.vertical(|ui| {
                                    if !row.images.is_empty() {
                                        ui.horizontal_wrapped(|ui| {
                                            for (idx, img) in row.images.iter_mut().enumerate() {
                                                if img.tex.is_none() {
                                                    if let Some((tw, th, rgba)) = &img.thumb_rgba {
                                                        let cimg = egui::ColorImage::from_rgba_unmultiplied([*tw as usize, *th as usize], rgba);
                                                        img.tex = Some(ui.ctx().load_texture(format!("msg_img_{idx}"), cimg, egui::TextureOptions::LINEAR));
                                                    }
                                                }
                                                if let Some(tex) = &img.tex {
                                                    ui.add(egui::Image::new((tex.id(), egui::vec2(56.0, 56.0))).rounding(8.0));
                                                } else {
                                                    ui.label(egui::RichText::new(format!("[图片: {}]", img.label)).small().color(palette.text_secondary));
                                                }
                                            }
                                        });
                                        ui.add_space(4.0);
                                    }
                                    ui.add(egui::Label::new(egui::RichText::new(&row.text).color(palette.text_user_bubble))
                                        .wrap().selectable(true));
                                });
                            });
                    });
                });
                // 用户消息底部微动作行（复制 / 编辑），对齐本地 ChatGPT 气泡下方动作栏
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let edit_btn = egui::Button::new(egui::RichText::new("编辑").size(11.5).color(palette.text_muted))
                            .fill(egui::Color32::TRANSPARENT)
                            .stroke(egui::Stroke::NONE)
                            .rounding(6.0);
                        if ui.add_enabled(idle, edit_btn).on_hover_text("编辑消息").clicked() {
                            *action = RowAction::StartEdit(index, row.text.clone());
                        }
                        let copy_btn = egui::Button::new(egui::RichText::new("复制").size(11.5).color(palette.text_muted))
                            .fill(egui::Color32::TRANSPARENT)
                            .stroke(egui::Stroke::NONE)
                            .rounding(6.0);
                        if ui.add(copy_btn).on_hover_text("复制").clicked() {
                            ui.ctx().copy_text(row.text.clone());
                        }
                    });
                });
            });
        }
    } else {
        let blocks = row.blocks.get_or_insert_with(|| markdown::parse_markdown(&row.text));
        markdown::render_blocks(ui, blocks, palette);
        if streaming_this {
            draw_chatgpt_loading_indicator(ui, palette);
        } else {
            ui.horizontal(|ui| {
                let copy_btn = egui::Button::new(egui::RichText::new("复制").size(12.0).color(palette.text_muted))
                    .fill(egui::Color32::TRANSPARENT)
                    .stroke(egui::Stroke::NONE)
                    .rounding(6.0);
                if ui.add(copy_btn).on_hover_text("复制").clicked() {
                    ui.ctx().copy_text(row.text.clone());
                }
                if is_last_ferro {
                    let retry_btn = egui::Button::new(egui::RichText::new("重试").size(12.0).color(palette.text_muted))
                        .fill(egui::Color32::TRANSPARENT)
                        .stroke(egui::Stroke::NONE)
                        .rounding(6.0);
                    if ui.add_enabled(idle, retry_btn).on_hover_text("重试").clicked() {
                        *action = RowAction::RetryLast;
                    }
                }
            });
        }
    }
    ui.add_space(20.0);
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
