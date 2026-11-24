//! GUI 通用配色：错误/告警/正常/提示。
//!
//! 颜色是跨页签的既有约定（错误条、未保存提示、状态灯、电平条、日志级别），
//! 集中在这里避免每个页签各写一份 RGB 字面量——改配色只改这一处。

use eframe::egui;

/// 错误（红）：保存失败、管线错误、崩溃、日志 `ERROR`、会话 `Failed`。
pub const RED: egui::Color32 = egui::Color32::from_rgb(0xe0, 0x4b, 0x4b);

/// 告警（琥珀）：运行告警、启动横幅、「● 未保存」、设备不匹配、日志 `WARN`。
pub const AMBER: egui::Color32 = egui::Color32::from_rgb(0xd8, 0x9a, 0x1b);

/// 正常（绿）：会话运行中、电平条安全段。
pub const GREEN: egui::Color32 = egui::Color32::from_rgb(0x3c, 0xc9, 0x6f);

/// 提示（橙）：语言数量等非致命提醒。
pub const ORANGE: egui::Color32 = egui::Color32::from_rgb(0xe6, 0x82, 0x00);
