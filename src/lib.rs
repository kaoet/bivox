//! kaols：基于百炼 Qwen-Audio 的超低延迟实时语音识别与语音合成。
//!
//! - 二进制入口：`src/main.rs`（GUI，见 GUI_MIGRATION.md）
//! - 独立调试工具：`src/bin/admin.rs`（已实现，百炼音色管理）
//!
//! 本 crate 只支持 Windows，且只支持便携模式：所有可写文件都在 exe 同目录。

// 渲染后端必须二选一；两个都关掉时给出明确编译错误，而不是运行期黑屏。
#[cfg(not(any(feature = "renderer-wgpu", feature = "renderer-glow")))]
compile_error!("必须启用 renderer-wgpu 或 renderer-glow 之一（见 Cargo.toml [features]）");

pub mod app;
pub mod audio;
pub mod client;
pub mod config;
pub mod config_edit;
pub mod gui;
pub mod language;
pub mod log_sink;
pub mod paths;
pub mod pipeline;
pub mod voice;
