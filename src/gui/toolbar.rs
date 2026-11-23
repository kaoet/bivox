//! 顶部工具栏：麦克风/他人侦听快捷开关与电平条、会话状态、开始/停止（D43）。
//!
//! 顺序固定为「音量开关和音量条 → 流水线状态 → 开始/停止」；版本号仍靠右。
//! VAD 指示已合并进电平条配色，不再单独占一段文字。

use eframe::egui;

use crate::app::BivoxApp;
use crate::events::SessionState;
use crate::gui::meter::{self, AudioIcon, MeterSpec, VadVisual};

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

/// 由「线路开关 + VAD 配置/瞬时状态」推导电平条配色。
fn vad_visual(on: bool, vad_enabled: bool, degraded: bool, active: bool) -> VadVisual {
    if !on {
        // 线路关闭时电平条不绘制，这里的值只用于 tooltip 的占位。
        VadVisual::Idle
    } else if !vad_enabled {
        VadVisual::Disabled
    } else if degraded {
        VadVisual::Degraded
    } else if active {
        VadVisual::Active
    } else {
        VadVisual::Idle
    }
}

pub fn show(app: &mut BivoxApp, ui: &mut egui::Ui) {
    ui.add_space(4.0);

    let mut toggle_mic = false;
    let mut toggle_others = false;
    let mut start = false;
    let mut stop = false;

    let state = app.state;
    let session_running = app.session.is_some();
    let vad_enabled = app.config.vad.enabled;
    let mic_degraded = app
        .session
        .as_ref()
        .is_some_and(|session| session.vad_degraded());
    let others_degraded = app
        .session
        .as_ref()
        .is_some_and(|session| session.vad_degraded_others());
    let mic_on = app.config.local.mic;
    let others_on = app.config.others.enabled;
    let mic_spec = MeterSpec {
        on: mic_on,
        level: app.mic_level,
        vad: vad_visual(mic_on, vad_enabled, mic_degraded, app.vad_active),
        session_running,
    };
    let others_spec = MeterSpec {
        on: others_on,
        level: app.others_level,
        vad: vad_visual(
            others_on,
            vad_enabled,
            others_degraded,
            app.vad_active_others,
        ),
        session_running,
    };
    let can_start = matches!(state, SessionState::Idle | SessionState::Failed);
    let can_stop = matches!(state, SessionState::Starting | SessionState::Running);

    ui.horizontal(|ui| {
        if meter::audio_group(ui, AudioIcon::Mic, mic_spec, "麦克风") {
            toggle_mic = true;
        }
        ui.add_space(14.0);
        if meter::audio_group(ui, AudioIcon::Listen, others_spec, "他人侦听") {
            toggle_others = true;
        }
        ui.separator();

        ui.colored_label(state_color(state), format!("● {}", state.label()));
        ui.separator();

        if ui
            .add_enabled(can_start, egui::Button::new("开始"))
            .clicked()
        {
            start = true;
        }
        if ui
            .add_enabled(can_stop, egui::Button::new("停止"))
            .clicked()
        {
            stop = true;
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(egui::RichText::new(format!("v{}", env!("CARGO_PKG_VERSION"))).weak());
        });
    });

    // 点击后再改状态：避免在绘制闭包里同时可变/不可变借用 app。
    if toggle_mic {
        app.set_local_mic(!mic_on);
    }
    if toggle_others {
        app.set_others_enabled(!others_on);
    }
    if start {
        let ctx = ui.ctx().clone();
        app.start_session(ctx);
    }
    if stop {
        app.stop_session();
    }

    ui.add_space(4.0);
}
