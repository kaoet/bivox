//! 设置页：百炼（API Key / 接入域名）、高级设置与整页保存。
//!
//! 本页像「语音合成」页一样使用 Tab 级「保存 / 放弃更改」：整页字段一起写回；
//! API Key 留空保存 = 清除该项。字段语义见 `src/config.rs`，写回走类型化
//! `AppConfig` + 最小化序列化。

use eframe::egui;

use crate::app::KaolsApp;

const RED: egui::Color32 = egui::Color32::from_rgb(0xe0, 0x4b, 0x4b);
const AMBER: egui::Color32 = egui::Color32::from_rgb(0xd8, 0x9a, 0x1b);

pub fn show(app: &mut KaolsApp, ui: &mut egui::Ui) {
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .id_salt("settings_scroll")
        .show(ui, |ui| {
            let mut save = false;
            let mut reset = false;
            ui.horizontal(|ui| {
                ui.heading("设置");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let dirty = app.settings_tab_dirty();
                    if ui.add_enabled(dirty, egui::Button::new("保存")).clicked() {
                        save = true;
                    }
                    if ui
                        .add_enabled(dirty, egui::Button::new("放弃更改"))
                        .clicked()
                    {
                        reset = true;
                    }
                    if dirty {
                        ui.label(egui::RichText::new("● 未保存").color(AMBER).small());
                    }
                });
            });
            if save {
                app.save_settings_tab();
            }
            if reset {
                app.reset_settings_tab();
            }
            if let Some(error) = &app.config_write_error {
                ui.colored_label(RED, format!("保存失败：{error}"));
            }
            if let Some(notice) = &app.api_key_notice {
                ui.colored_label(AMBER, notice);
            }
            ui.label(
                egui::RichText::new(
                    "只支持便携模式：配置、音色缓存、日志都放在程序（exe）所在目录，\
                     不读取 %APPDATA%、不读取当前工作目录。",
                )
                .weak(),
            );
            ui.add_space(8.0);

            show_bailian_section(app, ui);
            show_advanced_settings(app, ui);

            ui.add_space(12.0);
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("重新加载配置").clicked() {
                    app.reload_config();
                }
                if ui.button("打开程序目录").clicked() {
                    app.open_exe_dir();
                }
            });
        });
}

/// 百炼：API Key 与接入域名。
fn show_bailian_section(app: &mut KaolsApp, ui: &mut egui::Ui) {
    ui.label(egui::RichText::new("百炼").strong());
    if app.config.api.key.is_none() {
        ui.label(egui::RichText::new("当前未配置 API Key").weak());
    }

    ui.horizontal(|ui| {
        ui.label("API Key");
        ui.add(
            egui::TextEdit::singleline(&mut app.api_key_input)
                .password(true)
                .desired_width(420.0)
                .hint_text("sk-..."),
        );
    });
    ui.label(
        egui::RichText::new(
            "留空保存会清除 API Key；Key 以明文保存在 exe 同目录的 config.toml，\
             整个程序目录含密钥，请勿分享或上传。",
        )
        .weak(),
    );

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label("接入域名");
        ui.add(
            egui::TextEdit::singleline(&mut app.settings_workspace_host)
                .desired_width(420.0)
                .hint_text("dashscope.aliyuncs.com"),
        );
    });
    ui.label(
        egui::RichText::new("纯 host，不带 wss://、路径或端口；业务空间专属域名可覆盖默认值。")
            .weak(),
    );
}

/// 高级设置：缓存文件与分块词数。
fn show_advanced_settings(app: &mut KaolsApp, ui: &mut egui::Ui) {
    ui.add_space(8.0);
    ui.label(egui::RichText::new("高级设置").strong());

    ui.horizontal(|ui| {
        ui.label("缓存文件");
        ui.add(
            egui::TextEdit::singleline(&mut app.settings_cache_file)
                .desired_width(420.0)
                .hint_text("cache.json"),
        );
    });
    ui.label(
        egui::RichText::new(
            "相对 exe/配置文件目录，或绝对路径；不能为空。修改后旧缓存不会自动迁移。",
        )
        .weak(),
    );

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label("分块词数");
        ui.add(
            egui::DragValue::new(&mut app.settings_chunk_words)
                .range(1..=1000)
                .speed(1.0),
        );
        ui.label(egui::RichText::new("投机分块阈值，必须大于 0").weak());
    });
}
