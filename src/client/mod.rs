pub mod qwen_asr;
pub mod qwen_tts;
pub mod ws;

/// 语音识别结果事件（ASR 经 channel 送给下游分块与 TTS）
#[derive(Debug, Clone)]
pub struct AsrEvent {
    pub text: String,
    pub is_sentence_end: bool,
    /// 服务端句子 id：同一句可能被重复下发最终结果（id 相同），界面据此去重；
    /// 两次真实发音是不同 id，即使文本相同也必须各自保留。
    pub sentence_id: Option<u64>,
}
