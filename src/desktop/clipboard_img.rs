//! 剪贴板图片粘贴（Ctrl+V 截图 CF_DIB / 复制图片文件 CF_HDROP）、图片附件加载与内置免报错目录/文件选择器。

use super::markdown::{base64_encode, encode_rgba_png};
use super::theme::Palette;
use eframe::egui;
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub struct PendingImage {
    pub label: String,
    pub media_type: String,
    pub base64: String,
    pub thumb_rgba: Option<(u32, u32, Vec<u8>)>,
    pub tex: Option<egui::TextureHandle>,
}

pub fn is_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp"
            )
        })
        .unwrap_or(false)
}

pub fn load_image_file(path: &Path) -> Option<PendingImage> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let bytes = std::fs::read(path).ok()?;
    if bytes.is_empty() {
        return None;
    }
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();

    if ext == "bmp" && bytes.len() > 54 && &bytes[0..2] == b"BM" {
        let pixel_offset = u32::from_le_bytes(bytes[10..14].try_into().ok()?) as usize;
        if pixel_offset <= bytes.len() && bytes.len() > 14 + 40 {
            if let Some(img) = decode_dib_slice(&bytes[14..], Some(pixel_offset.saturating_sub(14)), &name) {
                return Some(img);
            }
        }
    }

    let media_type = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        _ => return None,
    };

    Some(PendingImage {
        label: name,
        media_type: media_type.into(),
        base64: base64_encode(&bytes),
        thumb_rgba: None,
        tex: None,
    })
}

fn make_thumbnail(width: u32, height: u32, rgba: &[u8], max_side: u32) -> (u32, u32, Vec<u8>) {
    if width == 0 || height == 0 {
        return (1, 1, vec![0, 0, 0, 0]);
    }
    let scale = (max_side as f32 / (width.max(height) as f32)).min(1.0);
    let tw = ((width as f32 * scale).round() as u32).max(1);
    let th = ((height as f32 * scale).round() as u32).max(1);
    let mut out = Vec::with_capacity((tw * th * 4) as usize);
    for ty in 0..th {
        let sy = ((ty as u64 * height as u64) / th as u64) as usize;
        for tx in 0..tw {
            let sx = ((tx as u64 * width as u64) / tw as u64) as usize;
            let idx = (sy * width as usize + sx) * 4;
            if idx + 4 <= rgba.len() {
                out.extend_from_slice(&rgba[idx..idx + 4]);
            } else {
                out.extend_from_slice(&[0, 0, 0, 255]);
            }
        }
    }
    (tw, th, out)
}

/// 解析 Windows DIB (BITMAPINFOHEADER + 像素数据) 为 PendingImage
fn decode_dib_slice(dib: &[u8], explicit_pixel_offset: Option<usize>, label_prefix: &str) -> Option<PendingImage> {
    if dib.len() < 40 {
        return None;
    }
    let header_size = u32::from_le_bytes(dib[0..4].try_into().ok()?) as usize;
    if header_size < 40 || dib.len() < header_size {
        return None;
    }
    let width_i = i32::from_le_bytes(dib[4..8].try_into().ok()?);
    let height_i = i32::from_le_bytes(dib[8..12].try_into().ok()?);
    let bpp = u16::from_le_bytes(dib[14..16].try_into().ok()?);
    let compression = u32::from_le_bytes(dib[16..20].try_into().ok()?);
    let clr_used = u32::from_le_bytes(dib[32..36].try_into().ok()?) as usize;

    if width_i <= 0 || width_i > 16384 || height_i == 0 || height_i.abs() > 16384 {
        return None;
    }
    if bpp != 24 && bpp != 32 {
        return None;
    }
    // BI_RGB = 0, BI_BITFIELDS = 3
    if compression != 0 && compression != 3 {
        return None;
    }

    let width = width_i as u32;
    let height = height_i.unsigned_abs();
    let bottom_up = height_i > 0;

    let masks_bytes = if compression == 3 && header_size == 40 { 12 } else { 0 };
    let palette_bytes = clr_used * 4;
    let pixel_offset = explicit_pixel_offset.unwrap_or(header_size + masks_bytes + palette_bytes);
    if pixel_offset >= dib.len() {
        return None;
    }
    let pixels = &dib[pixel_offset..];
    let bytes_per_pixel = (bpp as usize) / 8;
    let stride = (width as usize * bytes_per_pixel).div_ceil(4) * 4;
    if pixels.len() < stride * (height as usize).saturating_sub(1) + width as usize * bytes_per_pixel {
        return None;
    }

    let mut rgba = vec![0u8; (width * height * 4) as usize];
    let mut any_nonzero_alpha = false;
    for y in 0..(height as usize) {
        let src_y = if bottom_up { (height as usize) - 1 - y } else { y };
        let row = &pixels[src_y * stride..];
        let dst_row = &mut rgba[y * (width as usize) * 4..(y + 1) * (width as usize) * 4];
        for x in 0..(width as usize) {
            let b = row[x * bytes_per_pixel];
            let g = row[x * bytes_per_pixel + 1];
            let r = row[x * bytes_per_pixel + 2];
            let a = if bytes_per_pixel == 4 {
                let alpha = row[x * bytes_per_pixel + 3];
                if alpha != 0 {
                    any_nonzero_alpha = true;
                }
                alpha
            } else {
                255
            };
            dst_row[x * 4] = r;
            dst_row[x * 4 + 1] = g;
            dst_row[x * 4 + 2] = b;
            dst_row[x * 4 + 3] = a;
        }
    }
    // 部分 Windows GDI 截图的 32-bit DIB 将 Alpha 通道全置为 0，此时统一修正为不透明 255
    if bytes_per_pixel == 4 && !any_nonzero_alpha {
        for chunk in rgba.chunks_exact_mut(4) {
            chunk[3] = 255;
        }
    }

    let thumb = make_thumbnail(width, height, &rgba, 64);
    let png_bytes = encode_rgba_png(width, height, &rgba);
    let base64 = base64_encode(&png_bytes);
    Some(PendingImage {
        label: format!("{label_prefix} {width}×{height}"),
        media_type: "image/png".into(),
        base64,
        thumb_rgba: Some(thumb),
        tex: None,
    })
}

/// 检测本帧是否触发了 Win32 系统的 Ctrl+V 粘贴快捷键（绕过 egui-winit 在剪贴板仅有图片无文本时吞掉 Key::V pressed 事件的 Bug）
#[cfg(windows)]
pub fn poll_ctrl_v_pressed(window_focused: bool) -> bool {
    const VK_CONTROL: i32 = 0x11;
    const VK_V: i32 = 0x56;
    extern "system" {
        fn GetAsyncKeyState(vKey: i32) -> i16;
    }
    static WAS_V_DOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !window_focused {
        WAS_V_DOWN.store(false, std::sync::atomic::Ordering::Relaxed);
        return false;
    }
    unsafe {
        let ctrl_down = (GetAsyncKeyState(VK_CONTROL) as u16 & 0x8000) != 0;
        let v_down = (GetAsyncKeyState(VK_V) as u16 & 0x8000) != 0;
        let prev = WAS_V_DOWN.swap(v_down, std::sync::atomic::Ordering::Relaxed);
        ctrl_down && v_down && !prev
    }
}

#[cfg(not(windows))]
pub fn poll_ctrl_v_pressed(_window_focused: bool) -> bool {
    false
}

/// 检测是否触发了 Ctrl+V 粘贴（结合 Win32 GetAsyncKeyState 与 egui 事件，且带 280ms 防抖避免按下和松开重复粘贴两次）
pub fn should_paste_clipboard_image(ctx: &egui::Context) -> bool {
    let win_focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
    let win32_pressed = poll_ctrl_v_pressed(win_focused);
    let egui_triggered = ctx.input(|i| {
        i.events.iter().any(|ev| match ev {
            egui::Event::Paste(_) => true,
            egui::Event::Key { key: egui::Key::V, modifiers, .. } => {
                modifiers.command || modifiers.ctrl
            }
            _ => false,
        })
    });
    if !win32_pressed && !egui_triggered {
        return false;
    }
    static LAST_PASTE_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let prev = LAST_PASTE_MS.load(std::sync::atomic::Ordering::Relaxed);
    if now_ms.saturating_sub(prev) < 280 {
        return false;
    }
    LAST_PASTE_MS.store(now_ms, std::sync::atomic::Ordering::Relaxed);
    true
}

/// 从系统剪贴板读取截图位图 (CF_DIB / CF_DIBV5) 或复制的图片文件 (CF_HDROP)。
#[cfg(windows)]
pub fn try_paste_clipboard_images() -> Vec<PendingImage> {
    const CF_DIB: u32 = 8;
    const CF_HDROP: u32 = 15;
    const CF_DIBV5: u32 = 17;

    extern "system" {
        fn OpenClipboard(hWndNewOwner: isize) -> i32;
        fn CloseClipboard() -> i32;
        fn IsClipboardFormatAvailable(format: u32) -> i32;
        fn GetClipboardData(uFormat: u32) -> isize;
        fn GlobalLock(hMem: isize) -> *const u8;
        fn GlobalUnlock(hMem: isize) -> i32;
        fn GlobalSize(hMem: isize) -> usize;
        fn DragQueryFileW(hDrop: isize, iFile: u32, lpszFile: *mut u16, cch: u32) -> u32;
    }

    let mut out = Vec::new();
    unsafe {
        if IsClipboardFormatAvailable(CF_DIB) == 0
            && IsClipboardFormatAvailable(CF_DIBV5) == 0
            && IsClipboardFormatAvailable(CF_HDROP) == 0
        {
            return out;
        }
        if OpenClipboard(0) == 0 {
            return out;
        }

        // 1. 优先检查截图位图 CF_DIB / CF_DIBV5（Win+Shift+S / 微信截图 / 浏览器复制图片）
        for fmt in [CF_DIB, CF_DIBV5] {
            if !out.is_empty() {
                break;
            }
            if IsClipboardFormatAvailable(fmt) != 0 {
                let hmem = GetClipboardData(fmt);
                if hmem != 0 {
                    let ptr = GlobalLock(hmem);
                    let size = GlobalSize(hmem);
                    if !ptr.is_null() && size >= 40 {
                        let slice = std::slice::from_raw_parts(ptr, size);
                        if let Some(img) = decode_dib_slice(slice, None, "截图") {
                            out.push(img);
                        }
                    }
                    if !ptr.is_null() {
                        GlobalUnlock(hmem);
                    }
                }
            }
        }

        // 2. 检查资源管理器复制的图片文件 CF_HDROP
        if out.is_empty() && IsClipboardFormatAvailable(CF_HDROP) != 0 {
            let hdrop = GetClipboardData(CF_HDROP);
            if hdrop != 0 {
                let count = DragQueryFileW(hdrop, 0xFFFF_FFFF, std::ptr::null_mut(), 0);
                for idx in 0..count.min(8) {
                    let len = DragQueryFileW(hdrop, idx, std::ptr::null_mut(), 0);
                    if len > 0 {
                        let mut buf = vec![0u16; (len as usize) + 1];
                        let copied = DragQueryFileW(hdrop, idx, buf.as_mut_ptr(), len + 1);
                        if copied > 0 {
                            let path = PathBuf::from(String::from_utf16_lossy(&buf[..copied as usize]));
                            if is_image_path(&path) {
                                if let Some(img) = load_image_file(&path) {
                                    out.push(img);
                                }
                            }
                        }
                    }
                }
            }
        }

        CloseClipboard();
    }
    out
}

#[cfg(not(windows))]
pub fn try_paste_clipboard_images() -> Vec<PendingImage> {
    Vec::new()
}

/// 内置轻量原生目录/文件选择器（彻底避开 Windows 资源管理器 Desktop 悬空联接点弹窗报错）。
pub struct BuiltinBrowser {
    pub open: bool,
    pub pick_file_mode: bool,
    pub current_dir: PathBuf,
    pub path_input: String,
    pub drives: Vec<PathBuf>,
    pub entries: Vec<(String, PathBuf, bool)>, // (name, path, is_dir)
    pub error: String,
}

impl BuiltinBrowser {
    pub fn new() -> Self {
        Self {
            open: false,
            pick_file_mode: false,
            current_dir: PathBuf::new(),
            path_input: String::new(),
            drives: Vec::new(),
            entries: Vec::new(),
            error: String::new(),
        }
    }

    pub fn open_for(&mut self, initial: Option<PathBuf>, pick_file_mode: bool) {
        self.pick_file_mode = pick_file_mode;
        if self.drives.is_empty() {
            for letter in b'C'..=b'Z' {
                let root = PathBuf::from(format!("{}:\\", letter as char));
                if root.is_dir() {
                    self.drives.push(root);
                }
            }
        }
        let dir = initial
            .filter(|d| d.is_dir() && std::fs::read_dir(d).is_ok())
            .or_else(|| std::env::current_dir().ok().filter(|d| std::fs::read_dir(d).is_ok()))
            .or_else(|| dirs::home_dir().filter(|d| std::fs::read_dir(d).is_ok()))
            .unwrap_or_else(|| PathBuf::from("C:\\"));
        self.navigate(dir);
        self.open = true;
    }

    pub fn navigate(&mut self, dir: PathBuf) {
        let clean = crate::strip_unc(std::fs::canonicalize(&dir).unwrap_or(dir));
        match std::fs::read_dir(&clean) {
            Ok(rd) => {
                let mut list = Vec::new();
                for entry in rd.flatten() {
                    let path = entry.path();
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if name.starts_with('.') {
                        continue;
                    }
                    let is_dir = path.is_dir() && std::fs::read_dir(&path).is_ok();
                    if is_dir || (self.pick_file_mode && path.is_file()) {
                        list.push((name, path, is_dir));
                    }
                }
                list.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase())));
                self.current_dir = clean.clone();
                self.path_input = clean.display().to_string();
                self.entries = list;
                self.error.clear();
            }
            Err(e) => {
                self.error = format!("无法访问该路径：{e}");
            }
        }
    }

    /// 返回 (选中的项目目录, 选中的文件列表)
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        palette: &Palette,
    ) -> (Option<PathBuf>, Vec<PathBuf>) {
        let mut chosen_dir = None;
        let mut chosen_files = Vec::new();
        if !self.open {
            return (chosen_dir, chosen_files);
        }

        let mut win_open = self.open;
        // 半透明暗色背景遮罩（Backdrop），凸显现代无标题栏居中 Modal
        let screen = ctx.screen_rect();
        ctx.layer_painter(egui::LayerId::new(egui::Order::Middle, egui::Id::new("modal_backdrop")))
            .rect_filled(screen, 0.0, egui::Color32::from_black_alpha(72));

        let title = if self.pick_file_mode {
            "选择图片或代码文件"
        } else {
            "选择项目目录"
        };

        egui::Window::new(title)
            .title_bar(false)
            .resizable(false)
            .collapsible(false)
            .fixed_size(egui::vec2(500.0, 400.0))
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .frame(
                egui::Frame::none()
                    .fill(palette.bg_canvas)
                    .stroke(egui::Stroke::new(1.0_f32, palette.border))
                    .rounding(16.0)
                    .shadow(egui::Shadow {
                        offset: egui::vec2(0.0, 12.0),
                        blur: 32.0,
                        spread: 0.0,
                        color: egui::Color32::from_black_alpha(45),
                    })
                    .inner_margin(egui::Margin::same(20.0)),
            )
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                // 自定义极简弹窗顶栏
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(title).size(16.5).strong().color(palette.text_primary));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let close = egui::Button::new(egui::RichText::new("×").size(18.0).color(palette.text_secondary))
                            .fill(egui::Color32::TRANSPARENT)
                            .stroke(egui::Stroke::NONE)
                            .min_size(egui::vec2(28.0, 28.0));
                        if ui.add(close).clicked() {
                            win_open = false;
                        }
                    });
                });

                // 磁盘分区快捷胶囊行
                let drives = self.drives.clone();
                let mut drive_clicked = None;
                ui.horizontal_wrapped(|ui| {
                    for root in &drives {
                        let label = root.to_string_lossy().trim_end_matches('\\').to_string();
                        let is_cur_drive = self.current_dir.starts_with(root);
                        let btn = egui::Button::new(
                            egui::RichText::new(&label).size(12.0).color(if is_cur_drive { palette.accent_fg } else { palette.text_primary }),
                        )
                        .fill(if is_cur_drive { palette.accent } else { palette.bg_subtle })
                        .stroke(egui::Stroke::NONE)
                        .rounding(6.0)
                        .min_size(egui::vec2(34.0, 24.0));
                        if ui.add(btn).clicked() {
                            drive_clicked = Some(root.clone());
                        }
                    }
                    if let Some(parent) = self.current_dir.parent().map(Path::to_path_buf) {
                        let up_btn = egui::Button::new(egui::RichText::new("↑ 上级").size(12.0).color(palette.text_secondary))
                            .fill(palette.bg_subtle)
                            .stroke(egui::Stroke::NONE)
                            .rounding(6.0)
                            .min_size(egui::vec2(52.0, 24.0));
                        if ui.add(up_btn).clicked() {
                            drive_clicked = Some(parent);
                        }
                    }
                });
                if let Some(d) = drive_clicked {
                    self.navigate(d);
                }

                // 路径输入框：支持直接粘贴任意文件夹/文件路径跳转
                ui.horizontal(|ui| {
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut self.path_input)
                            .desired_width((ui.available_width() - 62.0).max(120.0))
                            .hint_text("输入或粘贴路径…"),
                    );
                    let go_btn = egui::Button::new(egui::RichText::new("前往").size(12.5))
                        .fill(palette.bg_subtle)
                        .stroke(egui::Stroke::NONE)
                        .rounding(8.0)
                        .min_size(egui::vec2(52.0, 28.0));
                    if ui.add(go_btn).clicked()
                        || (edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                    {
                        let target = PathBuf::from(self.path_input.trim());
                        if target.is_file() && self.pick_file_mode {
                            chosen_files.push(target);
                        } else {
                            self.navigate(target);
                        }
                    }
                });

                if !self.error.is_empty() {
                    ui.colored_label(palette.danger, &self.error);
                }

                // 扁平化目录与文件列表（矢量文件夹/文件图标，无 [文件夹] 土味前缀）
                let mut nav_next = None;
                egui::Frame::none()
                    .fill(palette.bg_sidebar)
                    .stroke(egui::Stroke::new(1.0_f32, palette.border_soft))
                    .rounding(10.0)
                    .inner_margin(6.0)
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("builtin_browser_scroll")
                            .max_height(225.0)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.spacing_mut().item_spacing.y = 2.0;
                                if self.entries.is_empty() {
                                    ui.add_space(12.0);
                                    ui.vertical_centered(|ui| {
                                        ui.label(egui::RichText::new("此目录下没有子文件夹").small().color(palette.text_muted));
                                    });
                                }
                                for (name, path, is_dir) in &self.entries {
                                    let w = ui.available_width();
                                    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 30.0), egui::Sense::click());
                                    if resp.hovered() {
                                        ui.painter().rect_filled(rect, 7.0, palette.bg_hover);
                                    }
                                    let ic = egui::pos2(rect.left() + 16.0, rect.center().y);
                                    let st = egui::Stroke::new(1.3_f32, if *is_dir { palette.text_secondary } else { palette.blue });
                                    if *is_dir {
                                        let r = egui::Rect::from_center_size(egui::pos2(ic.x, ic.y + 0.6), egui::vec2(13.0, 9.5));
                                        ui.painter().rect_stroke(r, 1.8, st);
                                        ui.painter().line_segment([egui::pos2(r.left() + 1.0, r.top() - 1.8), egui::pos2(r.left() + 5.5, r.top() - 1.8)], st);
                                    } else {
                                        let r = egui::Rect::from_center_size(ic, egui::vec2(10.5, 13.0));
                                        ui.painter().rect_stroke(r, 1.5, st);
                                    }
                                    let text_rect = egui::Rect::from_min_max(
                                        egui::pos2(rect.left() + 32.0, rect.top()),
                                        egui::pos2(rect.right() - 8.0, rect.bottom()),
                                    );
                                    let col = if *is_dir { palette.text_primary } else { palette.text_secondary };
                                    ui.allocate_new_ui(
                                        egui::UiBuilder::new().max_rect(text_rect).layout(egui::Layout::left_to_right(egui::Align::Center)),
                                        |ui| {
                                            ui.add(egui::Label::new(egui::RichText::new(name).size(13.0).color(col)).truncate().selectable(false));
                                        },
                                    );
                                    if resp.clicked() {
                                        if *is_dir {
                                            nav_next = Some(path.clone());
                                        } else if self.pick_file_mode {
                                            chosen_files.push(path.clone());
                                        }
                                    }
                                }
                            });
                    });
                if let Some(next) = nav_next {
                    self.navigate(next);
                }

                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if !self.pick_file_mode {
                            let ok_btn = egui::Button::new(
                                egui::RichText::new("选择此目录").size(13.0).strong().color(palette.accent_fg),
                            )
                            .fill(palette.accent)
                            .stroke(egui::Stroke::NONE)
                            .min_size(egui::vec2(104.0, 32.0))
                            .rounding(16.0);
                            if ui.add(ok_btn).clicked() {
                                chosen_dir = Some(self.current_dir.clone());
                            }
                        }
                        let cancel_btn = egui::Button::new(egui::RichText::new("取消").size(13.0).color(palette.text_secondary))
                            .fill(palette.bg_subtle)
                            .stroke(egui::Stroke::NONE)
                            .min_size(egui::vec2(68.0, 32.0))
                            .rounding(16.0);
                        if ui.add(cancel_btn).clicked() {
                            win_open = false;
                        }
                    });
                });
            });

        if chosen_dir.is_some() || !chosen_files.is_empty() {
            win_open = false;
        }
        self.open = win_open;
        (chosen_dir, chosen_files)
    }
}
