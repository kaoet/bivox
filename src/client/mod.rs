pub mod cosyvoice;
pub mod qwen_asr;
pub mod qwen_realtime;
pub mod ws;

/// 识别/翻译结果事件（ASR 与实时同传共用，经 channel 送给下游分块与 TTS）
#[derive(Debug, Clone)]
pub struct TranslationEvent {
    #[allow(dead_code)]
    pub transcription: String,
    pub translation: String,
    pub is_sentence_end: bool,
}
