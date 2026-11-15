//! 实时管线会话：建连、设备与生命周期的编排。
//!
//! 文本流转（ASR 事件 → 语言判定 → 分块 → 翻译/TTS/字幕、打字输入）在
//! [`crate::pipeline::self_line`]；本模块只负责把线路、音色、翻译 worker 与设备接起来，
//! 并在停止时按顺序收尾。
//!
//! 与 GUI 的边界（决策 D6/D13，见 docs/decisions.md）：
//! - 只通过 [`PipelineEvent`] 向上汇报；
//! - GUI 持有 [`PipelineSessionHandle`]（`Send + Clone`），不持有 cpal 的 `Stream`；
//! - 停止时 GUI 调 `handle.stop()`，本任务退出前会 drop `AudioCapture` 并等后台任务结束，
//!   确保麦克风/扬声器被释放。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Result;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

use crate::audio::capture::AudioCapture;
use crate::audio::device::AudioDeviceManager;
use crate::audio::playback::AudioPlaybackEngine;
use crate::client::qwen_mt::QwenMtClient;
use crate::client::qwen_tts::{QwenTtsClient, QwenTtsConfig, TtsChunkRequest};
use crate::config::AppConfig;
use crate::events::{PipelineEvent, SessionState};
use crate::language::canonical_language;
use crate::pipeline::self_line::TranslationRuntime;
use crate::pipeline::translator;
use crate::voice::ActiveVoice;

/// 取消检查间隔：ASR 事件本身是事件驱动的，短轮询兼顾停止响应速度与开销。
pub(crate) const CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// 停止时等待后台任务收尾的上限。
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);
/// 一句「打字输入」的最大字符数（界面校验用；官方单次 continue-task 上限远大于此）。
pub const MAX_TYPED_INPUT_CHARS: usize = 500;

/// GUI → 会话的后台命令：把一句打字输入排队朗读。
///
/// 只带原始文本；语言/音色/语速由会话按与 ASR 相同的规则解析。
#[derive(Debug, Clone)]
pub struct TypedInput {
    pub text: String,
}

/// 会话控制句柄：GUI 只持有它。
#[derive(Clone)]
pub struct PipelineSessionHandle {
    /// 传给 ASR/TTS 的"继续运行"标志（**初值 true**；客户端在致命错误时可能置 false）。
    running: Arc<AtomicBool>,
    /// 是否为用户主动停止（用于区分"用户停止"与"服务异常自行退出"）。
    stop_requested: Arc<AtomicBool>,
    /// 本地 VAD 当前是否处于语音段（由 `SpeechGate` 更新，GUI 只读）。
    vad_active: Arc<AtomicBool>,
    /// 本地 VAD 是否已因推理失败降级为全量上传（GUI 告警用）。
    vad_degraded: Arc<AtomicBool>,
    /// 请求 VAD 线程在下一块音频前重置瞬态（断线重连时由 ASR 置位）。
    vad_reset: Arc<AtomicBool>,
    /// 打字输入命令通道（GUI 提交，会话调度循环消费）。
    typed_tx: UnboundedSender<TypedInput>,
}

impl PipelineSessionHandle {
    pub fn new(typed_tx: UnboundedSender<TypedInput>) -> Self {
        Self {
            running: Arc::new(AtomicBool::new(true)),
            stop_requested: Arc::new(AtomicBool::new(false)),
            vad_active: Arc::new(AtomicBool::new(false)),
            vad_degraded: Arc::new(AtomicBool::new(false)),
            vad_reset: Arc::new(AtomicBool::new(false)),
            typed_tx,
        }
    }

    /// 提交一句打字输入排队朗读；会话已结束时返回 `false`（界面据此提示）。
    pub fn speak_typed(&self, text: String) -> bool {
        self.typed_tx.send(TypedInput { text }).is_ok()
    }

    /// 当前本地 VAD 是否处于语音段。
    pub fn vad_active(&self) -> bool {
        self.vad_active.load(Ordering::Relaxed)
    }

    /// 本地 VAD 是否已降级为全量上传。
    pub fn vad_degraded(&self) -> bool {
        self.vad_degraded.load(Ordering::Relaxed)
    }

    /// 传给 VAD 线程的重置标志。
    fn vad_reset_flag(&self) -> Arc<AtomicBool> {
        self.vad_reset.clone()
    }

    /// 传给 VAD 线程的降级标志。
    fn vad_degraded_flag(&self) -> Arc<AtomicBool> {
        self.vad_degraded.clone()
    }

    /// 会话停止/失败后把指示器与降级状态复位。
    pub fn reset_vad(&self) {
        self.vad_active.store(false, Ordering::Relaxed);
        self.vad_degraded.store(false, Ordering::Relaxed);
        self.vad_reset.store(false, Ordering::Relaxed);
    }

    /// 请求停止（用户点「停止」或关窗时调用；后台任务会在一个轮询周期内退出）。
    pub fn stop(&self) {
        self.stop_requested.store(true, Ordering::SeqCst);
        self.running.store(false, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        !self.running.load(Ordering::SeqCst)
    }

    /// 会话是否是用户主动停止的（不是则视为后台服务异常退出）。
    pub fn was_stopped_by_user(&self) -> bool {
        self.stop_requested.load(Ordering::SeqCst)
    }

    /// 内部收尾用：让后台任务退出，但不算"用户停止"。
    fn abort_background(&self) {
        self.running.store(false, Ordering::SeqCst);
    }
}

/// 启动并运行一个实时会话，直到停止或出错。
///
/// 正常停止返回 `Ok(())` 且已发出 `StateChanged(Idle)`；启动/运行失败返回 `Err`，
/// 由调用方（`app::start_session` 里的包装任务）转成 `Error` + `Failed` 事件。
pub async fn run(
    config: AppConfig,
    events: UnboundedSender<PipelineEvent>,
    chatbox_events: Option<UnboundedSender<PipelineEvent>>,
    repaint: eframe::egui::Context,
    handle: PipelineSessionHandle,
    typed_rx: UnboundedReceiver<TypedInput>,
) -> Result<()> {
    let mut emit = |event: PipelineEvent| {
        if let Some(chatbox) = &chatbox_events {
            // best-effort: OSC 服务可能已退出，不影响主事件流
            let _ = chatbox.send(event.clone());
        }
        let _ = events.send(event);
        repaint.request_repaint();
    };

    let api_key = config
        .api
        .key
        .clone()
        .ok_or_else(|| anyhow::anyhow!("未配置 API Key，请在「设置」页填写"))?;

    // 解析生效音色：系统音色原样返回；`custom:<名字>` 会把所有语言样本并行注册好（可能上传，耗时不定）。
    let voice = crate::voice::resolve_active_voice(&config).await?;
    emit(PipelineEvent::VoiceInfo {
        display: describe_voice(&voice),
    });
    if handle.is_cancelled() {
        emit(PipelineEvent::StateChanged(SessionState::Idle));
        return Ok(());
    }

    // 预编译热词（D31）：首次创建/词表变化时会上传并等待部署，失败只降级不阻断会话。
    let hotwords = crate::asr::resolve_vocabulary(&config).await;
    if let Some(warning) = &hotwords.warning {
        log::warn!("⚠️ 预编译热词：{warning}");
        emit(PipelineEvent::Warning {
            context: "预编译热词未生效".to_string(),
            detail: warning.clone(),
        });
    }
    if let Some(vocabulary_id) = &hotwords.vocabulary_id {
        log::info!("🔥 本次识别使用预编译热词列表: {vocabulary_id}");
    }
    if handle.is_cancelled() {
        emit(PipelineEvent::StateChanged(SessionState::Idle));
        return Ok(());
    }

    // 设备：按完整名精确匹配（D12），失败直接终止会话。
    let device_mgr = AudioDeviceManager::new();
    let in_dev = device_mgr.get_input_device(config.audio.input_device.as_deref())?;
    let out_dev = device_mgr.get_output_device(config.audio.output_device.as_deref())?;
    let in_name = crate::audio::device::device_name(&in_dev);
    let out_name = crate::audio::device::device_name(&out_dev);
    emit(PipelineEvent::Devices {
        input: in_name.clone(),
        output: out_name.clone(),
    });
    log::info!("输入设备：{in_name}；输出设备：{out_name}");
    if handle.is_cancelled() {
        emit(PipelineEvent::StateChanged(SessionState::Idle));
        return Ok(());
    }

    // 扬声器输出引擎（TTS 默认 22050Hz）
    let playback_engine = AudioPlaybackEngine::new(&out_dev, 22050)?;
    let playback = playback_engine.handle();

    // 麦克风采集：`capture` 必须活到会话结束，drop 即停止采集。
    let (tx_mic, rx_mic) = mpsc::channel::<Vec<u8>>(100);
    let capture = AudioCapture::start(&in_dev, tx_mic)?;

    let inference_ws_url = config.inference_ws_url();
    let mut tasks: Vec<JoinHandle<()>> = Vec::new();

    // 识别线路：VAD 线程 + ASR 任务（与他人线路共用同一套接线，D34/D39）。
    let asr_line = crate::pipeline::asr_line::spawn(crate::pipeline::asr_line::AsrLineParams {
        api_key: api_key.clone(),
        model: &config.asr.model,
        ws_url: &inference_ws_url,
        language_hints: config.local.languages.clone(),
        vocabulary_id: hotwords.vocabulary_id.clone(),
        vad: &config.vad,
        audio_rx: rx_mic,
        vad_active: handle.vad_active.clone(),
        vad_reset: handle.vad_reset_flag(),
        vad_degraded: handle.vad_degraded_flag(),
        running: handle.running.clone(),
        user_stopped: handle.stop_requested.clone(),
        events: events.clone(),
        repaint: repaint.clone(),
    })?;
    let asr_events = asr_line.events;
    let mut vad_worker_thread = asr_line.vad_thread;
    tasks.push(asr_line.task);

    // TTS 任务
    let (tx_tts, rx_tts) = mpsc::channel::<TtsChunkRequest>(100);
    {
        let events = events.clone();
        let repaint = repaint.clone();
        let running = handle.running.clone();
        let user_stopped = handle.stop_requested.clone();
        let tts_config = QwenTtsConfig {
            api_key: api_key.clone(),
            tts_model: config.tts.model.clone(),
            ws_url: inference_ws_url,
            dialect: config.tts.dialect.clone(),
        };
        let tts_playback = playback.clone();
        tasks.push(tokio::spawn(async move {
            if let Err(err) =
                QwenTtsClient::run_duplex(tts_config, rx_tts, tts_playback, running).await
            {
                // 用户主动停止时的连接错误属于预期收尾，不弹错误条
                if !user_stopped.load(Ordering::SeqCst) {
                    // best-effort: 界面可能已退出，失败只上报不阻塞
                    let _ = events.send(PipelineEvent::Error {
                        context: "语音合成服务异常".to_string(),
                        detail: format!("{err:#}"),
                    });
                    repaint.request_repaint();
                }
            }
        }));
    }

    // 翻译 worker：开启后 ASR/打字文本先经 MT 再送 TTS。目标语言与音色在会话内固定，
    // 运行中改配置由 D20 热重启处理；主循环只通过通道收发，不 await HTTP。
    let translate = config.translate.clone();
    let translate_enabled = config.local.translate;
    let translate_target = canonical_language(&config.local.target).to_string();
    let (translation, rx_mt) = if translate_enabled {
        let target = translate_target.clone();
        let client = QwenMtClient::new(
            api_key.clone(),
            translate.model.clone(),
            config.translate_chat_url(),
        )?;
        let (tx_job, done_rx, worker) =
            translator::spawn(client, target.clone(), handle.running.clone());
        tasks.push(worker);
        let (voice_id, rate) = voice.selection_for(&target);
        (
            Some(TranslationRuntime::new(
                target,
                voice_id.to_string(),
                rate,
                tx_job,
            )),
            Some(done_rx),
        )
    } else {
        (None, None)
    };

    emit(PipelineEvent::StateChanged(SessionState::Running));

    // 调度中枢：我方线路的文本流转（ASR → 语言判定 → 分块 → 翻译/TTS/字幕，含打字输入）。
    crate::pipeline::self_line::run(
        crate::pipeline::self_line::Params {
            handle: &handle,
            voice: &voice,
            chunk_words: config.chunking.words,
            languages: config.local.languages.clone(),
            translate_enabled,
            translate_granularity: config.translate.granularity,
            typed_rx,
            asr_events,
            rx_mt,
            translation,
            tx_tts: tx_tts.clone(),
        },
        &mut emit,
    )
    .await;
    // 收尾（D13）：先停采集，再关 TTS 通道，最后等后台任务退出。
    // 翻译 worker 的发送端随 `self_line::run` 返回一起释放（它持在 `TranslationRuntime` 里）。
    handle.abort_background();
    handle.reset_vad();
    drop(capture);
    if let Some(worker) = vad_worker_thread.take() {
        if !join_thread_with_timeout(worker, SHUTDOWN_TIMEOUT) {
            log::warn!(
                "等待本地 VAD 线程收尾超时（{}s）",
                SHUTDOWN_TIMEOUT.as_secs()
            );
        }
    }
    drop(tx_tts);
    if tokio::time::timeout(SHUTDOWN_TIMEOUT, async {
        for task in tasks {
            let _ = task.await;
        }
    })
    .await
    .is_err()
    {
        log::warn!(
            "等待 ASR/TTS 任务收尾超时（{}s）",
            SHUTDOWN_TIMEOUT.as_secs()
        );
    }

    // 不是用户主动停止 → 说明后台服务自行退出（通常是建连/鉴权/网络失败）。
    if !handle.was_stopped_by_user() {
        let detail = "ASR/TTS 后台服务已自行退出（通常是建连、鉴权或网络失败，详见日志）";
        log::error!("实时管线非用户主动结束：{detail}");
        emit(PipelineEvent::Error {
            context: "实时管线已停止".to_string(),
            detail: detail.to_string(),
        });
    }

    emit(PipelineEvent::StateChanged(SessionState::Idle));
    Ok(())
}

/// 等待后台线程退出，最多 `timeout`；超时返回 `false`（std 线程无法强杀，只能尽力）。
fn join_thread_with_timeout(handle: std::thread::JoinHandle<()>, timeout: Duration) -> bool {
    let start = std::time::Instant::now();
    while !handle.is_finished() {
        if start.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = handle.join();
    true
}

fn describe_voice(voice: &ActiveVoice) -> String {
    match voice {
        ActiveVoice::Builtin { name } => format!("系统/指定音色 {name}"),
        ActiveVoice::Custom(custom) => format!(
            "自定义「{}」语言声线：{}（默认 {}，缺语言回退第一个样本）",
            custom.display_name,
            custom.languages().join(", "),
            custom.default_language
        ),
    }
}
