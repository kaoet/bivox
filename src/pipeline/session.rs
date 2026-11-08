//! 实时管线会话：把「ASR → 语言判定 → 分块 → TTS → 播放」包成可启动/停止的会话。
//!
//! 与 GUI 的边界（决策 D6/D13，见 docs/decisions.md）：
//! - 只通过 [`PipelineEvent`] 向上汇报；
//! - GUI 持有 [`PipelineSessionHandle`]（`Send + Clone`），不持有 cpal 的 `Stream`；
//! - 停止时 GUI 调 `handle.stop()`，本任务退出前会 drop `AudioCapture` 并等后台任务结束，
//!   确保麦克风/扬声器被释放。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

use crate::audio::capture::AudioCapture;
use crate::audio::device::AudioDeviceManager;
use crate::audio::playback::AudioPlaybackEngine;
use crate::audio::vad;
use crate::client::qwen_asr::QwenAsrClient;
use crate::client::qwen_mt::QwenMtClient;
use crate::client::qwen_tts::{QwenTtsClient, QwenTtsConfig, TtsChunkRequest};
use crate::client::AsrEvent;
use crate::config::{AppConfig, TranslateGranularity};
use crate::events::{PipelineEvent, SessionState};
use crate::language::{canonical_language, LanguageTracker};
use crate::pipeline::chunker::{ChunkPolicy, TextChunker};
use crate::pipeline::translator::{self, TranslationDone, TranslationJob};
use crate::voice::ActiveVoice;

/// 取消检查间隔：ASR 事件本身是事件驱动的，短轮询兼顾停止响应速度与开销。
const CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(100);
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
    /// 打字输入命令通道（GUI 提交，会话调度循环消费）。
    typed_tx: UnboundedSender<TypedInput>,
}

impl PipelineSessionHandle {
    pub fn new(typed_tx: UnboundedSender<TypedInput>) -> Self {
        Self {
            running: Arc::new(AtomicBool::new(true)),
            stop_requested: Arc::new(AtomicBool::new(false)),
            vad_active: Arc::new(AtomicBool::new(false)),
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

/// 启动并运行一个实时会话，直到停止或出错。
///
/// 正常停止返回 `Ok(())` 且已发出 `StateChanged(Idle)`；启动/运行失败返回 `Err`，
/// 由调用方（`app::start_session` 里的包装任务）转成 `Error` + `Failed` 事件。
pub async fn run(
    config: AppConfig,
    events: UnboundedSender<PipelineEvent>,
    repaint: eframe::egui::Context,
    handle: PipelineSessionHandle,
    mut typed_rx: UnboundedReceiver<TypedInput>,
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
    let mut translation: Option<TranslationRuntime> = None;
    let mut rx_mt: Option<UnboundedReceiver<TranslationDone>> = None;
    if translate.enabled {
        let target = canonical_language(&translate.target).to_string();
        let client = QwenMtClient::new(
            api_key.clone(),
            translate.model.clone(),
            config.translate_chat_url(),
        )?;
        let (tx_job, done_rx, worker) =
            translator::spawn(client, target.clone(), handle.running.clone());
        tasks.push(worker);
        let (voice_id, rate) = voice.selection_for(&target);
        translation = Some(TranslationRuntime::new(
            target,
            voice_id.to_string(),
            rate,
            tx_job,
        ));
        rx_mt = Some(done_rx);
    }

    emit(PipelineEvent::StateChanged(SessionState::Running));

    // 调度中枢：ASR 事件 -> 句子级语言判定 -> 分块 -> 带 voice/语言推入 TTS；
    // 同时接收「打字输入」命令：排队朗读，不打断播放、不清空播放队列。
    // 翻译模式按粒度选更保守的切分策略；默认路径行为不变。
    let chunker_policy = match (translate.enabled, translate.granularity) {
        (true, TranslateGranularity::Clause) => ChunkPolicy::translation_clause(),
        (true, TranslateGranularity::Sentence) => ChunkPolicy::translation_sentence(),
        (false, _) => ChunkPolicy::default(),
    };
    let mut chunker = TextChunker::with_policy(config.chunking.words, chunker_policy);
    let mut language_state = SentenceLanguageState::new(config.asr.languages.clone());
    // 当前 TTS task 的语言（首个 chunk 送出时固定，task 结束后清空）。
    // TTS 每个 task 只认首 chunk 的 voice/language_hints，所以界面字幕语言必须用它，
    // 而不是后续 chunk 的语言判定结果（否则显示与实际合成不一致）。
    let mut tts_task_language: Option<String> = None;
    // 当前 ASR 句是否仍在服务端识别中（最后一条事件不是句终）。
    let mut asr_sentence_open = false;
    // 打字输入收口当前 ASR 句后，丢弃该句剩余事件，直到服务端给出句子结束。
    let mut discard_asr_sentence = false;
    // 句终屏障期间暂存的 ASR 事件：上一句的最后一条译文还没回来时，下一句的源文
    // 不能先落到界面（否则会把「新句原文」和「旧句译文」配成一句，见 D28）。
    let mut asr_backlog: VecDeque<AsrEvent> = VecDeque::new();
    let mut typed_rx_open = true;
    // 上一次推给界面的 VAD 状态（避免每 100ms 都发事件）
    let mut last_vad_active = false;

    enum Incoming {
        Typed(Option<TypedInput>),
        Asr(Option<AsrEvent>),
        Mt(Option<TranslationDone>),
        Tick,
    }

    loop {
        // 句终屏障未完成时先不把下一句的 ASR 事件交给分块器/界面：等上一句最后一条
        // 译文回来、句终事件发出后，再按原顺序处理缓存里的事件。
        let incoming = loop {
            let barrier_pending = translation
                .as_ref()
                .is_some_and(|runtime| runtime.finish_pending);
            if !barrier_pending {
                if let Some(event) = asr_backlog.pop_front() {
                    break Incoming::Asr(Some(event));
                }
            }
            break tokio::select! {
                // 打字输入优先于还没处理的 ASR 事件：按下回车后不再多送当前句的 chunk。
                biased;
                cmd = typed_rx.recv(), if typed_rx_open => Incoming::Typed(cmd),
                // 翻译结果要先于后续 ASR 事件处理，保证 TTS 入队顺序与源文一致。
                done = async {
                    match rx_mt.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending::<Option<TranslationDone>>().await,
                    }
                } => Incoming::Mt(done),
                event = rx_asr.recv() => Incoming::Asr(event),
                _ = tokio::time::sleep(CANCEL_POLL_INTERVAL) => Incoming::Tick,
            };
        };

        match incoming {
            Incoming::Tick => {
                if handle.is_cancelled() {
                    break;
                }
                // 静音期间没有 ASR 事件，也要把 VAD 状态变化推给界面
                let vad_active = handle.vad_active();
                if vad_active != last_vad_active {
                    last_vad_active = vad_active;
                    emit(PipelineEvent::VadActive(vad_active));
                }
            }
            Incoming::Typed(None) => {
                // 发送端已释放（通常会话正在结束），不再轮询该分支。
                typed_rx_open = false;
            }
            Incoming::Typed(Some(cmd)) => {
                let mut text = cmd.text.trim().to_string();
                if let Some(chars) = truncate_typed_input(&mut text) {
                    log::warn!(
                        "打字输入超长（{chars} 字符，上限 {MAX_TYPED_INPUT_CHARS}），已截断"
                    );
                }
                if !text.is_empty() {
                    if let Some(runtime) = translation.as_mut() {
                        // 翻译模式：立即收口旧 TTS task、丢弃该句未返回的译文，
                        // 再把打字内容交给翻译 worker（结果回来后才送 TTS/字幕）。
                        if runtime.tts_open && !runtime.tts_closed {
                            let close = TtsChunkRequest {
                                text: String::new(),
                                is_sentence_end: true,
                                voice: runtime.voice.clone(),
                                rate: runtime.rate,
                                language: runtime.target_language.clone(),
                            };
                            if tx_tts.send(close).await.is_err() {
                                break;
                            }
                        }
                        // 旧句已有部分译文：先让界面把当前句归档，避免被下一句覆盖。
                        if !runtime.end_event_sent {
                            emit(PipelineEvent::Translation {
                                text: runtime.translated.clone(),
                                sent_bytes: runtime.translated.len(),
                                is_sentence_end: true,
                                language: runtime.target_language.clone(),
                                sentence_id: runtime.sentence_id,
                            });
                            runtime.end_event_sent = true;
                        }
                        runtime.bump_generation();
                        chunker.reset();
                        language_state.on_event("", true);
                        // 缓存里可能已有「上一句句终后刚开始的新句」：打字同样要打断它，
                        // 否则新句会在打字内容之后继续冒出（D24）。
                        if !asr_backlog.is_empty() {
                            asr_backlog.clear();
                            asr_sentence_open = false;
                            discard_asr_sentence = true;
                        }
                        // 只有此刻确实有 ASR 句在服务端识别中，才需要丢弃它的剩余部分；
                        // 空闲时打字输入不能把「下一句」当成被收口句丢掉（否则下一句整句
                        // 的事件都会在 discard 分支里被吞掉，界面毫无反应）。
                        if asr_sentence_open {
                            asr_sentence_open = false;
                            discard_asr_sentence = true;
                        }
                        tts_task_language = None;

                        let source_language = language_state.typed_language(&text);
                        // 先把源文交给界面：译文回来之前就能看到原文行，句终仍由 Translation 收口。
                        emit(PipelineEvent::Asr {
                            text: text.clone(),
                            is_sentence_end: false,
                            sent_bytes: text.len(),
                            language: Some(source_language.clone()),
                            sentence_id: None,
                        });
                        let skip = canonical_language(&source_language)
                            == runtime.target_language.as_str();
                        runtime.pending = 1;
                        let job = TranslationJob {
                            generation: runtime.generation,
                            seq: runtime.next_seq,
                            text,
                            source_language,
                            skip_translation: skip,
                            is_sentence_end: true,
                        };
                        runtime.next_seq += 1;
                        if runtime.tx_job.send(job).is_err() {
                            break;
                        }
                    } else {
                        // 1) 当前 ASR 句的 TTS task 还开着：先让它正常收口（空文本 + 句终）。
                        //    服务端会把已收到的文本合成完、音频继续入队；不取消 task、不清播放队列。
                        let task_language = tts_task_language.clone();
                        if let Some(language) = task_language.as_deref() {
                            let (voice_id, rate) = voice.selection_for(language);
                            let close = TtsChunkRequest {
                                text: String::new(),
                                is_sentence_end: true,
                                voice: voice_id.to_string(),
                                rate,
                                language: language.to_string(),
                            };
                            if tx_tts.send(close).await.is_err() {
                                break;
                            }
                            tts_task_language = None;
                        }

                        // 2) ASR 当前句还没结束：字幕只保留「已送 TTS 的前缀」，该句剩余部分
                        //    丢弃到服务端句终为止（期间不再送 TTS；已送前缀的音频照常播完）。
                        if asr_sentence_open {
                            let full = language_state.current_text();
                            let sent = chunker.emitted_byte_len(full);
                            let prefix = sent_prefix(full, sent).to_string();
                            if prefix.is_empty() {
                                // 一个字都还没送 TTS：当前行没有会被朗读的内容，直接清空。
                                emit(PipelineEvent::Asr {
                                    text: String::new(),
                                    is_sentence_end: true,
                                    sent_bytes: 0,
                                    language: None,
                                    sentence_id: None,
                                });
                            } else {
                                emit(PipelineEvent::Asr {
                                    text: prefix.clone(),
                                    is_sentence_end: true,
                                    sent_bytes: prefix.len(),
                                    language: task_language,
                                    sentence_id: None,
                                });
                            }
                            chunker.reset();
                            language_state.on_event("", true);
                            asr_sentence_open = false;
                            discard_asr_sentence = true;
                        }

                        // 3) 语言与 ASR 共用同一个粘性 tracker：打字输入也属于当前对话，
                        //    判定结果会回写 current，影响后续句子的语言。
                        let language = language_state.typed_language(&text);
                        let (voice_id, rate) = voice.selection_for(&language);
                        let req = TtsChunkRequest {
                            text: text.clone(),
                            is_sentence_end: true,
                            voice: voice_id.to_string(),
                            rate,
                            language: language.clone(),
                        };
                        if tx_tts.send(req).await.is_ok() {
                            emit(PipelineEvent::TypedSentence { text, language });
                        } else {
                            break;
                        }
                    }
                }
                if handle.is_cancelled() {
                    break;
                }
            }
            Incoming::Asr(None) => {
                break;
            }
            Incoming::Asr(Some(event)) => {
                // 上一句还在等最后一条译文：先把事件缓存，句终屏障完成后按序补处理，
                // 避免新句源文先于旧句句终译文落到界面（D28）。
                if translation
                    .as_ref()
                    .is_some_and(|runtime| runtime.finish_pending)
                {
                    asr_backlog.push_back(event);
                    continue;
                }
                if discard_asr_sentence {
                    // 收口后丢弃当前句剩余内容；句终后恢复正常处理（不补读被丢弃部分）。
                    if event.is_sentence_end {
                        discard_asr_sentence = false;
                        asr_sentence_open = false;
                    }
                    continue;
                }

                if let Some(runtime) = translation.as_mut() {
                    // 翻译模式：源文只做语言判定与分块，译文回传后才送 TTS 与字幕；
                    // 源文本身不作为主字幕输出，但会随 Asr 事件更新「原文行」（D26）。
                    language_state.on_event(&event.text, event.is_sentence_end);
                    if runtime.sentence_id.is_none() {
                        runtime.sentence_id = event.sentence_id;
                    }
                    let chunks = chunker.process_stream(&event.text, event.is_sentence_end);
                    for chunk in chunks {
                        if handle.is_cancelled() {
                            break;
                        }
                        let source_language = language_state.language_for_chunk();
                        if runtime.source_language.is_none() {
                            runtime.source_language = Some(source_language.clone());
                        }
                        // 源语言与目标语言相同（含中文句子切到中文模式）：跳过 MT，直送 TTS。
                        let skip = canonical_language(&source_language)
                            == runtime.target_language.as_str();
                        let job = TranslationJob {
                            generation: runtime.generation,
                            seq: runtime.next_seq,
                            text: chunk.chunk_text,
                            source_language,
                            skip_translation: skip,
                            is_sentence_end: chunk.is_sentence_end,
                        };
                        runtime.next_seq = runtime.next_seq.wrapping_add(1);
                        runtime.pending = runtime.pending.saturating_add(1);
                        if runtime.tx_job.send(job).is_err() {
                            break;
                        }
                        language_state.on_chunk(chunk.is_sentence_end);
                    }

                    // 把源文交给界面（Translation 事件只带译文）；句终时整句都已送翻译，
                    // 前缀置满，避免原文行一直显示「未翻译」的弱色尾巴。
                    let source_text = language_state.current_text().to_string();
                    if !source_text.is_empty() {
                        let sent = if event.is_sentence_end {
                            source_text.len()
                        } else {
                            chunker.emitted_byte_len(&source_text)
                        };
                        emit(PipelineEvent::Asr {
                            text: source_text,
                            is_sentence_end: false,
                            sent_bytes: sent,
                            language: runtime.source_language.clone(),
                            sentence_id: event.sentence_id.or(runtime.sentence_id),
                        });
                    }

                    // 句终：还有译文在途就等最后一条回来再收口 TTS task（句终屏障）；
                    // 没有在途译文则立即收口，避免 task 跨句。
                    if event.is_sentence_end {
                        if runtime.pending > 0 {
                            runtime.finish_pending = true;
                        } else {
                            if runtime.tts_open && !runtime.tts_closed {
                                let req = TtsChunkRequest {
                                    text: String::new(),
                                    is_sentence_end: true,
                                    voice: runtime.voice.clone(),
                                    rate: runtime.rate,
                                    language: runtime.target_language.clone(),
                                };
                                if tx_tts.send(req).await.is_err() {
                                    break;
                                }
                                runtime.tts_closed = true;
                            }
                            if !runtime.end_event_sent {
                                // 即使没有任何译文（例如全部翻译失败）也要收口当前句，
                                // 否则界面上的源文行会一直挂着。
                                emit(PipelineEvent::Translation {
                                    text: runtime.translated.clone(),
                                    sent_bytes: runtime.translated.len(),
                                    is_sentence_end: true,
                                    language: runtime.target_language.clone(),
                                    sentence_id: runtime.sentence_id,
                                });
                                runtime.end_event_sent = true;
                            }
                            runtime.reset_sentence();
                        }
                    }
                    language_state.on_event_end(event.is_sentence_end);
                    asr_sentence_open = !event.is_sentence_end;
                } else {
                    language_state.on_event(&event.text, event.is_sentence_end);

                    let chunks = chunker.process_stream(&event.text, event.is_sentence_end);
                    let mut sent_sentence_end = false;
                    for chunk in chunks {
                        if handle.is_cancelled() {
                            break;
                        }

                        let language = language_state.language_for_chunk();
                        // TTS 在 task 首个 chunk 时固定 voice / rate / language_hints，后续 chunk 的
                        // 这些字段会被忽略；界面语言因此取 task 首 chunk 的语言，保证显示与实际合成一致。
                        if tts_task_language.is_none() {
                            tts_task_language = Some(language.clone());
                        }
                        let (voice_id, rate) = voice.selection_for(&language);
                        let voice_id = voice_id.to_string();
                        let is_sentence_end = chunk.is_sentence_end;

                        let req = TtsChunkRequest {
                            text: chunk.chunk_text,
                            is_sentence_end,
                            voice: voice_id,
                            rate,
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
                            let (voice_id, rate) = voice.selection_for(&language);
                            let req = TtsChunkRequest {
                                text: String::new(),
                                is_sentence_end: true,
                                voice: voice_id.to_string(),
                                rate,
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
                    asr_sentence_open = !event.is_sentence_end;
                }

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
            Incoming::Mt(None) => {
                // 翻译 worker 已退出（通常发生在停止会话、命令通道关闭之后）。
                break;
            }
            Incoming::Mt(Some(done)) => {
                let Some(runtime) = translation.as_mut() else {
                    continue;
                };
                // 打字会 bump generation：旧句在途译文直接丢弃，避免串到新句。
                if done.generation != runtime.generation {
                    continue;
                }
                runtime.pending = runtime.pending.saturating_sub(1);

                let text = match done.result {
                    Ok(text) => text,
                    Err(err) => {
                        log::error!("翻译失败：{err}");
                        emit(PipelineEvent::Error {
                            context: "翻译失败".to_string(),
                            detail: err,
                        });
                        String::new()
                    }
                };

                if !text.is_empty() {
                    runtime.translated.push_str(&text);
                    // TTS 与字幕只用译文；语种固定为目标语言，音色按目标语言选择。
                    let req = TtsChunkRequest {
                        text: text.clone(),
                        is_sentence_end: done.is_sentence_end,
                        voice: runtime.voice.clone(),
                        rate: runtime.rate,
                        language: runtime.target_language.clone(),
                    };
                    if tx_tts.send(req).await.is_err() {
                        break;
                    }
                    runtime.tts_open = true;
                    runtime.tts_closed = done.is_sentence_end;
                    emit(PipelineEvent::Translation {
                        text: runtime.translated.clone(),
                        sent_bytes: runtime.translated.len(),
                        is_sentence_end: done.is_sentence_end,
                        language: runtime.target_language.clone(),
                        sentence_id: runtime.sentence_id,
                    });
                    runtime.end_event_sent = done.is_sentence_end;
                } else if done.is_sentence_end && runtime.tts_open && !runtime.tts_closed {
                    // 译文为空也要收口：发「只结束」请求，让 TTS 侧正常 finish-task。
                    let req = TtsChunkRequest {
                        text: String::new(),
                        is_sentence_end: true,
                        voice: runtime.voice.clone(),
                        rate: runtime.rate,
                        language: runtime.target_language.clone(),
                    };
                    if tx_tts.send(req).await.is_err() {
                        break;
                    }
                    runtime.tts_closed = true;
                    emit(PipelineEvent::Translation {
                        text: runtime.translated.clone(),
                        sent_bytes: runtime.translated.len(),
                        is_sentence_end: true,
                        language: runtime.target_language.clone(),
                        sentence_id: runtime.sentence_id,
                    });
                    runtime.end_event_sent = true;
                }

                // 句终屏障：源文已句终且最后一条译文刚回来，补一个收口请求并复位本句状态。
                if runtime.pending == 0 && (runtime.finish_pending || done.is_sentence_end) {
                    if runtime.tts_open && !runtime.tts_closed {
                        let req = TtsChunkRequest {
                            text: String::new(),
                            is_sentence_end: true,
                            voice: runtime.voice.clone(),
                            rate: runtime.rate,
                            language: runtime.target_language.clone(),
                        };
                        if tx_tts.send(req).await.is_err() {
                            break;
                        }
                        runtime.tts_closed = true;
                    }
                    if !runtime.end_event_sent {
                        emit(PipelineEvent::Translation {
                            text: runtime.translated.clone(),
                            sent_bytes: runtime.translated.len(),
                            is_sentence_end: true,
                            language: runtime.target_language.clone(),
                            sentence_id: runtime.sentence_id,
                        });
                        runtime.end_event_sent = true;
                    }
                    runtime.reset_sentence();
                }

                if handle.is_cancelled() {
                    break;
                }
            }
        }
    }

    // 收尾（D13）：先停采集，再关翻译/TTS 通道，最后等后台任务退出。
    handle.abort_background();
    handle.reset_vad();
    drop(capture);
    drop(rx_mt);
    drop(translation);
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

/// 截取「已送 TTS」的字节前缀：`emitted_byte_len` 保证是字符边界，这里再防御一次。
fn sent_prefix(text: &str, sent_bytes: usize) -> &str {
    let mut end = sent_bytes.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// 界面已校验超长；这里对异常调用做防御性截断（按字符边界），返回原始字符数。
fn truncate_typed_input(text: &mut String) -> Option<usize> {
    let chars = text.chars().count();
    if chars <= MAX_TYPED_INPUT_CHARS {
        return None;
    }
    let cut = text
        .char_indices()
        .nth(MAX_TYPED_INPUT_CHARS)
        .map(|(index, _)| index)
        .unwrap_or(text.len());
    text.truncate(cut);
    Some(chars)
}

/// 翻译模式的会话内状态：目标语言固定，一句内的译文按到达顺序累积。
struct TranslationRuntime {
    target_language: String,
    voice: String,
    rate: f32,
    tx_job: UnboundedSender<TranslationJob>,
    /// 打字打断时 +1：旧 generation 的在途译文直接丢弃，避免串到新句。
    generation: u64,
    next_seq: u64,
    pending: u32,
    /// 本句已生成的译文（与已送 TTS 的前缀一致）。
    translated: String,
    tts_open: bool,
    tts_closed: bool,
    end_event_sent: bool,
    sentence_id: Option<u64>,
    /// 本句源语言（首个翻译 chunk 判定后冻结；用于原文行语言列）。
    source_language: Option<String>,
    /// 源文已句终，等最后一条译文回来再收口 TTS task。
    finish_pending: bool,
}

impl TranslationRuntime {
    fn new(
        target_language: String,
        voice: String,
        rate: f32,
        tx_job: UnboundedSender<TranslationJob>,
    ) -> Self {
        Self {
            target_language,
            voice,
            rate,
            tx_job,
            generation: 0,
            next_seq: 0,
            pending: 0,
            translated: String::new(),
            tts_open: false,
            tts_closed: false,
            end_event_sent: false,
            sentence_id: None,
            source_language: None,
            finish_pending: false,
        }
    }

    /// 打字打断：作废在途译文并清空当前句状态。
    fn bump_generation(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.reset_sentence();
    }

    fn reset_sentence(&mut self) {
        self.next_seq = 0;
        self.pending = 0;
        self.translated.clear();
        self.tts_open = false;
        self.tts_closed = false;
        self.end_event_sent = false;
        self.sentence_id = None;
        self.source_language = None;
        self.finish_pending = false;
    }
}

/// 句子级语言状态机。
///
/// 一个 TTS task（直到 `is_sentence_end` 的 chunk 序列）在**首个 chunk 送出前**判定一次语言，
/// 之后整句沿用（不等待整句、不回溯）；空文本 + 句终视为 ASR 重置信号。
///
/// 打字输入与 ASR 共用同一个 [`LanguageTracker`]：它继承当前对话的粘性语言，
/// 并把判定结果回写 `current`，影响后续句子（见 D24）。
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

    /// 当前 ASR 句的完整文本（可能仍在修订）。
    fn current_text(&self) -> &str {
        &self.sentence_text
    }

    /// 打字输入句的语言：沿用当前对话的粘性语言，并把结果回写 `current`。
    fn typed_language(&mut self, text: &str) -> String {
        self.tracker.decide(text).to_string()
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

    #[test]
    fn typed_language_inherits_and_updates_sticky_language() {
        let mut state = language_state(&["zh", "ja", "en"]);
        assert_eq!(task_language(&mut state, "あ、我忘记带水壶了。"), "ja");
        // 打字输入“东京”：纯汉字短句证据不足 → 沿用当前对话的日语
        assert_eq!(state.typed_language("东京"), "ja");
        // 打字输入英文句：证据足够 → 回写 current，后续句子按英语上下文延续
        assert_eq!(state.typed_language("Hello world, this is a test"), "en");
        // 证据不足的短文本保持回写后的 en
        assert_eq!(state.typed_language("OK"), "en");
    }

    #[test]
    fn sent_prefix_respects_char_boundary() {
        let text = "你好世界";
        assert_eq!(sent_prefix(text, 6), "你好");
        // 非字符边界（“好”的中间）回退到上一个字符边界
        assert_eq!(sent_prefix(text, 5), "你");
        assert_eq!(sent_prefix(text, 0), "");
        assert_eq!(sent_prefix(text, 999), text);
    }

    #[test]
    fn overlong_typed_input_is_truncated_at_char_boundary() {
        let mut ok = "短句".to_string();
        assert_eq!(truncate_typed_input(&mut ok), None);
        assert_eq!(ok, "短句");

        // 多字节汉字：按“字符”截断，不会被截在字节中间导致 panic
        let mut long = "字".repeat(MAX_TYPED_INPUT_CHARS + 1);
        assert_eq!(
            truncate_typed_input(&mut long),
            Some(MAX_TYPED_INPUT_CHARS + 1)
        );
        assert_eq!(long.chars().count(), MAX_TYPED_INPUT_CHARS);
        assert!(long.is_char_boundary(long.len()));
    }
}
