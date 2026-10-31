use anyhow::Result;
use colored::Colorize;
use cpal::traits::DeviceTrait;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::audio::capture::AudioCapture;
use crate::audio::device::AudioDeviceManager;
use crate::audio::playback::AudioPlaybackEngine;
use crate::audio::vad;
use crate::client::qwen_asr::QwenAsrClient;
use crate::client::qwen_tts::{QwenTtsClient, QwenTtsConfig, TtsChunkRequest};
use crate::config::AppConfig;
use crate::language::LanguageTracker;
use crate::pipeline::chunker::TextChunker;
use crate::voice::ActiveVoice;

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

pub struct PipelineOrchestrator {
    config: AppConfig,
}

impl PipelineOrchestrator {
    pub fn new(config: AppConfig) -> Self {
        Self { config }
    }

    pub async fn run(&self) -> Result<()> {
        let device_mgr = AudioDeviceManager::new();

        let api_key =
            self.config.api.key.clone().ok_or_else(|| {
                anyhow::anyhow!("未配置 API Key，请在配置文件的 [api] key 中设置")
            })?;

        // 解析生效音色：系统音色原样返回；`custom:<名字>` 会把所有语言样本并行注册好。
        let voice = crate::voice::resolve_active_voice(&self.config).await?;

        let in_dev = device_mgr.get_input_device(self.config.audio.input_device.as_deref())?;
        let in_dev_name = in_dev.name().unwrap_or_else(|_| "默认麦克风".into());

        let out_dev = device_mgr.get_output_device(self.config.audio.output_device.as_deref())?;
        let out_dev_name = out_dev.name().unwrap_or_else(|_| "默认扬声器".into());

        println!(
            "{}",
            "==================================================".dimmed()
        );
        println!(
            "{}",
            "🚀 超低延迟流式语音识别与语音合成 (Rust + 百炼)"
                .bold()
                .green()
        );
        println!("🎤 输入设备: {}", in_dev_name.cyan());
        println!("🔊 输出设备: {}", out_dev_name.cyan());
        println!("🌐 运行模式: {}", "语音识别转文字 -> 语音合成".yellow());
        println!(
            "🧠 识别模型: {}",
            format!("Qwen Audio ({})", self.config.asr.model).yellow()
        );
        println!(
            "🈶 识别语言: {}",
            format!("{}（首个为默认）", self.config.asr.languages.join(", ")).yellow()
        );
        if !self.config.vad.enabled {
            println!("🎚️ 本地 VAD: {}", "关闭（全量上传）".yellow());
        } else {
            println!(
                "🎚️ 本地 VAD: {}",
                format!(
                    "开启（margin={}dB, hangover={}ms, tail={}ms, keepalive=10s）",
                    self.config.vad.margin_db, self.config.vad.hangover_ms, self.config.vad.tail_ms
                )
                .yellow()
            );
        }
        println!("🤖 合成模型: {}", self.config.tts.model.yellow());
        match &voice {
            ActiveVoice::Builtin { name } => println!("🎙️ 合成音色: {}", name.yellow()),
            ActiveVoice::Custom(custom) => println!(
                "🎙️ 合成音色: {}",
                format!(
                    "自定义「{}」语言声线: {}（默认 {}，缺语言回退第一个样本）",
                    custom.display_name,
                    custom.languages().join(", "),
                    custom.default_language
                )
                .yellow()
            ),
        }
        println!(
            "🌐 业务空间域名: {}",
            self.config.workspace_ws_base().cyan()
        );
        println!(
            "{}",
            "==================================================".dimmed()
        );
        println!("💡 正在建立百炼双向流式管道，对着麦克风说话即可体验...");
        println!("👉 按 Ctrl+C 退出程序\n");

        let is_running = Arc::new(AtomicBool::new(true));

        // 1. 初始化扬声器输出引擎 (TTS 默认 22050Hz)
        let playback_engine = AudioPlaybackEngine::new(&out_dev, 22050)?;
        let playback = playback_engine.handle();

        // 2. 初始化麦克风采集管道
        let (tx_mic, rx_mic) = mpsc::channel::<Vec<u8>>(100);
        let _capture = AudioCapture::start(&in_dev, tx_mic)?;

        // 3. 初始化语音识别客户端（本地 VAD 门控可选；language_hints 来自 [asr] languages）
        let (tx_events, mut rx_events) = mpsc::channel(100);
        let engine_key = api_key.clone();
        let engine_running = is_running.clone();
        let inference_ws_url = self.config.inference_ws_url();
        let asr_model = self.config.asr.model.clone();
        let asr_ws_url = inference_ws_url.clone();
        let asr_language_hints = self.config.asr.languages.clone();
        let asr_gate = if !self.config.vad.enabled {
            None
        } else {
            Some(vad::SpeechGate::new(vad::GateConfig {
                vad: vad::VadConfig {
                    margin_db: self.config.vad.margin_db,
                    hangover_ms: self.config.vad.hangover_ms,
                    ..Default::default()
                },
                tail_ms: self.config.vad.tail_ms,
                debug: self.config.vad.debug,
                ..Default::default()
            }))
        };

        tokio::spawn(async move {
            if let Err(e) = QwenAsrClient::run_duplex(
                engine_key,
                asr_model,
                asr_ws_url,
                asr_language_hints,
                asr_gate,
                rx_mic,
                tx_events,
                engine_running,
            )
            .await
            {
                eprintln!("Qwen ASR 运行异常: {:?}", e);
            }
        });

        // 4. 初始化 TTS 全双工双向异步流式合成服务（voice/语言按句子由调度中枢给出）
        let (tx_tts, rx_tts) = mpsc::channel::<TtsChunkRequest>(100);
        let tts_config = QwenTtsConfig {
            api_key: api_key.clone(),
            tts_model: self.config.tts.model.clone(),
            ws_url: inference_ws_url,
        };
        let tts_playback = playback.clone();
        let tts_running = is_running.clone();

        tokio::spawn(async move {
            if let Err(e) =
                QwenTtsClient::run_duplex(tts_config, rx_tts, tts_playback, tts_running).await
            {
                eprintln!("TTS 流式合成服务异常: {:?}", e);
            }
        });

        // 5. 调度中枢：接收识别事件 -> 句子级语言判定 -> 流式分块 -> 带 voice/语言推入 TTS
        let mut chunker = TextChunker::new(self.config.chunking.words);
        let mut language_state = SentenceLanguageState::new(self.config.asr.languages.clone());

        while let Some(event) = rx_events.recv().await {
            language_state.on_event(&event.text, event.is_sentence_end);

            let chunks = chunker.process_stream(&event.text, event.is_sentence_end);

            for chunk in chunks {
                let language = language_state.language_for_chunk();
                let voice_id = voice.voice_for(&language);
                let is_sentence_end = chunk.is_sentence_end;

                let req = TtsChunkRequest {
                    text: chunk.chunk_text,
                    is_sentence_end,
                    voice: voice_id,
                    language,
                };
                println!(
                    "Queue chunk lang={} voice={} end={}: {}",
                    req.language, req.voice, req.is_sentence_end, req.text
                );

                if tx_tts.send(req).await.is_err() {
                    break;
                }
                language_state.on_chunk(is_sentence_end);
            }

            language_state.on_event_end(event.is_sentence_end);
        }

        Ok(())
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
