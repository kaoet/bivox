//! 音频页：音频输入/输出设备等音频相关设置。
//!
//! 设备规则（D12）：只保存**完整设备名**，启动时按名称精确匹配；设备改名或被拔掉时
//! 明确报错让用户重选，不做模糊匹配、也不静默回退到默认设备。
//!
//! 更大的 VAD / 采样等参数目前仍只支持手改 `config.toml`，后续再搬到本页。

use eframe::egui;

use crate::app::{ConfigState, KaolsApp};
use crate::gui::widgets;

/// 从配置里取出当前设备设置（`None` = 系统默认）。
fn current_devices(app: &KaolsApp) -> Option<(Option<String>, Option<String>)> {
    match &app.config {
        ConfigState::Loaded(config) => Some((
            config.audio.input_device.clone(),
            config.audio.output_device.clone(),
        )),
        ConfigState::Invalid { .. } => None,
    }
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
    ui.add_space(8.0);

    let Some((configured_input, configured_output)) = current_devices(app) else {
        ui.colored_label(
            egui::Color32::from_rgb(0xe0, 0x4b, 0x4b),
            "配置无效，无法保存设备选择；请先到「设置」页填写 API Key。",
        );
        return;
    };

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
    ui.label(
        egui::RichText::new(
            "VAD、采样率等参数暂未提供图形化编辑，可在 exe 同目录的 config.toml 中修改后点「设置 → 重新加载配置」。",
        )
        .weak(),
    );
}
