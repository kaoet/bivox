//! 语音识别页：两条线路**共用**的识别模型与预编译热词（D22/D31/D32/D38）。
//!
//! 本页编辑 `[asr] model` 与 `[asr] hotwords`；语言列表属于各条线路，在「自己」/「他人」页。
//! 改动只写本页草稿，点右上角「保存」后由 `BivoxApp::save_asr_tab` 合并成一次
//! 类型化写回（一次校验 + 一份 `.bak`）；「放弃更改」恢复配置文件里的值。
//! 运行中保存会按 D20 重启实时管线、丢失当前句；热词列表在下一次会话启动时
//! 自动创建/更新为百炼预编译热词列表（D31）。
//!
//! 布局只用 `ui.horizontal` 与 `ScrollArea`，不用 [`egui::Grid`]：Grid 会触发
//! multi-pass，而 eframe 的 `__screenshot` 调试特性会抓到空帧。

use eframe::egui;

use crate::app::BivoxApp;
use crate::config::{DEFAULT_ASR_MODEL, MAX_ASR_HOTWORDS, normalize_hotwords};
use crate::gui::{theme, widgets};

pub fn show(app: &mut BivoxApp, ui: &mut egui::Ui) {
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .id_salt("asr_tab_scroll")
        .show(ui, |ui| show_content(app, ui));
}

fn show_content(app: &mut BivoxApp, ui: &mut egui::Ui) {
    // Tab 级操作：保存 / 放弃更改 作用于「模型 + 热词」
    let action = widgets::save_bar(ui, "语音识别", app.asr_tab_dirty());
    if action.save {
        app.save_asr_tab();
    }
    if action.reset {
        app.reset_asr_tab();
    }
    widgets::error_line(ui, app.asr_edit_error.as_deref());
    ui.label(
        egui::RichText::new(
            "本页只放两条线路共用的部分：识别模型与预编译热词。语言列表在「自己」「他人」页。\
             热词会在下次会话启动时自动创建/更新到百炼，首次创建可能需要几十秒。",
        )
        .weak(),
    );

    ui.add_space(8.0);
    show_model(app, ui);
    ui.add_space(10.0);
    show_hotwords(app, ui);
    ui.add_space(8.0);
}

/// 「识别模型」：单行输入框 + 恢复默认。
fn show_model(app: &mut BivoxApp, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.label("识别模型");
        ui.add(egui::TextEdit::singleline(&mut app.asr_model).desired_width(380.0));
        if ui.button("恢复默认").clicked() {
            app.asr_model = DEFAULT_ASR_MODEL.to_string();
        }
    });
    ui.label(
        egui::RichText::new(
            "模型需与百炼支持的实时识别模型一致；更换模型会重建对应的预编译热词列表。",
        )
        .weak(),
    );
}

/// 「预编译热词」：多行文本（一行一个）；标题右侧显示归一化后的条数 / 上限。
fn show_hotwords(app: &mut BivoxApp, ui: &mut egui::Ui) {
    let lines: Vec<String> = app.asr_hotwords.lines().map(str::to_string).collect();
    let count = normalize_hotwords(&lines).len();
    let over_limit = count > MAX_ASR_HOTWORDS;

    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("预编译热词").strong());
        let counter = egui::RichText::new(format!("({count}/{MAX_ASR_HOTWORDS})")).small();
        if over_limit {
            ui.colored_label(theme::RED, counter);
        } else {
            ui.label(counter.weak());
        }
    });
    ui.add(
        egui::TextEdit::multiline(&mut app.asr_hotwords)
            .desired_rows(6)
            .desired_width(f32::INFINITY)
            .hint_text("一行一个热词，例如：原神"),
    );
}
