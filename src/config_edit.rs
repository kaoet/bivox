//! 用 `toml_edit` 对 `config.toml` 做**定点修改**（决策 D3，见 docs/decisions.md）。
//!
//! 目标：只动 GUI 负责的那几个字段（`[api] key`、`[audio]` 设备……），
//! 其余字段、注释、顺序原样保留；写盘前先用 `AppConfig::from_toml_str` 校验整份文本，
//! 校验失败不落盘；写盘前把旧文件备份成 `config.toml.bak`。

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table};

use crate::config::CustomVoiceConfig;

/// 读取配置原文。
pub fn read(path: &Path) -> Result<String> {
    fs::read_to_string(path).with_context(|| format!("读取配置文件 {} 失败", path.display()))
}

/// 设置 `[api] key`；`None` 或空字符串表示删除该键（= 未配置）。
pub fn set_api_key(original: &str, key: Option<&str>) -> Result<String> {
    let mut doc = parse(original)?;
    let api = ensure_table(&mut doc, "api")?;

    match normalized(key) {
        Some(key) => api["key"] = toml_edit::value(key),
        None => {
            api.remove("key");
        }
    }

    render(&doc, original)
}

/// 设置 `[audio] input_device` / `output_device`；`None` 表示删除该键（= 系统默认设备）。
pub fn set_audio_devices(
    original: &str,
    input: Option<&str>,
    output: Option<&str>,
) -> Result<String> {
    let mut doc = parse(original)?;
    let audio = ensure_table(&mut doc, "audio")?;

    set_or_remove(audio, "input_device", input);
    set_or_remove(audio, "output_device", output);

    render(&doc, original)
}

/// 整体替换 `[tts.custom_voices]` 表（键名 → 音色），其余字段与注释保留。
pub fn set_custom_voices(original: &str, voices: &[(String, CustomVoiceConfig)]) -> Result<String> {
    let mut doc = parse(original)?;
    let tts = ensure_table(&mut doc, "tts")?;

    let mut table = Table::new();
    for (key, voice) in voices {
        let mut voice_table = Table::new();
        if let Some(name) = voice
            .name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            voice_table["name"] = toml_edit::value(name);
        }

        let mut samples = ArrayOfTables::new();
        for sample in &voice.samples {
            let mut sample_table = Table::new();
            sample_table["language"] = toml_edit::value(sample.language.trim());
            sample_table["audio"] = toml_edit::value(sample.audio.to_string_lossy().as_ref());
            samples.push(sample_table);
        }
        voice_table["samples"] = Item::ArrayOfTables(samples);

        table[key.as_str()] = Item::Table(voice_table);
    }

    tts["custom_voices"] = Item::Table(table);
    render(&doc, original)
}

/// 设置生效音色 `[tts] voice`（内置音色名或 `custom:<键名>`）。
pub fn set_active_voice(original: &str, voice: &str) -> Result<String> {
    let mut doc = parse(original)?;
    let tts = ensure_table(&mut doc, "tts")?;
    tts["voice"] = toml_edit::value(voice.trim());
    render(&doc, original)
}

/// 写回配置：先校验新文本，再备份 `.bak`，最后写盘。
///
/// `original` 用来判断是否真的有改动；没有改动直接返回。
pub fn save(path: &Path, original: &str, updated: &str) -> Result<()> {
    if updated == original {
        return Ok(());
    }

    crate::config::AppConfig::from_toml_str(updated)
        .with_context(|| "新配置校验失败，已放弃写回（配置文件未被修改）")?;

    let backup = supplement_extension(path, "bak");
    if path.exists() {
        fs::copy(path, &backup)
            .with_context(|| format!("备份配置文件到 {} 失败", backup.display()))?;
    }
    fs::write(path, updated).with_context(|| format!("写入配置文件 {} 失败", path.display()))?;
    Ok(())
}

fn parse(original: &str) -> Result<DocumentMut> {
    original
        .parse::<DocumentMut>()
        .context("解析 config.toml 失败（文件可能已损坏）")
}

fn ensure_table<'a>(doc: &'a mut DocumentMut, name: &str) -> Result<&'a mut Table> {
    if !doc.contains_key(name) {
        doc[name] = Item::Table(Table::new());
    }
    doc[name]
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("配置项 [{name}] 不是表"))
}

fn set_or_remove(table: &mut Table, key: &str, value: Option<&str>) {
    match normalized(value) {
        Some(value) => table[key] = toml_edit::value(value),
        None => {
            table.remove(key);
        }
    }
}

/// 去掉首尾空白；空字符串按"未设置"处理。
fn normalized(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// 输出文本：`toml_edit` 可能不补末尾换行，这里跟原文保持一致。
fn render(doc: &DocumentMut, original: &str) -> Result<String> {
    let mut text = doc.to_string();
    if original.ends_with('\n') && !text.ends_with('\n') {
        text.push('\n');
    }
    Ok(text)
}

/// `config.toml` + `.bak` → `config.toml.bak`
fn supplement_extension(path: &Path, extra: &str) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_default();
    name.push(".");
    name.push(extra);
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;

    const SAMPLE: &str = r#"# 顶部注释
[api]
# key 的注释
key = "sk-old"
workspace_host = "dashscope.aliyuncs.com"

[audio]
# 设备注释
input_device = "old-in"
output_device = "old-out"

[asr]
languages = ["zh", "ja"]
"#;

    fn temp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "kaols-config-edit-{tag}-{}-{}.toml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn set_api_key_keeps_comments_and_other_fields() {
        let updated = set_api_key(SAMPLE, Some("sk-new")).unwrap();

        assert!(updated.contains("# 顶部注释"));
        assert!(updated.contains("# key 的注释"));
        assert!(updated.contains(r#"key = "sk-new""#));
        assert!(updated.contains(r#"workspace_host = "dashscope.aliyuncs.com""#));
        assert!(updated.contains(r#"languages = ["zh", "ja"]"#));
        assert!(AppConfig::from_toml_str(&updated).is_ok());
    }

    #[test]
    fn set_api_key_none_clears_key() {
        let updated = set_api_key(SAMPLE, None).unwrap();
        let err = AppConfig::from_toml_str(&updated).unwrap_err();
        assert!(format!("{err:#}").contains("未配置 API Key"));
    }

    #[test]
    fn set_audio_devices_updates_and_clears() {
        let updated = set_audio_devices(SAMPLE, Some("麦克风 (USB)"), None).unwrap();

        assert!(updated.contains(r#"input_device = "麦克风 (USB)""#));
        assert!(!updated.contains("output_device"));
        // 清空设备 = 系统默认，配置其余部分仍合法
        let config = AppConfig::from_toml_str(&updated).unwrap();
        assert_eq!(config.audio.input_device.as_deref(), Some("麦克风 (USB)"));
        assert_eq!(config.audio.output_device, None);
    }

    #[test]
    fn save_backs_up_and_writes() {
        let path = temp_path("write");
        fs::write(&path, SAMPLE).unwrap();

        let updated = set_api_key(SAMPLE, Some("sk-new")).unwrap();
        save(&path, SAMPLE, &updated).unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), updated);
        let backup = supplement_extension(&path, "bak");
        assert_eq!(fs::read_to_string(&backup).unwrap(), SAMPLE);

        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(&backup);
    }

    #[test]
    fn save_rejects_invalid_text_without_touching_file() {
        let path = temp_path("reject");
        fs::write(&path, SAMPLE).unwrap();

        let broken = format!("{SAMPLE}\nunknown_field = 1\n");
        let err = save(&path, SAMPLE, &broken).unwrap_err();
        assert!(format!("{err:#}").contains("校验失败"));
        assert_eq!(fs::read_to_string(&path).unwrap(), SAMPLE);

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn save_is_noop_when_unchanged() {
        let path = temp_path("noop");
        fs::write(&path, SAMPLE).unwrap();

        save(&path, SAMPLE, SAMPLE).unwrap();
        assert!(!supplement_extension(&path, "bak").exists());

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn invalid_original_toml_is_reported() {
        let err = set_api_key("[api\nkey = 1", Some("sk")).unwrap_err();
        assert!(format!("{err:#}").contains("解析 config.toml 失败"));
    }

    #[test]
    fn set_custom_voices_replaces_table_and_keeps_other_fields() {
        let original = format!(
            "# 顶部注释\n{SAMPLE}\n[tts]\nvoice = \"custom:old\"\n\n[tts.custom_voices.old]\n[[tts.custom_voices.old.samples]]\nlanguage = \"zh\"\naudio = \"old.wav\"\n"
        );

        let voices = vec![
            (
                "alpha".to_string(),
                CustomVoiceConfig {
                    name: Some("甲".to_string()),
                    samples: vec![
                        crate::config::CustomVoiceSample {
                            language: "zh".to_string(),
                            audio: "voices/a-zh.wav".into(),
                        },
                        crate::config::CustomVoiceSample {
                            language: "ja".to_string(),
                            audio: "voices/a-ja.wav".into(),
                        },
                    ],
                },
            ),
            (
                "beta".to_string(),
                CustomVoiceConfig {
                    name: None,
                    samples: vec![crate::config::CustomVoiceSample {
                        language: "en".to_string(),
                        audio: "voices/b-en.wav".into(),
                    }],
                },
            ),
        ];

        let updated = set_custom_voices(&original, &voices).unwrap();
        // 旧生效音色指向已删除的 custom:old，真实保存流程会同时改写 [tts] voice
        let updated = set_active_voice(&updated, "custom:alpha").unwrap();

        // 注释与其它字段保留
        assert!(updated.contains("# 顶部注释"));
        assert!(updated.contains("workspace_host"));
        // 旧音色被整体替换、新音色结构可被正确解析
        assert!(!updated.contains("custom_voices.old"));
        let config = AppConfig::from_toml_str(&updated).unwrap();
        assert_eq!(config.tts.custom_voices.len(), 2);
        assert_eq!(config.tts.voice, "custom:alpha");
        let alpha = &config.tts.custom_voices["alpha"];
        assert_eq!(alpha.name.as_deref(), Some("甲"));
        assert_eq!(alpha.samples.len(), 2);
        assert_eq!(alpha.samples[0].language, "zh");
        assert_eq!(
            config.tts.custom_voices["beta"].samples[0]
                .audio
                .to_string_lossy(),
            "voices/b-en.wav"
        );
    }

    #[test]
    fn set_active_voice_updates_tts_voice() {
        let original = format!("{SAMPLE}\n[tts]\nvoice = \"Cherry\"\n");
        let updated = set_active_voice(&original, "Ethan").unwrap();

        let config = AppConfig::from_toml_str(&updated).unwrap();
        assert_eq!(config.tts.voice, "Ethan");
        assert!(updated.contains("# 顶部注释"));
    }
}
