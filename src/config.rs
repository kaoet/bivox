use anyhow::{bail, Context, Result};
use clap::Parser;
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// 百炼默认接入域名（通用域名）。
///
/// 如使用业务空间专属域名，可在 `[api] workspace_host` 中覆盖，形如
/// `{WorkspaceId}.cn-beijing.maas.aliyuncs.com`（控制台「业务空间详情」可查看 WorkspaceId）。
pub const DEFAULT_WORKSPACE_HOST: &str = "dashscope.aliyuncs.com";

/// 实时识别模型默认值。
pub const DEFAULT_ASR_MODEL: &str = "qwen-audio-3.1-asr-flash-streaming";

/// 合成模型默认值（Qwen-Audio-3.1-TTS-Flash，低延迟实时语音合成）。
pub const DEFAULT_TTS_MODEL: &str = "qwen-audio-3.1-tts-flash";

/// 合成音色默认值（Qwen-Audio-TTS 系统音色：乔小娇，俏丽可爱女声）。
pub const DEFAULT_VOICE: &str = "qiaoxiaojiao_v3.1";

/// 本地 VAD 默认起音余量（dB）。
pub const DEFAULT_VAD_MARGIN_DB: f32 = 9.0;

/// 本地 VAD 默认拖尾保持时长（毫秒）。
pub const DEFAULT_VAD_HANGOVER_MS: u32 = 500;

/// 本地 VAD 默认句末补发静音时长（毫秒）。
pub const DEFAULT_VAD_TAIL_MS: u32 = 1000;

/// 默认投机分块词数阈值。
pub const DEFAULT_CHUNK_WORDS: usize = 6;

/// 命令行参数。
#[derive(Debug, Parser)]
#[command(
    name = "kaols",
    about = "Ultra-low-latency Speech Recognition & Synthesis Client (Rust + DashScope)",
    version = "0.1.0"
)]
pub struct CliArgs {
    /// TOML 配置文件路径
    #[arg(short = 'c', long, value_name = "path", default_value = "config.toml")]
    pub config: PathBuf,

    /// 列出所有可用音频输入/输出设备后退出（无需配置文件）
    #[arg(long)]
    pub list_devices: bool,
}

/// 应用配置，对应 `config.toml` 的顶层结构。
///
/// 所有字段都有默认值：最小的配置文件只需要提供 `[api] key`。
/// 同时开启 `deny_unknown_fields`，字段名拼错会直接解析失败，避免静默使用默认值。
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct AppConfig {
    pub api: ApiConfig,
    pub asr: AsrConfig,
    pub tts: TtsConfig,
    pub audio: AudioConfig,
    pub vad: VadConfig,
    pub chunking: ChunkingConfig,
}

/// `[api]`：鉴权与业务空间域名。
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ApiConfig {
    /// 百炼 API Key（原 `DASHSCOPE_API_KEY` 环境变量）。
    pub key: Option<String>,

    /// 业务空间专属域名 Host（不含 `wss://` 与路径）。
    pub workspace_host: String,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            key: None,
            workspace_host: DEFAULT_WORKSPACE_HOST.to_string(),
        }
    }
}

/// `[asr]`：实时语音识别。
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AsrConfig {
    pub model: String,
}

impl Default for AsrConfig {
    fn default() -> Self {
        Self {
            model: DEFAULT_ASR_MODEL.to_string(),
        }
    }
}

/// `[tts]`：语音合成。
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TtsConfig {
    pub model: String,
    pub voice: String,
}

impl Default for TtsConfig {
    fn default() -> Self {
        Self {
            model: DEFAULT_TTS_MODEL.to_string(),
            voice: DEFAULT_VOICE.to_string(),
        }
    }
}

/// `[audio]`：输入/输出音频设备。
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct AudioConfig {
    /// 输入设备（麦克风）名称；`None` 表示系统默认设备。
    pub input_device: Option<String>,

    /// 输出设备（扬声器 / 虚拟声卡）名称；`None` 表示系统默认设备。
    pub output_device: Option<String>,
}

/// `[vad]`：本地 VAD 发送门控。
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VadConfig {
    /// 是否启用本地 VAD 门控（关闭后全量上传音频）。
    pub enabled: bool,

    /// 起音阈值 = 噪声底 + 该 dB 值。
    pub margin_db: f32,

    /// 语音结束后的拖尾保持时长（毫秒）。
    pub hangover_ms: u32,

    /// 语音结束后补发的尾部静音时长（毫秒），用于触发服务端 sentence_end。
    pub tail_ms: u32,

    /// 打印每个 chunk 的 VAD 电平/阈值/发送决策，用于现场标定阈值。
    pub debug: bool,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            margin_db: DEFAULT_VAD_MARGIN_DB,
            hangover_ms: DEFAULT_VAD_HANGOVER_MS,
            tail_ms: DEFAULT_VAD_TAIL_MS,
            debug: false,
        }
    }
}

/// `[chunking]`：流式文本分块。
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ChunkingConfig {
    /// 投机分块词数阈值。
    pub words: usize,
}

impl Default for ChunkingConfig {
    fn default() -> Self {
        Self {
            words: DEFAULT_CHUNK_WORDS,
        }
    }
}

impl AppConfig {
    /// 从指定路径加载配置。
    ///
    /// 文件不存在或内容非法时返回带路径上下文的错误，便于定位问题。
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| {
            format!(
                "读取配置文件 {} 失败；请复制 config.example.toml 为 config.toml 并填写 [api] key",
                path.display()
            )
        })?;
        Self::from_toml_str(&text).with_context(|| format!("配置文件 {} 无效", path.display()))
    }

    /// 解析 TOML 文本，完成空值归一化与合法性校验（便于单元测试）。
    pub fn from_toml_str(text: &str) -> Result<Self> {
        let mut config: Self = toml::from_str(text).context("解析 TOML 失败")?;
        config.normalize();
        config.validate()?;
        Ok(config)
    }

    /// 把空字符串归一化为 `None`。
    fn normalize(&mut self) {
        self.api.key = normalize_optional(self.api.key.take());
        self.audio.input_device = normalize_optional(self.audio.input_device.take());
        self.audio.output_device = normalize_optional(self.audio.output_device.take());
    }

    /// 校验必填项与取值范围。
    fn validate(&self) -> Result<()> {
        if self.api.key.is_none() {
            bail!("未配置 API Key，请在配置文件的 [api] key 中设置");
        }
        validate_workspace_host(&self.api.workspace_host)?;
        if self.asr.model.trim().is_empty() {
            bail!("配置项 [asr] model 不能为空");
        }
        if self.tts.model.trim().is_empty() {
            bail!("配置项 [tts] model 不能为空");
        }
        if self.tts.voice.trim().is_empty() {
            bail!("配置项 [tts] voice 不能为空");
        }
        if self.chunking.words == 0 {
            bail!("配置项 [chunking] words 必须大于 0");
        }
        if !self.vad.margin_db.is_finite() {
            bail!("配置项 [vad] margin_db 必须是有效数字");
        }
        Ok(())
    }

    /// `[api] workspace_host` 对应的 WebSocket 基址，如 `wss://dashscope.aliyuncs.com`
    pub fn workspace_ws_base(&self) -> String {
        format!("wss://{}", self.api.workspace_host)
    }

    /// 实时语音识别（ASR）/ 语音合成（TTS）共用的 run-task 协议端点
    pub fn inference_ws_url(&self) -> String {
        format!("{}/api-ws/v1/inference", self.workspace_ws_base())
    }
}

/// 去掉首尾空白；空字符串视为未配置。
fn normalize_optional(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// 校验 `[api] workspace_host` 是否已是规范化 host：
/// 只允许纯域名（字母/数字/连字符/点），不允许协议前缀、路径、端口与空白字符。
fn validate_workspace_host(host: &str) -> Result<()> {
    if host.is_empty() {
        bail!("配置项 [api] workspace_host 不能为空");
    }
    if host.chars().any(char::is_whitespace) {
        bail!("配置项 [api] workspace_host 不能包含空白字符: {host:?}");
    }
    if host.contains("://") {
        bail!("配置项 [api] workspace_host 不能带协议前缀（如 wss://），只填写 host: {host:?}");
    }
    if host.contains('/') {
        bail!("配置项 [api] workspace_host 不能带路径或末尾斜杠: {host:?}");
    }
    if host.contains(':') {
        bail!("配置项 [api] workspace_host 不能带端口: {host:?}");
    }
    if host.len() > 253 {
        bail!("配置项 [api] workspace_host 长度不能超过 253 个字符");
    }
    for label in host.split('.') {
        if label.is_empty() {
            bail!("配置项 [api] workspace_host 的域名标签不能为空: {host:?}");
        }
        if label.len() > 63 {
            bail!("配置项 [api] workspace_host 的域名标签不能超过 63 个字符: {host:?}");
        }
        if label.starts_with('-') || label.ends_with('-') {
            bail!("配置项 [api] workspace_host 的域名标签不能以连字符开头或结尾: {host:?}");
        }
        if !label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            bail!("配置项 [api] workspace_host 包含非法字符: {host:?}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_workspace_host_is_rejected() {
        let cases = [
            ("wss://ws-test.cn-beijing.maas.aliyuncs.com", "协议前缀"),
            (
                "ws-test.cn-beijing.maas.aliyuncs.com/api-ws/v1/inference",
                "路径",
            ),
            (" ws-test.cn-beijing.maas.aliyuncs.com", "空白"),
            ("ws-test.cn-beijing.maas.aliyuncs.com:443", "端口"),
            ("-bad.example.com", "连字符"),
            ("bad..example.com", "域名标签不能为空"),
            ("bad_host.example.com", "非法字符"),
            ("", "不能为空"),
        ];

        for (host, keyword) in cases {
            let text = format!("[api]\nkey = \"sk-test\"\nworkspace_host = \"{host}\"\n");
            let err = AppConfig::from_toml_str(&text).unwrap_err();
            assert!(
                format!("{err:#}").contains(keyword),
                "workspace_host={host:?} 应报「{keyword}」错误: {err:#}"
            );
        }
    }

    #[test]
    fn minimal_config_uses_defaults() {
        let config = AppConfig::from_toml_str("[api]\nkey = \"sk-test\"\n").unwrap();

        assert_eq!(config.api.key.as_deref(), Some("sk-test"));
        assert_eq!(
            config.api.workspace_host, "dashscope.aliyuncs.com",
            "未配置 workspace_host 时应使用百炼通用域名"
        );
        assert_eq!(config.asr.model, DEFAULT_ASR_MODEL);
        assert_eq!(config.tts.model, DEFAULT_TTS_MODEL);
        assert_eq!(config.tts.voice, DEFAULT_VOICE);
        assert!(config.audio.input_device.is_none());
        assert!(config.audio.output_device.is_none());
        assert!(config.vad.enabled);
        assert_eq!(config.vad.margin_db, DEFAULT_VAD_MARGIN_DB);
        assert_eq!(config.vad.hangover_ms, DEFAULT_VAD_HANGOVER_MS);
        assert_eq!(config.vad.tail_ms, DEFAULT_VAD_TAIL_MS);
        assert!(!config.vad.debug);
        assert_eq!(config.chunking.words, DEFAULT_CHUNK_WORDS);
        assert_eq!(
            config.inference_ws_url(),
            "wss://dashscope.aliyuncs.com/api-ws/v1/inference"
        );
    }

    #[test]
    fn full_config_overrides_every_field() {
        let text = r#"
[api]
key = "sk-test"
workspace_host = "ws-test.cn-beijing.maas.aliyuncs.com"

[asr]
model = "asr-test"

[tts]
model = "tts-test"
voice = "voice-test"

[audio]
input_device = "Microphone"
output_device = "CABLE Input"

[vad]
enabled = false
margin_db = 6.5
hangover_ms = 300
tail_ms = 800
debug = true

[chunking]
words = 10
"#;
        let config = AppConfig::from_toml_str(text).unwrap();

        assert_eq!(config.api.key.as_deref(), Some("sk-test"));
        assert_eq!(
            config.inference_ws_url(),
            "wss://ws-test.cn-beijing.maas.aliyuncs.com/api-ws/v1/inference"
        );
        assert_eq!(config.asr.model, "asr-test");
        assert_eq!(config.tts.model, "tts-test");
        assert_eq!(config.tts.voice, "voice-test");
        assert_eq!(config.audio.input_device.as_deref(), Some("Microphone"));
        assert_eq!(config.audio.output_device.as_deref(), Some("CABLE Input"));
        assert!(!config.vad.enabled);
        assert_eq!(config.vad.margin_db, 6.5);
        assert_eq!(config.vad.hangover_ms, 300);
        assert_eq!(config.vad.tail_ms, 800);
        assert!(config.vad.debug);
        assert_eq!(config.chunking.words, 10);
    }

    #[test]
    fn missing_api_key_is_rejected() {
        let err = AppConfig::from_toml_str("").unwrap_err();
        assert!(
            format!("{err:#}").contains("[api] key"),
            "错误信息应提示配置 [api] key: {err:#}"
        );
    }

    #[test]
    fn unknown_field_is_rejected() {
        let err = AppConfig::from_toml_str("[api]\nkey = \"sk-test\"\ntypo = 1\n").unwrap_err();
        assert!(
            format!("{err:#}").contains("typo"),
            "未知字段应导致解析失败: {err:#}"
        );
    }

    #[test]
    fn empty_device_strings_become_none() {
        let config = AppConfig::from_toml_str(
            "[api]\nkey = \"sk-test\"\n[audio]\ninput_device = \"  \"\noutput_device = \"\"\n",
        )
        .unwrap();
        assert!(config.audio.input_device.is_none());
        assert!(config.audio.output_device.is_none());
    }

    #[test]
    fn zero_chunk_words_is_rejected() {
        let err = AppConfig::from_toml_str("[api]\nkey = \"sk-test\"\n[chunking]\nwords = 0\n")
            .unwrap_err();
        assert!(
            format!("{err:#}").contains("[chunking] words"),
            "应提示 [chunking] words 非法: {err:#}"
        );
    }

    #[test]
    fn cli_config_path_defaults_to_config_toml() {
        let cli = CliArgs::parse_from(["kaols"]);
        assert_eq!(cli.config, PathBuf::from("config.toml"));
        assert!(!cli.list_devices);
    }

    #[test]
    fn cli_accepts_explicit_config_path_and_list_devices() {
        let cli = CliArgs::parse_from(["kaols", "-c", "custom.toml", "--list-devices"]);
        assert_eq!(cli.config, PathBuf::from("custom.toml"));
        assert!(cli.list_devices);
    }

    /// 保证仓库中提交的 config.example.toml 始终是合法配置。
    #[test]
    fn example_config_is_valid() {
        let config = AppConfig::from_toml_str(include_str!("../config.example.toml")).unwrap();
        assert_eq!(
            config.api.key.as_deref(),
            Some("sk-your-dashscope-api-key-here")
        );
        assert_eq!(config.api.workspace_host, DEFAULT_WORKSPACE_HOST);
    }
}
