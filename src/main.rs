//! GUI 入口。
//!
//! 只做三件事：定位便携目录 → 单实例检查 → 交给 [`kaols::app::run`]。
//! 具体的日志、崩溃钩子、配置准备都在 `app::run` 里完成（见 docs/decisions.md D4/D8/D17）。

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

    // 单实例（D17）：同一便携目录只允许一个实例；第二个实例激活已有窗口后直接退出。
    #[cfg(windows)]
    let _single_instance =
        match std::env::current_exe().and_then(|exe| kaols::single_instance::acquire(&exe)) {
            Ok(Some(guard)) => Some(guard),
            Ok(None) => return ExitCode::SUCCESS,
            Err(err) => {
                // 极罕见（创建 Mutex 失败）：不因单实例检查失败而拒绝启动
                eprintln!("单实例检查失败（继续启动）：{err}");
                None
            }
        };

    kaols::app::run(paths)
}
