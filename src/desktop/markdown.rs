//! 消息排版和工具输出渲染。

use super::theme::Palette;
use eframe::egui::{self, text::LayoutJob, Color32, FontFamily, FontId, Rounding, Stroke, TextFormat};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MdBlock {
    Heading { level: u8, text: String },
    Paragraph(String),
    Bullet(String),
    Numbered { num: String, text: String },
    Quote(String),
    Divider,
    Code { lang: String, code: String },
}

pub fn parse_markdown(input: &str) -> Vec<MdBlock> {
    let mut blocks = Vec::new();
    let mut in_code = false;
    let mut code_lang = String::new();
    let mut code_lines: Vec<&str> = Vec::new();
    let mut para_lines: Vec<&str> = Vec::new();

    let flush_para = |para: &mut Vec<&str>, out: &mut Vec<MdBlock>| {
        if !para.is_empty() {
            out.push(MdBlock::Paragraph(para.join("\n")));
            para.clear();
        }
    };

    for raw_line in input.lines() {
        let line = raw_line.trim_end_matches('\r');
        let trimmed = line.trim_start();

        if let Some(fence) = trimmed.strip_prefix("```") {
            if in_code {
                blocks.push(MdBlock::Code {
                    lang: std::mem::take(&mut code_lang),
                    code: code_lines.join("\n"),
                });
                code_lines.clear();
                in_code = false;
            } else {
                flush_para(&mut para_lines, &mut blocks);
                code_lang = fence.trim().to_string();
                in_code = true;
            }
            continue;
        }

        if in_code {
            code_lines.push(line);
            continue;
        }

        if trimmed.is_empty() {
            flush_para(&mut para_lines, &mut blocks);
            continue;
        }

        // 分隔线
        if trimmed == "---" || trimmed == "***" || trimmed == "___" {
            flush_para(&mut para_lines, &mut blocks);
            blocks.push(MdBlock::Divider);
            continue;
        }

        // 标题 # / ## / ### / ####
        if let Some((level, rest)) = parse_heading(trimmed) {
            flush_para(&mut para_lines, &mut blocks);
            blocks.push(MdBlock::Heading {
                level,
                text: rest.to_string(),
            });
            continue;
        }

        // 引用 >
        if let Some(rest) = trimmed.strip_prefix("> ") {
            flush_para(&mut para_lines, &mut blocks);
            blocks.push(MdBlock::Quote(rest.to_string()));
            continue;
        }

        // 无序列表
        if let Some(rest) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .or_else(|| trimmed.strip_prefix("• "))
        {
            flush_para(&mut para_lines, &mut blocks);
            blocks.push(MdBlock::Bullet(rest.to_string()));
            continue;
        }

        // 有序列表
        if let Some((num, rest)) = parse_numbered(trimmed) {
            flush_para(&mut para_lines, &mut blocks);
            blocks.push(MdBlock::Numbered {
                num: num.to_string(),
                text: rest.to_string(),
            });
            continue;
        }

        para_lines.push(line);
    }

    if in_code {
        blocks.push(MdBlock::Code {
            lang: code_lang,
            code: code_lines.join("\n"),
        });
    } else {
        flush_para(&mut para_lines, &mut blocks);
    }

    blocks
}

fn parse_heading(trimmed: &str) -> Option<(u8, &str)> {
    let hashes = trimmed.bytes().take_while(|&b| b == b'#').count();
    if (1..=4).contains(&hashes) {
        let rest = &trimmed[hashes..];
        if let Some(text) = rest.strip_prefix(' ') {
            return Some((hashes as u8, text.trim()));
        }
    }
    None
}

fn parse_numbered(trimmed: &str) -> Option<(&str, &str)> {
    let digits = trimmed.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits > 0 && digits <= 3 {
        let after = &trimmed[digits..];
        if let Some(rest) = after.strip_prefix(". ") {
            return Some((&trimmed[..digits], rest));
        }
    }
    None
}

/// 将包含 `**加粗**` 与 `` `行内代码` `` 的文本构建为 LayoutJob
fn build_inline_job(text: &str, base_size: f32, base_color: Color32, p: &Palette) -> LayoutJob {
    let mut job = LayoutJob::default();
    let line_height = Some(base_size * 1.55);
    let normal_fmt = TextFormat {
        font_id: FontId::new(base_size, FontFamily::Proportional),
        color: base_color,
        line_height,
        ..Default::default()
    };
    let bold_fmt = TextFormat {
        font_id: FontId::new(base_size + 0.5, FontFamily::Proportional),
        color: p.text_primary,
        line_height,
        ..Default::default()
    };
    let code_fmt = TextFormat {
        font_id: FontId::new((base_size - 1.2).max(11.0), FontFamily::Monospace),
        color: p.blue,
        background: p.bg_subtle,
        line_height,
        ..Default::default()
    };

    let bytes = text.as_bytes();
    let mut i = 0;
    let mut start = 0;

    while i < bytes.len() {
        // 行内代码 `...`
        if bytes[i] == b'`' {
            if let Some(end_rel) = text[i + 1..].find('`') {
                if i > start {
                    job.append(&text[start..i], 0.0, normal_fmt.clone());
                }
                let code_slice = &text[i + 1..i + 1 + end_rel];
                if !code_slice.is_empty() {
                    job.append(&format!(" {code_slice} "), 0.0, code_fmt.clone());
                }
                i = i + 1 + end_rel + 1;
                start = i;
                continue;
            }
        }
        // 加粗 **...**
        if i + 1 < bytes.len() && bytes[i] == b'*' && bytes[i + 1] == b'*' {
            if let Some(end_rel) = text[i + 2..].find("**") {
                if i > start {
                    job.append(&text[start..i], 0.0, normal_fmt.clone());
                }
                let bold_slice = &text[i + 2..i + 2 + end_rel];
                if !bold_slice.is_empty() {
                    job.append(bold_slice, 0.0, bold_fmt.clone());
                }
                i = i + 2 + end_rel + 2;
                start = i;
                continue;
            }
        }
        i += text[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
    }

    if start < text.len() {
        job.append(&text[start..], 0.0, normal_fmt);
    }
    job
}

pub fn render_blocks(ui: &mut egui::Ui, blocks: &[MdBlock], p: &Palette) {
    for (idx, block) in blocks.iter().enumerate() {
        match block {
            MdBlock::Heading { level, text } => {
                if idx > 0 {
                    ui.add_space(6.0);
                }
                let size = match level {
                    1 => 20.5,
                    2 => 18.0,
                    3 => 16.0,
                    _ => 14.8,
                };
                let job = build_inline_job(text, size, p.text_primary, p);
                ui.add(egui::Label::new(job).wrap().selectable(true));
                ui.add_space(2.0);
            }
            MdBlock::Paragraph(text) => {
                let job = build_inline_job(text, 14.5, p.text_primary, p);
                ui.add(egui::Label::new(job).wrap().selectable(true));
                ui.add_space(6.0);
            }
            MdBlock::Bullet(text) => {
                ui.horizontal_top(|ui| {
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new("•").color(p.text_muted).size(14.0));
                    let job = build_inline_job(text, 14.5, p.text_primary, p);
                    ui.add(egui::Label::new(job).wrap().selectable(true));
                });
                ui.add_space(2.0);
            }
            MdBlock::Numbered { num, text } => {
                ui.horizontal_top(|ui| {
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new(format!("{num}.")).color(p.text_muted).size(13.5));
                    let job = build_inline_job(text, 14.5, p.text_primary, p);
                    ui.add(egui::Label::new(job).wrap().selectable(true));
                });
                ui.add_space(2.0);
            }
            MdBlock::Quote(text) => {
                egui::Frame::none()
                    .fill(p.bg_subtle)
                    .stroke(Stroke::new(1.0_f32, p.border_soft))
                    .rounding(Rounding::same(8.0))
                    .inner_margin(egui::Margin::symmetric(12.0, 8.0))
                    .show(ui, |ui| {
                        ui.set_min_width((ui.available_width() - 4.0).max(0.0));
                        let job = build_inline_job(text, 14.0, p.text_secondary, p);
                        ui.add(egui::Label::new(job).wrap().selectable(true));
                    });
                ui.add_space(6.0);
            }
            MdBlock::Divider => {
                ui.add_space(7.0);
                let rect = ui.available_rect_before_wrap();
                let y = rect.min.y + 2.0;
                ui.painter().line_segment(
                    [egui::pos2(rect.min.x, y), egui::pos2(rect.max.x, y)],
                    Stroke::new(1.0_f32, p.border_soft),
                );
                ui.add_space(8.0);
            }
            MdBlock::Code { lang, code } => {
                render_code_card(ui, lang, code, p, idx);
                ui.add_space(6.0);
            }
        }
    }
}

fn render_code_card(ui: &mut egui::Ui, lang: &str, code: &str, p: &Palette, idx: usize) {
    let header_label = if lang.is_empty() { "代码" } else { lang };
    egui::Frame::none()
        .fill(p.bg_code)
        .stroke(Stroke::new(1.0_f32, p.border))
        .rounding(Rounding::same(10.0))
        .inner_margin(egui::Margin::same(0.0))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            // 顶部语言栏 + 复制按钮
            egui::Frame::none()
                .fill(p.bg_code_hdr)
                .rounding(Rounding {
                    nw: 10.0,
                    ne: 10.0,
                    sw: 0.0,
                    se: 0.0,
                })
                .inner_margin(egui::Margin::symmetric(12.0, 6.0))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(header_label)
                                .size(11.5)
                                .color(p.text_muted)
                                .monospace(),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let btn = egui::Button::new(
                                egui::RichText::new("复制代码")
                                    .size(11.5)
                                    .color(p.text_secondary),
                            )
                            .fill(Color32::TRANSPARENT)
                            .stroke(Stroke::NONE);
                            if ui.add(btn).clicked() {
                                ui.ctx().copy_text(code.to_string());
                            }
                        });
                    });
                });

            // 代码主体（横向滚动，长行不折乱）
            egui::Frame::none()
                .inner_margin(egui::Margin::symmetric(12.0, 10.0))
                .show(ui, |ui| {
                    egui::ScrollArea::horizontal()
                        .id_salt(("code_scroll", idx, ui.next_auto_id()))
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(code)
                                        .font(FontId::new(12.5, FontFamily::Monospace))
                                        .color(p.text_primary),
                                )
                                .extend()
                                .selectable(true),
                            );
                        });
                });
        });
}

/// 渲染工具输出与 Unified Diff（+ 绿 / - 红 / @@ 蓝）
pub fn render_diff_or_output(ui: &mut egui::Ui, text: &str, p: &Palette) {
    let looks_like_diff = text
        .lines()
        .any(|l| l.starts_with("@@ ") || l.starts_with("+++ ") || l.starts_with("--- "));

    egui::Frame::none()
        .fill(p.bg_code)
        .stroke(Stroke::new(1.0_f32, p.border_soft))
        .rounding(Rounding::same(8.0))
        .inner_margin(egui::Margin::symmetric(10.0, 8.0))
        .show(ui, |ui| {
            ui.set_min_width((ui.available_width() - 4.0).max(0.0));
            egui::ScrollArea::horizontal()
                .id_salt(ui.next_auto_id())
                .max_height(280.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    if !looks_like_diff {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(text)
                                    .font(FontId::new(12.0, FontFamily::Monospace))
                                    .color(p.text_secondary),
                            )
                            .wrap()
                            .selectable(true),
                        );
                        return;
                    }
                    let mut job = LayoutJob::default();
                    let mono = FontId::new(12.0, FontFamily::Monospace);
                    for (idx, line) in text.lines().enumerate() {
                        if idx > 0 {
                            job.append(
                                "\n",
                                0.0,
                                TextFormat {
                                    font_id: mono.clone(),
                                    color: p.text_secondary,
                                    ..Default::default()
                                },
                            );
                        }
                        let (fg, bg) = if line.starts_with('+') && !line.starts_with("+++") {
                            (p.diff_add_fg, p.diff_add_bg)
                        } else if line.starts_with('-') && !line.starts_with("---") {
                            (p.diff_del_fg, p.diff_del_bg)
                        } else if line.starts_with("@@") {
                            (p.blue, Color32::TRANSPARENT)
                        } else {
                            (p.text_secondary, Color32::TRANSPARENT)
                        };
                        job.append(
                            line,
                            0.0,
                            TextFormat {
                                font_id: mono.clone(),
                                color: fg,
                                background: bg,
                                ..Default::default()
                            },
                        );
                    }
                    ui.add(egui::Label::new(job).extend().selectable(true));
                });
        });
}

/// 零依赖纯 Rust RGBA -> PNG 写入器（标准 zlib 非压缩块），用于保存真实窗口帧缓冲截图。
pub fn write_rgba_png(path: &std::path::Path, width: u32, height: u32, rgba: &[u8]) -> std::io::Result<()> {
    let row_bytes = (width as usize) * 4;
    let mut raw = Vec::with_capacity((row_bytes + 1) * (height as usize));
    for y in 0..(height as usize) {
        raw.push(0u8);
        let start = y * row_bytes;
        raw.extend_from_slice(&rgba[start..start + row_bytes]);
    }
    let mut zlib = Vec::with_capacity(raw.len() + (raw.len() / 65535 + 1) * 5 + 6);
    zlib.push(0x78);
    zlib.push(0x01);
    let mut pos = 0;
    while pos < raw.len() {
        let len = (raw.len() - pos).min(65535);
        let is_last = pos + len >= raw.len();
        zlib.push(if is_last { 0x01 } else { 0x00 });
        let len16 = len as u16;
        let nlen16 = !len16;
        zlib.extend_from_slice(&len16.to_le_bytes());
        zlib.extend_from_slice(&nlen16.to_le_bytes());
        zlib.extend_from_slice(&raw[pos..pos + len]);
        pos += len;
    }
    let (mut a, mut b): (u32, u32) = (1, 0);
    for &byte in &raw {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    zlib.extend_from_slice(&((b << 16) | a).to_be_bytes());

    let push_chunk = |out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]| {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let mut crc: u32 = 0xFFFF_FFFF;
        for &byte in kind.iter().chain(data.iter()) {
            crc ^= byte as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
            }
        }
        out.extend_from_slice(&(!crc).to_be_bytes());
    };

    let mut png = Vec::with_capacity(zlib.len() + 64);
    png.extend_from_slice(&[137, 80, 78, 71, 13, 10, 26, 10]);
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    push_chunk(&mut png, b"IHDR", &ihdr);
    push_chunk(&mut png, b"IDAT", &zlib);
    push_chunk(&mut png, b"IEND", &[]);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(path, png)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_headings_lists_quotes_and_fenced_code() {
        let src = "# 一级标题\n\n普通段落包含 **加粗** 和 `code`。\n\n- 列表项一\n- 列表项二\n\n1. 第一步\n2. 第二步\n\n> 引用说明\n\n```rust\nfn main() {}\n```";
        let blocks = parse_markdown(src);
        assert!(matches!(&blocks[0], MdBlock::Heading { level: 1, text } if text == "一级标题"));
        assert!(matches!(&blocks[1], MdBlock::Paragraph(_)));
        assert!(matches!(&blocks[2], MdBlock::Bullet(t) if t == "列表项一"));
        assert!(matches!(&blocks[4], MdBlock::Numbered { num, text } if num == "1" && text == "第一步"));
        assert!(matches!(&blocks[6], MdBlock::Quote(t) if t == "引用说明"));
        assert!(matches!(&blocks[7], MdBlock::Code { lang, code } if lang == "rust" && code == "fn main() {}"));
    }
}
