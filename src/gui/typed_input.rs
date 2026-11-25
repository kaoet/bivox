//! 实时页底部的「打字输入」行：输入一句或多句文字，Enter 或「朗读」交给会话排队朗读。
//!
//! 排版：左侧多行输入框（默认 1 行，超过约 4 行后在框内滚动），右侧「朗读」按钮垂直居中；
//! 输入框下方只显示错误（不再显示常驻使用说明）。
//!
//! 输入框高度：先用与 `TextEdit` 相同的字体/宽度预排版量出内容高度，再夹到
//! [`MIN_VISIBLE_ROWS`]..[`MAX_VISIBLE_ROWS`] 行之间，并交给一个固定高度的
//! `ScrollArea`：这样默认 1 行、内容变多时向上长高、超过上限后内部滚动，
//! 同时避免 `ScrollArea` 默认 `min_scrolled_size` 带来的空白。
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

use crate::app::BivoxApp;
use crate::events::SessionState;
use crate::gui::theme;

/// 底部输入行的面板 id。
const PANEL_ID: &str = "typed_input_panel";
/// 「朗读」按钮占位宽度：按钮文字宽 + 左右 button padding + 与输入框的间距。
/// 默认字体下按钮约 42pt 宽，加 8pt 间距取 52；靠右对齐时几乎贴住右边缘。
const BUTTON_SLOT_WIDTH: f32 = 52.0;
/// `TextEdit` 的内边距（显式指定，保证测量宽度与内部排版宽度一致）。
const TEXT_EDIT_MARGIN: i8 = 4;
/// 输入框至少显示的行数（空输入时的高度）。
const MIN_VISIBLE_ROWS: f32 = 1.0;
/// 输入框最多显示的行数，超出后在框内滚动。
const MAX_VISIBLE_ROWS: f32 = 4.0;
/// 高度余量：吸收 TextEdit 边框/描边与纯文本排版之间的 1~2px 差异，避免单行时误出滚动条。
const HEIGHT_SLACK: f32 = 2.0;

/// 按当前文本计算输入框应占的高度（1~4 行，含上下 margin）。
fn measured_input_height(ui: &egui::Ui, text: &str, outer_width: f32) -> f32 {
    let font_id = egui::TextStyle::Body.resolve(ui.style());
    let line_height =
        ui.fonts_mut(|fonts| fonts.row_height(&font_id)) + ui.spacing().extra_text_line_spacing;
    let margin = f32::from(TEXT_EDIT_MARGIN);
    let min_height = MIN_VISIBLE_ROWS * line_height + 2.0 * margin;
    let max_height = MAX_VISIBLE_ROWS * line_height + 2.0 * margin;
    let wrap_width = (outer_width - 2.0 * margin).max(1.0);

    let galley = ui.fonts_mut(|fonts| {
        fonts.layout(
            text.to_owned(),
            font_id,
            egui::Color32::PLACEHOLDER,
            wrap_width,
        )
    });
    (galley.size().y + 2.0 * margin + HEIGHT_SLACK).clamp(min_height, max_height)
}

pub fn show(app: &mut BivoxApp, ui: &mut egui::Ui) {
    let can_speak = app.session.is_some() && matches!(app.state, SessionState::Running);

    egui::Panel::bottom(PANEL_ID).show(ui, |ui| {
        ui.add_space(4.0);

        // 先算这一帧输入框应有的高度：默认 1 行，随内容长高，超过 4 行封顶。
        let input_width = (ui.available_width() - BUTTON_SLOT_WIDTH).max(160.0);
        let input_height = measured_input_height(ui, &app.typed_input, input_width);

        // 左侧输入框 + 右侧按钮；`horizontal` 默认垂直居中，输入框长高时按钮保持居中。
        let mut clicked = false;
        let response = ui
            .horizontal(|ui| {
                let response = ui
                    .allocate_ui_with_layout(
                        egui::vec2(input_width, input_height),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            egui::ScrollArea::vertical()
                                .id_salt("typed_input_scroll")
                                // 默认 min_scrolled_size 是 64，会把单行输入撑高；这里必须关掉。
                                .min_scrolled_height(0.0)
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    ui.add(
                                        egui::TextEdit::multiline(&mut app.typed_input)
                                            .id_salt("typed_input_field")
                                            .margin(egui::Margin::same(TEXT_EDIT_MARGIN))
                                            .return_key(egui::KeyboardShortcut::new(
                                                egui::Modifiers::SHIFT,
                                                egui::Key::Enter,
                                            ))
                                            .hint_text(
                                                "输入要朗读的文字，Enter 朗读，Shift+Enter 换行",
                                            )
                                            .desired_width(f32::INFINITY)
                                            .desired_rows(MIN_VISIBLE_ROWS as usize),
                                    )
                                })
                                .inner
                        },
                    )
                    .inner;
                clicked = ui
                    .add_enabled(can_speak, egui::Button::new("朗读"))
                    .clicked();
                response
            })
            .inner;
        let has_focus = ui.memory(|memory| memory.has_focus(response.id));

        if let Some(error) = app.typed_input_error.as_deref() {
            ui.colored_label(theme::RED, error);
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
