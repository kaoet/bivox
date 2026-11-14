//! 音频页：音频输入/输出设备与本地 VAD 参数。
//!
//! 设备规则（D12）：只保存**完整设备名**，启动时按名称精确匹配；设备改名或被拔掉时
//! 明确报错让用户重选，不做模糊匹配、也不静默回退到默认设备。
//!
//! VAD 参数保存后按 D20 热重启实时管线。

use eframe::egui;

use crate::app::{KaolsApp, Tab};
use crate::gui::widgets;

const RED: egui::Color32 = egui::Color32::from_rgb(0xe0, 0x4b, 0x4b);
const AMBER: egui::Color32 = egui::Color32::from_rgb(0xd8, 0x9a, 0x1b);

/// 从配置里取出当前设备设置（`None` = 系统默认）。
fn current_devices(app: &KaolsApp) -> (Option<String>, Option<String>) {
    (
        app.config.audio.input_device.clone(),
        app.config.audio.output_device.clone(),
    )
}

pub fn show(app: &mut KaolsApp, ui: &mut egui::Ui) {
    ui.heading("音频");
    ui.label(
        egui::RichText::new(
            "音频输入/输出设备只保存完整设备名，会话启动时按名称精确匹配；\
             设备改名或被拔掉会明确报错，不会静默回退。",
        )
        .weak(),
    );
    if let Some(error) = app.config_write_error_for(Tab::Audio) {
        ui.colored_label(RED, format!("保存失败：{error}"));
    }
    ui.add_space(8.0);

    let (configured_input, configured_output) = current_devices(app);

    let input_names = app.input_devices.clone();
    let output_names = app.output_devices.clone();
    let mut input = configured_input.clone();
    let mut output = configured_output.clone();

    let mut refreshed = false;
    let mut reset_requested = false;
    ui.horizontal(|ui| {
        widgets::device_combo(ui, "输入设备", &mut input, &input_names, "系统默认");
        widgets::device_combo(ui, "输出设备", &mut output, &output_names, "系统默认");
        refreshed = ui.button("刷新设备").clicked();
        reset_requested = ui.button("恢复系统默认").clicked();
    });

    if refreshed {
        app.refresh_devices();
    }
    if reset_requested {
        // 立刻写回并重新加载；本帧不再用旧值做差异比较。
        app.update_audio_devices(None, None);
        return;
    }

    let mut mismatch = Vec::new();
    if let Some(name) = &configured_input {
        if !input_names.contains(name) {
            mismatch.push(format!("配置的输入设备 {name:?} 不在当前设备列表中"));
        }
    }
    if let Some(name) = &configured_output {
        if !output_names.contains(name) {
            mismatch.push(format!("配置的输出设备 {name:?} 不在当前设备列表中"));
        }
    }
    for message in mismatch {
        ui.colored_label(
            egui::Color32::from_rgb(0xd8, 0x9a, 0x1b),
            format!("⚠ {message}（可能已改名或被拔掉）"),
        );
    }

    if input != configured_input || output != configured_output {
        app.update_audio_devices(input, output);
    }

    ui.add_space(10.0);
    show_vad_settings(app, ui);
}

/// 本地 Silero VAD 发送门控参数。
fn show_vad_settings(app: &mut KaolsApp, ui: &mut egui::Ui) {
    ui.separator();
    ui.label(egui::RichText::new("本地 VAD").strong());
    ui.checkbox(&mut app.vad_enabled, "启用本地 VAD（关闭后音频全量上传）");

    ui.add_enabled_ui(app.vad_enabled, |ui| {
        ui.horizontal(|ui| {
            ui.label("起音阈值");
            ui.add(
                egui::DragValue::new(&mut app.vad_threshold)
                    .range(0.01..=0.99)
                    .speed(0.01)
                    .max_decimals(2),
            );
            ui.label(egui::RichText::new("(0, 1)；低于 threshold - 0.15 视为静音").weak());
        });
        ui.horizontal(|ui| {
            ui.label("静音结束时长");
            ui.add(
                egui::DragValue::new(&mut app.vad_min_silence_ms)
                    .range(0..=5000)
                    .speed(10.0)
                    .suffix(" ms"),
            );
            ui.label(egui::RichText::new("低于负阈值持续多久判定说完").weak());
        });
        ui.horizontal(|ui| {
            ui.label("句末补发静音");
            ui.add(
                egui::DragValue::new(&mut app.vad_tail_ms)
                    .range(0..=5000)
                    .speed(10.0)
                    .suffix(" ms"),
            );
            ui.label(egui::RichText::new("用于触发服务端 sentence_end").weak());
        });
        ui.checkbox(&mut app.vad_debug, "打印每个 chunk 的 VAD 决策（排障用）");
    });

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let dirty = app.vad_settings_dirty();
        if ui
            .add_enabled(dirty, egui::Button::new("保存 VAD 设置"))
            .clicked()
        {
            app.save_vad_settings();
        }
        if ui
            .add_enabled(dirty, egui::Button::new("放弃更改"))
            .clicked()
        {
            app.reset_vad_settings();
        }
        if dirty {
            ui.label(egui::RichText::new("● 未保存").color(AMBER).small());
        }
    });
}
