//! 各页签的未保存草稿：从配置生成、判断是否有改动、写回配置。
//!
//! 一个配置字段在 GUI 侧只有三件事：草稿值（这里）、「是否脏」（和 `from_config`
//! 的结果比较，或跟着字段走一小段比较逻辑）、写回（`apply_to`）。集中到这里以后，
//! `BivoxApp` 不再逐字段维护 `sync_* / *_dirty`，新增字段只需要改本文件。
//!
//! 两条与「改了就脏」不同的约定（见 docs/decisions.md）：
//! - `[asr] hotwords` 按**归一化后的列表**比较：空行、重复项不算改动（D32）；
//! - 语音合成页「当前音色」的两个输入框是**粘性**的：跟随配置只改变选中的一侧，
//!   另一侧保留用户上次输入的文本（D22），因此脏状态只看选中的那一侧。

use std::path::PathBuf;

use crate::config::{
    AppConfig, CUSTOM_VOICE_PREFIX, DEFAULT_VOICE, normalize_hotwords, normalize_languages,
    normalize_target,
};
use crate::voice::editor::{self, VoiceDraft};

use super::VoiceMode;

/// 所有页签的草稿。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Drafts {
    pub settings: SettingsDraft,
    pub asr: AsrDraft,
    pub tts: TtsDraft,
    pub local: LocalDraft,
    pub others: OthersDraft,
    pub vad: VadDraft,
    pub translate_model: String,
    pub vrchat_process: String,
}

impl Drafts {
    /// 从配置重建草稿。
    ///
    /// `previous` 只用于语音合成页「当前音色」的粘性输入框；启动时传
    /// [`Drafts::default`] 即可。
    pub fn from_config(config: &AppConfig, previous: &Drafts) -> Self {
        Self {
            settings: SettingsDraft::from_config(config),
            asr: AsrDraft::from_config(config),
            tts: TtsDraft::from_config(config, &previous.tts),
            local: LocalDraft::from_config(config),
            others: OthersDraft::from_config(config),
            vad: VadDraft::from_config(config),
            translate_model: config.translate.model.clone(),
            vrchat_process: config.vrchat.process.clone(),
        }
    }

    /// 翻译页模型是否有改动。
    pub fn translate_model_dirty(&self, config: &AppConfig) -> bool {
        self.translate_model.trim() != config.translate.model
    }

    /// VRChat 页「监听进程」是否有改动。
    pub fn vrchat_process_dirty(&self, config: &AppConfig) -> bool {
        self.vrchat_process.trim() != config.vrchat.process
    }
}

/// `[api]` + `[cache] file` + `[chunking] words`（设置页）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SettingsDraft {
    /// API Key 草稿（留空保存 = 清除该项）。
    pub api_key: String,
    pub workspace_host: String,
    pub cache_file: String,
    pub chunk_words: usize,
}

impl SettingsDraft {
    pub(super) fn from_config(config: &AppConfig) -> Self {
        Self {
            api_key: config.api.key.clone().unwrap_or_default(),
            workspace_host: config.api.workspace_host.clone(),
            cache_file: config.cache.file.to_string_lossy().into_owned(),
            chunk_words: config.chunking.words,
        }
    }

    pub fn is_dirty(&self, config: &AppConfig) -> bool {
        let configured_key = config.api.key.as_deref().unwrap_or("");
        self.api_key.trim() != configured_key
            || self.workspace_host.trim() != config.api.workspace_host
            || self.cache_file.trim() != config.cache.file.to_string_lossy()
            || self.chunk_words != config.chunking.words
    }

    /// 写回；API Key 留空表示删除该项（不写空字符串）。
    pub fn apply_to(&self, config: &mut AppConfig) {
        let key = self.api_key.trim();
        config.api.key = (!key.is_empty()).then(|| key.to_string());
        config.api.workspace_host = self.workspace_host.trim().to_string();
        config.cache.file = PathBuf::from(self.cache_file.trim());
        config.chunking.words = self.chunk_words;
    }
}

/// `[asr]`（语音识别页）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AsrDraft {
    pub model: String,
    /// 多行文本，一行一个热词。
    pub hotwords: String,
}

impl AsrDraft {
    pub(super) fn from_config(config: &AppConfig) -> Self {
        Self {
            model: config.asr.model.clone(),
            hotwords: config.asr.hotwords.join("\n"),
        }
    }

    /// 归一化后的热词列表（一行一个；空行忽略）。
    pub fn hotwords_list(&self) -> Vec<String> {
        let lines: Vec<String> = self.hotwords.lines().map(str::to_string).collect();
        normalize_hotwords(&lines)
    }

    pub fn is_dirty(&self, config: &AppConfig) -> bool {
        self.model.trim() != config.asr.model || self.hotwords_list() != config.asr.hotwords
    }

    pub fn apply_to(&self, config: &mut AppConfig) {
        config.asr.model = self.model.trim().to_string();
        config.asr.hotwords = self.hotwords_list();
    }
}

/// `[tts]`（语音合成页）：合成模型、中文方言、自定义音色草稿、当前音色选择。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TtsDraft {
    pub model: String,
    pub dialect: String,
    /// 自定义音色草稿（保存前只改这里）。
    pub drafts: Vec<VoiceDraft>,
    /// 「当前音色」选择方式。
    pub mode: VoiceMode,
    /// 「现有音色」输入框（粘性：跟随配置时不覆盖已有文本）。
    pub input: String,
    /// 「自定义音色」下拉的配置键名（粘性同上）。
    pub custom_key: String,
}

impl TtsDraft {
    pub(super) fn from_config(config: &AppConfig, previous: &Self) -> Self {
        let mut draft = Self {
            model: config.tts.model.clone(),
            dialect: config.tts.dialect.clone(),
            drafts: editor::drafts_from_config(config),
            mode: previous.mode,
            input: previous.input.clone(),
            custom_key: previous.custom_key.clone(),
        };
        draft.follow_config_selection(config);
        draft
    }

    /// 让「当前音色」跟随配置：自定义音色优先；两个输入框保留上次的文本。
    fn follow_config_selection(&mut self, config: &AppConfig) {
        if let Some(name) = config.custom_voice_name().map(str::to_string) {
            self.mode = VoiceMode::Custom;
            self.custom_key = name;
            if self.input.trim().is_empty() {
                self.input = DEFAULT_VOICE.to_string();
            }
        } else {
            self.mode = VoiceMode::Existing;
            self.input = config.tts.voice.trim().to_string();
            if self.custom_key.is_empty() {
                self.custom_key = config
                    .tts
                    .custom_voices
                    .keys()
                    .next()
                    .cloned()
                    .unwrap_or_default();
            }
        }
    }

    /// 顶部「当前音色」的草稿值（`[tts] voice` 形态）。
    pub fn selection(&self) -> String {
        match self.mode {
            VoiceMode::Existing => self.input.trim().to_string(),
            VoiceMode::Custom => {
                let key = self.custom_key.trim();
                if key.is_empty() {
                    String::new()
                } else {
                    format!("{CUSTOM_VOICE_PREFIX}{key}")
                }
            }
        }
    }

    pub fn model_dirty(&self, config: &AppConfig) -> bool {
        self.model.trim() != config.tts.model
    }

    pub fn dialect_dirty(&self, config: &AppConfig) -> bool {
        self.dialect.trim() != config.tts.dialect
    }

    /// 自定义音色表是否有改动。
    pub fn voices_dirty(&self, config: &AppConfig) -> bool {
        self.drafts != editor::drafts_from_config(config)
    }

    /// 「当前音色」是否有改动（只看选中的那一侧）。
    pub fn selection_dirty(&self, config: &AppConfig) -> bool {
        self.selection() != config.tts.voice.trim()
    }

    pub fn is_dirty(&self, config: &AppConfig) -> bool {
        self.model_dirty(config)
            || self.dialect_dirty(config)
            || self.voices_dirty(config)
            || self.selection_dirty(config)
    }
}

/// `[self]`（「自己」页）。
///
/// 麦克风开关不在这里：它切换即写回配置（D43），不参与本页的脏状态。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LocalDraft {
    pub languages: Vec<String>,
    pub translate: bool,
    pub target: String,
    pub tts: bool,
}

impl LocalDraft {
    pub(super) fn from_config(config: &AppConfig) -> Self {
        Self {
            languages: config.local.languages.clone(),
            translate: config.local.translate,
            target: config.local.target.clone(),
            tts: config.local.tts,
        }
    }

    pub fn is_dirty(&self, config: &AppConfig) -> bool {
        self.languages != config.local.languages
            || self.translate != config.local.translate
            || self.target != config.local.target
            || self.tts != config.local.tts
    }

    pub fn apply_to(&self, config: &mut AppConfig) {
        config.local.languages = self.languages.clone();
        config.local.translate = self.translate;
        config.local.target = self.target.clone();
        config.local.tts = self.tts;
        // 语言列表与译文语言在写回前归一（配置解析时也会归一，这里保证内存态一致）
        normalize_languages(&mut config.local.languages);
        normalize_target(&mut config.local.target);
    }
}

/// `[others]`（「他人」页）。侦听开关同样不进草稿（D43）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OthersDraft {
    pub languages: Vec<String>,
    pub translate: bool,
    pub target: String,
}

impl OthersDraft {
    pub(super) fn from_config(config: &AppConfig) -> Self {
        Self {
            languages: config.others.languages.clone(),
            translate: config.others.translate,
            target: config.others.target.clone(),
        }
    }

    pub fn is_dirty(&self, config: &AppConfig) -> bool {
        self.languages != config.others.languages
            || self.translate != config.others.translate
            || self.target != config.others.target
    }

    pub fn apply_to(&self, config: &mut AppConfig) {
        config.others.languages = self.languages.clone();
        config.others.translate = self.translate;
        config.others.target = self.target.clone();
        normalize_languages(&mut config.others.languages);
        normalize_target(&mut config.others.target);
    }
}

/// `[vad]`（音频页下半部分）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VadDraft {
    pub enabled: bool,
    pub threshold: f32,
    pub min_silence_ms: u32,
    pub tail_ms: u32,
    pub debug: bool,
}

impl VadDraft {
    pub(super) fn from_config(config: &AppConfig) -> Self {
        Self {
            enabled: config.vad.enabled,
            threshold: config.vad.threshold,
            min_silence_ms: config.vad.min_silence_ms,
            tail_ms: config.vad.tail_ms,
            debug: config.vad.debug,
        }
    }

    pub fn is_dirty(&self, config: &AppConfig) -> bool {
        self.enabled != config.vad.enabled
            || self.threshold != config.vad.threshold
            || self.min_silence_ms != config.vad.min_silence_ms
            || self.tail_ms != config.vad.tail_ms
            || self.debug != config.vad.debug
    }

    pub fn apply_to(&self, config: &mut AppConfig) {
        config.vad.enabled = self.enabled;
        config.vad.threshold = self.threshold;
        config.vad.min_silence_ms = self.min_silence_ms;
        config.vad.tail_ms = self.tail_ms;
        config.vad.debug = self.debug;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppConfig, CustomVoiceConfig, CustomVoiceSample};

    fn sample_config() -> AppConfig {
        AppConfig::from_toml_str(
            r#"
[api]
key = "sk-test"
workspace_host = "ws.example.com"

[asr]
model = "asr-test"
hotwords = ["原神", "Kaoet"]

[self]
languages = ["ja", "zh"]
translate = true
target = "ja"
tts = false

[others]
languages = ["en"]
translate = true
target = "zh"

[tts]
model = "tts-test"
dialect = "四川话"

[vad]
enabled = false
threshold = 0.42
min_silence_ms = 320

[chunking]
words = 10

[translate]
model = "mt-test"
"#,
        )
        .unwrap()
    }

    #[test]
    fn drafts_start_clean_and_write_back_the_same_values() {
        let config = sample_config();
        let drafts = Drafts::from_config(&config, &Drafts::default());

        // 刚从配置生成时都不脏
        assert!(!drafts.settings.is_dirty(&config));
        assert!(!drafts.asr.is_dirty(&config));
        assert!(!drafts.tts.is_dirty(&config));
        assert!(!drafts.local.is_dirty(&config));
        assert!(!drafts.others.is_dirty(&config));
        assert!(!drafts.vad.is_dirty(&config));
        assert!(!drafts.translate_model_dirty(&config));
        assert!(!drafts.vrchat_process_dirty(&config));

        // 写回一份默认配置后，值应与原配置一致
        let mut target = AppConfig::default();
        drafts.settings.apply_to(&mut target);
        drafts.asr.apply_to(&mut target);
        drafts.local.apply_to(&mut target);
        drafts.others.apply_to(&mut target);
        drafts.vad.apply_to(&mut target);
        target.translate.model = drafts.translate_model.clone();

        assert_eq!(target.api.key, config.api.key);
        assert_eq!(target.api.workspace_host, config.api.workspace_host);
        assert_eq!(target.asr.model, config.asr.model);
        assert_eq!(target.asr.hotwords, config.asr.hotwords);
        assert_eq!(target.local.languages, config.local.languages);
        assert_eq!(target.local.target, config.local.target);
        assert!(!target.local.tts);
        assert_eq!(target.others.target, config.others.target);
        assert_eq!(target.vad.threshold, config.vad.threshold);
        assert_eq!(target.vad.min_silence_ms, config.vad.min_silence_ms);
        assert_eq!(target.chunking.words, config.chunking.words);
        assert_eq!(target.translate.model, config.translate.model);
    }

    #[test]
    fn asr_blank_hotword_lines_are_not_dirty() {
        let config = sample_config();
        let mut asr = AsrDraft::from_config(&config);
        asr.hotwords.push_str("\n\n   \n");
        assert!(!asr.is_dirty(&config), "空行不算改动（D32）");
        asr.hotwords.push_str("新词");
        assert!(asr.is_dirty(&config));
    }

    #[test]
    fn tts_keeps_the_inactive_voice_input_but_only_compares_the_selected_side() {
        let mut config = sample_config();
        config.tts.voice = "custom:a".to_string();
        config.tts.custom_voices.insert(
            "a".to_string(),
            CustomVoiceConfig {
                name: None,
                samples: vec![CustomVoiceSample {
                    language: "zh".to_string(),
                    audio: std::path::PathBuf::from("a.wav"),
                    rate: 1.0,
                }],
            },
        );

        let previous = TtsDraft {
            input: "typed-earlier".to_string(),
            ..Default::default()
        };
        let tts = TtsDraft::from_config(&config, &previous);

        assert_eq!(tts.mode, VoiceMode::Custom);
        assert_eq!(tts.input, "typed-earlier", "非选中侧保留上次输入");
        assert!(!tts.is_dirty(&config), "跟随配置后不应是脏的");

        // 只改非选中侧不算改动
        let mut tts = tts;
        tts.input = "another".to_string();
        assert!(!tts.is_dirty(&config));
        // 改选中侧才算
        tts.custom_key = "b".to_string();
        assert!(tts.is_dirty(&config));
    }
}
