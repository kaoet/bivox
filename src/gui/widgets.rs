//! 通用小部件：错误横幅、键值行等。

use eframe::egui;

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
