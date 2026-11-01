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
