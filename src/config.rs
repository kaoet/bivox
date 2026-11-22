//! 配置模型、默认值与校验。
//!
//! 本文件是配置项的唯一完整字段说明：字段注释写用途、允许值/范围与跨字段约束；
//! 默认值由对应的 `Default` 实现或默认常量给出，不在字段注释中重复。
//! 用户侧操作见 README，运行期行为见 docs/architecture.md。

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use toml::{Table, Value};

use crate::language::{self, is_asr_language, is_cloning_language};

/// 百炼默认接入域名（通用域名）。
///
/// 如使用业务空间专属域名，可在 `[api] workspace_host` 中覆盖，形如
/// `{WorkspaceId}.cn-beijing.maas.aliyuncs.com`（控制台「业务空间详情」可查看 WorkspaceId）。
pub const DEFAULT_WORKSPACE_HOST: &str = "dashscope.aliyuncs.com";

/// 实时识别模型默认值。
pub const DEFAULT_ASR_MODEL: &str = "qwen-audio-3.1-asr-flash-streaming";

/// 识别语言列表默认值（有序，首个为默认；`[self]` 与 `[others]` 共用这个默认值）。
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

/// 我方译文语言的默认值（英语）：`[self] target`。
pub const DEFAULT_SELF_TARGET: &str = "en";

/// 对方译文语言的默认值（中文）：`[others] target`。
pub const DEFAULT_OTHERS_TARGET: &str = "zh";

/// 默认翻译粒度：标点/句终即翻（低延迟，可能翻碎句）。
pub const DEFAULT_TRANSLATE_GRANULARITY: TranslateGranularity = TranslateGranularity::Clause;

/// 默认 VRChat OSC 接收端口（VRChat 的默认 inPort）。
pub const DEFAULT_VRCHAT_OSC_PORT: u16 = 9000;

/// 默认聊天框文本保留时长（秒）：定稿后多久主动清空（D42）。
pub const DEFAULT_VRCHAT_RETENTION_SECS: u32 = 30;

/// 聊天框文本保留时长的最小值（秒）。
pub const MIN_VRCHAT_RETENTION_SECS: u32 = 1;

/// 聊天框文本保留时长的最大值（秒）。
///
/// 取 30 秒与 VRChat 聊天框的自然过期时长对齐，避免额外的心跳重发（D42）。
pub const MAX_VRCHAT_RETENTION_SECS: u32 = 30;

/// 对方线路默认监听的目标进程（VRChat 的 exe 文件名）。
pub const DEFAULT_VRCHAT_PROCESS: &str = "VRChat.exe";

/// 应用配置，对应 `config.toml` 的顶层结构。
///
/// 所有字段都有默认值，空文件是合法的默认配置；`[api] key` 缺省表示未配置，
/// 不算配置非法。字段一旦显式写出就必须合法（显式空字符串会被拒绝）。
/// 启用 `deny_unknown_fields`：未知字段/拼写错误直接解析失败。
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct AppConfig {
    pub api: ApiConfig,
    pub asr: AsrConfig,
    pub tts: TtsConfig,
    pub cache: CacheConfig,
    pub translate: TranslateConfig,
    /// `[self]`：我方线路（麦克风 + 打字输入）。
    ///
    /// TOML 段名用 `self`，Rust 字段用 `local`（`self` 是关键字）。
    #[serde(rename = "self")]
    pub local: LocalConfig,
    /// `[others]`：对方线路（侦听本机另一个进程的音频输出，D36）。
    pub others: OthersConfig,
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
    /// 百炼 API Key，用于全部百炼 API/WebSocket 请求。
    ///
    /// `None` = 未配置，配置本身有效，开始会话前由 GUI 引导填写；
    /// 显式写空字符串非法。
    pub key: Option<String>,

    /// 业务空间接入域名 Host。
    ///
    /// 只能填纯 host：不能带 `wss://`、路径、端口或空白；域名标签需合法且长度符合
    /// DNS 规则。业务空间专属域名形如 `{WorkspaceId}.cn-beijing.maas.aliyuncs.com`。
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

/// `[asr]`：实时语音识别中**两条线路共用**的部分。
///
/// 语言列表不在这里：每条线路各有一份（`[self] languages` / `[others] languages`）。
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AsrConfig {
    /// 实时识别模型 ID；必须非空。我方与对方线路共用。
    ///
    /// 更换模型会改变预编译热词列表的归属。
    pub model: String,

    /// 预编译热词列表，空 = 不使用热词。我方与对方线路共用一个词表。
    ///
    /// 词条丢弃空项、去重保序；权重固定 50（超级热词）；最多
    /// [`MAX_ASR_HOTWORDS`] 条；单条含非 ASCII 字符时 ≤ [`MAX_HOTWORD_CHARS`]
    /// 字符，纯 ASCII 时 ≤ [`MAX_ASCII_HOTWORD_WORDS`] 个空格分隔片段。
    /// 会话启动时同步为百炼预编译热词列表（D31）。
    pub hotwords: Vec<String>,
}

impl Default for AsrConfig {
    fn default() -> Self {
        Self {
            model: DEFAULT_ASR_MODEL.to_string(),
            hotwords: Vec::new(),
        }
    }
}

/// `[tts]`：语音合成。
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TtsConfig {
    /// 合成模型 ID；必须非空，与音色/复刻缓存绑定。
    pub model: String,

    /// 当前音色：系统音色名、复刻 voice_id，或 `custom:<配置名>`。
    ///
    /// `custom:` 引用必须存在于 `custom_voices`；不能为空。
    pub voice: String,

    /// 中文方言，取值必须是 [`language::CHINESE_DIALECTS`] 中的官方名称。
    ///
    /// 只对判定为中文的句子生效；普通话不附加方言指令（D29）。
    pub dialect: String,

    /// 自定义（复刻）音色表；键名用于 `voice = "custom:<键名>"`。
    ///
    /// 每个音色至少一个样本，结构见 [`CustomVoiceConfig`]。
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
    /// 本地缓存文件，供音色复刻记录与 ASR 预编译热词 ID 共用。
    ///
    /// 相对路径以配置文件所在目录为基准；不能为空。删除后会重新复刻音色、
    /// 重建热词列表。
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

/// `[translate]`：两条线路**共用**的翻译设置。
///
/// 每条线路是否翻译、译成什么语言在 `[self] translate/target` 与
/// `[others] translate/target`；这里只保留共用的模型与粒度。
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TranslateConfig {
    /// 翻译模型 ID；必须非空。
    pub model: String,

    /// 翻译粒度，取值见 [`TranslateGranularity`]。
    pub granularity: TranslateGranularity,

    /// 字幕/气泡是否同时显示原文；只影响渲染，不触发会话重启。
    pub show_source: bool,
}

impl Default for TranslateConfig {
    fn default() -> Self {
        Self {
            model: DEFAULT_TRANSLATE_MODEL.to_string(),
            granularity: DEFAULT_TRANSLATE_GRANULARITY,
            show_source: true,
        }
    }
}

/// `[self]`：我方线路——麦克风采集与打字输入。
///
/// 这条线路没有总开关：`mic` / `tts` / `[vrchat.osc] enabled` 各自控制一个能力；
/// 三者都关时仍然可以打字上屏。
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LocalConfig {
    /// 是否采集麦克风；关闭后只能用打字输入（不采集、不上传，也不启动本地 VAD）。
    pub mic: bool,

    /// 有序语言列表，首项为默认语言。
    ///
    /// - 只列一种 = 强提示只按该语言识别；
    /// - 语言判定以完整列表为范围，整份列表作为本方 ASR 的 `language_hints`；
    /// - 打字输入的语言判定也用它；
    /// - 取值见 [`language::ASR_LANGUAGES`]；不能为空、不能重复，归一化去空白并转小写。
    pub languages: Vec<String>,

    /// 是否把我方文本翻译成 [`Self::target`]；关闭时原文直接送 TTS / OSC / 字幕。
    pub translate: bool,

    /// 我方译文语言代码，必须是 [`language::TRANSLATION_LANGUAGES`] 之一。
    ///
    /// 启用翻译时校验；别名会归一（如 `tl` → `fil`），不能为空。
    pub target: String,

    /// 是否把我方文本合成语音；关闭时不解析音色、不建 TTS，只上屏（可继续发 OSC）。
    pub tts: bool,
}

impl Default for LocalConfig {
    fn default() -> Self {
        Self {
            mic: true,
            languages: vec![DEFAULT_ASR_LANGUAGE.to_string()],
            translate: false,
            target: DEFAULT_SELF_TARGET.to_string(),
            tts: true,
        }
    }
}

/// `[others]`：对方线路——侦听 `[vrchat] process` 的音频输出（D36）。
///
/// 这条线路只上屏：不写 OSC、不做 TTS；目标进程不可达按临时错误处理。
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OthersConfig {
    /// 是否启用对方线路；关闭时不采集进程音频、不建第二条 ASR 连接。
    pub enabled: bool,

    /// 对方语言列表，语义与 [`LocalConfig::languages`] 相同，用于对方线路的 ASR。
    pub languages: Vec<String>,

    /// 是否把对方文本翻译成 [`Self::target`]；关闭时只显示原文。
    pub translate: bool,

    /// 对方译文语言代码，必须是 [`language::TRANSLATION_LANGUAGES`] 之一。
    ///
    /// 启用翻译时校验；别名会归一，不能为空。
    pub target: String,
}

impl Default for OthersConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            languages: vec![DEFAULT_ASR_LANGUAGE.to_string()],
            translate: false,
            target: DEFAULT_OTHERS_TARGET.to_string(),
        }
    }
}

/// `[vrchat]`：VRChat 相关设置。
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VrchatConfig {
    /// 对方线路要侦听的进程（exe 文件名，含其子进程，见 D36）；不能为空。
    ///
    /// 进程不在时按临时错误处理：界面告警 + 自动重连，不算会话失败。
    pub process: String,

    pub osc: VrchatOscConfig,
}

impl Default for VrchatConfig {
    fn default() -> Self {
        Self {
            process: DEFAULT_VRCHAT_PROCESS.to_string(),
            osc: VrchatOscConfig::default(),
        }
    }
}

/// `[vrchat.osc]`：OSC 聊天框输出。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VrchatOscConfig {
    /// 是否把实时字幕发送到 VRChat 的 OSC 聊天框。
    pub enabled: bool,

    /// VRChat 的 OSC 接收端口，必须为 1–65535。
    ///
    /// 需与 VRChat 启动参数 `--osc=inPort:...` 中的 `inPort` 一致。
    pub port: u16,

    /// 翻译模式下的聊天框显示内容；非翻译模式固定显示 ASR 原文。
    pub display: VrchatDisplay,

    /// 是否在聊天框里同时保留当前句之后的历史句（D42）。
    ///
    /// 关闭时只显示当前句；没有当前句时显示最后一条定稿消息。
    pub show_history: bool,

    /// 聊天框文本保留时长（秒），范围 [`MIN_VRCHAT_RETENTION_SECS`]–[`MAX_VRCHAT_RETENTION_SECS`]（D42）。
    ///
    /// 两种模式都生效：历史句的年龄上限；关闭历史句时是最后一条消息定稿后的存活时间。
    /// 当前句（未定稿）不受此限制。
    pub retention_secs: u32,
}

impl Default for VrchatOscConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port: DEFAULT_VRCHAT_OSC_PORT,
            display: VrchatDisplay::default(),
            show_history: false,
            retention_secs: DEFAULT_VRCHAT_RETENTION_SECS,
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
/// 每个样本按（TTS 模型, 音频内容）单独复刻并缓存；合成时按语言选样本，
/// 缺语言回退到首个样本。音色前缀由配置键名自动生成。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CustomVoiceConfig {
    /// 显示名；省略时使用配置键名，显式空字符串非法。
    pub name: Option<String>,

    /// 有序声线样本；至少一个，首个是默认声线。
    pub samples: Vec<CustomVoiceSample>,
}

/// 某个语言的一份声线样本。
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CustomVoiceSample {
    /// 声音复刻语言代码，取值见 [`language::CLONING_LANGUAGES`]。
    ///
    /// 归一化做别名处理（如 `tl` → `fil`）；同一音色内归一后不能重复。
    pub language: String,

    /// 本地样例音频路径（相对配置文件所在目录或绝对路径）；不能为空。
    ///
    /// 音频格式/时长等要求见 README。
    pub audio: PathBuf,

    /// 合成语速倍率，有效范围 [`MIN_SAMPLE_RATE`]–[`MAX_SAMPLE_RATE`]。
    ///
    /// 属于样本级：只在本句选用该样本时生效，回退样本时用回退样本的 rate；
    /// 不参与复刻/缓存。
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
    /// 输入设备完整名称；`None` = 系统默认。
    ///
    /// 显式空字符串非法；启动时按完整名称精确匹配，改名/拔出会报错而不回退。
    pub input_device: Option<String>,

    /// 输出设备完整名称；`None` = 系统默认。
    ///
    /// 显式空字符串非法；启动时按完整名称精确匹配，改名/拔出会报错而不回退。
    pub output_device: Option<String>,
}

/// `[vad]`：本地 Silero VAD 发送门控。
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VadConfig {
    /// 是否启用本地 VAD 门控；关闭后全量上传音频。
    pub enabled: bool,

    /// 起音概率阈值，必须是 (0, 1) 内的有限数。
    ///
    /// 低于 `threshold - 0.15` 视为静音。
    pub threshold: f32,

    /// 低于负阈值持续多久判定语音结束，0–5000 毫秒。
    pub min_silence_ms: u32,

    /// 语音结束后补发的尾部静音，0–5000 毫秒；用于触发服务端 sentence_end。
    pub tail_ms: u32,

    /// 打印每个 chunk 的 VAD 概率/阈值/发送决策，用于排障。
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
    /// 投机分块词数阈值，必须大于 0。
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

    /// 序列化为最小 TOML：只写与默认值不同的字段，空表不输出。
    ///
    /// 序列化前会先对副本做归一化和校验，避免把带空白/非法值写到磁盘。
    pub fn to_toml_string(&self) -> Result<String> {
        let mut config = self.clone();
        config.normalize();
        config.validate()?;
        toml::to_string(&config.to_toml_table()).context("序列化配置为 TOML 失败")
    }

    /// 构建只含非默认值的 TOML 表。
    fn to_toml_table(&self) -> Table {
        let default = Self::default();
        let mut root = Table::new();

        let mut api = Table::new();
        if let Some(key) = &self.api.key {
            api.insert("key".to_string(), Value::String(key.clone()));
        }
        if self.api.workspace_host != default.api.workspace_host {
            api.insert(
                "workspace_host".to_string(),
                Value::String(self.api.workspace_host.clone()),
            );
        }
        insert_table(&mut root, "api", api);

        let mut asr = Table::new();
        if self.asr.model != default.asr.model {
            asr.insert("model".to_string(), Value::String(self.asr.model.clone()));
        }
        if !self.asr.hotwords.is_empty() {
            asr.insert("hotwords".to_string(), string_array(&self.asr.hotwords));
        }
        insert_table(&mut root, "asr", asr);

        let mut local = Table::new();
        if self.local.mic != default.local.mic {
            local.insert("mic".to_string(), Value::Boolean(self.local.mic));
        }
        if self.local.languages != default.local.languages {
            local.insert("languages".to_string(), string_array(&self.local.languages));
        }
        if self.local.translate != default.local.translate {
            local.insert(
                "translate".to_string(),
                Value::Boolean(self.local.translate),
            );
        }
        if self.local.target != default.local.target {
            local.insert(
                "target".to_string(),
                Value::String(self.local.target.clone()),
            );
        }
        if self.local.tts != default.local.tts {
            local.insert("tts".to_string(), Value::Boolean(self.local.tts));
        }
        insert_table(&mut root, "self", local);

        let mut others = Table::new();
        if self.others.enabled != default.others.enabled {
            others.insert("enabled".to_string(), Value::Boolean(self.others.enabled));
        }
        if self.others.languages != default.others.languages {
            others.insert(
                "languages".to_string(),
                string_array(&self.others.languages),
            );
        }
        if self.others.translate != default.others.translate {
            others.insert(
                "translate".to_string(),
                Value::Boolean(self.others.translate),
            );
        }
        if self.others.target != default.others.target {
            others.insert(
                "target".to_string(),
                Value::String(self.others.target.clone()),
            );
        }
        insert_table(&mut root, "others", others);

        let mut tts = Table::new();
        if self.tts.model != default.tts.model {
            tts.insert("model".to_string(), Value::String(self.tts.model.clone()));
        }
        if self.tts.voice != default.tts.voice {
            tts.insert("voice".to_string(), Value::String(self.tts.voice.clone()));
        }
        if self.tts.dialect != default.tts.dialect {
            tts.insert(
                "dialect".to_string(),
                Value::String(self.tts.dialect.clone()),
            );
        }
        if !self.tts.custom_voices.is_empty() {
            let mut voices = Table::new();
            let mut names: Vec<&String> = self.tts.custom_voices.keys().collect();
            names.sort();
            for name in names {
                let voice = &self.tts.custom_voices[name];
                let mut voice_table = Table::new();
                if let Some(display_name) = &voice.name {
                    voice_table.insert("name".to_string(), Value::String(display_name.clone()));
                }

                let mut samples = Vec::with_capacity(voice.samples.len());
                for sample in &voice.samples {
                    let mut sample_table = Table::new();
                    sample_table.insert(
                        "language".to_string(),
                        Value::String(sample.language.clone()),
                    );
                    sample_table.insert(
                        "audio".to_string(),
                        Value::String(sample.audio.to_string_lossy().into_owned()),
                    );
                    // 默认语速不写，缺省即 1.0。
                    if sample.rate != DEFAULT_SAMPLE_RATE {
                        sample_table.insert("rate".to_string(), float_value(sample.rate));
                    }
                    samples.push(Value::Table(sample_table));
                }
                voice_table.insert("samples".to_string(), Value::Array(samples));
                voices.insert(name.clone(), Value::Table(voice_table));
            }
            tts.insert("custom_voices".to_string(), Value::Table(voices));
        }
        insert_table(&mut root, "tts", tts);

        let mut cache = Table::new();
        if self.cache.file != default.cache.file {
            cache.insert(
                "file".to_string(),
                Value::String(self.cache.file.to_string_lossy().into_owned()),
            );
        }
        insert_table(&mut root, "cache", cache);

        let mut audio = Table::new();
        if let Some(device) = &self.audio.input_device {
            audio.insert("input_device".to_string(), Value::String(device.clone()));
        }
        if let Some(device) = &self.audio.output_device {
            audio.insert("output_device".to_string(), Value::String(device.clone()));
        }
        insert_table(&mut root, "audio", audio);

        let mut vad = Table::new();
        if self.vad.enabled != default.vad.enabled {
            vad.insert("enabled".to_string(), Value::Boolean(self.vad.enabled));
        }
        if self.vad.threshold != default.vad.threshold {
            vad.insert("threshold".to_string(), float_value(self.vad.threshold));
        }
        if self.vad.min_silence_ms != default.vad.min_silence_ms {
            vad.insert(
                "min_silence_ms".to_string(),
                Value::Integer(i64::from(self.vad.min_silence_ms)),
            );
        }
        if self.vad.tail_ms != default.vad.tail_ms {
            vad.insert(
                "tail_ms".to_string(),
                Value::Integer(i64::from(self.vad.tail_ms)),
            );
        }
        if self.vad.debug != default.vad.debug {
            vad.insert("debug".to_string(), Value::Boolean(self.vad.debug));
        }
        insert_table(&mut root, "vad", vad);

        let mut chunking = Table::new();
        if self.chunking.words != default.chunking.words {
            chunking.insert(
                "words".to_string(),
                Value::Integer(i64::try_from(self.chunking.words).unwrap_or(i64::MAX)),
            );
        }
        insert_table(&mut root, "chunking", chunking);

        let mut translate = Table::new();
        if self.translate.model != default.translate.model {
            translate.insert(
                "model".to_string(),
                Value::String(self.translate.model.clone()),
            );
        }
        if self.translate.granularity != default.translate.granularity {
            translate.insert(
                "granularity".to_string(),
                Value::String(self.translate.granularity.label().to_string()),
            );
        }
        if self.translate.show_source != default.translate.show_source {
            translate.insert(
                "show_source".to_string(),
                Value::Boolean(self.translate.show_source),
            );
        }
        insert_table(&mut root, "translate", translate);

        let mut vrchat = Table::new();
        if self.vrchat.process != default.vrchat.process {
            vrchat.insert(
                "process".to_string(),
                Value::String(self.vrchat.process.clone()),
            );
        }
        let mut osc = Table::new();
        if self.vrchat.osc.enabled != default.vrchat.osc.enabled {
            osc.insert(
                "enabled".to_string(),
                Value::Boolean(self.vrchat.osc.enabled),
            );
        }
        if self.vrchat.osc.port != default.vrchat.osc.port {
            osc.insert(
                "port".to_string(),
                Value::Integer(i64::from(self.vrchat.osc.port)),
            );
        }
        if self.vrchat.osc.display != default.vrchat.osc.display {
            osc.insert(
                "display".to_string(),
                Value::String(self.vrchat.osc.display.label().to_string()),
            );
        }
        if self.vrchat.osc.show_history != default.vrchat.osc.show_history {
            osc.insert(
                "show_history".to_string(),
                Value::Boolean(self.vrchat.osc.show_history),
            );
        }
        if self.vrchat.osc.retention_secs != default.vrchat.osc.retention_secs {
            osc.insert(
                "retention_secs".to_string(),
                Value::Integer(i64::from(self.vrchat.osc.retention_secs)),
            );
        }
        if !osc.is_empty() {
            vrchat.insert("osc".to_string(), Value::Table(osc));
        }
        insert_table(&mut root, "vrchat", vrchat);

        root
    }

    /// 去掉各字符串字段的首尾空白，并完成语言代码等归一化。
    ///
    /// 注意：这里**保留显式空字符串**（`Some("")`），交给 [`Self::validate`]
    /// 判定为非法；只有字段缺省时才是 `None`（未设置）。
    fn normalize(&mut self) {
        self.api.key = trim_optional(self.api.key.take());
        self.api.workspace_host = self.api.workspace_host.trim().to_string();
        self.asr.model = self.asr.model.trim().to_string();
        self.asr.hotwords = normalize_hotwords(&self.asr.hotwords);
        self.tts.model = self.tts.model.trim().to_string();
        self.tts.voice = self.tts.voice.trim().to_string();
        self.tts.dialect = self.tts.dialect.trim().to_string();
        normalize_languages(&mut self.local.languages);
        normalize_target(&mut self.local.target);
        normalize_languages(&mut self.others.languages);
        normalize_target(&mut self.others.target);
        self.translate.model = self.translate.model.trim().to_string();
        self.vrchat.process = self.vrchat.process.trim().to_string();
        self.audio.input_device = trim_optional(self.audio.input_device.take());
        self.audio.output_device = trim_optional(self.audio.output_device.take());
        for custom in self.tts.custom_voices.values_mut() {
            custom.name = trim_optional(custom.name.take());
            for sample in &mut custom.samples {
                sample.language = sample.language.trim().to_lowercase();
            }
        }
    }

    /// 校验显式写出的值、取值范围与跨字段约束。
    ///
    /// 缺省字段已经由 `Default` 填好，因此这里校验的是最终生效的配置；
    /// `[api] key` 例外：`None` 表示尚未配置，是合法状态。
    fn validate(&self) -> Result<()> {
        validate_optional_non_empty(self.api.key.as_deref(), "[api] key")?;
        validate_workspace_host(&self.api.workspace_host)?;
        validate_optional_non_empty(self.audio.input_device.as_deref(), "[audio] input_device")?;
        validate_optional_non_empty(self.audio.output_device.as_deref(), "[audio] output_device")?;
        if self.asr.model.is_empty() {
            bail!("配置项 [asr] model 不能为空");
        }
        validate_hotwords(&self.asr.hotwords)?;
        validate_languages(&self.local.languages, "[self] languages")?;
        validate_target(&self.local.target, self.local.translate, "[self] target")?;
        validate_languages(&self.others.languages, "[others] languages")?;
        validate_target(
            &self.others.target,
            self.others.translate,
            "[others] target",
        )?;
        if self.tts.model.is_empty() {
            bail!("配置项 [tts] model 不能为空");
        }
        if self.tts.voice.is_empty() {
            bail!("配置项 [tts] voice 不能为空");
        }
        if !language::is_chinese_dialect(&self.tts.dialect) {
            bail!(
                "配置项 [tts] dialect {:?} 不受支持；可用值: {:?}",
                self.tts.dialect,
                language::CHINESE_DIALECTS
            );
        }
        if self.cache.file.as_os_str().is_empty()
            || self.cache.file.to_string_lossy().trim().is_empty()
        {
            bail!("配置项 [cache] file 不能为空");
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
            validate_optional_non_empty(
                custom.name.as_deref(),
                &format!("[tts.custom_voices.{name}] name"),
            )?;
            if custom.samples.is_empty() {
                bail!(
                    "自定义音色 [{name}] 至少要有一个 [[tts.custom_voices.{name}.samples]] 声线样本"
                );
            }

            let mut seen_languages = std::collections::HashSet::new();
            for (index, sample) in custom.samples.iter().enumerate() {
                if sample.audio.as_os_str().is_empty()
                    || sample.audio.to_string_lossy().trim().is_empty()
                {
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

    /// 校验 `[translate]`：模型必须有值（开关与目标语言在 `[self]` / `[others]`）。
    fn validate_translate(&self) -> Result<()> {
        if self.translate.model.is_empty() {
            bail!("配置项 [translate] model 不能为空");
        }
        Ok(())
    }

    fn validate_vrchat(&self) -> Result<()> {
        if self.vrchat.process.is_empty() {
            bail!("配置项 [vrchat] process 不能为空（填入目标进程的 exe 名，如 \"VRChat.exe\"）");
        }
        if self.vrchat.osc.port == 0 {
            bail!("配置项 [vrchat.osc] port 必须在 1–65535 之间");
        }
        if !(MIN_VRCHAT_RETENTION_SECS..=MAX_VRCHAT_RETENTION_SECS)
            .contains(&self.vrchat.osc.retention_secs)
        {
            bail!(
                "配置项 [vrchat.osc] retention_secs 必须在 {MIN_VRCHAT_RETENTION_SECS}–{MAX_VRCHAT_RETENTION_SECS} 秒之间"
            );
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

/// 只在表非空时插入，避免生成 `[section]` 空段。
fn insert_table(root: &mut Table, name: &str, table: Table) {
    if !table.is_empty() {
        root.insert(name.to_string(), Value::Table(table));
    }
}

/// 字符串数组 -> TOML array。
fn string_array(values: &[String]) -> Value {
    Value::Array(values.iter().cloned().map(Value::String).collect())
}

/// f32 按最短十进制表示转成 TOML float，避免 1.3 写成二进制尾差。
fn float_value(value: f32) -> Value {
    match format!("{value}").parse::<f64>() {
        Ok(value) => Value::Float(value),
        // best-effort: f32 的 Display 输出一定可被 f64 解析，这里只是不 panic 的兜底。
        Err(_) => Value::Float(f64::from(value)),
    }
}

/// 去掉可选字符串的首尾空白，但**保留显式空字符串**。
///
/// 字段缺省是 `None`；`Some("")` 表示用户显式写了空值，由校验拒绝。
fn trim_optional(value: Option<String>) -> Option<String> {
    value.map(|value| value.trim().to_string())
}

/// 可选字符串如果显式存在，就不能是空字符串。
///
/// 这与“缺省 = 未设置”区分开：想让某项保持未设置，应省略该键，而不是写空值。
fn validate_optional_non_empty(value: Option<&str>, field: &str) -> Result<()> {
    if value.is_some_and(str::is_empty) {
        bail!("配置项 {field} 被显式设置为空；不想设置请删除该键");
    }
    Ok(())
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

/// 语言列表归一：逐项去首尾空白并转小写（别名由 [`language::canonical_language`] 处理）。
fn normalize_languages(languages: &mut [String]) {
    for language in languages.iter_mut() {
        *language = language.trim().to_lowercase();
    }
}

/// 译文语言归一：去首尾空白、转小写，并做别名归一（如 `tl` → `fil`）。
fn normalize_target(target: &mut String) {
    *target = language::canonical_language(target.trim().to_lowercase().as_str()).to_string();
}

/// 校验一条线路的语言列表：非空、代码合法、不重复。
///
/// 列表原样作为该线路 ASR 的 `language_hints`（数量上限由服务端处理），
/// 本地语言判定也使用完整列表。
fn validate_languages(languages: &[String], field: &str) -> Result<()> {
    if languages.is_empty() {
        bail!("配置项 {field} 不能为空（至少填写一种语言，如 [\"zh\"]）");
    }
    let mut seen = std::collections::HashSet::new();
    for lang in languages {
        if !is_asr_language(lang) {
            bail!(
                "配置项 {field} 含有不支持的语言代码 {lang:?}；\
                 可用代码（部分）: zh/en/ja/ko/vi/th/id/ms/tl/hi/ar/fr/de/es/pt/ru/it/...（见文档）"
            );
        }
        if !seen.insert(lang.as_str()) {
            bail!("配置项 {field} 中存在重复语言 {lang:?}");
        }
    }
    Ok(())
}

/// 校验一条线路的译文语言：不能为空；启用翻译时必须落在可合成语言集合内。
fn validate_target(target: &str, translate_enabled: bool, field: &str) -> Result<()> {
    if target.is_empty() {
        bail!("配置项 {field} 不能为空");
    }
    if translate_enabled && !is_cloning_language(language::canonical_language(target)) {
        bail!(
            "配置项 {field} {target:?} 不是可合成的目标语言；可用代码: {:?}",
            language::TRANSLATION_LANGUAGES
        );
    }
    Ok(())
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
        assert_eq!(config.local.languages, vec!["zh".to_string()]);
        assert!(config.local.mic, "默认采集麦克风");
        assert!(!config.local.translate, "默认不翻译我方文本");
        assert_eq!(config.local.target, DEFAULT_SELF_TARGET);
        assert!(config.local.tts, "默认合成我方语音");
        assert!(!config.others.enabled, "默认不侦听对方线路");
        assert_eq!(config.others.languages, vec!["zh".to_string()]);
        assert!(!config.others.translate);
        assert_eq!(config.others.target, DEFAULT_OTHERS_TARGET);
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
        assert_eq!(config.translate.model, DEFAULT_TRANSLATE_MODEL);
        assert_eq!(
            config.translate.granularity,
            TranslateGranularity::Clause,
            "默认翻译粒度是标点即翻"
        );
        assert!(config.translate.show_source);
        assert_eq!(config.vrchat.process, DEFAULT_VRCHAT_PROCESS);
        assert!(!config.vrchat.osc.enabled);
        assert_eq!(config.vrchat.osc.port, DEFAULT_VRCHAT_OSC_PORT);
        assert_eq!(config.vrchat.osc.display, VrchatDisplay::Bilingual);
        assert!(!config.vrchat.osc.show_history, "默认不显示历史句");
        assert_eq!(
            config.vrchat.osc.retention_secs, DEFAULT_VRCHAT_RETENTION_SECS,
            "默认保留 30 秒"
        );
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
hotwords = ["原神", "崩坏：星穹铁道", "Kaoet"]

[self]
mic = false
languages = ["ja", "zh", "en"]
translate = true
target = "ja"
tts = false

[others]
enabled = true
languages = ["en"]
translate = true
target = "zh"

[tts]
model = "tts-test"
voice = "voice-test"
dialect = "四川话"

[cache]
file = "cache/bivox.json"

[translate]
granularity = "sentence"
show_source = false

[vrchat]
process = "VRChat-test.exe"

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
        assert!(!config.local.mic);
        assert_eq!(
            config.local.languages,
            vec!["ja".to_string(), "zh".to_string(), "en".to_string()]
        );
        assert!(config.local.translate);
        assert_eq!(config.local.target, "ja");
        assert!(!config.local.tts);
        assert!(config.others.enabled);
        assert_eq!(config.others.languages, vec!["en".to_string()]);
        assert!(config.others.translate);
        assert_eq!(config.others.target, "zh");
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
        assert_eq!(config.translate.model, DEFAULT_TRANSLATE_MODEL);
        assert_eq!(config.translate.granularity, TranslateGranularity::Sentence);
        assert!(!config.translate.show_source);
        assert_eq!(config.vrchat.process, "VRChat-test.exe");
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
    fn empty_config_is_valid_without_api_key() {
        let config = AppConfig::from_toml_str("").unwrap();
        assert!(config.api.key.is_none(), "空文件应视为未配置 API Key");
        assert_eq!(config.asr.model, DEFAULT_ASR_MODEL);
        assert_eq!(config.tts.voice, DEFAULT_VOICE);
        assert_eq!(config.cache.file, PathBuf::from("cache.json"));
    }

    #[test]
    fn default_config_serializes_to_empty_string() {
        assert_eq!(AppConfig::default().to_toml_string().unwrap(), "");
    }

    #[test]
    fn explicit_defaults_are_omitted_on_serialization() {
        let text = r#"
[api]
key = "sk-test"

[translate]
model = "qwen-mt-flash"
granularity = "clause"
show_source = true

[self]
mic = true
languages = ["zh"]
translate = false
target = "en"
tts = true

[others]
enabled = false
languages = ["zh"]
translate = false
target = "zh"

[vrchat]
process = "VRChat.exe"

[vad]
enabled = true
threshold = 0.5
min_silence_ms = 500
tail_ms = 1000
debug = false
"#;
        let config = AppConfig::from_toml_str(text).unwrap();
        let out = config.to_toml_string().unwrap();

        assert!(out.contains("[api]"));
        assert!(out.contains("key = \"sk-test\""));
        assert!(!out.contains("[translate]"), "{out}");
        assert!(!out.contains("[self]"), "{out}");
        assert!(!out.contains("[others]"), "{out}");
        assert!(!out.contains("[vrchat]"), "{out}");
        assert!(!out.contains("[vad]"), "{out}");
    }

    #[test]
    fn non_default_fields_round_trip_through_minimal_toml() {
        let text = r#"
[api]
key = "sk-test"
workspace_host = "ws-test.cn-beijing.maas.aliyuncs.com"

[asr]
model = "asr-test"
hotwords = ["原神", "Kaoet"]

[self]
mic = false
languages = ["ja", "zh"]
translate = true
target = "ja"
tts = false

[others]
enabled = true
languages = ["en"]
translate = true
target = "zh"

[vrchat]
process = "VRChat-test.exe"

[tts]
model = "tts-test"
voice = "custom:foo"
dialect = "四川话"

[tts.custom_voices.foo]
name = "甲"

[[tts.custom_voices.foo.samples]]
language = "zh"
audio = "a.wav"
rate = 1.3

[[tts.custom_voices.foo.samples]]
language = "ja"
audio = "b.wav"

[cache]
file = "cache/bivox.json"

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

[translate]
model = "mt-test"
granularity = "sentence"
show_source = false

[vrchat.osc]
enabled = true
port = 9001
display = "translation"
show_history = true
retention_secs = 12
"#;
        let config = AppConfig::from_toml_str(text).unwrap();
        let out = config.to_toml_string().unwrap();
        assert!(
            out.contains("[self]"),
            "段名必须是 self（不是 local）: {out}"
        );
        assert!(out.contains("[others]"), "{out}");
        let round = AppConfig::from_toml_str(&out).unwrap();

        assert_eq!(round.api.workspace_host, config.api.workspace_host);
        assert_eq!(round.asr.model, "asr-test");
        assert!(!round.local.mic);
        assert_eq!(
            round.local.languages,
            vec!["ja".to_string(), "zh".to_string()]
        );
        assert!(round.local.translate);
        assert_eq!(round.local.target, "ja");
        assert!(!round.local.tts);
        assert!(round.others.enabled);
        assert_eq!(round.others.languages, vec!["en".to_string()]);
        assert!(round.others.translate);
        assert_eq!(round.others.target, "zh");
        assert_eq!(round.vrchat.process, "VRChat-test.exe");
        assert_eq!(
            round.asr.hotwords,
            vec!["原神".to_string(), "Kaoet".to_string()]
        );
        assert_eq!(round.tts.model, "tts-test");
        assert_eq!(round.tts.voice, "custom:foo");
        assert_eq!(round.tts.dialect, "四川话");
        assert_eq!(round.tts.custom_voices["foo"].name.as_deref(), Some("甲"));
        assert_eq!(round.tts.custom_voices["foo"].samples.len(), 2);
        assert!((round.tts.custom_voices["foo"].samples[0].rate - 1.3).abs() < 1e-6);
        assert_eq!(round.tts.custom_voices["foo"].samples[1].rate, 1.0);
        assert_eq!(round.cache.file, PathBuf::from("cache/bivox.json"));
        assert_eq!(round.audio.input_device.as_deref(), Some("Microphone"));
        assert!(!round.vad.enabled);
        assert_eq!(round.vad.threshold, 0.42);
        assert_eq!(round.vad.min_silence_ms, 320);
        assert_eq!(round.vad.tail_ms, 800);
        assert!(round.vad.debug);
        assert_eq!(round.chunking.words, 10);
        assert_eq!(round.translate.model, "mt-test");
        assert_eq!(round.translate.granularity, TranslateGranularity::Sentence);
        assert!(!round.translate.show_source);
        assert!(round.vrchat.osc.enabled);
        assert_eq!(round.vrchat.osc.port, 9001);
        assert_eq!(round.vrchat.osc.display, VrchatDisplay::Translation);
        assert!(round.vrchat.osc.show_history);
        assert_eq!(round.vrchat.osc.retention_secs, 12);

        assert_eq!(round.to_toml_string().unwrap(), out, "序列化应稳定");
        assert_eq!(out.matches("rate = ").count(), 1, "默认语速不应写回: {out}");
    }

    #[test]
    fn explicit_empty_api_key_is_rejected() {
        for text in ["[api]\nkey = \"\"\n", "[api]\nkey = \"   \"\n"] {
            let err = AppConfig::from_toml_str(text).unwrap_err();
            assert!(
                format!("{err:#}").contains("[api] key"),
                "显式空 Key 应被拒绝: {err:#}"
            );
        }
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
    fn explicit_empty_optional_device_is_rejected() {
        for text in [
            "[api]\nkey = \"sk-test\"\n[audio]\ninput_device = \"  \"\n",
            "[api]\nkey = \"sk-test\"\n[audio]\noutput_device = \"\"\n",
        ] {
            let err = AppConfig::from_toml_str(text).unwrap_err();
            assert!(
                format!("{err:#}").contains("[audio]"),
                "显式空设备名应被拒绝: {err:#}"
            );
        }
    }

    #[test]
    fn explicit_empty_cache_file_is_rejected() {
        let err = AppConfig::from_toml_str("[api]\nkey = \"sk-test\"\n[cache]\nfile = \"  \"\n")
            .unwrap_err();
        assert!(
            format!("{err:#}").contains("[cache] file"),
            "显式空缓存路径应被拒绝: {err:#}"
        );
    }

    #[test]
    fn explicit_empty_translate_target_is_rejected_even_when_disabled() {
        for text in [
            "[api]\nkey = \"sk-test\"\n[self]\ntarget = \"\"\n",
            "[api]\nkey = \"sk-test\"\n[others]\ntarget = \"\"\n",
        ] {
            let err = AppConfig::from_toml_str(text).unwrap_err();
            assert!(
                format!("{err:#}").contains("target 不能为空"),
                "显式空目标语言应被拒绝: {err:#}"
            );
        }
    }

    #[test]
    fn explicit_empty_sample_audio_is_rejected() {
        let err = AppConfig::from_toml_str(
            "[api]\nkey = \"sk-test\"\n\n[tts.custom_voices.foo]\n\n[[tts.custom_voices.foo.samples]]\nlanguage = \"zh\"\naudio = \"   \"\n",
        )
        .unwrap_err();
        assert!(
            format!("{err:#}").contains("audio"),
            "显式空样例音频路径应被拒绝: {err:#}"
        );
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
        for section in ["[self]", "[others]"] {
            let empty = AppConfig::from_toml_str(&format!(
                "[api]\nkey = \"sk-test\"\n{section}\nlanguages = []\n"
            ))
            .unwrap_err();
            assert!(
                format!("{empty:#}").contains("languages 不能为空"),
                "{section} 的空语言列表应被拒绝: {empty:#}"
            );

            let bad = AppConfig::from_toml_str(&format!(
                "[api]\nkey = \"sk-test\"\n{section}\nlanguages = [\"xx\"]\n"
            ))
            .unwrap_err();
            assert!(format!("{bad:#}").contains("不支持的语言代码"));

            let duplicate = AppConfig::from_toml_str(&format!(
                "[api]\nkey = \"sk-test\"\n{section}\nlanguages = [\"ja\",\"ja\"]\n"
            ))
            .unwrap_err();
            assert!(format!("{duplicate:#}").contains("重复"));
        }
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

[self]
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

    #[test]
    fn vrchat_osc_config_is_parsed_and_validated() {
        let config = AppConfig::from_toml_str(
            "[api]\nkey = \"sk-test\"\n\n[vrchat.osc]\nenabled = true\nport = 9001\ndisplay = \"translation\"\nshow_history = true\nretention_secs = 20\n",
        )
        .unwrap();
        assert!(config.vrchat.osc.enabled);
        assert_eq!(config.vrchat.osc.port, 9001);
        assert_eq!(config.vrchat.osc.display, VrchatDisplay::Translation);
        assert!(config.vrchat.osc.show_history);
        assert_eq!(config.vrchat.osc.retention_secs, 20);

        let err = AppConfig::from_toml_str("[api]\nkey = \"sk-test\"\n\n[vrchat.osc]\nport = 0\n")
            .unwrap_err();
        assert!(
            format!("{err:#}").contains("[vrchat.osc] port"),
            "port=0 应被拒绝: {err:#}"
        );

        for retention in ["0", "31"] {
            let err = AppConfig::from_toml_str(&format!(
                "[api]\nkey = \"sk-test\"\n\n[vrchat.osc]\nretention_secs = {retention}\n"
            ))
            .unwrap_err();
            assert!(
                format!("{err:#}").contains("[vrchat.osc] retention_secs"),
                "retention_secs={retention} 应被拒绝: {err:#}"
            );
        }

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
    fn vrchat_process_must_not_be_empty() {
        let err =
            AppConfig::from_toml_str("[api]\nkey = \"sk-test\"\n[vrchat]\nprocess = \"  \"\n")
                .unwrap_err();
        assert!(
            format!("{err:#}").contains("[vrchat] process 不能为空"),
            "显式空进程名应被拒绝: {err:#}"
        );
    }

    #[test]
    fn translate_target_is_normalized_and_validated() {
        let config = AppConfig::from_toml_str(
            "[api]\nkey = \"sk-test\"\n[self]\ntranslate = true\ntarget = \" TL \"\n\
             \n[others]\nenabled = true\ntranslate = true\ntarget = \" TL \"\n",
        )
        .unwrap();
        assert_eq!(config.local.target, "fil", "tl 应归一为 fil，并去掉空白");
        assert_eq!(config.others.target, "fil");

        let err = AppConfig::from_toml_str(
            "[api]\nkey = \"sk-test\"\n[self]\ntranslate = true\ntarget = \"xx\"\n",
        )
        .unwrap_err();
        assert!(
            format!("{err:#}").contains("[self] target"),
            "启用翻译时非法目标语言应被拒绝: {err:#}"
        );

        let err = AppConfig::from_toml_str(
            "[api]\nkey = \"sk-test\"\n[others]\nenabled = true\ntranslate = true\ntarget = \"xx\"\n",
        )
        .unwrap_err();
        assert!(
            format!("{err:#}").contains("[others] target"),
            "对方线路启用翻译时非法目标语言应被拒绝: {err:#}"
        );

        // 关闭翻译时不校验目标语言，允许先填错、之后再改
        let config =
            AppConfig::from_toml_str("[api]\nkey = \"sk-test\"\n[self]\ntarget = \"xx\"\n")
                .unwrap();
        assert_eq!(config.local.target, "xx");
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
