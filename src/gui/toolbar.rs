//! 顶部工具栏：会话状态、VAD 指示器、开始/停止。

use eframe::egui;

use crate::app::{ConfigState, KaolsApp};
use crate::events::SessionState;

/// 状态灯颜色。
fn state_color(state: SessionState) -> egui::Color32 {
    match state {
        SessionState::Idle => egui::Color32::GRAY,
        SessionState::Starting | SessionState::Stopping => {
            egui::Color32::from_rgb(0xd8, 0x9a, 0x1b)
        }
        SessionState::Running => egui::Color32::from_rgb(0x3c, 0xc9, 0x6f),
        SessionState::Failed => egui::Color32::from_rgb(0xe0, 0x4b, 0x4b),
    }
}

/// 本地 VAD 指示器（放在开始/停止按钮左侧）：
/// 绿点 = 当前检测到语音；灰点 = 静音 / 管线未运行 / VAD 已被配置关闭。
fn vad_indicator(app: &KaolsApp, ui: &mut egui::Ui) {
    let vad_enabled = matches!(&app.config, ConfigState::Loaded(config) if config.vad.enabled);
    let (color, state_text, hint) = if !vad_enabled {
        (
            egui::Color32::GRAY,
            "关闭",
            "本地 VAD 已关闭（config.toml 的 [vad] enabled = false，音频将全量上传）",
        )
    } else if app.session.is_none() {
        (
            egui::Color32::GRAY,
            "静音",
            "实时管线未运行，没有本地 VAD 状态",
        )
    } else if app.vad_active {
        (
            egui::Color32::from_rgb(0x3c, 0xc9, 0x6f),
            "激活",
            "检测到语音（本地 VAD 已激活，正在上传语音段）",
        )
    } else {
        (
            egui::Color32::GRAY,
            "静音",
            "未检测到语音（本地 VAD 未激活）",
        )
    };

    let response = ui.label(egui::RichText::new(format!("VAD ● {state_text}")).color(color));
    response.on_hover_text(hint);
}

pub fn show(app: &mut KaolsApp, ui: &mut egui::Ui) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.heading("kaols");
        ui.label(egui::RichText::new("超低延迟实时识别与合成").weak());
        ui.separator();

        ui.colored_label(state_color(app.state), format!("● {}", app.state.label()));
        ui.separator();

        let can_start = matches!(app.state, SessionState::Idle | SessionState::Failed);
        let can_stop = matches!(app.state, SessionState::Starting | SessionState::Running);

        // VAD 指示器：就在开始/停止按钮左边
        vad_indicator(app, ui);
        ui.add_space(4.0);

        if ui
            .add_enabled(can_start, egui::Button::new("开始"))
            .clicked()
        {
            let ctx = ui.ctx().clone();
            app.start_session(ctx);
        }
        if ui
            .add_enabled(can_stop, egui::Button::new("停止"))
            .clicked()
        {
            app.stop_session();
        }

        if matches!(app.config, ConfigState::Invalid { .. }) {
            ui.label(egui::RichText::new("配置无效，无法开始").weak());
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(egui::RichText::new(format!("v{}", env!("CARGO_PKG_VERSION"))).weak());
        });
    });
    ui.add_space(4.0);
}
