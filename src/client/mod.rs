pub mod cosyvoice;
pub mod qwen_asr;
pub mod ws;

/// 语音识别结果事件（ASR 经 channel 送给下游分块与 TTS）
#[derive(Debug, Clone)]
pub struct AsrEvent {
    pub text: String,
    pub is_sentence_end: bool,
}
