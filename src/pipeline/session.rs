//! 实时管线会话：建连、设备与生命周期的编排。
//!
//! 文本流转（ASR 事件 → 语言判定 → 分块 → 翻译/TTS/字幕、打字输入）在
//! [`crate::pipeline::self_line`]；本模块只负责把线路、音色、翻译 worker 与设备接起来，
//! 并在停止时按顺序收尾。
//!
//! 与 GUI 的边界（决策 D6/D13/D43，见 docs/decisions.md）：
//! - 只通过 [`PipelineEvent`] 向上汇报；
//! - GUI 持有 [`PipelineSessionHandle`]（`Send + Clone`），不持有 cpal 的 `Stream`；
//! - 顶栏的两个快捷开关通过 handle 里的 `watch` 通知本模块，由线路监督者按需
//!   **只启停该线路**，不触发 D20 全量热重启；
//! - 停止时 GUI 调 `handle.stop()`，本任务退出前会等监督者 drop `AudioCapture` 并收尾，
//!   确保麦克风/扬声器被释放。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use anyhow::Result;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::audio::capture::AudioCapture;
use crate::audio::device::AudioDeviceManager;
use crate::audio::playback::AudioPlaybackEngine;
use crate::client::AsrEvent;
use crate::client::qwen_mt::QwenMtClient;
use crate::client::qwen_tts::{QwenTtsClient, QwenTtsConfig, TtsChunkRequest};
use crate::config::{AppConfig, VadConfig};
use crate::events::{PipelineEvent, SessionState, Speaker};
use crate::language::canonical_language;
use crate::pipeline::asr_line::{self, AsrLine, AsrLineParams};
use crate::pipeline::self_line::TranslationRuntime;
use crate::pipeline::translator;
use crate::voice::ActiveVoice;

/// 取消检查间隔：ASR 事件本身是事件驱动的，短轮询兼顾停止响应速度与开销。
pub(crate) const CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// 单条线路（ASR/VAD）停止时等待后台任务收尾的上限。
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);
/// 会话停止时等待两个线路监督者退出的上限（其中可能包含两次 3s 级别收尾）。
const SUPERVISOR_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(6);
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
    /// 他人线路的 VAD 是否已降级（只用于顶栏电平条配色）。
    vad_degraded_others: Arc<AtomicBool>,
    /// 请求 VAD 线程在下一块音频前重置瞬态（断线重连时由 ASR 置位）。
    vad_reset: Arc<AtomicBool>,
    /// 打字输入命令通道（GUI 提交，会话调度循环消费）。
    typed_tx: UnboundedSender<TypedInput>,
    /// 顶栏麦克风开关的期望状态（监督者 watch）。
    mic_enabled_tx: watch::Sender<bool>,
    /// 顶栏他人侦听开关的期望状态（监督者 watch）。
    others_enabled_tx: watch::Sender<bool>,
    /// 保留一个接收端：会话尚未订阅时，watch 通道也不能因为无接收者而关闭，
    /// 否则 Starting 期间的顶栏切换会丢失（D43）。
    mic_enabled_keep: watch::Receiver<bool>,
    others_enabled_keep: watch::Receiver<bool>,
    /// 麦克风实时电平（f32 bits）。
    mic_level: Arc<AtomicU32>,
    /// 他人线路实时电平（f32 bits）。
    others_level: Arc<AtomicU32>,
}

impl PipelineSessionHandle {
    pub fn new(
        typed_tx: UnboundedSender<TypedInput>,
        mic_enabled: bool,
        others_enabled: bool,
    ) -> Self {
        let (mic_enabled_tx, mic_enabled_keep) = watch::channel(mic_enabled);
        let (others_enabled_tx, others_enabled_keep) = watch::channel(others_enabled);
        Self {
            running: Arc::new(AtomicBool::new(true)),
            stop_requested: Arc::new(AtomicBool::new(false)),
            vad_active: Arc::new(AtomicBool::new(false)),
            vad_degraded: Arc::new(AtomicBool::new(false)),
            vad_degraded_others: Arc::new(AtomicBool::new(false)),
            vad_reset: Arc::new(AtomicBool::new(false)),
            typed_tx,
            mic_enabled_tx,
            others_enabled_tx,
            mic_enabled_keep,
            others_enabled_keep,
            mic_level: Arc::new(AtomicU32::new(0)),
            others_level: Arc::new(AtomicU32::new(0)),
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

    /// 他人线路的 VAD 是否已降级为全量上传。
    pub fn vad_degraded_others(&self) -> bool {
        self.vad_degraded_others.load(Ordering::Relaxed)
    }

    /// 麦克风实时电平（0..1）。
    pub fn mic_level(&self) -> f32 {
        f32::from_bits(self.mic_level.load(Ordering::Relaxed))
    }

    /// 他人线路实时电平（0..1）。
    pub fn others_level(&self) -> f32 {
        f32::from_bits(self.others_level.load(Ordering::Relaxed))
    }

    /// 顶栏麦克风开关：更新期望状态，监督者会立即启停该线路。
    pub fn set_mic_enabled(&self, enabled: bool) {
        let _ = self.mic_enabled_tx.send(enabled);
    }

    /// 顶栏他人侦听开关：更新期望状态，监督者会立即启停该线路。
    pub fn set_others_enabled(&self, enabled: bool) {
        let _ = self.others_enabled_tx.send(enabled);
    }

    pub fn mic_enabled(&self) -> bool {
        *self.mic_enabled_keep.borrow()
    }

    pub fn others_enabled(&self) -> bool {
        *self.others_enabled_keep.borrow()
    }

    fn mic_enabled_rx(&self) -> watch::Receiver<bool> {
        self.mic_enabled_tx.subscribe()
    }

    fn others_enabled_rx(&self) -> watch::Receiver<bool> {
        self.others_enabled_tx.subscribe()
    }

    fn vad_active_handle(&self) -> Arc<AtomicBool> {
        self.vad_active.clone()
    }

    fn vad_degraded_handle(&self) -> Arc<AtomicBool> {
        self.vad_degraded.clone()
    }

    fn vad_degraded_others_handle(&self) -> Arc<AtomicBool> {
        self.vad_degraded_others.clone()
    }

    fn vad_reset_handle(&self) -> Arc<AtomicBool> {
        self.vad_reset.clone()
    }

    fn mic_level_handle(&self) -> Arc<AtomicU32> {
        self.mic_level.clone()
    }

    fn others_level_handle(&self) -> Arc<AtomicU32> {
        self.others_level.clone()
    }

    /// 会话停止/失败后把 VAD 指示、降级标志与电平复位。
    pub fn reset_vad(&self) {
        self.vad_active.store(false, Ordering::Relaxed);
        self.vad_degraded.store(false, Ordering::Relaxed);
        self.vad_degraded_others.store(false, Ordering::Relaxed);
        self.vad_reset.store(false, Ordering::Relaxed);
        self.mic_level.store(0.0_f32.to_bits(), Ordering::Relaxed);
        self.others_level
            .store(0.0_f32.to_bits(), Ordering::Relaxed);
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

/// 麦克风线路重建所需的固定参数。
struct MicParams {
    api_key: String,
    model: String,
    ws_url: String,
    languages: Vec<String>,
    vocabulary_id: Option<String>,
    vad: VadConfig,
    input_device: Option<String>,
    events: UnboundedSender<PipelineEvent>,
    repaint: eframe::egui::Context,
    /// 本地线路事件复用的稳定通道；线路重建时把新 ASR 事件转发进来。
    local_tx: mpsc::Sender<AsrEvent>,
}

/// 一条正在运行的麦克风线路。
struct MicLine {
    input_name: String,
    capture: AudioCapture,
    asr: AsrLine,
    forward_task: JoinHandle<()>,
    line_running: Arc<AtomicBool>,
    line_stopped: Arc<AtomicBool>,
}

impl MicLine {
    /// 停止这条线路：先释放采集，再收尾 VAD/ASR 与事件转发。
    async fn stop(self) {
        self.line_running.store(false, Ordering::SeqCst);
        self.line_stopped.store(true, Ordering::SeqCst);
        // 先 drop 采集：释放麦克风，并关闭音频通道。
        drop(self.capture);
        // 事件转发只服务于本线路，先停掉；ASR 的发送失败属于预期收尾。
        self.forward_task.abort();
        let _ = self.forward_task.await;
        self.asr.shutdown(SHUTDOWN_TIMEOUT).await;
    }
}

/// 打开麦克风并启动「采集 → 电平 tap → VAD → ASR」整条线路。
fn start_mic_line(params: &MicParams, handle: &PipelineSessionHandle) -> Result<MicLine> {
    let device = AudioDeviceManager::new().get_input_device(params.input_device.as_deref())?;
    let input_name = crate::audio::device::device_name(&device);

    let (tx_mic, rx_mic) = mpsc::channel::<Vec<u8>>(100);
    let capture = AudioCapture::start(&device, tx_mic)?;

    let line_running = Arc::new(AtomicBool::new(true));
    let line_stopped = Arc::new(AtomicBool::new(false));
    handle.vad_active.store(false, Ordering::Relaxed);
    handle.vad_degraded.store(false, Ordering::Relaxed);
    handle.vad_reset.store(false, Ordering::Relaxed);
    handle.mic_level.store(0.0_f32.to_bits(), Ordering::Relaxed);

    let mut asr = asr_line::spawn(AsrLineParams {
        api_key: params.api_key.clone(),
        model: &params.model,
        ws_url: &params.ws_url,
        language_hints: params.languages.clone(),
        vocabulary_id: params.vocabulary_id.clone(),
        vad: &params.vad,
        audio_rx: rx_mic,
        level: handle.mic_level_handle(),
        vad_active: handle.vad_active_handle(),
        vad_reset: handle.vad_reset_handle(),
        vad_degraded: handle.vad_degraded_handle(),
        running: line_running.clone(),
        user_stopped: line_stopped.clone(),
        events: params.events.clone(),
        repaint: params.repaint.clone(),
    })?;

    let events = asr
        .events
        .take()
        .ok_or_else(|| anyhow::anyhow!("麦克风线路缺少识别事件通道"))?;
    let local_tx = params.local_tx.clone();
    let forward_task = tokio::spawn(async move {
        let mut events = events;
        while let Some(event) = events.recv().await {
            if local_tx.send(event).await.is_err() {
                break;
            }
        }
    });

    Ok(MicLine {
        input_name,
        capture,
        asr,
        forward_task,
        line_running,
        line_stopped,
    })
}

/// 麦克风线路监督者：按 `mic_enabled` 的期望状态反复启停线路。
struct MicSupervisor {
    params: MicParams,
    handle: PipelineSessionHandle,
    enabled_rx: watch::Receiver<bool>,
    line: Option<MicLine>,
}

impl MicSupervisor {
    async fn run(mut self) {
        loop {
            if self.handle.is_cancelled() {
                break;
            }
            // ASR 任务是致命错误才会退出的；线路还开着时不能让采集变僵尸。
            let asr_exited = self
                .line
                .as_ref()
                .is_some_and(|line| line.asr.task.is_finished());
            if asr_exited {
                let line = self.line.take().expect("line 已判断为 Some");
                line.stop().await;
                self.handle.vad_active.store(false, Ordering::Relaxed);
                self.handle.vad_degraded.store(false, Ordering::Relaxed);
                self.handle.vad_reset.store(false, Ordering::Relaxed);
                self.handle
                    .mic_level
                    .store(0.0_f32.to_bits(), Ordering::Relaxed);
                let _ = self.params.local_tx.try_send(AsrEvent {
                    text: String::new(),
                    is_sentence_end: true,
                    sentence_id: None,
                });
                if *self.enabled_rx.borrow() {
                    self.handle.set_mic_enabled(false);
                    let _ = self.params.events.send(PipelineEvent::LineError {
                        speaker: Speaker::Local,
                        detail: "麦克风识别任务已退出，已自动关闭".to_string(),
                    });
                    self.params.repaint.request_repaint();
                }
            }
            let want = *self.enabled_rx.borrow();
            match (want, self.line.is_some()) {
                (true, false) => match start_mic_line(&self.params, &self.handle) {
                    Ok(line) => {
                        self.handle
                            .mic_level
                            .store(0.0_f32.to_bits(), Ordering::Relaxed);
                        log::info!("麦克风线路已启动");
                        self.line = Some(line);
                    }
                    Err(err) => {
                        let detail = format!("{err:#}");
                        log::error!("麦克风线路启动失败：{detail}");
                        // 先让期望状态回到关，避免每 100ms 重试一次；GUI 会收到
                        // LineError 后把配置与页签 checkbox 一起回退。
                        self.handle.set_mic_enabled(false);
                        self.handle.vad_active.store(false, Ordering::Relaxed);
                        self.handle
                            .mic_level
                            .store(0.0_f32.to_bits(), Ordering::Relaxed);
                        let _ = self.params.events.send(PipelineEvent::LineError {
                            speaker: Speaker::Local,
                            detail,
                        });
                        self.params.repaint.request_repaint();
                    }
                },
                (false, true) => {
                    let line = self.line.take().expect("line 已判断为 Some");
                    line.stop().await;
                    self.handle.vad_active.store(false, Ordering::Relaxed);
                    self.handle.vad_degraded.store(false, Ordering::Relaxed);
                    self.handle.vad_reset.store(false, Ordering::Relaxed);
                    self.handle
                        .mic_level
                        .store(0.0_f32.to_bits(), Ordering::Relaxed);
                    // 让 self_line 丢弃当前半句并复位分块器/翻译状态。
                    let _ = self.params.local_tx.try_send(AsrEvent {
                        text: String::new(),
                        is_sentence_end: true,
                        sentence_id: None,
                    });
                }
                _ => {}
            }

            tokio::select! {
                changed = self.enabled_rx.changed() => {
                    if changed.is_err() {
                        break;
                    }
                }
                _ = tokio::time::sleep(CANCEL_POLL_INTERVAL) => {}
            }
        }

        if let Some(line) = self.line.take() {
            line.stop().await;
        }
        self.handle.vad_active.store(false, Ordering::Relaxed);
        self.handle.vad_degraded.store(false, Ordering::Relaxed);
        self.handle.vad_reset.store(false, Ordering::Relaxed);
        self.handle
            .mic_level
            .store(0.0_f32.to_bits(), Ordering::Relaxed);
    }
}

/// 他人线路监督者：按 `others_enabled` 的期望状态 spawn/停止独立任务。
struct OthersSupervisor {
    params: crate::pipeline::others_line::Params,
    handle: PipelineSessionHandle,
    enabled_rx: watch::Receiver<bool>,
    task: Option<JoinHandle<()>>,
    line_running: Option<Arc<AtomicBool>>,
}

impl OthersSupervisor {
    async fn run(mut self) {
        loop {
            if self.handle.is_cancelled() {
                break;
            }
            if self.task.as_ref().is_some_and(|task| task.is_finished()) {
                if let Some(task) = self.task.take() {
                    match task.await {
                        Ok(()) => log::warn!("他人线路任务已退出，准备按开关状态重建"),
                        Err(err) => log::warn!("他人线路任务异常退出：{err}"),
                    }
                }
                self.line_running = None;
                self.handle
                    .others_level
                    .store(0.0_f32.to_bits(), Ordering::Relaxed);
            }
            let want = *self.enabled_rx.borrow();
            if want && self.task.is_none() {
                self.handle
                    .others_level
                    .store(0.0_f32.to_bits(), Ordering::Relaxed);
                self.handle
                    .vad_degraded_others
                    .store(false, Ordering::Relaxed);
                let mut params = self.params.clone();
                let line_running = Arc::new(AtomicBool::new(true));
                params.running = line_running.clone();
                log::info!("他人线路已启动");
                self.line_running = Some(line_running);
                self.task = Some(tokio::spawn(crate::pipeline::others_line::run(params)));
            } else if !want && self.task.is_some() {
                self.stop_task().await;
            }

            tokio::select! {
                changed = self.enabled_rx.changed() => {
                    if changed.is_err() {
                        break;
                    }
                }
                _ = tokio::time::sleep(CANCEL_POLL_INTERVAL) => {}
            }
        }

        self.stop_task().await;
    }

    async fn stop_task(&mut self) {
        if let Some(running) = self.line_running.take() {
            running.store(false, Ordering::SeqCst);
        }
        if let Some(task) = self.task.take() {
            if tokio::time::timeout(SHUTDOWN_TIMEOUT, task).await.is_err() {
                log::warn!("等待他人线路收尾超时（{}s）", SHUTDOWN_TIMEOUT.as_secs());
            }
        }
        self.handle
            .others_level
            .store(0.0_f32.to_bits(), Ordering::Relaxed);
        self.handle
            .vad_degraded_others
            .store(false, Ordering::Relaxed);
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
    // `[self] tts = false` 时完全不解析音色（也不上传复刻），只上屏。
    let voice = if config.local.tts {
        let voice = crate::voice::resolve_active_voice(&config).await?;
        emit(PipelineEvent::VoiceInfo {
            display: describe_voice(&voice),
        });
        Some(voice)
    } else {
        log::info!("未启用语音合成：跳过音色解析与 TTS");
        None
    };
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

    // 设备：按完整名精确匹配（D12），失败直接终止会话；未启用的输出设备不打开。
    let device_mgr = AudioDeviceManager::new();
    let out_dev = if config.local.tts {
        Some(device_mgr.get_output_device(config.audio.output_device.as_deref())?)
    } else {
        None
    };
    let out_name = out_dev.as_ref().map(crate::audio::device::device_name);

    // 本地线路的稳定事件通道：麦克风线路可以反复重建，self_line 只认这一条通道。
    let (local_tx, local_rx) = mpsc::channel::<AsrEvent>(100);
    let inference_ws_url = config.inference_ws_url();
    let mic_params = MicParams {
        api_key: api_key.clone(),
        model: config.asr.model.clone(),
        ws_url: inference_ws_url.clone(),
        languages: config.local.languages.clone(),
        vocabulary_id: hotwords.vocabulary_id.clone(),
        vad: config.vad.clone(),
        input_device: config.audio.input_device.clone(),
        events: events.clone(),
        repaint: repaint.clone(),
        local_tx: local_tx.clone(),
    };

    if handle.is_cancelled() {
        emit(PipelineEvent::StateChanged(SessionState::Idle));
        return Ok(());
    }

    // 扬声器输出引擎（TTS 默认 22050Hz）：只在启用语音合成时创建。
    let playback_engine = match &out_dev {
        Some(dev) => Some(AudioPlaybackEngine::new(dev, 22050)?),
        None => None,
    };
    let playback = playback_engine.as_ref().map(|engine| engine.handle());

    let mut tasks: Vec<JoinHandle<()>> = Vec::new();

    // TTS 任务：只在启用语音合成时创建（没有 TTS 时渠道为空，所有发送变成空操作）。
    let tx_tts = match &playback {
        Some(tts_playback) => {
            let (tx, rx) = mpsc::channel::<TtsChunkRequest>(100);
            let tts_config = QwenTtsConfig {
                api_key: api_key.clone(),
                tts_model: config.tts.model.clone(),
                ws_url: inference_ws_url.clone(),
                dialect: config.tts.dialect.clone(),
            };
            let tts_playback = tts_playback.clone();
            let events = events.clone();
            let repaint = repaint.clone();
            let running = handle.running.clone();
            let user_stopped = handle.stop_requested.clone();
            tasks.push(tokio::spawn(async move {
                if let Err(err) =
                    QwenTtsClient::run_duplex(tts_config, rx, tts_playback, running).await
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
            Some(tx)
        }
        None => None,
    };

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
        (Some(TranslationRuntime::new(target, tx_job)), Some(done_rx))
    } else {
        (None, None)
    };

    // 两个线路监督者：顶栏开关只影响对应线路，不重启整个会话（D43）。
    let others_params = crate::pipeline::others_line::Params {
        api_key: api_key.clone(),
        model: config.asr.model.clone(),
        ws_url: inference_ws_url,
        vocabulary_id: hotwords.vocabulary_id.clone(),
        vad: config.vad.clone(),
        chunk_words: config.chunking.words,
        process: config.vrchat.process.clone(),
        languages: config.others.languages.clone(),
        translate: config
            .others
            .translate
            .then(|| crate::pipeline::others_line::OthersTranslate {
                model: config.translate.model.clone(),
                chat_url: config.translate_chat_url(),
                target: canonical_language(&config.others.target).to_string(),
                granularity: config.translate.granularity,
            }),
        running: Arc::new(AtomicBool::new(false)),
        level: handle.others_level_handle(),
        vad_degraded: handle.vad_degraded_others_handle(),
        events: events.clone(),
        repaint: repaint.clone(),
    };

    // 所有可能失败的初始化都已完成，这时才真正打开麦克风；避免后面的
    // 播放/翻译初始化失败时留下一条没人收尾的采集/ASR 任务。
    let initial_mic = if handle.mic_enabled() {
        match start_mic_line(&mic_params, &handle) {
            Ok(line) => Some(line),
            Err(err) => {
                // TTS/翻译任务已经启动：先让它们退出，再把错误交回 app。
                handle.stop();
                return Err(err);
            }
        }
    } else {
        None
    };
    let in_name = initial_mic
        .as_ref()
        .map(|line| line.input_name.clone())
        .unwrap_or_default();
    emit(PipelineEvent::Devices {
        input: in_name.clone(),
        output: out_name.clone().unwrap_or_default(),
    });
    log::info!(
        "输入设备：{}；输出设备：{}",
        if in_name.is_empty() {
            "（未启用麦克风）"
        } else {
            in_name.as_str()
        },
        out_name.as_deref().unwrap_or("（未启用语音合成）")
    );

    let mic_supervisor = tokio::spawn(
        MicSupervisor {
            params: mic_params,
            handle: handle.clone(),
            enabled_rx: handle.mic_enabled_rx(),
            line: initial_mic,
        }
        .run(),
    );
    let others_supervisor = tokio::spawn(
        OthersSupervisor {
            params: others_params,
            handle: handle.clone(),
            enabled_rx: handle.others_enabled_rx(),
            task: None,
            line_running: None,
        }
        .run(),
    );

    emit(PipelineEvent::StateChanged(SessionState::Running));

    // 调度中枢：我方线路的文本流转（ASR → 语言判定 → 分块 → 翻译/TTS/字幕，含打字输入）。
    crate::pipeline::self_line::run(
        crate::pipeline::self_line::Params {
            handle: &handle,
            voice: voice.as_ref(),
            chunk_words: config.chunking.words,
            languages: config.local.languages.clone(),
            translate_enabled,
            translate_granularity: config.translate.granularity,
            typed_rx,
            asr_events: Some(local_rx),
            rx_mt,
            translation,
            tx_tts,
        },
        &mut emit,
    )
    .await;

    // 收尾（D13/D43）：先取消会话，再等两个线路监督者按顺序释放设备/线程。
    handle.abort_background();
    handle.reset_vad();
    let (mic_shutdown, others_shutdown) = tokio::join!(
        tokio::time::timeout(SUPERVISOR_SHUTDOWN_TIMEOUT, mic_supervisor),
        tokio::time::timeout(SUPERVISOR_SHUTDOWN_TIMEOUT, others_supervisor),
    );
    match mic_shutdown {
        Ok(Ok(())) => {}
        Ok(Err(err)) => log::warn!("麦克风线路监督者异常退出：{err}"),
        Err(_) => log::warn!(
            "等待麦克风线路监督者收尾超时（{}s）",
            SUPERVISOR_SHUTDOWN_TIMEOUT.as_secs()
        ),
    }
    match others_shutdown {
        Ok(Ok(())) => {}
        Ok(Err(err)) => log::warn!("他人线路监督者异常退出：{err}"),
        Err(_) => log::warn!(
            "等待他人线路监督者收尾超时（{}s）",
            SUPERVISOR_SHUTDOWN_TIMEOUT.as_secs()
        ),
    }
    drop(local_tx);

    // 最后等 TTS / 翻译 worker 收尾。
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
