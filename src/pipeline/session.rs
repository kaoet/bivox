//! 实时管线会话：把「ASR → 语言判定 → 分块 → TTS → 播放」包成可启动/停止的会话。
//!
//! 与 GUI 的边界（决策 D6/D13，见 docs/decisions.md）：
//! - 只通过 [`PipelineEvent`] 向上汇报；
//! - GUI 持有 [`PipelineSessionHandle`]（`Send + Clone`），不持有 cpal 的 `Stream`；
//! - 停止时 GUI 调 `handle.stop()`，本任务退出前会 drop `AudioCapture` 并等后台任务结束，
//!   确保麦克风/扬声器被释放。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::task::JoinHandle;

use crate::audio::capture::AudioCapture;
use crate::audio::device::AudioDeviceManager;
use crate::audio::playback::AudioPlaybackEngine;
use crate::audio::vad;
use crate::client::qwen_asr::QwenAsrClient;
use crate::client::qwen_tts::{QwenTtsClient, QwenTtsConfig, TtsChunkRequest};
use crate::config::AppConfig;
use crate::events::{PipelineEvent, SessionState};
use crate::language::LanguageTracker;
use crate::pipeline::chunker::TextChunker;
use crate::voice::ActiveVoice;

/// 取消检查间隔：ASR 事件本身是事件驱动的，短轮询兼顾停止响应速度与开销。
const CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// 停止时等待后台任务收尾的上限。
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

/// 会话控制句柄：GUI 只持有它。
#[derive(Clone)]
pub struct PipelineSessionHandle {
    /// 传给 ASR/TTS 的"继续运行"标志（**初值 true**；客户端在致命错误时可能置 false）。
    running: Arc<AtomicBool>,
    /// 是否为用户主动停止（用于区分"用户停止"与"服务异常自行退出"）。
    stop_requested: Arc<AtomicBool>,
    /// 本地 VAD 当前是否处于语音段（由 `SpeechGate` 更新，GUI 只读）。
    vad_active: Arc<AtomicBool>,
}

impl PipelineSessionHandle {
    pub fn new() -> Self {
        Self {
            running: Arc::new(AtomicBool::new(true)),
            stop_requested: Arc::new(AtomicBool::new(false)),
            vad_active: Arc::new(AtomicBool::new(false)),
        }
    }

    /// 当前本地 VAD 是否处于语音段。
    pub fn vad_active(&self) -> bool {
        self.vad_active.load(Ordering::Relaxed)
    }

    /// 会话停止/失败后把指示器复位。
    pub fn reset_vad(&self) {
        self.vad_active.store(false, Ordering::Relaxed);
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

impl Default for PipelineSessionHandle {
    fn default() -> Self {
        Self::new()
    }
}

/// 启动并运行一个实时会话，直到停止或出错。
///
/// 正常停止返回 `Ok(())` 且已发出 `StateChanged(Idle)`；启动/运行失败返回 `Err`，
/// 由调用方（`app::start_session` 里的包装任务）转成 `Error` + `Failed` 事件。
pub async fn run(
    config: AppConfig,
    events: UnboundedSender<PipelineEvent>,
    repaint: eframe::egui::Context,
    handle: PipelineSessionHandle,
) -> Result<()> {
    let emit = |event: PipelineEvent| {
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

    // ASR 任务
    let (tx_asr, mut rx_asr) = mpsc::channel(100);
    let asr_gate = if config.vad.enabled {
        Some(vad::SpeechGate::new(vad::GateConfig {
            vad: vad::VadConfig {
                margin_db: config.vad.margin_db,
                hangover_ms: config.vad.hangover_ms,
                ..Default::default()
            },
            tail_ms: config.vad.tail_ms,
            debug: config.vad.debug,
            // 把语音段状态发布给 GUI 顶栏的 VAD 指示器
            active_flag: Some(handle.vad_active.clone()),
            ..Default::default()
        }))
    } else {
        None
    };
    {
        let events = events.clone();
        let repaint = repaint.clone();
        let running = handle.running.clone();
        let user_stopped = handle.stop_requested.clone();
        let api_key = api_key.clone();
        let model = config.asr.model.clone();
        let ws_url = inference_ws_url.clone();
        let language_hints = config.asr.languages.clone();
        tasks.push(tokio::spawn(async move {
            if let Err(err) = QwenAsrClient::run_duplex(
                api_key,
                model,
                ws_url,
                language_hints,
                asr_gate,
                rx_mic,
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
        }));
    }

    // TTS 任务
    let (tx_tts, rx_tts) = mpsc::channel::<TtsChunkRequest>(100);
    {
        let events = events.clone();
        let repaint = repaint.clone();
        let running = handle.running.clone();
        let user_stopped = handle.stop_requested.clone();
        let tts_config = QwenTtsConfig {
            api_key,
            tts_model: config.tts.model.clone(),
            ws_url: inference_ws_url,
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

    emit(PipelineEvent::StateChanged(SessionState::Running));

    // 调度中枢：ASR 事件 -> 句子级语言判定 -> 分块 -> 带 voice/语言推入 TTS
    let mut chunker = TextChunker::new(config.chunking.words);
    let mut language_state = SentenceLanguageState::new(config.asr.languages.clone());
    // 当前 TTS task 的语言（首个 chunk 送出时固定，task 结束后清空）。
    // TTS 每个 task 只认首 chunk 的 voice/language_hints，所以界面字幕语言必须用它，
    // 而不是后续 chunk 的语言判定结果（否则显示与实际合成不一致）。
    let mut tts_task_language: Option<String> = None;
    // 上一次推给界面的 VAD 状态（避免每 100ms 都发事件）
    let mut last_vad_active = false;

    loop {
        let maybe_event = tokio::select! {
            event = rx_asr.recv() => event,
            _ = tokio::time::sleep(CANCEL_POLL_INTERVAL) => {
                if handle.is_cancelled() {
                    break;
                }
                // 静音期间没有 ASR 事件，也要把 VAD 状态变化推给界面
                let vad_active = handle.vad_active();
                if vad_active != last_vad_active {
                    last_vad_active = vad_active;
                    emit(PipelineEvent::VadActive(vad_active));
                }
                continue;
            }
        };

        let Some(event) = maybe_event else {
            break;
        };

        language_state.on_event(&event.text, event.is_sentence_end);

        let chunks = chunker.process_stream(&event.text, event.is_sentence_end);
        let mut sent_sentence_end = false;
        for chunk in chunks {
            if handle.is_cancelled() {
                break;
            }

            let language = language_state.language_for_chunk();
            // TTS 在 task 首个 chunk 时固定 voice / language_hints，后续 chunk 的这两个
            // 字段会被忽略；界面语言因此取 task 首 chunk 的语言，保证显示与实际合成一致。
            if tts_task_language.is_none() {
                tts_task_language = Some(language.clone());
            }
            let voice_id = voice.voice_for(&language).to_string();
            let is_sentence_end = chunk.is_sentence_end;

            let req = TtsChunkRequest {
                text: chunk.chunk_text,
                is_sentence_end,
                voice: voice_id,
                language,
            };
            if tx_tts.send(req).await.is_err() {
                // TTS 任务已退出：结束会话
                break;
            }
            if is_sentence_end {
                sent_sentence_end = true;
            }
            language_state.on_chunk(is_sentence_end);
        }

        // 句终但本事件没有可送的新文本（chunker 已消费完）：TTS 侧 task 还开着，
        // 补一个「只结束、不追加文本」的请求，避免下一句的音色/语言漏进旧 task。
        if event.is_sentence_end && !sent_sentence_end {
            if let Some(language) = tts_task_language.clone() {
                let req = TtsChunkRequest {
                    text: String::new(),
                    is_sentence_end: true,
                    voice: voice.voice_for(&language).to_string(),
                    language,
                };
                if tx_tts.send(req).await.is_ok() {
                    sent_sentence_end = true;
                }
            }
        }

        // 带上「已送入 TTS 的字节前缀」：界面把这段显示为正常色，其余显示为弱色；
        // language 取当前 TTS task 实际使用的语言（首 chunk 决定），与实际合成严格一致。
        emit(PipelineEvent::Asr {
            text: event.text.clone(),
            is_sentence_end: event.is_sentence_end,
            sent_bytes: chunker.emitted_byte_len(&event.text),
            language: tts_task_language.clone(),
            sentence_id: event.sentence_id,
        });

        if sent_sentence_end {
            tts_task_language = None;
        }
        language_state.on_event_end(event.is_sentence_end);

        // 处理 ASR 事件后同样检查 VAD 状态变化（语音段起止）
        let vad_active = handle.vad_active();
        if vad_active != last_vad_active {
            last_vad_active = vad_active;
            emit(PipelineEvent::VadActive(vad_active));
        }

        if handle.is_cancelled() {
            break;
        }
    }

    // 收尾（D13）：先停采集，再关 TTS 通道，最后等后台任务退出。
    handle.abort_background();
    handle.reset_vad();
    drop(capture);
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

/// 句子级语言状态机。
///
/// 一个 TTS task（直到 `is_sentence_end` 的 chunk 序列）在**首个 chunk 送出前**判定一次语言，
/// 之后整句沿用（不等待整句、不回溯）；空文本 + 句终视为 ASR 重置信号。
struct SentenceLanguageState {
    tracker: LanguageTracker,
    /// 最近一次 ASR 给出的当前句完整文本（ASR 可能持续修订同一句）
    sentence_text: String,
    /// 当前 TTS task 是否已开始
    task_active: bool,
}

impl SentenceLanguageState {
    fn new(languages: Vec<String>) -> Self {
        Self {
            tracker: LanguageTracker::new(languages),
            sentence_text: String::new(),
            task_active: false,
        }
    }

    /// 处理一条 ASR 事件（在分块前调用）。
    fn on_event(&mut self, text: &str, is_sentence_end: bool) {
        if text.is_empty() {
            if is_sentence_end {
                self.sentence_text.clear();
                self.task_active = false;
            }
        } else {
            self.sentence_text.clear();
            self.sentence_text.push_str(text);
        }
    }

    /// 当前 chunk 应使用的语言；task 首个 chunk 时判定并冻结。
    fn language_for_chunk(&mut self) -> String {
        if self.task_active {
            self.tracker.current().to_string()
        } else {
            self.task_active = true;
            self.tracker.decide(&self.sentence_text).to_string()
        }
    }

    /// 处理一条已送出的 chunk（`is_sentence_end` 时 task 结束）。
    fn on_chunk(&mut self, is_sentence_end: bool) {
        if is_sentence_end {
            self.task_active = false;
        }
    }

    /// 处理 ASR 事件结束（句终时确保 task 已复位）。
    fn on_event_end(&mut self, is_sentence_end: bool) {
        if is_sentence_end {
            self.task_active = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn language_state(languages: &[&str]) -> SentenceLanguageState {
        SentenceLanguageState::new(languages.iter().map(|lang| lang.to_string()).collect())
    }

    /// 模拟一个完整 task：返回本 task 首个 chunk 的语言。
    fn task_language(state: &mut SentenceLanguageState, text: &str) -> String {
        state.on_event(text, true);
        let language = state.language_for_chunk();
        state.on_chunk(true);
        state.on_event_end(true);
        language
    }

    #[test]
    fn switches_language_between_tasks() {
        let mut state = language_state(&["zh", "ja"]);
        assert_eq!(task_language(&mut state, "あ、我忘记带水壶了。"), "ja");
        // 纯汉字长句 → 切回中文
        assert_eq!(task_language(&mut state, "今天天气真不错。"), "zh");
        assert_eq!(task_language(&mut state, "我们一起去散步吧。"), "zh");
    }

    #[test]
    fn short_han_utterance_keeps_previous_language() {
        let mut state = language_state(&["zh", "ja"]);
        assert_eq!(task_language(&mut state, "あ、我忘记带水壶了。"), "ja");
        assert_eq!(task_language(&mut state, "东京"), "ja");
        assert_eq!(task_language(&mut state, "今天天气真不错。"), "zh");
    }

    #[test]
    fn language_is_frozen_within_one_task_across_revisions() {
        let mut state = language_state(&["zh", "ja"]);
        state.on_event("こんにちは", false);
        assert_eq!(state.language_for_chunk(), "ja");
        // 同一 task 内 ASR 修订文本（新增假名/汉字）不应再次切换
        state.on_event("こんにちは、元気ですか。", false);
        assert_eq!(state.language_for_chunk(), "ja");
        state.on_event("こんにちは、元気ですか。", true);
        assert_eq!(state.language_for_chunk(), "ja");
        state.on_chunk(true);
        state.on_event_end(true);

        // 下一个 task 才重新判定
        assert_eq!(task_language(&mut state, "今天天气真不错。"), "zh");
    }

    #[test]
    fn empty_sentence_end_resets_task_state() {
        let mut state = language_state(&["zh", "ja"]);
        state.on_event("こんにちは", false);
        assert_eq!(state.language_for_chunk(), "ja");
        // ASR 重置信号：空文本 + 句终
        state.on_event("", true);
        state.on_event_end(true);
        // 重置后第一句不应沿用上一句的一半状态
        assert_eq!(task_language(&mut state, "Hello world"), "zh");
    }
}
