use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::language::{self, is_asr_language, is_cloning_language};

/// 百炼默认接入域名（通用域名）。
///
/// 如使用业务空间专属域名，可在 `[api] workspace_host` 中覆盖，形如
/// `{WorkspaceId}.cn-beijing.maas.aliyuncs.com`（控制台「业务空间详情」可查看 WorkspaceId）。
pub const DEFAULT_WORKSPACE_HOST: &str = "dashscope.aliyuncs.com";

/// 实时识别模型默认值。
pub const DEFAULT_ASR_MODEL: &str = "qwen-audio-3.1-asr-flash-streaming";

/// 识别语言列表默认值（用户保证只会说其中一种；首个为默认）。
pub const DEFAULT_ASR_LANGUAGE: &str = "zh";

/// 合成模型默认值（Qwen-Audio-3.1-TTS-Flash，低延迟实时语音合成）。
pub const DEFAULT_TTS_MODEL: &str = "qwen-audio-3.1-tts-flash";

/// 合成音色默认值（Qwen-Audio-TTS 系统音色：乔小娇，俏丽可爱女声）。
pub const DEFAULT_VOICE: &str = "qiaoxiaojiao_v3.1";

/// 自定义音色引用前缀：`[tts] voice = "custom:<配置名>"` 表示使用复刻音色。
pub const CUSTOM_VOICE_PREFIX: &str = "custom:";

/// 自定义音色缓存文件默认名（相对配置文件所在目录）。
pub const DEFAULT_VOICE_CACHE_FILE: &str = "voice_cache.json";

/// 百炼音色前缀最大长度（官方：数字+英文字母，不超过 10 个字符）。
pub const MAX_VOICE_PREFIX_LEN: usize = 10;

/// 本地 VAD 默认起音余量（dB）。
pub const DEFAULT_VAD_MARGIN_DB: f32 = 9.0;

/// 本地 VAD 默认拖尾保持时长（毫秒）。
pub const DEFAULT_VAD_HANGOVER_MS: u32 = 500;

/// 本地 VAD 默认句末补发静音时长（毫秒）。
pub const DEFAULT_VAD_TAIL_MS: u32 = 1000;

/// 默认投机分块词数阈值。
pub const DEFAULT_CHUNK_WORDS: usize = 6;

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

    /// 配置文件所在目录：相对路径（缓存文件、样例音频）均以此为基准。
    /// 由 [`AppConfig::load`] 注入，不参与 TOML 解析。
    #[serde(skip)]
    base_dir: PathBuf,
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

    /// 用户可能说的语言列表（有序）：
    /// - 首个元素是没有"上一句"时的默认语言；
    /// - 只写一个元素 = 强提示只按该语言识别；
    /// - 检测用户当前说哪种语言时，永远以完整列表为范围；
    /// - 整个列表会作为 ASR API 的 `language_hints` 传递（数量上限由服务端处理）。
    pub languages: Vec<String>,
}

impl Default for AsrConfig {
    fn default() -> Self {
        Self {
            model: DEFAULT_ASR_MODEL.to_string(),
            languages: vec![DEFAULT_ASR_LANGUAGE.to_string()],
        }
    }
}

/// `[tts]`：语音合成。
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TtsConfig {
    pub model: String,

    /// 系统音色名、复刻音色 voice_id，或 `custom:<配置名>`（引用 `[tts.custom_voices]`）。
    pub voice: String,

    /// 自定义（复刻）音色表：键名用于在 `voice` 中以 `custom:<键名>` 引用。
    pub custom_voices: HashMap<String, CustomVoiceConfig>,

    /// 自定义音色缓存文件（相对配置文件所在目录），保存「模型 → 音频 → voice_id」。
    pub cache_file: PathBuf,
}

impl Default for TtsConfig {
    fn default() -> Self {
        Self {
            model: DEFAULT_TTS_MODEL.to_string(),
            voice: DEFAULT_VOICE.to_string(),
            custom_voices: HashMap::new(),
            cache_file: PathBuf::from(DEFAULT_VOICE_CACHE_FILE),
        }
    }
}

/// 一个自定义（复刻）音色，可包含多种语言的声线样本。
///
/// 每个样本都会按 (`[tts] model`, 音频内容) 单独复刻并缓存一个 voice_id；
/// 合成时按判定出的语言选择对应样本，缺语言时回退到 `samples` 的第一个。
/// 百炼音色前缀由**配置键名**自动生成，不提供自定义项。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CustomVoiceConfig {
    /// 显示名（可选，默认使用配置键名）。
    pub name: Option<String>,

    /// 声线样本列表（有序）：第一个是默认声线。
    pub samples: Vec<CustomVoiceSample>,
}

/// 某个语言的一份声线样本。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CustomVoiceSample {
    /// 该样本的语言（百炼复刻语言代码，如 zh / ja / en）。
    pub language: String,

    /// 本地样例音频路径（相对配置文件所在目录）。
    pub audio: PathBuf,
}

impl CustomVoiceConfig {
    /// 展示用名称：`name` 优先，缺省用配置键名。
    pub fn display_name(&self, config_name: &str) -> String {
        self.name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(config_name)
            .to_string()
    }

    /// 按语言查找样本（语言代码做别名归一，例如 `tl`/`fil`）。
    pub fn sample_for_language(&self, language: &str) -> Option<&CustomVoiceSample> {
        let wanted = language::canonical_language(language);
        self.samples
            .iter()
            .find(|sample| language::canonical_language(&sample.language) == wanted)
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
                "读取配置文件 {} 失败；主程序会在 exe 同目录自动生成 config.toml，\
                 请确认该目录可写、且不要删除此文件",
                path.display()
            )
        })?;
        let mut config = Self::from_toml_str(&text)
            .with_context(|| format!("配置文件 {} 无效", path.display()))?;
        config.base_dir = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        Ok(config)
    }

    /// 解析 TOML 文本，完成空值归一化与合法性校验（便于单元测试）。
    pub fn from_toml_str(text: &str) -> Result<Self> {
        let mut config: Self = toml::from_str(text).context("解析 TOML 失败")?;
        config.normalize();
        config.validate()?;
        Ok(config)
    }

    /// 把空字符串归一化为 `None`，并去掉各字符串字段的首尾空白。
    fn normalize(&mut self) {
        self.api.key = normalize_optional(self.api.key.take());
        self.api.workspace_host = self.api.workspace_host.trim().to_string();
        self.asr.model = self.asr.model.trim().to_string();
        self.asr.languages = self
            .asr
            .languages
            .iter()
            .map(|language| language.trim().to_lowercase())
            .collect();
        self.tts.model = self.tts.model.trim().to_string();
        self.tts.voice = self.tts.voice.trim().to_string();
        self.audio.input_device = normalize_optional(self.audio.input_device.take());
        self.audio.output_device = normalize_optional(self.audio.output_device.take());
        for custom in self.tts.custom_voices.values_mut() {
            custom.name = normalize_optional(custom.name.take());
            for sample in &mut custom.samples {
                sample.language = sample.language.trim().to_lowercase();
            }
        }
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
        self.validate_asr_languages()?;
        if self.tts.model.trim().is_empty() {
            bail!("配置项 [tts] model 不能为空");
        }
        if self.tts.voice.trim().is_empty() {
            bail!("配置项 [tts] voice 不能为空");
        }
        self.validate_custom_voices()?;
        if self.chunking.words == 0 {
            bail!("配置项 [chunking] words 必须大于 0");
        }
        if !self.vad.margin_db.is_finite() {
            bail!("配置项 [vad] margin_db 必须是有效数字");
        }
        Ok(())
    }

    fn validate_asr_languages(&self) -> Result<()> {
        if self.asr.languages.is_empty() {
            bail!("配置项 [asr] languages 不能为空（至少填写一种语言，如 [\"zh\"]）");
        }
        let mut seen = std::collections::HashSet::new();
        for lang in &self.asr.languages {
            if !is_asr_language(lang) {
                bail!(
                    "配置项 [asr] languages 含有不支持的语言代码 {lang:?}；\
                     可用代码（部分）: zh/en/ja/ko/vi/th/id/ms/tl/hi/ar/fr/de/es/pt/ru/it/...（见文档）"
                );
            }
            if !seen.insert(lang.as_str()) {
                bail!("配置项 [asr] languages 中存在重复语言 {lang:?}");
            }
        }
        // language_hints 原样传给 ASR API（数量上限由服务端处理），
        // 本地语言判定也使用完整列表。
        Ok(())
    }

    /// 校验 `[tts] voice` 的自定义音色引用，以及每个自定义音色条目的合法性。
    fn validate_custom_voices(&self) -> Result<()> {
        if self.tts.voice.starts_with(CUSTOM_VOICE_PREFIX) {
            match self.custom_voice_name() {
                Some(name) => {
                    if !self.tts.custom_voices.contains_key(name) {
                        let defined: Vec<&str> =
                            self.tts.custom_voices.keys().map(String::as_str).collect();
                        bail!(
                            "[tts] voice 引用了未定义的自定义音色 {CUSTOM_VOICE_PREFIX}{name}；\
                             请在 [tts.custom_voices.{name}] 中定义（当前已定义: {:?}）",
                            defined
                        );
                    }
                }
                None => bail!(
                    "[tts] voice 格式应为 {CUSTOM_VOICE_PREFIX}<配置名>，例如 {CUSTOM_VOICE_PREFIX}kaoet"
                ),
            }
        }

        for (name, custom) in &self.tts.custom_voices {
            if name.trim().is_empty() {
                bail!("[tts.custom_voices] 中存在空的配置名");
            }
            if custom.samples.is_empty() {
                bail!(
                    "自定义音色 [{name}] 至少要有一个 [[tts.custom_voices.{name}.samples]] 声线样本"
                );
            }

            let mut seen_languages = std::collections::HashSet::new();
            for (index, sample) in custom.samples.iter().enumerate() {
                if sample.audio.as_os_str().is_empty() {
                    bail!(
                        "自定义音色 [{name}] 的第 {} 个声线样本缺少 audio（样例音频路径）",
                        index + 1
                    );
                }
                if !is_cloning_language(language::canonical_language(&sample.language)) {
                    bail!(
                        "自定义音色 [{name}] 的声线样本 language {:?} 不受支持；\
                         可用代码: {:?}",
                        sample.language,
                        language::CLONING_LANGUAGES
                    );
                }
                let canonical = language::canonical_language(&sample.language).to_string();
                if !seen_languages.insert(canonical) {
                    bail!(
                        "自定义音色 [{name}] 中语言 {:?} 的声线样本重复",
                        sample.language
                    );
                }
            }
        }
        Ok(())
    }

    /// 把配置里的相对路径解析为相对「配置文件所在目录」的绝对/可用路径。
    pub fn resolve_path(&self, path: &Path) -> PathBuf {
        if path.is_absolute() || self.base_dir.as_os_str().is_empty() {
            path.to_path_buf()
        } else {
            self.base_dir.join(path)
        }
    }

    /// 自定义音色缓存文件路径。
    pub fn voice_cache_path(&self) -> PathBuf {
        self.resolve_path(&self.tts.cache_file)
    }

    /// `[tts] voice` 中引用的自定义音色名（`custom:<name>` 的 `<name>`）。
    pub fn custom_voice_name(&self) -> Option<&str> {
        self.tts
            .voice
            .strip_prefix(CUSTOM_VOICE_PREFIX)
            .map(str::trim)
            .filter(|name| !name.is_empty())
    }

    /// 当前生效的自定义音色配置（未引用自定义音色时返回 `None`）。
    pub fn active_custom_voice(&self) -> Option<(&str, &CustomVoiceConfig)> {
        let name = self.custom_voice_name()?;
        self.tts
            .custom_voices
            .get_key_value(name)
            .map(|(key, value)| (key.as_str(), value))
    }

    /// `[api] workspace_host` 对应的 WebSocket 基址，如 `wss://dashscope.aliyuncs.com`
    pub fn workspace_ws_base(&self) -> String {
        format!("wss://{}", self.api.workspace_host)
    }

    /// 实时语音识别（ASR）/ 语音合成（TTS）共用的 run-task 协议端点
    pub fn inference_ws_url(&self) -> String {
        format!("{}/api-ws/v1/inference", self.workspace_ws_base())
    }

    /// 百炼 HTTP API 基址（音色上传 / 管理等），如 `https://dashscope.aliyuncs.com/api/v1`
    pub fn http_api_base(&self) -> String {
        format!("https://{}/api/v1", self.api.workspace_host)
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
            ("ws test.cn-beijing.maas.aliyuncs.com", "空白"),
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
    fn workspace_host_surrounding_whitespace_is_trimmed() {
        let config = AppConfig::from_toml_str(
            "[api]\nkey = \"sk-test\"\nworkspace_host = \"  dashscope.aliyuncs.com  \"\n",
        )
        .unwrap();
        assert_eq!(config.api.workspace_host, "dashscope.aliyuncs.com");
        assert_eq!(
            config.inference_ws_url(),
            "wss://dashscope.aliyuncs.com/api-ws/v1/inference"
        );
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
        assert_eq!(config.asr.languages, vec!["zh".to_string()]);
        assert_eq!(config.tts.model, DEFAULT_TTS_MODEL);
        assert_eq!(config.tts.voice, DEFAULT_VOICE);
        assert!(config.tts.custom_voices.is_empty());
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
languages = ["ja", "zh", "en"]

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
        assert_eq!(
            config.asr.languages,
            vec!["ja".to_string(), "zh".to_string(), "en".to_string()]
        );
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
    fn asr_languages_are_validated() {
        let empty = AppConfig::from_toml_str("[api]\nkey = \"sk-test\"\n[asr]\nlanguages = []\n")
            .unwrap_err();
        assert!(format!("{empty:#}").contains("[asr] languages 不能为空"));

        let bad =
            AppConfig::from_toml_str("[api]\nkey = \"sk-test\"\n[asr]\nlanguages = [\"xx\"]\n")
                .unwrap_err();
        assert!(format!("{bad:#}").contains("不支持的语言代码"));

        let duplicate = AppConfig::from_toml_str(
            "[api]\nkey = \"sk-test\"\n[asr]\nlanguages = [\"ja\",\"ja\"]\n",
        )
        .unwrap_err();
        assert!(format!("{duplicate:#}").contains("重复"));
    }

    #[test]
    fn asr_languages_more_than_four_are_kept_for_detection() {
        // 配置层不限制语言数量；ASR language_hints 原样传递
        let config = AppConfig::from_toml_str(
            "[api]\nkey = \"sk-test\"\n[asr]\nlanguages = [\"zh\",\"ja\",\"en\",\"ko\",\"fr\"]\n",
        )
        .unwrap();
        assert_eq!(config.asr.languages.len(), 5);
    }

    #[test]
    fn custom_voice_with_ordered_samples_is_parsed() {
        let text = r#"
[api]
key = "sk-test"

[asr]
languages = ["zh", "ja"]

[tts]
voice = "custom:edgar"

[tts.custom_voices.edgar]
name = "  异环 - 埃德嘉  "

[[tts.custom_voices.edgar.samples]]
language = "zh"
audio = "samples/edgar-zh.wav"

[[tts.custom_voices.edgar.samples]]
language = "JA"
audio = "samples/edgar-ja.wav"
"#;
        let config = AppConfig::from_toml_str(text).unwrap();

        assert_eq!(config.tts.voice, "custom:edgar");
        assert_eq!(config.custom_voice_name(), Some("edgar"));

        let (name, custom) = config.active_custom_voice().unwrap();
        assert_eq!(name, "edgar");
        assert_eq!(custom.display_name("edgar"), "异环 - 埃德嘉");
        assert_eq!(custom.samples.len(), 2);
        assert_eq!(custom.samples[0].language, "zh");
        assert_eq!(
            custom.samples[0].audio,
            PathBuf::from("samples/edgar-zh.wav")
        );
        assert_eq!(custom.samples[1].language, "ja", "language 应小写化");
        assert_eq!(custom.samples[0].language, "zh", "第一个样本是默认声线");
        assert_eq!(custom.sample_for_language("ja").unwrap().language, "ja");
        assert_eq!(
            config.voice_cache_path(),
            PathBuf::from("voice_cache.json"),
            "默认缓存文件相对配置文件目录"
        );
    }

    #[test]
    fn custom_voice_requires_at_least_one_sample() {
        let err = AppConfig::from_toml_str(
            "[api]\nkey = \"sk-test\"\n[tts.custom_voices.kaoet]\nname = \"Kaoet\"\n",
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("至少要有一个"), "{err:#}");
    }

    #[test]
    fn custom_voice_sample_requires_audio() {
        let err = AppConfig::from_toml_str(
            "[api]\nkey = \"sk-test\"\n[[tts.custom_voices.kaoet.samples]]\nlanguage = \"zh\"\n",
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("缺少 audio"), "{err:#}");
    }

    #[test]
    fn custom_voice_sample_language_is_validated() {
        let err = AppConfig::from_toml_str(
            "[api]\nkey = \"sk-test\"\n[[tts.custom_voices.kaoet.samples]]\nlanguage = \"xx\"\naudio = \"a.wav\"\n",
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("不受支持"), "{err:#}");
    }

    #[test]
    fn custom_voice_sample_languages_must_be_unique() {
        let err = AppConfig::from_toml_str(
            "[api]\nkey = \"sk-test\"\n\
             [[tts.custom_voices.kaoet.samples]]\nlanguage = \"zh\"\naudio = \"a.wav\"\n\
             [[tts.custom_voices.kaoet.samples]]\nlanguage = \"zh\"\naudio = \"b.wav\"\n",
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("重复"), "{err:#}");
    }

    #[test]
    fn custom_voice_reference_must_be_defined() {
        let err = AppConfig::from_toml_str(
            "[api]\nkey = \"sk-test\"\n[tts]\nvoice = \"custom:missing\"\n",
        )
        .unwrap_err();
        assert!(
            format!("{err:#}").contains("custom:missing"),
            "应提示未定义的自定义音色: {err:#}"
        );
    }

    #[test]
    fn builtin_voice_has_no_active_custom_voice() {
        let config = AppConfig::from_toml_str(
            "[api]\nkey = \"sk-test\"\n[tts]\nvoice = \"qiaoxiaojiao_v3.1\"\n",
        )
        .unwrap();
        assert_eq!(config.custom_voice_name(), None);
        assert!(config.active_custom_voice().is_none());
    }

    #[test]
    fn paths_resolve_relative_to_config_dir() {
        let mut config = AppConfig::from_toml_str(
            "[api]\nkey = \"sk-test\"\n[tts]\ncache_file = \"cache/voices.json\"\n",
        )
        .unwrap();
        config.base_dir = PathBuf::from("C:/configs");

        assert_eq!(
            config.voice_cache_path(),
            PathBuf::from("C:/configs/cache/voices.json")
        );
        assert_eq!(
            config.resolve_path(Path::new("samples/a.wav")),
            PathBuf::from("C:/configs/samples/a.wav")
        );
        assert_eq!(
            config.resolve_path(Path::new("C:/abs/a.wav")),
            PathBuf::from("C:/abs/a.wav")
        );
        assert_eq!(
            config.http_api_base(),
            "https://dashscope.aliyuncs.com/api/v1"
        );
    }

    /// 默认模板必须"结构合法但没填 key"：首启生成的 config.toml 要让校验明确提示
    /// "未配置 API Key"，而不是看起来正常却连不上（见 paths.rs 的模板测试）。
    #[test]
    fn example_config_is_valid_template_without_key() {
        let raw = include_str!("../config.example.toml");

        let err = AppConfig::from_toml_str(raw).unwrap_err();
        assert!(
            format!("{err:#}").contains("未配置 API Key"),
            "模板应因缺少 key 而校验失败: {err:#}"
        );

        let with_key = raw.replace(
            "# key = \"sk-your-dashscope-api-key-here\"",
            "key = \"sk-test\"",
        );
        let config = AppConfig::from_toml_str(&with_key).unwrap();
        assert_eq!(config.api.key.as_deref(), Some("sk-test"));
        assert_eq!(config.api.workspace_host, DEFAULT_WORKSPACE_HOST);
    }
}
