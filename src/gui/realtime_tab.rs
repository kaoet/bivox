//! 实时页：气泡时间线（D16/D39）。最新一条在最上面，越早越靠下。
//!
//! 本模块只负责列表：底部打字输入行、滚动区与空状态；单条消息的排版（左右分侧、宽度、
//! 原文/译文两行、外侧语言代码、分色）在 [`super::bubble`]。
//!
//! 布局：我方靠右、他人靠左；气泡宽度贴合内容、上限为可用宽度的一部分，长句在气泡内换行。

use eframe::egui;

use crate::app::BivoxApp;
use crate::gui::bubble;

pub fn show(app: &mut BivoxApp, ui: &mut egui::Ui) {
    // 底部输入行先占位（嵌套 Panel 从当前 Ui 底部切走输入行高度），剩余空间给气泡列表。
    super::typed_input::show(app, ui);
    show_timeline(app, ui);
}

/// 气泡列表：从底部输入行上方剩余的空间向下填满。
fn show_timeline(app: &mut BivoxApp, ui: &mut egui::Ui) {
    let show_source = app.translate_show_source();

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .id_salt("subtitle_list_scroll")
        .show(ui, |ui| {
            // ScrollArea 的内容 Ui 会继承外层 bottom_up 布局，必须显式改为 top_down，
            // 否则空状态提示等会贴到字幕区底部。
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                if app.timeline.messages().is_empty() {
                    ui.add_space(16.0);
                    ui.label(
                        egui::RichText::new("（点工具栏「开始」后，说话或打字即可看到字幕）")
                            .weak(),
                    );
                    return;
                }

                // 时间线本身就是「最新在前」，直接按顺序渲染。
                for (index, message) in app.timeline.messages().iter().enumerate() {
                    bubble::show(ui, message, show_source, index);
                }
            });
        });
}
