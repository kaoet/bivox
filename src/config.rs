use clap::Parser;

/// 百炼业务空间专属域名默认值（华北2 北京）。
/// 形如 `{WorkspaceId}.cn-beijing.maas.aliyuncs.com`，可在控制台「业务空间详情」查看。
pub const DEFAULT_WORKSPACE_HOST: &str = "ws-e01bu1mp0vk0o909.cn-beijing.maas.aliyuncs.com";

/// 把用户传入的域名规范化为不带协议、不带路径、不带末尾斜杠的 host。
///
/// 允许 `ws-e01bu1mp0vk0o909.cn-beijing.maas.aliyuncs.com`、`wss://xxx`、
/// `https://xxx/api-ws/v1/inference` 等多种写法，统一提取 host 部分。
pub fn normalize_ws_host(raw: &str) -> String {
    let mut host = raw.trim();
    for prefix in ["wss://", "ws://", "https://", "http://"] {
        if let Some(rest) = host.strip_prefix(prefix) {
            host = rest;
            break;
        }
    }
    // 去掉可能误带的路径部分，例如 "wss://{WorkspaceId}.cn-beijing.maas.aliyuncs.com/api-ws/v1/inference"
    let host = host.split('/').next().unwrap_or(host);
    host.trim().to_string()
}

#[derive(Parser, Debug, Clone)]
#[command(
    name = "kaols",
    about = "Ultra-low-latency Speech Translation & Synthesis Client (Rust + DashScope)",
    version = "0.1.0"
)]
pub struct AppConfig {
    /// Alibaba DashScope API Key (defaults to DASHSCOPE_API_KEY environment variable)
    #[arg(short = 'k', long, env = "DASHSCOPE_API_KEY", hide_env_values = true)]
    pub api_key: Option<String>,

    /// 百炼业务空间专属域名（Host，不含 wss:// 与路径）
    /// 例：ws-e01bu1mp0vk0o909.cn-beijing.maas.aliyuncs.com
    #[arg(
        long,
        env = "DASHSCOPE_WORKSPACE_HOST",
        hide_env_values = true,
        default_value = DEFAULT_WORKSPACE_HOST
    )]
    pub workspace_host: String,

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
        default_value = "cosyvoice-v3.5-flash-bailian-c5f751276fcc47f0850d9dafd8cb9781"
    )]
    pub voice: String,

    /// Specific input audio device (microphone) name
    #[arg(long)]
    pub input_device: Option<String>,

    /// Specific output audio device (speaker / VB-Cable) name
    #[arg(long)]
    pub output_device: Option<String>,

    /// List all available audio input and output devices and exit
    #[arg(long)]
    pub list_devices: bool,

    /// 关闭本地 VAD 门控（默认开启：静音不上传，只发保活/尾部静音以节省 ASR 费用）
    #[arg(long)]
    pub no_vad: bool,

    /// 本地 VAD 起音阈值：噪声底 + 该 dB 值
    #[arg(long, default_value_t = 9.0)]
    pub vad_margin_db: f32,

    /// 本地 VAD 语音结束后的拖尾保持时长（毫秒）
    #[arg(long, default_value_t = 500)]
    pub vad_hangover_ms: u32,

    /// 语音结束后补发的尾部静音时长（毫秒），用于触发服务端 sentence_end
    #[arg(long, default_value_t = 1000)]
    pub vad_tail_ms: u32,

    /// 打印每个 chunk 的 VAD 电平/阈值/发送决策（用于现场标定阈值）
    #[arg(long)]
    pub vad_debug: bool,

    /// Word threshold for speculative chunking (default 6)
    #[arg(long, default_value_t = 6)]
    pub chunk_words: usize,
}

impl AppConfig {
    pub fn parse_with_env() -> Self {
        let _ = dotenvy::dotenv();
        Self::parse()
    }

    /// 业务空间专属域名的 WebSocket 基址，如 `wss://{WorkspaceId}.cn-beijing.maas.aliyuncs.com`
    pub fn workspace_ws_base(&self) -> String {
        format!("wss://{}", normalize_ws_host(&self.workspace_host))
    }

    /// 实时语音识别（ASR）/ 语音合成（CosyVoice）共用的 run-task 协议端点
    pub fn inference_ws_url(&self) -> String {
        format!("{}/api-ws/v1/inference", self.workspace_ws_base())
    }

    /// Realtime API 端点（Qwen-LiveTranslate / Qwen-ASR-Realtime）
    pub fn realtime_ws_url(&self, model: &str) -> String {
        format!(
            "{}/api-ws/v1/realtime?model={}",
            self.workspace_ws_base(),
            model
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_ws_host_accepts_bare_host() {
        assert_eq!(
            normalize_ws_host("ws-e01bu1mp0vk0o909.cn-beijing.maas.aliyuncs.com"),
            "ws-e01bu1mp0vk0o909.cn-beijing.maas.aliyuncs.com"
        );
    }

    #[test]
    fn normalize_ws_host_strips_scheme_and_path() {
        assert_eq!(
            normalize_ws_host(
                "wss://ws-e01bu1mp0vk0o909.cn-beijing.maas.aliyuncs.com/api-ws/v1/inference"
            ),
            "ws-e01bu1mp0vk0o909.cn-beijing.maas.aliyuncs.com"
        );
        assert_eq!(
            normalize_ws_host("  https://ws-e01bu1mp0vk0o909.cn-beijing.maas.aliyuncs.com/  "),
            "ws-e01bu1mp0vk0o909.cn-beijing.maas.aliyuncs.com"
        );
    }

    #[test]
    fn build_ws_urls_from_config() {
        let config = AppConfig::parse_from([
            "kaols",
            "--workspace-host",
            "wss://ws-test.cn-beijing.maas.aliyuncs.com/api-ws/v1/inference",
        ]);
        assert_eq!(
            config.inference_ws_url(),
            "wss://ws-test.cn-beijing.maas.aliyuncs.com/api-ws/v1/inference"
        );
        assert_eq!(
            config.realtime_ws_url("qwen3.8-livetranslate-flash-realtime"),
            "wss://ws-test.cn-beijing.maas.aliyuncs.com/api-ws/v1/realtime?model=qwen3.8-livetranslate-flash-realtime"
        );
    }

    #[test]
    fn default_workspace_host_is_used() {
        let config = AppConfig::parse_from(["kaols"]);
        assert_eq!(config.workspace_host, DEFAULT_WORKSPACE_HOST);
        assert_eq!(
            config.inference_ws_url(),
            format!("wss://{}/api-ws/v1/inference", DEFAULT_WORKSPACE_HOST)
        );
    }
}
