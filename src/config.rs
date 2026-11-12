use anyhow::{Context, Result, bail};
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

/// 自定义音色样本的默认语速倍率（百炼 TTS `rate` 参数的默认值）。
pub const DEFAULT_SAMPLE_RATE: f32 = 1.0;

/// 百炼 TTS `rate` 参数允许的最小语速倍率（官方范围 [0.5, 2.0]）。
pub const MIN_SAMPLE_RATE: f32 = 0.5;

/// 百炼 TTS `rate` 参数允许的最大语速倍率（官方范围 [0.5, 2.0]）。
pub const MAX_SAMPLE_RATE: f32 = 2.0;

/// 语速倍率是否有效：必须是有限数且在百炼 TTS 允许范围内。
pub fn is_valid_sample_rate(rate: f32) -> bool {
    rate.is_finite() && (MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&rate)
}

/// 自定义音色引用前缀：`[tts] voice = "custom:<配置名>"` 表示使用复刻音色。
pub const CUSTOM_VOICE_PREFIX: &str = "custom:";

/// 本地缓存文件默认名（相对配置文件所在目录）：音色复刻记录 + ASR 预编译热词表。
pub const DEFAULT_CACHE_FILE: &str = "cache.json";

/// 预编译热词列表上限：权重固定 50 属于「超级热词」，官方限制每个词表最多 50 个。
pub const MAX_ASR_HOTWORDS: usize = 50;

/// 含非 ASCII 字符的热词最大字符数（官方限制，空格计入）。
pub const MAX_HOTWORD_CHARS: usize = 15;

/// 纯 ASCII 热词按空格切分后的最大片段数（官方限制）。
pub const MAX_ASCII_HOTWORD_WORDS: usize = 7;

/// 百炼音色前缀最大长度（官方：数字+英文字母，不超过 10 个字符）。
pub const MAX_VOICE_PREFIX_LEN: usize = 10;

/// 本地 VAD 默认起音概率阈值（Silero 官方默认）。
pub const DEFAULT_VAD_THRESHOLD: f32 = 0.5;

/// 本地 VAD 默认静音结束时长（毫秒）：低于负阈值持续该时长判定语音结束。
pub const DEFAULT_VAD_MIN_SILENCE_MS: u32 = 500;

/// 本地 VAD 默认句末补发静音时长（毫秒）。
pub const DEFAULT_VAD_TAIL_MS: u32 = 1000;

/// 默认投机分块词数阈值。
pub const DEFAULT_CHUNK_WORDS: usize = 6;

/// 翻译模型默认值（qwen-mt-flash：低延迟文本翻译，支持 92 种语言互译）。
pub const DEFAULT_TRANSLATE_MODEL: &str = "qwen-mt-flash";

/// 翻译目标语言默认值（英语）。
pub const DEFAULT_TRANSLATE_TARGET: &str = "en";

/// 默认翻译粒度：标点/句终即翻（低延迟，可能翻碎句）。
pub const DEFAULT_TRANSLATE_GRANULARITY: TranslateGranularity = TranslateGranularity::Clause;

/// 默认 VRChat OSC 接收端口（VRChat 的默认 inPort）。
pub const DEFAULT_VRCHAT_OSC_PORT: u16 = 9000;

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
    pub cache: CacheConfig,
    pub translate: TranslateConfig,
    pub vrchat: VrchatConfig,
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
    /// 百炼 API Key（只从配置文件读取，不读环境变量）。
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

    /// 预编译热词（ASR 专用）。
    ///
    /// 会话启动时自动上传到百炼并生成/复用预编译热词列表，识别时通过
    /// `vocabulary_id` 引用；权重固定 50（超级热词，每个词表最多 50 个）、
    /// 不限定词条语种（D31）。空列表 = 不使用热词，不访问网络。
    pub hotwords: Vec<String>,
}

impl Default for AsrConfig {
    fn default() -> Self {
        Self {
            model: DEFAULT_ASR_MODEL.to_string(),
            languages: vec![DEFAULT_ASR_LANGUAGE.to_string()],
            hotwords: Vec::new(),
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

    /// 中文方言（官方方言名，默认「普通话」= 不附加方言指令）。
    ///
    /// 只在本句语言判定为中文（`zh`）时通过合成请求的 `instruction` 生效；
    /// 取值见 [`language::CHINESE_DIALECTS`]。
    pub dialect: String,

    /// 自定义（复刻）音色表：键名用于在 `voice` 中以 `custom:<键名>` 引用。
    pub custom_voices: HashMap<String, CustomVoiceConfig>,
}

impl Default for TtsConfig {
    fn default() -> Self {
        Self {
            model: DEFAULT_TTS_MODEL.to_string(),
            voice: DEFAULT_VOICE.to_string(),
            dialect: language::DEFAULT_CHINESE_DIALECT.to_string(),
            custom_voices: HashMap::new(),
        }
    }
}

/// `[cache]`：本地缓存文件（音色复刻记录 + ASR 预编译热词 ID）。
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CacheConfig {
    /// 缓存文件（相对配置文件所在目录），保存「音色 → voice_id」与「ASR 模型 → vocabulary_id」。
    pub file: PathBuf,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            file: PathBuf::from(DEFAULT_CACHE_FILE),
        }
    }
}

/// 翻译粒度：决定多长的源文片段送去翻译。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TranslateGranularity {
    /// 标点或句终即送翻（延迟低，可能翻碎句）。
    #[default]
    Clause,
    /// 整句结束再送翻（质量优先，延迟高）。
    Sentence,
}

impl TranslateGranularity {
    /// 配置/日志里的稳定写法。
    pub fn label(self) -> &'static str {
        match self {
            Self::Clause => "clause",
            Self::Sentence => "sentence",
        }
    }
}

/// `[translate]`：翻译模式。
///
/// 开启后：输入文本先翻成 `target` 语言，再交给 TTS 合成并以字幕显示；
/// 关闭时完全走原有识别/合成链路（见 docs/translation.md）。
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TranslateConfig {
    /// 是否启用翻译模式（GUI 可改）。
    pub enabled: bool,

    /// 目标语言代码（[`language::TRANSLATION_LANGUAGES`] 之一，默认 en）。
    pub target: String,

    /// 翻译模型（只读；手改配置后「重新加载配置」生效）。
    pub model: String,

    /// 翻译粒度（GUI 可改）。
    pub granularity: TranslateGranularity,

    /// 字幕是否同时显示原文（GUI 可改；只影响渲染，不触发会话重启）。
    pub show_source: bool,
}

impl Default for TranslateConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            target: DEFAULT_TRANSLATE_TARGET.to_string(),
            model: DEFAULT_TRANSLATE_MODEL.to_string(),
            granularity: DEFAULT_TRANSLATE_GRANULARITY,
            show_source: true,
        }
    }
}

/// `[vrchat]`：VRChat 相关功能的父段（预留给以后的 avatar 等功能）。
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct VrchatConfig {
    pub osc: VrchatOscConfig,
}

/// `[vrchat.osc]`：OSC 聊天框输出。
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VrchatOscConfig {
    /// 是否把实时字幕发送到 VRChat 的 OSC 聊天框。
    pub enabled: bool,

    /// VRChat 的 OSC 接收端口（默认 9000）。
    pub port: u16,

    /// 翻译模式下的聊天框显示内容；非翻译模式固定显示 ASR 原文。
    pub display: VrchatDisplay,
}

impl Default for VrchatOscConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port: DEFAULT_VRCHAT_OSC_PORT,
            display: VrchatDisplay::default(),
        }
    }
}

/// VRChat 聊天框显示模式（仅翻译模式生效）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum VrchatDisplay {
    /// 只显示原文。
    Source,
    /// 原文和译文（译文在前、原文在后）。
    #[default]
    Bilingual,
    /// 只显示译文。
    Translation,
}

impl VrchatDisplay {
    /// 配置/日志里的稳定写法。
    pub fn label(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Bilingual => "bilingual",
            Self::Translation => "translation",
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
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CustomVoiceSample {
    /// 该样本的语言（百炼复刻语言代码，如 zh / ja / en）。
    pub language: String,

    /// 本地样例音频路径（相对配置文件所在目录）。
    pub audio: PathBuf,

    /// 语速倍率（百炼 TTS `rate` 参数，0.5–2.0，默认 1.0）。
    ///
    /// 只有本句实际选用该样本合成时才会使用；回退到默认样本时使用默认样本的 rate。
    pub rate: f32,
}

impl Default for CustomVoiceSample {
    fn default() -> Self {
        Self {
            language: String::new(),
            audio: PathBuf::new(),
            rate: DEFAULT_SAMPLE_RATE,
        }
    }
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

/// `[vad]`：本地 Silero VAD 发送门控。
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VadConfig {
    /// 是否启用本地 VAD 门控（关闭后全量上传音频）。
    pub enabled: bool,

    /// 起音概率阈值（0–1）；负阈值自动取 `threshold - 0.15`。
    pub threshold: f32,

    /// 低于负阈值持续多久判定语音结束（毫秒）。
    pub min_silence_ms: u32,

    /// 语音结束后补发的尾部静音时长（毫秒），用于触发服务端 sentence_end。
    pub tail_ms: u32,

    /// 打印每个 chunk 的 VAD 概率/阈值/发送决策，用于现场排障。
    pub debug: bool,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold: DEFAULT_VAD_THRESHOLD,
            min_silence_ms: DEFAULT_VAD_MIN_SILENCE_MS,
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
        self.asr.hotwords = normalize_hotwords(&self.asr.hotwords);
        self.tts.model = self.tts.model.trim().to_string();
        self.tts.voice = self.tts.voice.trim().to_string();
        self.tts.dialect = self.tts.dialect.trim().to_string();
        self.translate.target =
            language::canonical_language(self.translate.target.trim().to_lowercase().as_str())
                .to_string();
        self.translate.model = self.translate.model.trim().to_string();
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
        validate_hotwords(&self.asr.hotwords)?;
        if self.tts.model.trim().is_empty() {
            bail!("配置项 [tts] model 不能为空");
        }
        if self.tts.voice.trim().is_empty() {
            bail!("配置项 [tts] voice 不能为空");
        }
        if !language::is_chinese_dialect(&self.tts.dialect) {
            bail!(
                "配置项 [tts] dialect {:?} 不受支持；可用值: {:?}",
                self.tts.dialect,
                language::CHINESE_DIALECTS
            );
        }
        self.validate_custom_voices()?;
        self.validate_translate()?;
        self.validate_vrchat()?;
        if self.chunking.words == 0 {
            bail!("配置项 [chunking] words 必须大于 0");
        }
        if !self.vad.threshold.is_finite() || self.vad.threshold <= 0.0 || self.vad.threshold >= 1.0
        {
            bail!("配置项 [vad] threshold 必须是 (0, 1) 内的有效数字");
        }
        if self.vad.min_silence_ms > 5_000 {
            bail!("配置项 [vad] min_silence_ms 不能超过 5000");
        }
        if self.vad.tail_ms > 5_000 {
            bail!("配置项 [vad] tail_ms 不能超过 5000");
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
                if !is_valid_sample_rate(sample.rate) {
                    bail!(
                        "自定义音色 [{name}] 的第 {} 个声线样本 rate {:?} 无效；\
                         语速倍率必须是 {MIN_SAMPLE_RATE}~{MAX_SAMPLE_RATE} 之间的数字（默认 {DEFAULT_SAMPLE_RATE}）",
                        index + 1,
                        sample.rate
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

    /// 校验 `[translate]`：模型必须有值；启用翻译时目标语言必须可合成。
    ///
    /// 关闭翻译时不校验目标语言，允许用户先改开关、后选语言；
    /// 真正启用时会在下一次配置加载/写回校验中报错。
    fn validate_translate(&self) -> Result<()> {
        if self.translate.model.trim().is_empty() {
            bail!("配置项 [translate] model 不能为空");
        }
        if self.translate.enabled
            && !language::is_cloning_language(language::canonical_language(&self.translate.target))
        {
            bail!(
                "配置项 [translate] target {:?} 不是可合成的目标语言；可用代码: {:?}",
                self.translate.target,
                language::TRANSLATION_LANGUAGES
            );
        }
        Ok(())
    }

    fn validate_vrchat(&self) -> Result<()> {
        if self.vrchat.osc.port == 0 {
            bail!("配置项 [vrchat.osc] port 必须在 1–65535 之间");
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

    /// 本地缓存文件路径（音色 + ASR 预编译热词共用）。
    pub fn cache_path(&self) -> PathBuf {
        self.resolve_path(&self.cache.file)
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

    /// 翻译（qwen-mt）使用的 OpenAI 兼容 chat 端点，如
    /// `https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions`
    pub fn translate_chat_url(&self) -> String {
        format!(
            "https://{}/compatible-mode/v1/chat/completions",
            self.api.workspace_host
        )
    }
}

/// 去掉首尾空白；空字符串视为未配置。
fn normalize_optional(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// 归一化热词列表：去首尾空白、丢弃空条目、按首次出现去重并保留顺序。
///
/// 配置解析与 GUI 草稿都走这里，保证「草稿 → 配置 → 运行期」使用同一套归一化规则。
pub fn normalize_hotwords(hotwords: &[String]) -> Vec<String> {
    let mut normalized = Vec::with_capacity(hotwords.len());
    for word in hotwords {
        let word = word.trim();
        if word.is_empty() || normalized.iter().any(|existing| existing == word) {
            continue;
        }
        normalized.push(word.to_string());
    }
    normalized
}

/// 校验热词列表是否符合百炼预编译热词的限制（数量 + 单条长度）。
///
/// 官方限制：普通热词每列表最多 2000 条，但本项目固定使用「超级热词」权重 50，
/// 官方限制每个词表最多 50 个超级热词，因此上限取 50；
/// 含非 ASCII 字符时整条 ≤15 字符（空格计入），纯 ASCII 时按空格切分 ≤7 个片段。
/// 提前在本地报错，避免上传后才失败。
pub fn validate_hotwords(hotwords: &[String]) -> Result<()> {
    if hotwords.len() > MAX_ASR_HOTWORDS {
        bail!(
            "配置项 [asr] hotwords 最多 {MAX_ASR_HOTWORDS} 条（当前 {} 条）；\
             热词固定使用超级热词权重 50，官方限制每个词表最多 {MAX_ASR_HOTWORDS} 个",
            hotwords.len()
        );
    }
    for (index, word) in hotwords.iter().enumerate() {
        let word = word.trim();
        if word.is_empty() {
            bail!("配置项 [asr] hotwords 第 {} 条不能为空", index + 1);
        }
        if word.chars().any(|c| !c.is_ascii()) {
            let chars = word.chars().count();
            if chars > MAX_HOTWORD_CHARS {
                bail!(
                    "配置项 [asr] hotwords 第 {} 条 {:?} 过长：含非 ASCII 字符时最多 {MAX_HOTWORD_CHARS} 个字符（当前 {chars} 个）",
                    index + 1,
                    word
                );
            }
        } else {
            let fragments = word.split_whitespace().count();
            if fragments > MAX_ASCII_HOTWORD_WORDS {
                bail!(
                    "配置项 [asr] hotwords 第 {} 条 {:?} 过长：纯 ASCII 时最多 {MAX_ASCII_HOTWORD_WORDS} 个空格分隔片段（当前 {fragments} 个）",
                    index + 1,
                    word
                );
            }
        }
    }
    Ok(())
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
        assert!(config.asr.hotwords.is_empty(), "未配置热词时为空列表");
        assert_eq!(config.tts.model, DEFAULT_TTS_MODEL);
        assert_eq!(config.tts.voice, DEFAULT_VOICE);
        assert_eq!(
            config.tts.dialect,
            language::DEFAULT_CHINESE_DIALECT,
            "未配置方言时默认普通话"
        );
        assert!(config.tts.custom_voices.is_empty());
        assert!(config.audio.input_device.is_none());
        assert!(config.audio.output_device.is_none());
        assert!(config.vad.enabled);
        assert_eq!(config.vad.threshold, DEFAULT_VAD_THRESHOLD);
        assert_eq!(config.vad.min_silence_ms, DEFAULT_VAD_MIN_SILENCE_MS);
        assert_eq!(config.vad.tail_ms, DEFAULT_VAD_TAIL_MS);
        assert!(!config.vad.debug);
        assert_eq!(config.chunking.words, DEFAULT_CHUNK_WORDS);
        assert!(!config.translate.enabled);
        assert_eq!(config.translate.target, DEFAULT_TRANSLATE_TARGET);
        assert_eq!(config.translate.model, DEFAULT_TRANSLATE_MODEL);
        assert_eq!(
            config.translate.granularity,
            TranslateGranularity::Clause,
            "默认翻译粒度是标点即翻"
        );
        assert!(config.translate.show_source);
        assert!(!config.vrchat.osc.enabled);
        assert_eq!(config.vrchat.osc.port, DEFAULT_VRCHAT_OSC_PORT);
        assert_eq!(config.vrchat.osc.display, VrchatDisplay::Bilingual);
        assert_eq!(
            config.cache_path(),
            PathBuf::from("cache.json"),
            "默认缓存文件相对配置文件目录"
        );
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
hotwords = ["原神", "崩坏：星穹铁道", "Kaoet"]

[tts]
model = "tts-test"
voice = "voice-test"
dialect = "四川话"

[cache]
file = "cache/kaols.json"

[translate]
enabled = true
target = "ja"
granularity = "sentence"
show_source = false

[audio]
input_device = "Microphone"
output_device = "CABLE Input"

[vad]
enabled = false
threshold = 0.42
min_silence_ms = 320
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
        assert_eq!(
            config.asr.hotwords,
            vec![
                "原神".to_string(),
                "崩坏：星穹铁道".to_string(),
                "Kaoet".to_string()
            ]
        );
        assert_eq!(
            config
                .cache_path()
                .parent()
                .map(|p| p.to_string_lossy().to_string()),
            Some("cache".to_string()),
            "自定义缓存路径相对配置文件目录"
        );
        assert_eq!(config.tts.model, "tts-test");
        assert_eq!(config.tts.voice, "voice-test");
        assert_eq!(config.tts.dialect, "四川话");
        assert!(config.translate.enabled);
        assert_eq!(config.translate.target, "ja");
        assert_eq!(config.translate.model, DEFAULT_TRANSLATE_MODEL);
        assert_eq!(config.translate.granularity, TranslateGranularity::Sentence);
        assert!(!config.translate.show_source);
        assert_eq!(config.audio.input_device.as_deref(), Some("Microphone"));
        assert_eq!(config.audio.output_device.as_deref(), Some("CABLE Input"));
        assert!(!config.vad.enabled);
        assert_eq!(config.vad.threshold, 0.42);
        assert_eq!(config.vad.min_silence_ms, 320);
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
    fn invalid_vad_threshold_is_rejected() {
        for bad in ["0.0", "1.0", "1.5", "-0.2"] {
            let err = AppConfig::from_toml_str(&format!(
                "[api]\nkey = \"sk-test\"\n[vad]\nthreshold = {bad}\n"
            ))
            .unwrap_err();
            assert!(
                format!("{err:#}").contains("[vad] threshold"),
                "threshold={bad} 应被拒绝: {err:#}"
            );
        }
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
    fn asr_hotwords_are_normalized_and_parsed() {
        let config = AppConfig::from_toml_str(
            "[api]\nkey = \"sk-test\"\n[asr]\nhotwords = [\"  原神  \", \"\", \"原神\", \"Kaoet\"]\n",
        )
        .unwrap();
        assert_eq!(
            config.asr.hotwords,
            vec!["原神".to_string(), "Kaoet".to_string()],
            "热词应去首尾空白、丢弃空条目并按首次出现去重"
        );

        let empty =
            AppConfig::from_toml_str("[api]\nkey = \"sk-test\"\n[asr]\nhotwords = []\n").unwrap();
        assert!(empty.asr.hotwords.is_empty());
    }

    #[test]
    fn asr_hotword_length_limits_are_validated() {
        // 含非 ASCII：整条最多 15 个字符（空格计入）
        let ok = AppConfig::from_toml_str(
            "[api]\nkey = \"sk-test\"\n[asr]\nhotwords = [\"一二三四五六七八九十一二三四五\"]\n",
        );
        assert!(ok.is_ok(), "15 个汉字应合法: {ok:?}");

        let too_long = AppConfig::from_toml_str(
            "[api]\nkey = \"sk-test\"\n[asr]\nhotwords = [\"一二三四五六七八九十一二三四五六\"]\n",
        )
        .unwrap_err();
        assert!(
            format!("{too_long:#}").contains("过长"),
            "超过 15 个字符应报错: {too_long:#}"
        );

        // 纯 ASCII：按空格切分最多 7 个片段
        let ascii_ok = AppConfig::from_toml_str(
            "[api]\nkey = \"sk-test\"\n[asr]\nhotwords = [\"Human immunodeficiency virus type 1\"]\n",
        );
        assert!(ascii_ok.is_ok(), "5 个片段应合法: {ascii_ok:?}");

        let ascii_too_long = AppConfig::from_toml_str(
            "[api]\nkey = \"sk-test\"\n[asr]\nhotwords = [\"a b c d e f g h\"]\n",
        )
        .unwrap_err();
        assert!(
            format!("{ascii_too_long:#}").contains("纯 ASCII"),
            "超过 7 个片段应报错: {ascii_too_long:#}"
        );
    }

    #[test]
    fn asr_hotword_count_limit_is_validated() {
        let words: Vec<String> = (0..=MAX_ASR_HOTWORDS)
            .map(|index| format!("w{index}"))
            .collect();
        let err = validate_hotwords(&words).unwrap_err();
        assert!(format!("{err:#}").contains("最多"), "{err:#}");
    }

    #[test]
    fn tts_dialect_is_normalized_and_validated() {
        let config =
            AppConfig::from_toml_str("[api]\nkey = \"sk-test\"\n[tts]\ndialect = \"  东北话  \"\n")
                .unwrap();
        assert_eq!(config.tts.dialect, "东北话", "方言名应去首尾空白");

        // 只认官方名称：官方写法是「广东话」，不接受「粤语」等别名
        for bad in ["粤语", "东北", "xx", ""] {
            let text = format!("[api]\nkey = \"sk-test\"\n[tts]\ndialect = \"{bad}\"\n");
            let err = AppConfig::from_toml_str(&text).unwrap_err();
            assert!(
                format!("{err:#}").contains("[tts] dialect"),
                "dialect={bad:?} 应被拒绝: {err:#}"
            );
        }
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
            config.cache_path(),
            PathBuf::from("cache.json"),
            "默认缓存文件相对配置文件目录"
        );
    }

    #[test]
    fn custom_voice_sample_rate_defaults_to_one_and_is_parsed() {
        let text = r#"
[api]
key = "sk-test"

[[tts.custom_voices.edgar.samples]]
language = "zh"
audio = "a.wav"

[[tts.custom_voices.edgar.samples]]
language = "ja"
audio = "b.wav"
rate = 1.25
"#;
        let config = AppConfig::from_toml_str(text).unwrap();
        let samples = &config.tts.custom_voices["edgar"].samples;
        assert_eq!(
            samples[0].rate, DEFAULT_SAMPLE_RATE,
            "缺省 rate 应为默认语速 1.0"
        );
        assert!((samples[1].rate - 1.25).abs() < 1e-6);
    }

    #[test]
    fn custom_voice_sample_rate_out_of_range_is_rejected() {
        for rate in ["0.4", "2.1"] {
            let text = format!(
                "[api]\nkey = \"sk-test\"\n[[tts.custom_voices.kaoet.samples]]\n\
                 language = \"zh\"\naudio = \"a.wav\"\nrate = {rate}\n"
            );
            let err = AppConfig::from_toml_str(&text).unwrap_err();
            assert!(
                format!("{err:#}").contains("rate"),
                "rate={rate} 应被拒绝: {err:#}"
            );
        }
    }

    #[test]
    fn sample_rate_validity_requires_finite_and_in_range() {
        assert!(is_valid_sample_rate(MIN_SAMPLE_RATE));
        assert!(is_valid_sample_rate(MAX_SAMPLE_RATE));
        assert!(is_valid_sample_rate(1.0));
        assert!(!is_valid_sample_rate(0.49));
        assert!(!is_valid_sample_rate(2.01));
        assert!(!is_valid_sample_rate(f32::NAN));
        assert!(!is_valid_sample_rate(f32::INFINITY));
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
            "[api]\nkey = \"sk-test\"\n[cache]\nfile = \"cache/voices.json\"\n",
        )
        .unwrap();
        config.base_dir = PathBuf::from("C:/configs");

        assert_eq!(
            config.cache_path(),
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

    #[test]
    fn vrchat_osc_config_is_parsed_and_validated() {
        let config = AppConfig::from_toml_str(
            "[api]\nkey = \"sk-test\"\n\n[vrchat.osc]\nenabled = true\nport = 9001\ndisplay = \"translation\"\n",
        )
        .unwrap();
        assert!(config.vrchat.osc.enabled);
        assert_eq!(config.vrchat.osc.port, 9001);
        assert_eq!(config.vrchat.osc.display, VrchatDisplay::Translation);

        let err = AppConfig::from_toml_str("[api]\nkey = \"sk-test\"\n\n[vrchat.osc]\nport = 0\n")
            .unwrap_err();
        assert!(
            format!("{err:#}").contains("[vrchat.osc] port"),
            "port=0 应被拒绝: {err:#}"
        );

        let err = AppConfig::from_toml_str(
            "[api]\nkey = \"sk-test\"\n\n[vrchat.osc]\ndisplay = \"both\"\n",
        )
        .unwrap_err();
        assert!(
            format!("{err:#}").contains("display"),
            "未知 display 值应被拒绝: {err:#}"
        );
    }

    #[test]
    fn translate_target_is_normalized_and_validated() {
        let config = AppConfig::from_toml_str(
            "[api]\nkey = \"sk-test\"\n[translate]\nenabled = true\ntarget = \" TL \"\n",
        )
        .unwrap();
        assert_eq!(
            config.translate.target, "fil",
            "tl 应归一为 fil，并去掉空白"
        );

        let err = AppConfig::from_toml_str(
            "[api]\nkey = \"sk-test\"\n[translate]\nenabled = true\ntarget = \"xx\"\n",
        )
        .unwrap_err();
        assert!(
            format!("{err:#}").contains("[translate] target"),
            "启用翻译时非法目标语言应被拒绝: {err:#}"
        );

        // 关闭翻译时不校验目标语言，允许先填错、之后再改
        let config = AppConfig::from_toml_str(
            "[api]\nkey = \"sk-test\"\n[translate]\nenabled = false\ntarget = \"xx\"\n",
        )
        .unwrap();
        assert_eq!(config.translate.target, "xx");
    }

    #[test]
    fn translate_granularity_is_parsed() {
        let default = AppConfig::from_toml_str("[api]\nkey = \"sk-test\"\n").unwrap();
        assert_eq!(
            default.translate.granularity,
            TranslateGranularity::Clause,
            "缺省粒度应为 clause"
        );

        let sentence = AppConfig::from_toml_str(
            "[api]\nkey = \"sk-test\"\n[translate]\ngranularity = \"sentence\"\n",
        )
        .unwrap();
        assert_eq!(
            sentence.translate.granularity,
            TranslateGranularity::Sentence
        );
        assert_eq!(sentence.translate.granularity.label(), "sentence");

        let err = AppConfig::from_toml_str(
            "[api]\nkey = \"sk-test\"\n[translate]\ngranularity = \"word\"\n",
        )
        .unwrap_err();
        assert!(
            format!("{err:#}").contains("unknown variant"),
            "非法粒度应解析失败: {err:#}"
        );
    }

    #[test]
    fn translate_model_must_not_be_empty() {
        let err =
            AppConfig::from_toml_str("[api]\nkey = \"sk-test\"\n[translate]\nmodel = \"  \"\n")
                .unwrap_err();
        assert!(format!("{err:#}").contains("[translate] model"), "{err:#}");
    }

    #[test]
    fn translate_endpoint_uses_compatible_chat_path() {
        let config = AppConfig::from_toml_str(
            "[api]\nkey = \"sk-test\"\n\
             workspace_host = \"ws-test.cn-beijing.maas.aliyuncs.com\"\n",
        )
        .unwrap();
        assert_eq!(
            config.translate_chat_url(),
            "https://ws-test.cn-beijing.maas.aliyuncs.com/compatible-mode/v1/chat/completions"
        );
    }
}
