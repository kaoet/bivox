//! 日志与崩溃信息收集（决策 D7/D8，见 docs/decisions.md）。
//!
//! 只做两件事：
//! 1. 把日志同时写进内存队列（界面每帧取走显示）和按天文件（`logs\bivox-YYYY-MM-DD.log`）；
//! 2. 提供 panic 钩子，把崩溃信息写到 `crash.log` 并交给界面显示。
//!
//! 这里不持有 `egui::Context`：接入后台任务后，由发日志的一方决定是否调用
//! `ctx.request_repaint()`，避免日志模块反向依赖界面。

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use chrono::Local;

/// 界面日志面板保留的最大行数（D16）。
pub const MAX_LOG_LINES: usize = 5000;

/// 按天日志文件保留天数（含当天）。
pub const LOG_RETENTION_DAYS: i64 = 7;

/// 删除超过保留期的按天日志文件（`bivox-YYYY-MM-DD.log`），返回删除数量。
///
/// 只处理本程序命名的文件；其它文件（用户放的说明、`.bak` 等）一律不动。
pub fn cleanup_old_logs(log_dir: &Path, today: chrono::NaiveDate) -> usize {
    let cutoff = today - chrono::Duration::days(LOG_RETENTION_DAYS - 1);
    let Ok(entries) = fs::read_dir(log_dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(date_part) = name
            .strip_prefix("bivox-")
            .and_then(|rest| rest.strip_suffix(".log"))
        else {
            continue;
        };
        let Ok(date) = chrono::NaiveDate::parse_from_str(date_part, "%Y-%m-%d") else {
            continue;
        };
        if date < cutoff {
            // best-effort: 清理失败不影响启动，下次启动再试
            if fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
    }
    removed
}

/// 一条待展示的日志。
#[derive(Debug, Clone)]
pub struct LogLine {
    /// 本地时间，`HH:MM:SS.mmm`
    pub time: String,
    pub level: log::Level,
    pub message: String,
}

struct Sink {
    pending: Mutex<VecDeque<LogLine>>,
    file: Mutex<Option<File>>,
}

static SINK: OnceLock<Sink> = OnceLock::new();
static CRASH: Mutex<Option<String>> = Mutex::new(None);

/// 初始化日志系统：建目录、按天打开文件、注册全局 logger。
///
/// 目录/文件不可写时降级为"只有内存日志"，不阻止程序启动——界面上的日志面板仍然可用，
/// 启动检查产生的错误也会通过 `startup_error` 显示出来。
pub fn init(log_dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(log_dir)?;

    let path = today_log_path(log_dir);
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok();

    let sink = Sink {
        pending: Mutex::new(VecDeque::new()),
        file: Mutex::new(file),
    };

    // 重复初始化（例如测试里）保留第一次的 logger。
    if SINK.set(sink).is_err() {
        return Ok(());
    }

    if log::set_logger(SINK.get().expect("SINK 刚刚 set 成功")).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
    Ok(())
}

/// 当天日志文件路径。
pub fn today_log_path(log_dir: &Path) -> PathBuf {
    let today = Local::now().format("%Y-%m-%d").to_string();
    log_dir.join(format!("bivox-{today}.log"))
}

/// 取走累计的待展示日志（界面每帧调用一次）。
pub fn take_lines() -> Vec<LogLine> {
    let Some(sink) = SINK.get() else {
        return Vec::new();
    };
    let Ok(mut pending) = sink.pending.lock() else {
        return Vec::new();
    };
    pending.drain(..).collect()
}

/// 记录一次崩溃信息，供界面下一帧显示红色错误条。
pub fn set_crash(message: String) {
    if let Ok(mut guard) = CRASH.lock() {
        *guard = Some(message);
    }
}

/// 取走崩溃信息（显示过一次就清除）。
pub fn take_crash() -> Option<String> {
    CRASH.lock().ok().and_then(|mut guard| guard.take())
}

/// 尽力把日志刷到磁盘（退出前调用）。
pub fn flush() {
    if let Some(sink) = SINK.get() {
        log::Log::flush(sink);
    }
}

/// 安装 panic 钩子：写 `crash.log`（含 backtrace）+ 把信息交给界面显示。
///
/// 在 release（无控制台）构建里，这是唯一能看到 panic 原因的途径，所以必须尽早安装。
pub fn install_panic_hook(crash_log: PathBuf) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = info.payload();
        let text = if let Some(text) = payload.downcast_ref::<&str>() {
            (*text).to_string()
        } else if let Some(text) = payload.downcast_ref::<String>() {
            text.clone()
        } else {
            "未知 panic（payload 不是字符串）".to_string()
        };
        let location = info
            .location()
            .map(|loc| format!("{}:{}:{}", loc.file(), loc.line(), loc.column()))
            .unwrap_or_else(|| "未知位置".to_string());
        let backtrace = std::backtrace::Backtrace::force_capture();

        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&crash_log)
        {
            // best-effort: 崩溃信息落盘失败时仍继续走默认钩子（stderr）
            let _ = writeln!(
                file,
                "===== {} =====\n{text}\n位置: {location}\n{backtrace}\n",
                Local::now().format("%Y-%m-%d %H:%M:%S")
            );
        }
        set_crash(format!("{text}（{location}）"));

        previous(info);
    }));
}

/// 去掉 ANSI 颜色/控制序列（`colored` 会把 `\x1b[36m...\x1b[0m` 写进日志字符串）。
///
/// GUI 与日志文件都不支持终端颜色，保留转义码只会显示成 `[36m` 之类的乱码。
fn strip_ansi(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            output.push(ch);
            continue;
        }
        // CSI：ESC '[' 参数... 终止字节（@ ..= ~）
        if chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
        }
    }
    output
}

impl log::Log for Sink {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }

        let now = Local::now();
        // `colored` 生成的 ANSI 颜色码在 GUI/日志文件里没有意义，会出现 `[36m` 之类的乱码
        let message = strip_ansi(&record.args().to_string());

        if let Ok(mut guard) = self.file.lock() {
            if let Some(file) = guard.as_mut() {
                // best-effort: 日志写盘失败不应影响主流程
                let _ = writeln!(
                    file,
                    "[{}] {:5} {}",
                    now.format("%Y-%m-%d %H:%M:%S%.3f"),
                    record.level(),
                    message
                );
                let _ = file.flush();
            }
        }

        if let Ok(mut pending) = self.pending.lock() {
            if pending.len() >= MAX_LOG_LINES {
                pending.pop_front();
            }
            pending.push_back(LogLine {
                time: now.format("%H:%M:%S%.3f").to_string(),
                level: record.level(),
                message,
            });
        }
    }

    fn flush(&self) {
        if let Ok(mut guard) = self.file.lock() {
            if let Some(file) = guard.as_mut() {
                // best-effort: 退出前尽力把日志刷到磁盘
                let _ = file.flush();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_ansi_removes_color_codes() {
        assert_eq!(
            strip_ansi("\u{1b}[36mqwen-audio\u{1b}[0m 创建"),
            "qwen-audio 创建"
        );
        assert_eq!(strip_ansi("普通文本"), "普通文本");
    }

    #[test]
    fn strip_ansi_keeps_unicode_and_emoji() {
        assert_eq!(strip_ansi("⬆️ 已上传样例音频"), "⬆️ 已上传样例音频");
    }

    #[test]
    fn cleanup_removes_only_expired_bivox_logs() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("bivox-log-cleanup-{nanos}"));
        fs::create_dir_all(&dir).unwrap();

        // 保留窗口：today 和之前 6 天（cutoff = today - 6）
        let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 3).unwrap();
        for name in [
            "bivox-2026-10-20.log", // 过期
            "bivox-2026-10-27.log", // 过期（cutoff=2026-10-28）
            "bivox-2026-10-28.log", // 保留（边界）
            "bivox-2026-11-03.log", // 今天
            "notes.txt",            // 非日志文件
            "bivox-bad-date.log",   // 格式不合法
        ] {
            fs::write(dir.join(name), b"x").unwrap();
        }

        assert_eq!(cleanup_old_logs(&dir, today), 2);
        assert!(!dir.join("bivox-2026-10-20.log").exists());
        assert!(!dir.join("bivox-2026-10-27.log").exists());
        assert!(dir.join("bivox-2026-10-28.log").exists());
        assert!(dir.join("bivox-2026-11-03.log").exists());
        assert!(dir.join("notes.txt").exists());
        assert!(dir.join("bivox-bad-date.log").exists());

        let _ = fs::remove_dir_all(&dir);
    }
}
