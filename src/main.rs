//! GUI 入口。
//!
//! 只做三件事：定位便携目录 → 交给 [`kaols::app::run`] → 返回退出码。
//! 具体的日志、崩溃钩子、配置准备都在 `app::run` 里完成（见 GUI_MIGRATION.md D4/D8）。

// release 构建不弹控制台；debug 保留控制台，方便直接看 panic 与日志输出。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::process::ExitCode;

fn main() -> ExitCode {
    let paths = match kaols::paths::AppPaths::resolve() {
        Ok(paths) => paths,
        Err(err) => {
            // 此时还没初始化日志/崩溃文件，只能走 stderr；属于极罕见的基础设施错误。
            eprintln!("无法定位程序所在目录：{err}");
            return ExitCode::FAILURE;
        }
    };

    kaols::app::run(paths)
}
