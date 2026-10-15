use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug, Clone)]
#[command(
    name = "kaols",
    about = "Ultra-low-latency Speech Translation & Synthesis Client (Rust + DashScope)",
    version = "0.1.0"
)]
pub struct AppConfig {
    /// Alibaba DashScope API Key (defaults to DASHSCOPE_API_KEY environment variable)
    #[arg(short = 'k', long, env = "DASHSCOPE_API_KEY")]
    pub api_key: Option<String>,

    /// Source language code (e.g., zh, en, ja)
    #[arg(long, default_value = "zh")]
    pub source_lang: String,

    /// Target language code (e.g., en, ja, zh)
    #[arg(long, default_value = "en")]
    pub target_lang: String,

    /// Disable translation: directly recognize speech to text and synthesize speech
    #[arg(long)]
    pub no_translate: bool,

    /// ASR streaming model name for non-translation mode (default: qwen-audio-3.1-asr-flash-streaming)
    #[arg(long, default_value = "qwen-audio-3.1-asr-flash-streaming")]
    pub asr_model: String,

    /// CosyVoice TTS model name (default: cosyvoice-v3.5-flash)
    #[arg(long, default_value = "cosyvoice-v3.5-flash")]
    pub tts_model: String,

    /// CosyVoice preset voice name or custom cloned voice ID
    #[arg(
        long,
        default_value = "cosyvoice-v3.5-flash-bailian-561a3afc8ae64aee8a826c5611d80f66"
    )]
    pub voice: String,

    /// Path to reference audio WAV file for voice cloning (3-10 seconds)
    #[arg(long)]
    pub ref_audio: Option<PathBuf>,

    /// Transcription text of reference audio file for voice cloning
    #[arg(long)]
    pub ref_text: Option<String>,

    /// Specific input audio device (microphone) name
    #[arg(long)]
    pub input_device: Option<String>,

    /// Specific output audio device (speaker / VB-Cable) name
    #[arg(long)]
    pub output_device: Option<String>,

    /// List all available audio input and output devices and exit
    #[arg(long)]
    pub list_devices: bool,

    /// Word threshold for speculative chunking (default 6)
    #[arg(long, default_value_t = 6)]
    pub chunk_words: usize,
}

impl AppConfig {
    pub fn parse_with_env() -> Self {
        let _ = dotenvy::dotenv();
        Self::parse()
    }
}
