use anyhow::Result;
use colored::Colorize;
use cpal::traits::DeviceTrait;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::audio::capture::AudioCapture;
use crate::audio::device::AudioDeviceManager;
use crate::audio::playback::AudioPlaybackEngine;
use crate::client::cosyvoice::{CosyVoiceClient, CosyVoiceConfig, TtsChunkRequest};
use crate::client::qwen_asr::QwenAsrClient;
use crate::client::qwen_realtime::QwenRealtimeClient;
use crate::config::AppConfig;
use crate::pipeline::chunker::TextChunker;

pub struct PipelineOrchestrator {
    config: AppConfig,
}

impl PipelineOrchestrator {
    pub fn new(config: AppConfig) -> Self {
        Self { config }
    }

    pub async fn run(&self) -> Result<()> {
        let device_mgr = AudioDeviceManager::new();

        if self.config.list_devices {
            device_mgr.list_devices()?;
            return Ok(());
        }

        let api_key = self.config.api_key.clone().ok_or_else(|| {
            anyhow::anyhow!("未配置 DASHSCOPE_API_KEY，请在 .env 中设置或通过 --api-key 传入")
        })?;

        let in_dev = device_mgr.get_input_device(self.config.input_device.as_deref())?;
        let in_dev_name = in_dev.name().unwrap_or_else(|_| "默认麦克风".into());

        let out_dev = device_mgr.get_output_device(self.config.output_device.as_deref())?;
        let out_dev_name = out_dev.name().unwrap_or_else(|_| "默认扬声器".into());

        println!(
            "{}",
            "==================================================".dimmed()
        );
        println!(
            "{}",
            "🚀 超低延迟流式同传与语音合成 MVP (Rust + 百炼)"
                .bold()
                .green()
        );
        println!("🎤 输入设备: {}", in_dev_name.cyan());
        println!("🔊 输出设备: {}", out_dev_name.cyan());
        if self.config.no_translate {
            println!(
                "🌐 运行模式: {}",
                "纯语音识别转文字 -> 语音合成 (关闭翻译)".yellow()
            );
            println!(
                "🧠 识别模型: {}",
                format!("Qwen Audio ({})", self.config.asr_model).yellow()
            );
        } else {
            println!(
                "🌐 语言方向: {} -> {}",
                self.config.source_lang.yellow(),
                self.config.target_lang.yellow()
            );
            println!(
                "🧠 同传模型: {}",
                "Qwen (qwen3.8-livetranslate-flash-realtime)".yellow()
            );
        }
        println!("🤖 合成模型: {}", self.config.tts_model.yellow());
        println!("🎙️ 合成音色: {}", self.config.voice.yellow());
        if let Some(ref_path) = &self.config.ref_audio {
            println!("🧬 声音克隆: 开启 ({:?})", ref_path);
        }
        println!(
            "{}",
            "==================================================".dimmed()
        );
        println!("💡 正在建立百炼双向流式管道，对着麦克风说话即可体验...");
        println!("👉 按 Ctrl+C 退出程序\n");

        let is_running = Arc::new(AtomicBool::new(true));

        // 1. 初始化扬声器输出引擎 (CosyVoice 默认 22050Hz)
        let playback_engine = AudioPlaybackEngine::new(&out_dev, 22050)?;
        let playback = playback_engine.handle();

        // 2. 初始化麦克风采集管道
        let (tx_mic, rx_mic) = mpsc::channel::<Vec<u8>>(100);
        let _capture = AudioCapture::start(&in_dev, tx_mic)?;

        // 3. 初始化识别/同传客户端
        let (tx_events, mut rx_events) = mpsc::channel(100);
        let engine_key = api_key.clone();
        let src_lang = self.config.source_lang.clone();
        let tgt_lang = self.config.target_lang.clone();
        let engine_running = is_running.clone();

        if self.config.no_translate {
            let asr_model = self.config.asr_model.clone();
            tokio::spawn(async move {
                if let Err(e) = QwenAsrClient::run_duplex(
                    engine_key,
                    asr_model,
                    rx_mic,
                    tx_events,
                    engine_running,
                )
                .await
                {
                    eprintln!("Qwen ASR 运行异常: {:?}", e);
                }
            });
        } else {
            tokio::spawn(async move {
                if let Err(e) = QwenRealtimeClient::run_duplex(
                    engine_key,
                    src_lang,
                    tgt_lang,
                    rx_mic,
                    tx_events,
                    engine_running,
                )
                .await
                {
                    eprintln!("Qwen LiveTranslate 运行异常: {:?}", e);
                }
            });
        }

        // 4. 初始化 CosyVoice 全双工双向异步流式合成服务
        let (tx_tts, rx_tts) = mpsc::channel::<TtsChunkRequest>(100);
        let tts_config = CosyVoiceConfig {
            api_key: api_key.clone(),
            tts_model: self.config.tts_model.clone(),
            voice: self.config.voice.clone(),
            ref_audio: self.config.ref_audio.clone(),
            ref_text: self.config.ref_text.clone(),
        };
        let tts_playback = playback.clone();
        let tts_running = is_running.clone();

        tokio::spawn(async move {
            if let Err(e) =
                CosyVoiceClient::run_duplex(tts_config, rx_tts, tts_playback, tts_running).await
            {
                eprintln!("CosyVoice 流式合成服务异常: {:?}", e);
            }
        });

        // 5. 调度中枢：接收事件 -> 投机分块 -> 全双工并发推入 CosyVoice 管道
        let mut chunker = TextChunker::new(self.config.chunk_words);

        while let Some(event) = rx_events.recv().await {
            let chunks = chunker.process_stream(&event.translation, event.is_sentence_end);

            for chunk in chunks {
                let req = TtsChunkRequest {
                    text: chunk.chunk_text,
                    is_sentence_end: chunk.is_sentence_end,
                };
                println!("Queue chunk end={}: {}", req.is_sentence_end, req.text);

                if tx_tts.send(req).await.is_err() {
                    break;
                }
            }
        }

        Ok(())
    }
}
