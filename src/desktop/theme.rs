//! Ferro 桌面色板、控件样式与中文提示（1:1 ChatGPT 极简黑白灰中性体系）。

use eframe::egui::{self, Color32, FontFamily, FontId, Rounding, Shadow, Stroke, TextStyle};

#[derive(Clone, Copy)]
pub struct Palette {
    pub bg_sidebar: Color32,
    pub bg_canvas: Color32,
    pub bg_card: Color32,
    pub bg_hover: Color32,
    pub bg_active: Color32,
    pub bg_user_bubble: Color32,
    pub text_user_bubble: Color32,
    pub bg_subtle: Color32,
    pub bg_code: Color32,
    pub bg_code_hdr: Color32,
    pub border: Color32,
    pub border_soft: Color32,
    pub text_primary: Color32,
    pub text_secondary: Color32,
    pub text_muted: Color32,
    /// 代码高亮与链接色
    pub blue: Color32,
    /// 主操作强调色（浅色为深墨黑，深色为纯白）
    pub accent: Color32,
    pub accent_fg: Color32,
    /// 完全访问模式专属暖橙色（对齐 ChatGPT 完全访问胶囊）
    pub orange: Color32,
    pub orange_soft: Color32,
    pub danger: Color32,
    pub diff_add_bg: Color32,
    pub diff_add_fg: Color32,
    pub diff_del_bg: Color32,
    pub diff_del_fg: Color32,
}

impl Palette {
    pub fn light() -> Self {
        Self {
            bg_sidebar: Color32::from_rgb(249, 249, 249),      // #f9f9f9 (--gray-fixed-50) ChatGPT 侧栏浅灰
            bg_canvas: Color32::from_rgb(255, 255, 255),       // #ffffff (--gray-fixed-0) 主画布纯白
            bg_card: Color32::from_rgb(255, 255, 255),         // #ffffff (--gray-fixed-0) 浮空纯白输入舱
            bg_hover: Color32::from_rgb(243, 243, 243),        // #f3f3f3 (--gray-fixed-75) 扁平项悬停态
            bg_active: Color32::from_rgb(237, 237, 237),       // #ededed (--gray-fixed-100) 扁平项选中态
            bg_user_bubble: Color32::from_rgb(243, 243, 243),  // #f3f3f3 (--gray-fixed-75) 用户消息胶囊气泡
            text_user_bubble: Color32::from_rgb(13, 13, 13),   // #0d0d0d (--gray-fixed-1000)
            bg_subtle: Color32::from_rgb(243, 243, 243),       // #f3f3f3 (--gray-fixed-75) 内部小胶囊底色
            bg_code: Color32::from_rgb(249, 249, 249),         // #f9f9f9 (--gray-fixed-50)
            bg_code_hdr: Color32::from_rgb(243, 243, 243),     // #f3f3f3 (--gray-fixed-75)
            border: Color32::from_rgb(223, 223, 223),          // #dfdfdf (--gray-fixed-150)
            border_soft: Color32::from_rgb(237, 237, 237),     // #ededed (--gray-fixed-100)
            text_primary: Color32::from_rgb(13, 13, 13),       // #0d0d0d (--gray-fixed-1000)
            text_secondary: Color32::from_rgb(93, 93, 93),     // #5d5d5d (--gray-fixed-500)
            text_muted: Color32::from_rgb(143, 143, 143),      // #8f8f8f (--gray-fixed-400)
            blue: Color32::from_rgb(22, 121, 91),
            accent: Color32::from_rgb(13, 13, 13),             // #0d0d0d 纯黑主操作按钮
            accent_fg: Color32::WHITE,
            orange: Color32::from_rgb(180, 76, 16),            // #b44c10 完全访问暖棕橙
            orange_soft: Color32::from_rgb(253, 243, 234),     // #fdf3ea 完全访问柔和底
            danger: Color32::from_rgb(220, 38, 38),
            diff_add_bg: Color32::from_rgb(220, 252, 231),
            diff_add_fg: Color32::from_rgb(22, 101, 52),
            diff_del_bg: Color32::from_rgb(254, 226, 226),
            diff_del_fg: Color32::from_rgb(153, 27, 27),
        }
    }

    pub fn dark() -> Self {
        Self {
            bg_sidebar: Color32::from_rgb(24, 24, 24),         // #181818 (--gray-fixed-900) ChatGPT 深色侧栏
            bg_canvas: Color32::from_rgb(33, 33, 33),          // #212121 (--gray-fixed-800) ChatGPT 深色主画布
            bg_card: Color32::from_rgb(48, 48, 48),            // #303030 (--gray-fixed-700) ChatGPT 深色输入舱
            bg_hover: Color32::from_rgb(40, 40, 40),           // #282828 (--gray-fixed-750)
            bg_active: Color32::from_rgb(48, 48, 48),          // #303030 (--gray-fixed-700)
            bg_user_bubble: Color32::from_rgb(48, 48, 48),     // #303030 (--gray-fixed-700)
            text_user_bubble: Color32::from_rgb(243, 243, 243),
            bg_subtle: Color32::from_rgb(40, 40, 40),
            bg_code: Color32::from_rgb(22, 22, 22),
            bg_code_hdr: Color32::from_rgb(33, 33, 33),
            border: Color32::from_rgb(57, 57, 57),             // #393939 (--gray-fixed-650)
            border_soft: Color32::from_rgb(48, 48, 48),        // #303030 (--gray-fixed-700)
            text_primary: Color32::from_rgb(243, 243, 243),    // #f3f3f3 (--gray-fixed-75)
            text_secondary: Color32::from_rgb(175, 175, 175),  // #afafaf (--gray-fixed-300)
            text_muted: Color32::from_rgb(143, 143, 143),      // #8f8f8f (--gray-fixed-400)
            blue: Color32::from_rgb(52, 211, 153),
            accent: Color32::from_rgb(249, 249, 249),          // 纯白主操作按钮
            accent_fg: Color32::from_rgb(13, 13, 13),
            orange: Color32::from_rgb(251, 146, 60),
            orange_soft: Color32::from_rgb(58, 34, 18),
            danger: Color32::from_rgb(248, 113, 113),
            diff_add_bg: Color32::from_rgba_unmultiplied(34, 197, 94, 42),
            diff_add_fg: Color32::from_rgb(134, 239, 172),
            diff_del_bg: Color32::from_rgba_unmultiplied(239, 68, 68, 42),
            diff_del_fg: Color32::from_rgb(252, 165, 165),
        }
    }
}

pub fn apply_theme(ctx: &egui::Context, dark: bool) -> Palette {
    let p = if dark { Palette::dark() } else { Palette::light() };
    let mut style = (*ctx.style()).clone();

    style.text_styles.insert(TextStyle::Heading, FontId::new(18.5, FontFamily::Proportional));
    style.text_styles.insert(TextStyle::Body, FontId::new(14.0, FontFamily::Proportional));
    style.text_styles.insert(TextStyle::Button, FontId::new(13.0, FontFamily::Proportional));
    style.text_styles.insert(TextStyle::Small, FontId::new(12.0, FontFamily::Proportional));
    style.text_styles.insert(TextStyle::Monospace, FontId::new(12.5, FontFamily::Monospace));

    style.spacing.item_spacing = egui::vec2(8.0, 5.0);
    style.spacing.button_padding = egui::vec2(9.0, 5.0);
    style.spacing.menu_margin = egui::Margin::same(8.0);
    style.spacing.window_margin = egui::Margin::same(18.0);
    style.spacing.combo_width = 200.0;
    style.spacing.scroll.bar_width = 5.0;
    style.spacing.scroll.floating = true;

    let mut v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.panel_fill = p.bg_canvas;
    v.window_fill = p.bg_canvas;
    v.window_stroke = Stroke::new(1.0_f32, p.border);
    v.window_rounding = Rounding::same(14.0);
    v.menu_rounding = Rounding::same(12.0);
    v.window_shadow = Shadow {
        offset: egui::vec2(0.0, 6.0),
        blur: 22.0,
        spread: 0.0,
        color: Color32::from_black_alpha(if dark { 110 } else { 26 }),
    };
    v.popup_shadow = Shadow {
        offset: egui::vec2(0.0, 4.0),
        blur: 16.0,
        spread: 0.0,
        color: Color32::from_black_alpha(if dark { 100 } else { 22 }),
    };
    v.extreme_bg_color = p.bg_subtle;
    v.faint_bg_color = p.bg_subtle;
    v.override_text_color = Some(p.text_primary);
    v.selection.bg_fill = p.bg_active;
    v.selection.stroke = Stroke::new(1.0_f32, p.text_primary);

    let r = Rounding::same(8.0);
    v.widgets.noninteractive.bg_fill = p.bg_canvas;
    v.widgets.noninteractive.weak_bg_fill = Color32::TRANSPARENT;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, p.border_soft);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, p.text_secondary);
    v.widgets.noninteractive.rounding = r;

    v.widgets.inactive.bg_fill = p.bg_subtle;
    v.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, p.border);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, p.text_primary);
    v.widgets.inactive.rounding = r;

    v.widgets.hovered.bg_fill = p.bg_hover;
    v.widgets.hovered.weak_bg_fill = p.bg_hover;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, p.border);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, p.text_primary);
    v.widgets.hovered.rounding = r;

    v.widgets.active.bg_fill = p.bg_active;
    v.widgets.active.weak_bg_fill = p.bg_active;
    v.widgets.active.bg_stroke = Stroke::new(1.0_f32, p.border);
    v.widgets.active.fg_stroke = Stroke::new(1.0_f32, p.text_primary);
    v.widgets.active.rounding = r;

    v.widgets.open.bg_fill = p.bg_active;
    v.widgets.open.weak_bg_fill = p.bg_active;
    v.widgets.open.bg_stroke = Stroke::new(1.0_f32, p.border);
    v.widgets.open.fg_stroke = Stroke::new(1.0_f32, p.text_primary);
    v.widgets.open.rounding = r;

    style.visuals = v;
    ctx.set_style(style);
    p
}

/// 将内部工具名映射为简体中文展示名
pub fn tool_label_zh(name: &str) -> &'static str {
    match name {
        "read" => "读取文件",
        "edit" => "修改文件",
        "multi_edit" => "批量修改",
        "write" => "写入文件",
        "bash" => "执行命令",
        "glob" => "查找文件",
        "grep" => "搜索代码",
        "ls" => "列出目录",
        "todo_write" => "更新待办",
        "memory" => "持久记忆",
        "skill_list" => "列出技能",
        "skill_view" => "查看技能",
        "skill_manage" => "管理技能",
        "session_search" => "检索历史",
        _ => "调用工具",
    }
}

/// 将外部底层报错翻译为可读的简体中文说明，并保留原始详情供展开查看
pub fn friendly_error_zh(raw: &str) -> (String, Option<String>) {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return (String::new(), None);
    }
    let lower = trimmed.to_lowercase();
    let is_external = lower.contains("http")
        || lower.contains("error")
        || lower.contains("status")
        || lower.contains("401")
        || lower.contains("403")
        || lower.contains("404")
        || lower.contains("429")
        || lower.contains("500")
        || lower.contains("502")
        || lower.contains("503")
        || lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("connection refused")
        || lower.contains("dns")
        || lower.contains("failed_precondition")
        || lower.contains("invalid_api_key")
        || lower.contains("context_length")
        || lower.contains("context window")
        || lower.contains("maximum context")
        || lower.contains("too many tokens")
        || lower.contains("prompt is too long");

    if !is_external && trimmed.chars().any(|ch| ('一'..='龥').contains(&ch)) {
        return (trimmed.to_string(), None);
    }

    let summary = if lower.contains("context_length")
        || lower.contains("context window")
        || lower.contains("maximum context")
        || lower.contains("too many tokens")
        || lower.contains("prompt is too long")
    {
        "当前会话上下文已达到该模型上限，程序已安全拦截；请新建会话或切换更大上下文的模型继续"
    } else if lower.contains("401") || lower.contains("unauthorized") || lower.contains("invalid_api_key") {
        "模型认证失败：请检查接口密钥是否有效或已过期"
    } else if lower.contains("403") || lower.contains("failed_precondition") || lower.contains("location is not supported") {
        "模型服务拒绝访问：当前账号权限或地区受限，请更换可用模型或接口"
    } else if lower.contains("404") || lower.contains("not found") || lower.contains("model_not_found") {
        "接口或模型不存在：请检查接口地址与模型名称是否填写正确"
    } else if lower.contains("429") || lower.contains("rate limit") || lower.contains("quota") {
        "请求过于频繁或额度不足：请稍后重试或切换其他模型"
    } else if lower.contains("connection refused") || lower.contains("tcp connect") || lower.contains("os error 10061") {
        "无法连接到模型服务：请确认本地 Ollama 或网关已启动且地址端口正确"
    } else if lower.contains("timeout") || lower.contains("timed out") {
        "请求超时：模型服务长时间未响应，请稍后重试"
    } else if lower.contains("500") || lower.contains("502") || lower.contains("503") || lower.contains("504") {
        "模型服务端暂时异常：请稍后重试或切换备用模型"
    } else {
        "模型请求或工具执行出现异常，请检查网络与模型配置"
    };
    (summary.to_string(), Some(trimmed.to_string()))
}
