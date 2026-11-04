//! 实时页底部的「打字输入」行：输入一句或多句文字，Enter 或「朗读」交给会话排队朗读。
//!
//! 排版：左侧多行输入框（默认 1 行，随内容长高；单次上限 500 字符，高度有界），
//! 右侧「朗读」按钮垂直居中；输入框下方只显示错误（不再显示常驻使用说明）。
//!
//! 多行输入与回车（egui 0.37）：
//! - 用 `TextEdit::multiline`；把 `return_key` 设为 **Shift+Enter**，它在输入框内部插入
//!   换行（保留撤销栈与光标行为）；单独 Enter 不被 TextEdit 处理，由本模块检测并提交。
//! - 提交判断必须排除带修饰键的 Enter（Shift/Ctrl/Alt/Cmd），否则 Shift+Enter 会同时换行+提交。
//! - 过滤 `repeat`：`key_pressed` 包含长按重复事件，否则按住 Enter 会连续朗读。
//! - Windows 上 IME 用 Enter 确认候选时，winit 会以 `NamedKey::Process` 上报，
//!   egui-winit 将其整条过滤掉，因此不会误提交；`ime_active` 只是跨平台兜底。
//! - 保存焦点判断用 `memory.has_focus(id)` 而不是 `response.has_focus()`：后者还要求
//!   整个窗口有 OS 焦点（egui_mcp 注入事件时恒为 false；真实键盘输入时两者等价）。

use eframe::egui;

use crate::app::KaolsApp;
use crate::events::SessionState;

/// 底部输入行的面板 id。
const PANEL_ID: &str = "typed_input_panel";
/// 「朗读」按钮占位宽度：按钮文字宽 + 左右 button padding + 与输入框的间距。
/// 默认字体下按钮约 42pt 宽，加 8pt 间距取 52；靠右对齐时几乎贴住右边缘。
const BUTTON_SLOT_WIDTH: f32 = 52.0;
/// 错误文字颜色。
const ERROR_COLOR: egui::Color32 = egui::Color32::from_rgb(0xe0, 0x4b, 0x4b);

pub fn show(app: &mut KaolsApp, ui: &mut egui::Ui) {
    let can_speak = app.session.is_some() && matches!(app.state, SessionState::Running);

    egui::Panel::bottom(PANEL_ID).show(ui, |ui| {
        ui.add_space(4.0);

        // 左侧输入框 + 右侧按钮；`horizontal` 默认垂直居中，输入框长高时按钮保持居中。
        let input_width = (ui.available_width() - BUTTON_SLOT_WIDTH).max(160.0);
        let mut clicked = false;
        let response = ui
            .horizontal(|ui| {
                let response = ui.add(
                    egui::TextEdit::multiline(&mut app.typed_input)
                        .id_salt("typed_input_field")
                        .return_key(egui::KeyboardShortcut::new(
                            egui::Modifiers::SHIFT,
                            egui::Key::Enter,
                        ))
                        .hint_text("输入要朗读的文字，Enter 朗读，Shift+Enter 换行")
                        .desired_width(input_width)
                        .desired_rows(1),
                );
                clicked = ui
                    .add_enabled(can_speak, egui::Button::new("朗读"))
                    .clicked();
                response
            })
            .inner;
        let has_focus = ui.memory(|memory| memory.has_focus(response.id));

        // 输入框下方只保留错误提示（使用说明已移除）。
        if let Some(error) = app.typed_input_error.as_deref() {
            ui.colored_label(ERROR_COLOR, error);
        }

        // 只有“无修饰键的 Enter”才提交；Shift+Enter 已由 TextEdit 处理成换行。
        let enter = ui.input(|input| {
            input.events.iter().any(|event| {
                matches!(
                    event,
                    egui::Event::Key {
                        key: egui::Key::Enter,
                        pressed: true,
                        repeat: false,
                        modifiers,
                        ..
                    } if !modifiers.shift
                        && !modifiers.ctrl
                        && !modifiers.alt
                        && !modifiers.command
                        && !modifiers.mac_cmd
                )
            })
        });
        let ime_active = ui.input(|input| {
            input
                .events
                .iter()
                .any(|event| matches!(event, egui::Event::Ime(_)))
        });
        if clicked || (has_focus && enter && !ime_active) {
            app.submit_typed_input();
        }
        ui.add_space(2.0);
    });
}
