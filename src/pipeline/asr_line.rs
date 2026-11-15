//! 「未门控音频块 → 本地 VAD → ASR」的共用接线（D34/D36/D39）。
//!
//! 我方（麦克风）与他人（进程环回）线路只负责把 16 kHz / 单声道 / PCM16 块送进
//! `audio_rx`；门控与识别都在这里：
//!
//! ```text
//! audio_rx ──▶ VadWorker（独立 std 线程，Silero）──▶ 应上传的块 ──▶ QwenAsrClient ──▶ AsrEvent
//! ```
//!
//! 断线重连由 ASR 客户端自己处理，它通过 `vad_reset` 让 VAD 线程在下一块音频前复位瞬态。
//! 为什么不把 VAD 放进采集回调或 ASR 任务里：见 D34。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use anyhow::Result;
use eframe::egui;
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::task::JoinHandle as TaskHandle;

use crate::audio::silero::SileroDetector;
use crate::audio::{vad, vad_thread};
use crate::client::AsrEvent;
use crate::client::qwen_asr::QwenAsrClient;
use crate::config::VadConfig;
use crate::events::PipelineEvent;

/// 一条识别线路：VAD 线程 + ASR 任务。
pub struct AsrLine {
    /// 识别事件流（句级中间结果/最终结果）。
    pub events: mpsc::Receiver<AsrEvent>,
    /// ASR 任务；会话收尾时 await。
    pub task: TaskHandle<()>,
    /// 本地 VAD 线程（`[vad] enabled = false` 时为 `None`）。
    pub vad_thread: Option<JoinHandle<()>>,
}

/// 启动一条识别线路所需的参数。
pub struct AsrLineParams<'a> {
    pub api_key: String,
    pub model: &'a str,
    pub ws_url: &'a str,
    pub language_hints: Vec<String>,
    pub vocabulary_id: Option<String>,
    pub vad: &'a VadConfig,
    /// 未门控的 16 kHz 单声道 PCM16 块（来自麦克风或进程环回）。
    pub audio_rx: mpsc::Receiver<Vec<u8>>,
    /// 本地 VAD 是否处于语音段（顶栏指示器）。
    pub vad_active: Arc<AtomicBool>,
    /// 断线重连时置位，请 VAD 线程复位瞬态。
    pub vad_reset: Arc<AtomicBool>,
    /// 推理失败降级标志（会话轮询后向 GUI 报一次告警）。
    pub vad_degraded: Arc<AtomicBool>,
    /// 会话是否仍在运行（ASR 客户端在致命错误时也会置 false）。
    pub running: Arc<AtomicBool>,
    /// 是否用户主动停止（决定错误要不要上报）。
    pub user_stopped: Arc<AtomicBool>,
    pub events: UnboundedSender<PipelineEvent>,
    pub repaint: egui::Context,
}

/// 启动 VAD 线程与 ASR 任务，返回识别事件流与需要收尾的线程/任务。
pub fn spawn(params: AsrLineParams<'_>) -> Result<AsrLine> {
    let AsrLineParams {
        api_key,
        model,
        ws_url,
        language_hints,
        vocabulary_id,
        vad: vad_config,
        audio_rx,
        vad_active,
        vad_reset,
        vad_degraded,
        running,
        user_stopped,
        events,
        repaint,
    } = params;

    // VAD 线程：原始 chunk 先经 Silero 门控，再交给 ASR 任务上传。
    // 不放在 cpal 实时回调（禁止推理）与 ASR 任务（避免阻塞 socket 读写）里，见 D34。
    let (tx_gated, rx_gated) = mpsc::channel::<Vec<u8>>(100);
    let (asr_audio_rx, vad_thread) = if vad_config.enabled {
        let gate = vad::SpeechGate::with_detector(
            vad::GateConfig {
                threshold: vad_config.threshold,
                min_silence_ms: vad_config.min_silence_ms,
                tail_ms: vad_config.tail_ms,
                debug: vad_config.debug,
                // 把语音段状态发布给 GUI 顶栏的 VAD 指示器
                active_flag: Some(vad_active),
                ..Default::default()
            },
            SileroDetector::new()?,
        );
        let worker = vad_thread::spawn(gate, audio_rx, tx_gated, vad_reset.clone(), vad_degraded)?;
        (rx_gated, Some(worker))
    } else {
        (audio_rx, None)
    };

    // ASR 任务
    let (tx_asr, rx_asr) = mpsc::channel(100);
    let vad_enabled = vad_config.enabled;
    let model = model.to_string();
    let ws_url = ws_url.to_string();
    let task = tokio::spawn(async move {
        if let Err(err) = QwenAsrClient::run_duplex(
            api_key,
            model,
            ws_url,
            language_hints,
            vocabulary_id,
            if vad_enabled { Some(vad_reset) } else { None },
            asr_audio_rx,
            tx_asr,
            running,
        )
        .await
        {
            // 用户主动停止时的连接错误属于预期收尾，不弹错误条
            if !user_stopped.load(Ordering::SeqCst) {
                // best-effort: 界面可能已退出，失败只上报不阻塞
                let _ = events.send(PipelineEvent::Error {
                    context: "语音识别服务异常".to_string(),
                    detail: format!("{err:#}"),
                });
                repaint.request_repaint();
            }
        }
    });

    Ok(AsrLine {
        events: rx_asr,
        task,
        vad_thread,
    })
}
