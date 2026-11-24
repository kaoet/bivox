//! 他人线路：进程环回采集 → 本地 VAD → ASR →（可选翻译）→ 只上屏（D36/D39/D40）。
//!
//! 与 [`crate::pipeline::self_line`] 不同，这条线路没有 TTS、不写 OSC，也没有打字输入；
//! 它要处理的是「目标进程随时可能不在」：找不到进程、进程退出、采集断流都退回
//! [`OthersLineState::Waiting`]，按 1s → 2s → 4s → 8s → 10s 退避**无限重连**。这些都属于
//! 临时错误，只发 [`PipelineEvent::OthersStatus`] 与日志，不影响会话本身。
//!
//! 翻译（`[others] translate`）：复用 `self_line` 的句子级语言状态、分块器与
//! `TranslationRuntime`；译文只进时间线，不产生 TTS 请求。翻译 worker 在整个运行期
//! 只建一次，重连不重建。
//!
//! 状态跳变才发状态事件与日志：进入 `Waiting` 记一条 warn，恢复到 `Capturing` 记一条 info，
//! `Waiting` 期间的每次重试不重复记。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use anyhow::{Result, anyhow};
use eframe::egui;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::audio::process_capture::{self, ProcessCapture};
use crate::client::AsrEvent;
use crate::client::qwen_mt::QwenMtClient;
use crate::config::{TranslateGranularity, VadConfig};
use crate::events::{OthersLineState, OthersStatus, PipelineEvent, Speaker};
use crate::language::LanguageTracker;
use crate::pipeline::asr_line::{self, AsrLineParams};
use crate::pipeline::chunker::{ChunkPolicy, TextChunker};
use crate::pipeline::self_line::{
    SentenceLanguageState, TranslationRuntime, dispatch_translation_jobs,
};
use crate::pipeline::translator::{self, TranslationDone};

/// 重连退避：1s → 2s → 4s → 8s → 10s（封顶，之后一直用 10s）。
const BACKOFF: [Duration; 5] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(10),
];
/// 等待环回采集激活的上限。
const CAPTURE_READY_TIMEOUT: Duration = Duration::from_secs(10);
/// 取消/状态轮询间隔。
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// 该线路 ASR 任务收尾的上限。
const ASR_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);
/// 翻译 worker 收尾的上限。
const MT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

/// 他人线路的翻译设置（与 `[translate]` 共用模型，目标语言取 `[others] target`）。
#[derive(Clone)]
pub struct OthersTranslate {
    pub model: String,
    pub chat_url: String,
    pub target: String,
    pub granularity: TranslateGranularity,
}

/// 启动他人线路所需的参数（由会话准备好后传入）。
///
/// 监督者每次启用时 clone 一份，只替换 `running`；其余共享量（电平、降级标志）
/// 保持在 handle 上，线路重建不会丢。
#[derive(Clone)]
pub struct Params {
    pub api_key: String,
    pub model: String,
    pub ws_url: String,
    pub vocabulary_id: Option<String>,
    pub vad: VadConfig,
    /// 分块词数阈值（`[chunking] words`，翻译分块用）。
    pub chunk_words: usize,
    /// 要采集的进程（exe 文件名，含子进程）。
    pub process: String,
    /// 他人语言列表（有序，首个为默认）。
    pub languages: Vec<String>,
    /// 翻译设置；`None` = 只显示原文。
    pub translate: Option<OthersTranslate>,
    /// 会话级取消标志：置 false 时本任务尽快退出。
    pub running: Arc<AtomicBool>,
    /// 顶栏电平条共享值（f32 bits，由 asr_line 的电平 tap 写入）。
    pub level: Arc<AtomicU32>,
    /// 本线路 VAD 降级标志（只用于顶栏电平条配色）。
    pub vad_degraded: Arc<AtomicBool>,
    pub events: UnboundedSender<PipelineEvent>,
    pub repaint: egui::Context,
}

/// 一条采集会话需要的固定参数（重连时复用）。
struct LineConfig {
    api_key: String,
    model: String,
    ws_url: String,
    vocabulary_id: Option<String>,
    vad: VadConfig,
    languages: Vec<String>,
    repaint: egui::Context,
    level: Arc<AtomicU32>,
    vad_degraded: Arc<AtomicBool>,
}

/// 会话事件发送器：发事件并请求重绘，避免每处都重复写两行。
struct Emitter {
    events: UnboundedSender<PipelineEvent>,
    repaint: egui::Context,
}

impl Emitter {
    fn send(&self, event: PipelineEvent) {
        // best-effort: 界面可能已退出，失败不阻塞后台任务
        let _ = self.events.send(event);
        self.repaint.request_repaint();
    }

    fn status(
        &self,
        state: OthersLineState,
        detail: impl Into<String>,
        retry_in: Option<Duration>,
        pid: Option<u32>,
    ) {
        self.send(PipelineEvent::OthersStatus(OthersStatus {
            state,
            detail: detail.into(),
            retry_in,
            pid,
        }));
    }
}

/// 一条已经建立（或正在收尾）的采集 + 识别会话。
struct ActiveLine {
    capture: ProcessCapture,
    asr: asr_line::AsrLine,
    /// 本线路 ASR 的「继续运行」标志（退出采集时置 false）。
    line_running: Arc<AtomicBool>,
    /// ASR 任务的错误出口（只本线路可见，不让它升级为会话级错误）。
    line_errors: mpsc::UnboundedReceiver<PipelineEvent>,
    vad_active: Arc<AtomicBool>,
    vad_degraded: Arc<AtomicBool>,
}

/// 翻译模式下每条采集会话的句子状态。
struct TranslateState {
    chunker: TextChunker,
    sentence: SentenceLanguageState,
}

/// 运行他人线路，直到会话被取消。内部自己负责重连。
pub async fn run(params: Params) {
    let Params {
        api_key,
        model,
        ws_url,
        vocabulary_id,
        vad,
        chunk_words,
        process,
        languages,
        translate,
        running,
        level,
        vad_degraded,
        events,
        repaint,
    } = params;
    let emit = Emitter { events, repaint };
    let config = LineConfig {
        api_key: api_key.clone(),
        model,
        ws_url,
        vocabulary_id,
        vad,
        languages: languages.clone(),
        repaint: emit.repaint.clone(),
        level,
        vad_degraded,
    };

    // 翻译 worker：整个运行期共用一份，重连不重建。
    let (mut translation, mut rx_mt, mut mt_worker) = match &translate {
        Some(cfg) => match QwenMtClient::new(api_key, cfg.model.clone(), cfg.chat_url.clone()) {
            Ok(client) => {
                let (tx_job, done_rx, worker) =
                    translator::spawn(client, cfg.target.clone(), running.clone());
                (
                    Some(TranslationRuntime::new(cfg.target.clone(), tx_job)),
                    Some(done_rx),
                    Some(worker),
                )
            }
            Err(err) => {
                log::error!("他人语音：翻译客户端初始化失败，本次只显示原文：{err:#}");
                emit.send(PipelineEvent::Warning {
                    context: "他人语音翻译未启用".to_string(),
                    detail: format!("翻译客户端初始化失败，本次只显示原文：{err:#}"),
                });
                (None, None, None)
            }
        },
        None => (None, None, None),
    };

    let mut backoff_index = 0usize;
    // 从「失败」到「恢复」算一个失败周期：周期内只在进入 Waiting 时记一条日志。
    let mut failing = false;

    loop {
        if !running.load(Ordering::SeqCst) {
            break;
        }

        // 1) 找进程并尝试开采集。
        let failure;
        match process_capture::find_pid(&process) {
            Ok(Some(pid)) => {
                emit.status(OthersLineState::Connecting, String::new(), None, Some(pid));
                match start(pid, &config).await {
                    Ok(active) => {
                        // 成功：复位退避。
                        backoff_index = 0;
                        failing = false;
                        if let Some(runtime) = translation.as_mut() {
                            runtime.reset_sentence();
                        }
                        log::info!("他人语音：已连接进程 {process}（PID {pid}）");
                        emit.status(OthersLineState::Capturing, String::new(), None, Some(pid));
                        drive(
                            active,
                            &running,
                            &emit,
                            &languages,
                            chunk_words,
                            translate.as_ref(),
                            &mut translation,
                            &mut rx_mt,
                        )
                        .await;
                        // 用户停止会话导致的采集结束不算失败：不记告警、不发 Waiting。
                        if !running.load(Ordering::SeqCst) {
                            break;
                        }
                        failure = "采集已断开".to_string();
                    }
                    Err(err) => failure = format!("打开采集失败：{err:#}"),
                }
            }
            Ok(None) => failure = format!("未找到进程 {process}"),
            Err(err) => failure = format!("枚举进程失败：{err:#}"),
        }

        // 2) 进入等待并退避。
        let delay = BACKOFF[backoff_index.min(BACKOFF.len() - 1)];
        backoff_index = (backoff_index + 1).min(BACKOFF.len() - 1);
        if !failing {
            failing = true;
            log::warn!("他人语音：{failure}；{} 秒后重试", delay.as_secs());
        }
        emit.status(OthersLineState::Waiting, failure, Some(delay), None);

        if !sleep_cancellable(delay, &running).await {
            break;
        }
    }

    // 收尾：放掉翻译 runtime（关闭 job 通道），再等 worker 退出。
    drop(translation);
    drop(rx_mt);
    if let Some(worker) = mt_worker.take()
        && tokio::time::timeout(MT_SHUTDOWN_TIMEOUT, worker)
            .await
            .is_err()
    {
        log::warn!(
            "他人语音：等待翻译 worker 收尾超时（{}s）",
            MT_SHUTDOWN_TIMEOUT.as_secs()
        );
    }
}

/// 打开环回采集并启动识别线路；失败时返回原因（由调用方进入 Waiting）。
async fn start(pid: u32, config: &LineConfig) -> Result<ActiveLine> {
    let mut capture = ProcessCapture::start(pid)?;
    let ready = capture
        .ready
        .take()
        .ok_or_else(|| anyhow!("采集句柄缺少激活结果通道"))?;

    match tokio::time::timeout(CAPTURE_READY_TIMEOUT, ready).await {
        Ok(Ok(Ok(()))) => {}
        Ok(Ok(Err(err))) => return Err(err),
        Ok(Err(_)) => return Err(anyhow!("采集线程在激活前退出")),
        Err(_) => return Err(anyhow!("等待采集激活超时")),
    }

    let chunks = capture
        .take_chunks()
        .ok_or_else(|| anyhow!("采集句柄缺少音频块接收端"))?;

    let vad_active = Arc::new(AtomicBool::new(false));
    let vad_reset = Arc::new(AtomicBool::new(false));
    let vad_degraded = config.vad_degraded.clone();
    vad_degraded.store(false, Ordering::Relaxed);
    let line_running = Arc::new(AtomicBool::new(true));
    // ASR 的致命错误只触发本线路重连，不升级为会话级错误（D40）。
    let (line_tx, line_errors) = mpsc::unbounded_channel();

    let asr = asr_line::spawn(AsrLineParams {
        api_key: config.api_key.clone(),
        model: &config.model,
        ws_url: &config.ws_url,
        language_hints: config.languages.clone(),
        vocabulary_id: config.vocabulary_id.clone(),
        vad: &config.vad,
        audio_rx: chunks,
        level: config.level.clone(),
        vad_active: vad_active.clone(),
        vad_reset,
        vad_degraded: vad_degraded.clone(),
        running: line_running.clone(),
        // 本线路不区分「用户主动停止」：任何错误都只用于触发重连。
        user_stopped: Arc::new(AtomicBool::new(false)),
        events: line_tx,
        repaint: config.repaint.clone(),
    })?;

    Ok(ActiveLine {
        capture,
        asr,
        line_running,
        line_errors,
        vad_active,
        vad_degraded,
    })
}

/// 采集建立后持续转发识别/翻译事件，直到采集结束 / 会话取消 / ASR 任务退出。
#[allow(clippy::too_many_arguments)]
async fn drive(
    mut active: ActiveLine,
    session_running: &Arc<AtomicBool>,
    emit: &Emitter,
    languages: &[String],
    chunk_words: usize,
    translate: Option<&OthersTranslate>,
    translation: &mut Option<TranslationRuntime>,
    rx_mt: &mut Option<UnboundedReceiver<TranslationDone>>,
) {
    let mut done = match active.capture.done.take() {
        Some(done) => done,
        None => {
            log::warn!("他人语音：采集句柄缺少结果通道，跳过本次采集");
            return;
        }
    };

    // 只上屏（不翻译）时用轻量语言跟踪；翻译模式复用 self_line 的句子状态与分块器。
    let mut tracker = LanguageTracker::new(languages.to_vec());
    let mut sentence_language: Option<String> = None;
    let mut translate_state = match (translate, translation.as_ref()) {
        (Some(cfg), Some(_)) => {
            let policy = match cfg.granularity {
                TranslateGranularity::Clause => ChunkPolicy::translation_clause(),
                TranslateGranularity::Sentence => ChunkPolicy::translation_sentence(),
            };
            Some(TranslateState {
                chunker: TextChunker::with_policy(chunk_words, policy),
                sentence: SentenceLanguageState::new(languages.to_vec()),
            })
        }
        _ => None,
    };
    let mut backlog: std::collections::VecDeque<AsrEvent> = std::collections::VecDeque::new();
    let mut last_vad_active = false;
    let mut degraded_reported = false;

    loop {
        // 句终屏障解除后，先按序处理缓存的 ASR 事件。
        if let (Some(_), Some(state), Some(runtime)) =
            (translate, translate_state.as_mut(), translation.as_mut())
            && !runtime.finish_pending
            && let Some(event) = backlog.pop_front()
        {
            if !on_asr_translated(event, emit, state, runtime, || {
                !session_running.load(Ordering::SeqCst)
            }) {
                break;
            }
            continue;
        }

        tokio::select! {
            _ = tokio::time::sleep(POLL_INTERVAL) => {
                if !session_running.load(Ordering::SeqCst) {
                    break;
                }
                let vad_active = active.vad_active.load(Ordering::Relaxed);
                if vad_active != last_vad_active {
                    last_vad_active = vad_active;
                    emit.send(PipelineEvent::VadActive {
                        speaker: Speaker::Others,
                        active: vad_active,
                    });
                }
                if active.vad_degraded.load(Ordering::Relaxed) && !degraded_reported {
                    degraded_reported = true;
                    log::warn!("他人语音：本地 VAD 推理失败，已降级为全量上传");
                    emit.send(PipelineEvent::Warning {
                        context: "他人线路的本地 VAD 推理失败".to_string(),
                        detail: "已降级为全量上传音频（会增加 ASR 费用）；重新开始会话可重试"
                            .to_string(),
                    });
                }
            }
            result = &mut done => {
                match result {
                    Ok(Ok(())) => log::debug!("他人语音：采集线程正常结束"),
                    Ok(Err(err)) => log::warn!("他人语音：采集结束：{err:#}"),
                    Err(_) => log::warn!("他人语音：采集线程未回传结果就退出了"),
                }
                break;
            }
            event = async {
                match active.asr.events.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending::<Option<AsrEvent>>().await,
                }
            } => {
                match event {
                    Some(event) => {
                        if let (Some(_), Some(state), Some(runtime)) = (
                            translate,
                            translate_state.as_mut(),
                            translation.as_mut(),
                        ) {
                            if runtime.finish_pending {
                                backlog.push_back(event);
                            } else {
                                if !on_asr_translated(event, emit, state, runtime, || {
                                !session_running.load(Ordering::SeqCst)
                            }) {
                                break;
                            }
                            }
                        } else {
                            forward_asr(event, emit, &mut tracker, &mut sentence_language);
                        }
                    }
                    None => break,
                }
            }
            done_mt = async {
                match rx_mt.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending::<Option<TranslationDone>>().await,
                }
            } => {
                // 翻译 worker 已退出（通道关闭）：已关闭的 `recv()` 会立刻返回 `None`，
                // 若继续 select 会每轮都命中有就绪臂而忙等，因此与 self_line 一致直接结束。
                let Some(done) = done_mt else {
                    break;
                };
                if let Some(runtime) = translation.as_mut() {
                    on_translation_done(done, emit, runtime);
                }
            }
            line_event = active.line_errors.recv() => {
                if let Some(PipelineEvent::Error { context, detail }) = line_event {
                    log::warn!("他人语音：{context}：{detail}");
                }
                break;
            }
        }
    }

    // 收尾（D13/D43）：先让 ASR 停（它自己会发 finish-task），再停采集，
    // 最后等 VAD/ASR/电平 tap 全部退出。
    let ActiveLine {
        capture,
        asr,
        line_running,
        line_errors: _,
        vad_active: _,
        vad_degraded: _,
    } = active;
    line_running.store(false, Ordering::SeqCst);
    capture.request_stop();
    drop(capture);
    emit.send(PipelineEvent::VadActive {
        speaker: Speaker::Others,
        active: false,
    });
    asr.shutdown(ASR_SHUTDOWN_TIMEOUT).await;
}

/// 翻译模式：把一条 ASR 事件并入句子状态、送翻译，并把源文交给界面。
///
/// 返回 alse 表示翻译 worker 已退出：本句之后不会再有译文，线路应当结束
/// （与我方线路统一，见 self_line::dispatch_translation_jobs）。
fn on_asr_translated(
    event: AsrEvent,
    emit: &Emitter,
    state: &mut TranslateState,
    runtime: &mut TranslationRuntime,
    cancelled: impl Fn() -> bool,
) -> bool {
    state.sentence.on_event(&event.text, event.is_sentence_end);
    if runtime.sentence_id.is_none() {
        runtime.sentence_id = event.sentence_id;
    }

    let chunks = state
        .chunker
        .process_stream(&event.text, event.is_sentence_end);
    if !dispatch_translation_jobs(chunks, runtime, &mut state.sentence, &cancelled) {
        log::warn!("他人语音：翻译 worker 已退出，停止他人线路");
        return false;
    }

    // 源文随 Asr 事件刷新（译文由 Translation 事件累积）；句终时整句都已送翻译。
    let source_text = state.sentence.current_text().to_string();
    if !source_text.is_empty() {
        let sent = if event.is_sentence_end {
            source_text.len()
        } else {
            state.chunker.emitted_byte_len(&source_text)
        };
        emit.send(PipelineEvent::Asr {
            speaker: Speaker::Others,
            text: source_text,
            is_sentence_end: false,
            sent_bytes: sent,
            language: runtime.source_language.clone(),
            sentence_id: event.sentence_id.or(runtime.sentence_id),
        });
    }

    if event.is_sentence_end {
        if runtime.pending > 0 {
            runtime.finish_pending = true;
        } else {
            emit_translation_end(emit, runtime);
            runtime.reset_sentence();
        }
    }
    state.sentence.on_event_end(event.is_sentence_end);
    true
}

/// 翻译模式：并入一条译文；屏障满足时把当前句定稿。
fn on_translation_done(done: TranslationDone, emit: &Emitter, runtime: &mut TranslationRuntime) {
    if done.generation != runtime.generation {
        return;
    }
    runtime.pending = runtime.pending.saturating_sub(1);

    match done.result {
        Ok(text) if !text.is_empty() => {
            runtime.translated.push_str(&text);
            emit.send(PipelineEvent::Translation {
                speaker: Speaker::Others,
                text: runtime.translated.clone(),
                sent_bytes: runtime.translated.len(),
                is_sentence_end: done.is_sentence_end,
                language: runtime.target_language.clone(),
                sentence_id: runtime.sentence_id,
            });
            runtime.end_event_sent = done.is_sentence_end;
        }
        Ok(_) => {}
        Err(err) => {
            log::error!("他人语音：翻译失败：{err}");
            emit.send(PipelineEvent::Error {
                context: "他人语音翻译失败".to_string(),
                detail: err,
            });
        }
    }

    if runtime.pending == 0 && (runtime.finish_pending || done.is_sentence_end) {
        if !runtime.end_event_sent {
            emit_translation_end(emit, runtime);
        }
        runtime.reset_sentence();
    }
}

/// 发出「本句译文到此为止」事件（空译文也发，避免源文行一直挂着）。
fn emit_translation_end(emit: &Emitter, runtime: &TranslationRuntime) {
    emit.send(PipelineEvent::Translation {
        speaker: Speaker::Others,
        text: runtime.translated.clone(),
        sent_bytes: runtime.translated.len(),
        is_sentence_end: true,
        language: runtime.target_language.clone(),
        sentence_id: runtime.sentence_id,
    });
}

/// 只上屏（不翻译）：把一条识别事件直接转成时间线事件。
fn forward_asr(
    event: AsrEvent,
    emit: &Emitter,
    tracker: &mut LanguageTracker,
    sentence_language: &mut Option<String>,
) {
    if event.text.is_empty() {
        if event.is_sentence_end {
            *sentence_language = None;
        }
    } else if sentence_language.is_none() {
        *sentence_language = Some(tracker.decide(&event.text).to_string());
    }

    emit.send(PipelineEvent::Asr {
        speaker: Speaker::Others,
        text: event.text.clone(),
        is_sentence_end: event.is_sentence_end,
        sent_bytes: event.text.len(),
        language: sentence_language.clone(),
        sentence_id: event.sentence_id,
    });

    if event.is_sentence_end {
        *sentence_language = None;
    }
}

/// 等待 `delay`，期间每 100ms 检查会话是否被取消；返回 `false` 表示应退出。
async fn sleep_cancellable(delay: Duration, running: &Arc<AtomicBool>) -> bool {
    let deadline = tokio::time::Instant::now() + delay;
    loop {
        if !running.load(Ordering::SeqCst) {
            return false;
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return true;
        }
        let step = (deadline - now).min(POLL_INTERVAL);
        tokio::time::sleep(step).await;
    }
}
