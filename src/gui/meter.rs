//! 顶栏音频控件：图标开关 + 分段电平条（D43）。
//!
//! VAD 指示与电平条合并，不再单独占一段文字：
//!
//! | 线路 | 显示 |
//! |---|---|
//! | 关闭 | 保留 100×14 空位，不画任何段 |
//! | 开启 + VAD 功能关闭 | 彩色段（全量上传） |
//! | 开启 + VAD 开启 + 无语音 | 灰阶段（按主题取黑/白亮段） |
//! | 开启 + VAD 开启 + 有语音 | 彩色段 |
//! | 开启 + VAD 推理降级 | 彩色段（等同全量上传） |
//!
//! 颜色只是主提示；tooltip 同时给出「VAD 关闭 / 未检测到语音 / 检测到语音」和 dBFS。

use eframe::egui;

use crate::audio::meter::percent_to_dbfs;

/// 一条电平条的段数。
pub const BAR_SEGMENTS: usize = 20;
const BAR_WIDTH: f32 = 100.0;
const BAR_HEIGHT: f32 = 14.0;
const SEG_WIDTH: f32 = 3.4;
const SEG_GAP: f32 = 1.6;

const GREEN: egui::Color32 = egui::Color32::from_rgb(0x3c, 0xc9, 0x6f);
const AMBER: egui::Color32 = egui::Color32::from_rgb(0xd8, 0x9a, 0x1b);
const RED: egui::Color32 = egui::Color32::from_rgb(0xe0, 0x4b, 0x4b);

/// 一条线路的 VAD 视觉状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VadVisual {
    /// VAD 功能关闭：全量上传，彩色。
    Disabled,
    /// VAD 开启但当前没有语音：灰阶。
    Idle,
    /// VAD 开启且检测到语音：彩色。
    Active,
    /// VAD 推理失败已降级：全量上传，彩色。
    Degraded,
}

impl VadVisual {
    fn colored(self) -> bool {
        !matches!(self, Self::Idle)
    }

    fn label(self) -> &'static str {
        match self {
            Self::Disabled => "VAD 功能关闭（全量上传）",
            Self::Idle => "VAD 开启 · 未检测到语音",
            Self::Active => "VAD 开启 · 检测到语音",
            Self::Degraded => "VAD 推理失败，已降级为全量上传",
        }
    }
}

/// 一条线路的完整显示参数。
#[derive(Debug, Clone, Copy)]
pub struct MeterSpec {
    pub on: bool,
    pub level: f32,
    pub vad: VadVisual,
    /// 会话是否在运行；只影响 tooltip 文案。
    pub session_running: bool,
}

#[derive(Clone, Copy)]
pub enum AudioIcon {
    Mic,
    Listen,
}

fn draw_mic(painter: &egui::Painter, center: egui::Pos2, color: egui::Color32, on: bool) {
    let stroke = egui::Stroke::new(1.7, color);
    painter.rect_filled(
        egui::Rect::from_center_size(center + egui::vec2(0.0, -2.2), egui::vec2(7.0, 11.0)),
        egui::CornerRadius::same(4),
        color,
    );
    let mut arc = Vec::new();
    for i in 0..=16 {
        let angle = std::f32::consts::PI * (i as f32 / 16.0);
        arc.push(center + egui::vec2(angle.cos() * 6.0, angle.sin() * 6.0));
    }
    painter.add(egui::Shape::line(arc, stroke));
    painter.line_segment(
        [center + egui::vec2(0.0, 6.0), center + egui::vec2(0.0, 9.0)],
        stroke,
    );
    painter.line_segment(
        [
            center + egui::vec2(-3.2, 9.0),
            center + egui::vec2(3.2, 9.0),
        ],
        stroke,
    );
    if !on {
        painter.line_segment(
            [
                center + egui::vec2(-7.0, -9.0),
                center + egui::vec2(7.0, 9.0),
            ],
            egui::Stroke::new(2.0, color),
        );
    }
}

fn draw_speaker(painter: &egui::Painter, center: egui::Pos2, color: egui::Color32, on: bool) {
    let stroke = egui::Stroke::new(1.6, color);
    painter.rect_filled(
        egui::Rect::from_min_max(
            center + egui::vec2(-8.5, -3.4),
            center + egui::vec2(-4.0, 3.4),
        ),
        egui::CornerRadius::same(1),
        color,
    );
    painter.add(egui::Shape::convex_polygon(
        vec![
            center + egui::vec2(-4.0, -3.4),
            center + egui::vec2(1.2, -8.2),
            center + egui::vec2(1.2, 8.2),
            center + egui::vec2(-4.0, 3.4),
        ],
        color,
        egui::Stroke::NONE,
    ));
    if on {
        for radius in [3.6_f32, 6.4] {
            let mut arc = Vec::new();
            for i in 0..=10 {
                let angle = -0.9 + 1.8 * (i as f32 / 10.0);
                arc.push(center + egui::vec2(radius * angle.cos(), radius * angle.sin()));
            }
            painter.add(egui::Shape::line(arc, stroke));
        }
    } else {
        let c = center + egui::vec2(5.2, 0.0);
        painter.line_segment(
            [c + egui::vec2(-2.6, -2.6), c + egui::vec2(2.6, 2.6)],
            egui::Stroke::new(1.8, color),
        );
        painter.line_segment(
            [c + egui::vec2(-2.6, 2.6), c + egui::vec2(2.6, -2.6)],
            egui::Stroke::new(1.8, color),
        );
    }
}

fn icon_button(
    ui: &mut egui::Ui,
    icon: AudioIcon,
    on: bool,
    name: &str,
    session_running: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(24.0, 22.0), egui::Sense::click());
    let visuals = ui.visuals().clone();
    if response.hovered() {
        ui.painter().rect_filled(
            rect.expand(2.0),
            egui::CornerRadius::same(4),
            visuals.widgets.hovered.bg_fill,
        );
    }
    let color = if on {
        visuals.text_color()
    } else {
        visuals.weak_text_color()
    };
    match icon {
        AudioIcon::Mic => draw_mic(ui.painter(), rect.center(), color, on),
        AudioIcon::Listen => draw_speaker(ui.painter(), rect.center(), color, on),
    }
    response
        .widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, on, name.to_string()));
    let hint = if session_running {
        if on {
            format!("{name}：开 · 点击立即关闭")
        } else {
            format!("{name}：关 · 点击立即开启")
        }
    } else if on {
        format!("{name}：开 · 会话未运行，将在下次开始时生效")
    } else {
        format!("{name}：关 · 点击立即开启（下次开始时生效）")
    };
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(hint)
}

fn segment_color(index: usize) -> egui::Color32 {
    if index < 14 {
        GREEN
    } else if index < 18 {
        AMBER
    } else {
        RED
    }
}

fn segment_bar(ui: &mut egui::Ui, spec: MeterSpec, name: &str) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(BAR_WIDTH, BAR_HEIGHT), egui::Sense::hover());
    let visuals = ui.visuals().clone();
    if spec.on {
        let unlit = if visuals.dark_mode {
            egui::Color32::from_rgb(0x2b, 0x30, 0x38)
        } else {
            egui::Color32::from_rgb(0xc2, 0xc7, 0xcf)
        };
        let mono_on = visuals.text_color();
        let lit_count = (spec.level.clamp(0.0, 1.0) * BAR_SEGMENTS as f32).round() as usize;
        let total = BAR_SEGMENTS as f32 * SEG_WIDTH + (BAR_SEGMENTS - 1) as f32 * SEG_GAP;
        let x0 = rect.center().x - total / 2.0;
        let y0 = rect.center().y - BAR_HEIGHT / 2.0;
        let painter = ui.painter();
        for i in 0..BAR_SEGMENTS {
            let x = x0 + i as f32 * (SEG_WIDTH + SEG_GAP);
            let seg =
                egui::Rect::from_min_size(egui::pos2(x, y0), egui::vec2(SEG_WIDTH, BAR_HEIGHT));
            let lit = i < lit_count;
            let color = if !lit {
                unlit
            } else if spec.vad.colored() {
                segment_color(i)
            } else {
                mono_on
            };
            painter.rect_filled(seg, egui::CornerRadius::same(1), color);
        }
    }

    response
        .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, format!("{name}电平")));
    response.on_hover_text(meter_hint(spec, name))
}

fn meter_hint(spec: MeterSpec, name: &str) -> String {
    if !spec.on {
        return format!("{name}：线路关闭，音量条隐藏");
    }
    let prefix = if spec.session_running {
        ""
    } else {
        "会话未运行 · "
    };
    let db = percent_to_dbfs(spec.level)
        .map(|db| format!("{db:.1} dBFS"))
        .unwrap_or_else(|| "无输入".to_string());
    format!("{name}：{prefix}{} · {db}", spec.vad.label())
}

/// 画一组「图标开关 + 电平条」，返回图标是否被点击。
pub fn audio_group(ui: &mut egui::Ui, icon: AudioIcon, spec: MeterSpec, name: &str) -> bool {
    let clicked = icon_button(ui, icon, spec.on, name, spec.session_running).clicked();
    ui.add_space(6.0);
    segment_bar(ui, spec, name);
    clicked
}
