//! 顶部横幅：崩溃、启动检查、配置重载/写回失败、他人线路状态、运行告警、管线错误。
//!
//! 从 `app::central` 拆出来的纯绘制：只读应用状态，「清除」按钮把对应字段置空。
//! 他人线路那条是**实时状态**（不可手动关闭，D40），其余是一次性提示（可清除）。
//!
//! 加载顺序固定：崩溃 → 启动检查 → 配置 → 他人线路 → 运行告警 → 管线错误。

use std::time::{Duration, Instant};

use eframe::egui;

use crate::app::BivoxApp;
use crate::events::OthersLineState;
use crate::gui::{theme, widgets};

/// 绘制全部顶部横幅（没有内容时不占位）。
pub fn show(app: &mut BivoxApp, ui: &mut egui::Ui) {
    if let Some(detail) = app.crash.clone() {
        widgets::banner(
            ui,
            "程序发生崩溃",
            &format!("{detail}（详情见 crash.log）"),
            theme::RED,
        );
    }
    if let Some(detail) = app.startup_error.clone() {
        widgets::banner(ui, "启动检查未通过", &detail, theme::AMBER);
    }
    if let Some(detail) = app.config_reload_error.clone() {
        widgets::banner(
            ui,
            "配置重新加载失败",
            &format!("{detail}（已保留当前运行配置；修复磁盘配置前无法写回）"),
            theme::RED,
        );
    }
    if let Some(detail) = app.config_write_error.clone() {
        widgets::banner(ui, "配置写回失败", &detail, theme::RED);
        if ui.button("清除错误").clicked() {
            app.config_write_error = None;
        }
    }
    others_status(app, ui);
    if let Some(detail) = app.pipeline_warning.clone() {
        widgets::banner(ui, "运行告警", &detail, theme::AMBER);
        if ui.button("清除告警").clicked() {
            app.pipeline_warning = None;
        }
    }
    if let Some(detail) = app.pipeline_error.clone() {
        widgets::banner(ui, "实时管线错误", &detail, theme::RED);
        if ui.button("清除错误").clicked() {
            app.pipeline_error = None;
        }
    }
}

/// 他人线路的实时状态横幅：`Waiting` 显示原因与重试倒计时，`Capturing` 时不显示。
fn others_status(app: &mut BivoxApp, ui: &mut egui::Ui) {
    let Some(status) = &app.others_status else {
        return;
    };
    let detail = status.detail.trim();
    let text = match status.state {
        OthersLineState::Capturing => return,
        OthersLineState::Connecting => {
            if detail.is_empty() {
                "正在连接…".to_string()
            } else {
                format!("正在连接…（{detail}）")
            }
        }
        OthersLineState::Waiting => {
            let seconds = app
                .others_retry_deadline
                .map(|deadline| retry_seconds(deadline, Instant::now()));
            match (detail.is_empty(), seconds) {
                (true, Some(seconds)) => format!("{seconds} 秒后重试"),
                (false, Some(seconds)) => format!("{detail}；{seconds} 秒后重试"),
                (true, None) => "等待重试".to_string(),
                (false, None) => detail.to_string(),
            }
        }
    };
    widgets::banner(ui, "他人语音", &text, theme::AMBER);
    // 倒计时每秒都在变：即使没有新事件也要让界面继续刷新。
    ui.ctx().request_repaint_after(Duration::from_millis(500));
}

/// 横幅倒计时：把「下一次重试时刻」换算成向上取整的秒数（已过期按 0）。
fn retry_seconds(deadline: Instant, now: Instant) -> u64 {
    deadline.saturating_duration_since(now).as_secs_f64().ceil() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_countdown_rounds_up() {
        let now = Instant::now();
        assert_eq!(retry_seconds(now + Duration::from_millis(1), now), 1);
        assert_eq!(retry_seconds(now + Duration::from_millis(1000), now), 1);
        assert_eq!(retry_seconds(now + Duration::from_millis(1001), now), 2);
        assert_eq!(retry_seconds(now, now), 0);
        assert_eq!(retry_seconds(now - Duration::from_secs(5), now), 0);
    }
}
