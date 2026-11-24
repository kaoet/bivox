//! 通用小部件：错误横幅、页签保存栏、键值行、设备/方言下拉等。

use eframe::egui;

use crate::gui::theme;
use crate::language::CHINESE_DIALECTS;

/// 页签「保存 / 放弃更改」的点击结果。
///
/// 由 [`save_bar`] / [`save_row`] 返回、调用方读它去改 `BivoxApp`：绘制闭包里只收集
/// 意图，不在闭包内同时可变借用整个 `app`。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TabAction {
    pub save: bool,
    pub reset: bool,
}

/// 页签标题行：标题 + 右对齐的「保存 / 放弃更改」+「● 未保存」。
///
/// 只在有改动时按钮可用（`dirty`）。
pub fn save_bar(ui: &mut egui::Ui, title: &str, dirty: bool) -> TabAction {
    let mut action = TabAction::default();
    ui.horizontal(|ui| {
        ui.heading(title);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            action = save_buttons(ui, "保存", dirty);
            dirty_mark(ui, dirty);
        });
    });
    action
}

/// 页内小节的「保存 / 放弃更改」按钮行（无标题，如「保存 VAD 设置」）。
pub fn save_row(ui: &mut egui::Ui, save_label: &str, dirty: bool) -> TabAction {
    let mut action = TabAction::default();
    ui.horizontal(|ui| {
        action = save_buttons(ui, save_label, dirty);
        dirty_mark(ui, dirty);
    });
    action
}

/// 「● 未保存」小字提示（无改动时不占位）。
pub fn dirty_mark(ui: &mut egui::Ui, dirty: bool) {
    if dirty {
        ui.label(egui::RichText::new("● 未保存").color(theme::AMBER).small());
    }
}

/// 页内「保存失败：…」红字；`None` 时不占位。
pub fn error_line(ui: &mut egui::Ui, error: Option<&str>) {
    if let Some(error) = error {
        ui.colored_label(theme::RED, format!("保存失败：{error}"));
    }
}

/// 「保存 + 放弃更改」两个按钮（右向左布局时「保存」在最右）。
fn save_buttons(ui: &mut egui::Ui, save_label: &str, dirty: bool) -> TabAction {
    let mut action = TabAction::default();
    if ui
        .add_enabled(dirty, egui::Button::new(save_label))
        .clicked()
    {
        action.save = true;
    }
    if ui
        .add_enabled(dirty, egui::Button::new("放弃更改"))
        .clicked()
    {
        action.reset = true;
    }
    action
}

/// 顶部错误横幅（启动检查失败、崩溃信息都走这里）。
pub fn banner(ui: &mut egui::Ui, title: &str, detail: &str, color: egui::Color32) {
    ui.group(|ui| {
        ui.horizontal_wrapped(|ui| {
            ui.colored_label(color, egui::RichText::new(format!("⚠ {title}")).strong());
            ui.label(egui::RichText::new(detail).color(color));
        });
    });
    ui.add_space(4.0);
}

/// 一行「标签：值」。
///
/// 特意不用 [`egui::Grid`]：Grid 会触发 `request_discard`（multi-pass），
/// 而 eframe 的 `__screenshot` 调试特性在 multi-pass 下会抓到空帧，影响截图核对。
pub fn kv_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal(|ui| {
        let height = ui.spacing().interact_size.y;
        ui.add_sized([104.0, height], egui::Label::new(label));
        ui.monospace(value);
    });
}

/// 一行「标签：路径」，等宽字体方便复制。
pub fn path_row(ui: &mut egui::Ui, label: &str, path: &std::path::Path) {
    kv_row(ui, label, &path.display().to_string());
}

/// 设备下拉：`None` = 系统默认；选项来自设备枚举结果（D12，完整名精确匹配）。
///
/// 用 `from_id_salt` + 单独的 label，避免 0.37 的 `ComboBox::from_label` 把标签画在下拉框右侧。
pub fn device_combo(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut Option<String>,
    names: &[String],
    default_label: &str,
) {
    ui.label(label);
    egui::ComboBox::from_id_salt(label)
        .selected_text(value.as_deref().unwrap_or(default_label))
        .width(280.0)
        .show_ui(ui, |ui| {
            ui.selectable_value(value, None, default_label);
            for name in names {
                ui.selectable_value(value, Some(name.clone()), name.as_str());
            }
        });
}

/// 中文方言下拉：选项来自 [`CHINESE_DIALECTS`]（首个为默认「普通话」）。
///
/// 与 [`device_combo`] 一样用 `from_id_salt` + 单独 label，避免 0.37 的
/// `ComboBox::from_label` 把标签画在下拉框右侧。
pub fn dialect_combo(ui: &mut egui::Ui, label: &str, value: &mut String) {
    ui.label(label);
    egui::ComboBox::from_id_salt(label)
        .selected_text(value.as_str())
        .width(120.0)
        .show_ui(ui, |ui| {
            for dialect in CHINESE_DIALECTS {
                ui.selectable_value(value, (*dialect).to_string(), *dialect);
            }
        });
}
