//! 音色编辑草稿模型：`config.toml` ↔ 草稿 ↔ 写回数据（纯函数，便于单测）。
//!
//! GUI 流程：`drafts_from_config` 读出草稿 → 界面直接改草稿（含每个样本的语速）→
//! `drafts_to_entries` 校验并生成 `(配置键名, CustomVoiceConfig)` 列表 →
//! 合并进 `AppConfig` 后由 `config_edit::save` 写回（写前校验 + `.bak`）。

use std::collections::HashSet;

use anyhow::{Result, bail};

use crate::config::{
    AppConfig, CustomVoiceConfig, CustomVoiceSample, DEFAULT_SAMPLE_RATE, MAX_SAMPLE_RATE,
    MIN_SAMPLE_RATE, is_valid_sample_rate,
};

/// 一个自定义音色的编辑草稿。
///
/// 不含 `Eq`：`rate` 是 `f32`，草稿比较只用于判断“是否有未保存改动”，`PartialEq` 足够。
#[derive(Debug, Clone, PartialEq)]
pub struct VoiceDraft {
    /// 配置键名（`[tts] voice = "custom:<key>"` 用它引用）。
    pub key: String,
    /// 显示名（空 = 用配置键名）。
    pub display_name: String,
    /// 有序样本：第一个是默认声线。
    pub samples: Vec<SampleDraft>,
}

/// 一份声线样本草稿。
#[derive(Debug, Clone, PartialEq)]
pub struct SampleDraft {
    /// 声音复刻语言代码（如 `zh` / `ja`）。
    pub language: String,
    /// 样例音频路径（相对 exe 目录或绝对路径）。
    pub audio: String,
    /// 该样本的语速倍率（百炼 TTS `rate`，0.5–2.0，默认 1.0）。
    pub rate: f32,
}

impl VoiceDraft {
    /// 从配置结构生成草稿。
    pub fn from_config(key: &str, voice: &CustomVoiceConfig) -> Self {
        Self {
            key: key.to_string(),
            display_name: voice.display_name(key),
            samples: voice
                .samples
                .iter()
                .map(|sample| SampleDraft {
                    language: sample.language.clone(),
                    audio: sample.audio.to_string_lossy().into_owned(),
                    rate: sample.rate,
                })
                .collect(),
        }
    }

    /// 转回配置结构（不做合法性校验，校验在 [`drafts_to_entries`]）。
    pub fn to_config(&self) -> CustomVoiceConfig {
        let key = self.key.trim();
        let name = self.display_name.trim();
        // 显示名与键名相同 = 未设置，写 None（保持 TOML 简洁）
        let name = (!name.is_empty() && name != key).then(|| name.to_string());
        CustomVoiceConfig {
            name,
            samples: self
                .samples
                .iter()
                .map(|sample| CustomVoiceSample {
                    language: sample.language.trim().to_string(),
                    audio: std::path::PathBuf::from(sample.audio.trim()),
                    rate: sample.rate,
                })
                .collect(),
        }
    }
}

/// 从配置读草稿，按显示名（再按键名）排序，保证列表稳定。
pub fn drafts_from_config(config: &AppConfig) -> Vec<VoiceDraft> {
    let mut drafts: Vec<VoiceDraft> = config
        .tts
        .custom_voices
        .iter()
        .map(|(key, voice)| VoiceDraft::from_config(key, voice))
        .collect();
    drafts.sort_by(|a, b| {
        a.display_name
            .cmp(&b.display_name)
            .then_with(|| a.key.cmp(&b.key))
    });
    drafts
}

/// 校验草稿并转成写回用的 `(配置键名, 音色)` 列表（保留草稿顺序）。
pub fn drafts_to_entries(drafts: &[VoiceDraft]) -> Result<Vec<(String, CustomVoiceConfig)>> {
    let mut keys = HashSet::new();
    let mut entries = Vec::with_capacity(drafts.len());

    for (index, draft) in drafts.iter().enumerate() {
        let key = draft.key.trim();
        if key.is_empty() {
            bail!("第 {} 个音色的配置键名不能为空", index + 1);
        }
        if !keys.insert(key.to_string()) {
            bail!("配置键名 `{key}` 重复");
        }
        if draft.samples.is_empty() {
            bail!("音色 `{key}` 至少需要一个声线样本");
        }

        let mut languages = HashSet::new();
        for (sample_index, sample) in draft.samples.iter().enumerate() {
            let language = sample.language.trim();
            if language.is_empty() {
                bail!("音色 `{key}` 的第 {} 个样本缺少语言", sample_index + 1);
            }
            if !crate::language::is_cloning_language(language) {
                bail!("音色 `{key}` 的语言 `{language}` 不在声音复刻支持列表里");
            }
            if !languages.insert(language.to_string()) {
                bail!("音色 `{key}` 的语言 `{language}` 重复，请删除多余样本");
            }
            if sample.audio.trim().is_empty() {
                bail!("音色 `{key}` 的语言 `{language}` 缺少样例音频");
            }
            if !is_valid_sample_rate(sample.rate) {
                bail!(
                    "音色 `{key}` 的语言 `{language}` 的语速 {} 无效；\
                     语速倍率必须在 {MIN_SAMPLE_RATE}~{MAX_SAMPLE_RATE} 之间（默认 {DEFAULT_SAMPLE_RATE}）",
                    sample.rate
                );
            }
        }

        entries.push((key.to_string(), draft.to_config()));
    }

    Ok(entries)
}

/// 把用户选中的音频路径转成写进配置的形式：在 exe 目录内 → 相对路径，否则绝对路径。
pub fn config_audio_path(exe_dir: &std::path::Path, picked: &std::path::Path) -> String {
    match picked.strip_prefix(exe_dir) {
        Ok(relative) => relative.to_string_lossy().into_owned(),
        Err(_) => picked.to_string_lossy().into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOML: &str = r#"
[api]
key = "sk-test"

[tts]
voice = "custom:a"

[tts.custom_voices.a]
name = "甲音色"
[[tts.custom_voices.a.samples]]
language = "zh"
audio = "voices/a-zh.wav"
rate = 1.2

[tts.custom_voices.b]
[[tts.custom_voices.b.samples]]
language = "ja"
audio = "C:/voices/b-ja.wav"
"#;

    fn config() -> AppConfig {
        AppConfig::from_toml_str(TOML).unwrap()
    }

    #[test]
    fn drafts_roundtrip_config() {
        let config = config();
        let drafts = drafts_from_config(&config);

        // 按显示名字节序排序：ASCII "b" 在中文"甲音色"之前
        assert_eq!(drafts.len(), 2);
        assert_eq!(drafts[0].key, "b");
        assert_eq!(drafts[0].display_name, "b");
        // name 缺省 → 显示名用键名，写回时 name 为 None
        assert_eq!(drafts[0].to_config().name, None);
        assert_eq!(drafts[1].key, "a");
        assert_eq!(drafts[1].display_name, "甲音色");
        assert_eq!(drafts[1].samples[0].language, "zh");
        assert!((drafts[1].samples[0].rate - 1.2).abs() < 1e-6);

        let entries = drafts_to_entries(&drafts).unwrap();
        let a = &entries.iter().find(|(key, _)| key == "a").unwrap().1;
        assert_eq!(a.name.as_deref(), Some("甲音色"));
        assert_eq!(a.samples[0].audio.to_string_lossy(), "voices/a-zh.wav");
        assert!((a.samples[0].rate - 1.2).abs() < 1e-6);
        let b = &entries.iter().find(|(key, _)| key == "b").unwrap().1;
        assert_eq!(b.samples[0].language, "ja");
        assert_eq!(
            b.samples[0].rate, DEFAULT_SAMPLE_RATE,
            "未配置 rate 的样本应使用默认语速 1.0"
        );
    }

    #[test]
    fn same_display_name_as_key_is_written_as_none() {
        let mut draft = VoiceDraft {
            key: "kaoet".to_string(),
            display_name: "kaoet".to_string(),
            samples: vec![SampleDraft {
                language: "zh".to_string(),
                audio: "a.wav".to_string(),
                rate: DEFAULT_SAMPLE_RATE,
            }],
        };
        assert_eq!(draft.to_config().name, None);
        draft.display_name = "  卡 . ".to_string();
        assert_eq!(draft.to_config().name.as_deref(), Some("卡 ."));
    }

    #[test]
    fn rejects_invalid_drafts() {
        let sample = |language: &str, audio: &str| SampleDraft {
            language: language.to_string(),
            audio: audio.to_string(),
            rate: DEFAULT_SAMPLE_RATE,
        };
        let draft = |key: &str, samples: Vec<SampleDraft>| VoiceDraft {
            key: key.to_string(),
            display_name: String::new(),
            samples,
        };

        let err = |drafts: &[VoiceDraft]| drafts_to_entries(drafts).unwrap_err().to_string();

        assert!(err(&[draft("", vec![sample("zh", "a.wav")])]).contains("键名不能为空"));
        assert!(
            err(&[
                draft("a", vec![sample("zh", "a.wav")]),
                draft("a", vec![sample("ja", "b.wav")])
            ])
            .contains("重复")
        );
        assert!(err(&[draft("a", vec![])]).contains("至少需要一个声线样本"));
        assert!(err(&[draft("a", vec![sample("", "a.wav")])]).contains("缺少语言"));
        assert!(err(&[draft("a", vec![sample("xx", "a.wav")])]).contains("不在声音复刻支持列表"));
        assert!(
            err(&[draft(
                "a",
                vec![sample("zh", "a.wav"), sample("zh", "b.wav")]
            )])
            .contains("重复")
        );
        assert!(err(&[draft("a", vec![sample("zh", "  ")])]).contains("缺少样例音频"));
        assert!(
            err(&[draft(
                "a",
                vec![SampleDraft {
                    language: "zh".to_string(),
                    audio: "a.wav".to_string(),
                    rate: 2.5,
                }]
            )])
            .contains("语速")
        );
    }

    #[test]
    fn audio_path_is_relative_inside_exe_dir() {
        let exe_dir = std::path::Path::new("C:/kaols");
        let inside = std::path::Path::new("C:/kaols/voices/a.wav");
        let outside = std::path::Path::new("C:/elsewhere/b.wav");
        assert_eq!(config_audio_path(exe_dir, inside), "voices/a.wav");
        assert_eq!(config_audio_path(exe_dir, outside), "C:/elsewhere/b.wav");
    }
}
