//! GUI 组件：工具栏、页签、日志面板与通用小部件。
//!
//! 约定（D15）：这里的函数只负责绘制和把用户操作转成状态变更，
//! 不直接做 IO、网络或阻塞调用；耗时动作交给 `app` 里的后台任务。

pub mod asr_tab;
pub mod audio_tab;
pub mod language_list;
pub mod log_panel;
pub mod meter;
pub mod others_tab;
pub mod realtime_tab;
pub mod self_tab;
pub mod settings_tab;
pub mod toolbar;
pub mod translate_tab;
pub mod tts_current_voice;
pub mod tts_tab;
pub mod tts_voice_editor;
pub mod typed_input;
pub mod vrchat_tab;
pub mod widgets;
