//! 用 `toml_edit` 对 `config.toml` 做**定点修改**（决策 D3，见 docs/decisions.md）。
//!
//! 目标：只动 GUI 负责的那几个字段（`[api] key`、`[audio]` 设备、`[asr]`
//! model/languages/hotwords、`[tts] voice` / `dialect` / `custom_voices`、`[translate]`
//! 的可写字段），其余字段、注释、顺序原样保留；
//! 写盘前先校验磁盘原配置仍然合法（非法则拒绝写回），再校验新文本；
//! 写盘前把旧文件备份成 `config.toml.bak`。

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use toml_edit::{Array, ArrayOfTables, DocumentMut, Item, Table, Value};

use crate::config::{CustomVoiceConfig, TranslateGranularity, VrchatDisplay};

/// 读取配置原文。
pub fn read(path: &Path) -> Result<String> {
    fs::read_to_string(path).with_context(|| format!("读取配置文件 {} 失败", path.display()))
}

/// 设置 `[api] key`；`None` 或空字符串表示删除该键（= 未配置）。
///
/// 删除后如果 `[api]` 表已经没有任何字段，把整个表也删掉，保持空配置干净。
pub fn set_api_key(original: &str, key: Option<&str>) -> Result<String> {
    let mut doc = parse(original)?;

    match normalized(key) {
        Some(key) => {
            let api = ensure_table(&mut doc, "api")?;
            api["key"] = toml_edit::value(key);
        }
        None => {
            let remove_api = if let Some(api) = doc.get_mut("api").and_then(Item::as_table_mut) {
                api.remove("key");
                api.is_empty()
            } else {
                false
            };
            if remove_api {
                doc.remove("api");
            }
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

/// 设置 `[translate]` 中由翻译页负责的字段（D3/D27）。
///
/// `enabled` / `target` / `granularity` / `show_source` 可写；`model` 只读，
/// 保持配置文件里的原值。
pub fn set_translate(
    original: &str,
    enabled: bool,
    target: &str,
    granularity: TranslateGranularity,
    show_source: bool,
) -> Result<String> {
    let mut doc = parse(original)?;
    let translate = ensure_table(&mut doc, "translate")?;

    translate["enabled"] = toml_edit::value(enabled);
    translate["target"] = toml_edit::value(target.trim());
    translate["granularity"] = toml_edit::value(granularity.label());
    translate["show_source"] = toml_edit::value(show_source);

    render(&doc, original)
}

/// 设置 `[vrchat.osc]` 中由 VRChat 页负责的字段（D3/D33）。
///
/// 只改 `enabled` / `port` / `display`；父段 `[vrchat]` 与其它子表（未来的 avatar 等）
/// 原样保留。写回后由整份配置校验端口范围。
pub fn set_vrchat_osc(
    original: &str,
    enabled: bool,
    port: u16,
    display: VrchatDisplay,
) -> Result<String> {
    let mut doc = parse(original)?;
    let vrchat = ensure_table(&mut doc, "vrchat")?;
    let osc = ensure_subtable(vrchat, "osc")?;

    osc["enabled"] = toml_edit::value(enabled);
    osc["port"] = toml_edit::value(i64::from(port));
    osc["display"] = toml_edit::value(display.label());

    render(&doc, original)
}

/// 设置 `[asr]` 中由「语音识别」页负责的字段：`model` / `languages` / `hotwords`（D32）。
///
/// `languages` 顺序即配置顺序（首个是默认语言）；`hotwords` 为空时删除该键，
/// 保持 TOML 简洁。写回后交给整份配置校验（语言代码、热词限制等）。
pub fn set_asr(
    original: &str,
    model: &str,
    languages: &[String],
    hotwords: &[String],
) -> Result<String> {
    let mut doc = parse(original)?;
    let asr = ensure_table(&mut doc, "asr")?;

    asr["model"] = toml_edit::value(model.trim());
    asr["languages"] = string_array(languages.iter().map(String::as_str));
    if hotwords.is_empty() {
        asr.remove("hotwords");
    } else {
        asr["hotwords"] = string_array(hotwords.iter().map(String::as_str));
    }

    render(&doc, original)
}

/// 字符串数组 → `toml_edit::Item`（保留调用方给的顺序）。
fn string_array<'a>(values: impl IntoIterator<Item = &'a str>) -> Item {
    let mut array = Array::new();
    for value in values {
        array.push(value.trim());
    }
    Item::Value(Value::Array(array))
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
            // 默认语速不写进配置，保持 TOML 简洁（缺省即 1.0）。
            if sample.rate != crate::config::DEFAULT_SAMPLE_RATE {
                sample_table["rate"] = rate_value(sample.rate);
            }
            samples.push(sample_table);
        }
        voice_table["samples"] = Item::ArrayOfTables(samples);

        table[key.as_str()] = Item::Table(voice_table);
    }

    tts["custom_voices"] = Item::Table(table);
    render(&doc, original)
}

/// 语速写回值：用 f32 的最短十进制表示再转 f64 写出，避免 `f32 → f64` 的二进制尾差
/// （否则 1.3 会被写成 `1.2999999523162842`）。
fn rate_value(rate: f32) -> Item {
    match format!("{rate}").parse::<f64>() {
        Ok(value) => toml_edit::value(value),
        // best-effort: f32 的 Display 输出一定可被 f64 解析，这里只是不 panic 的兜底。
        Err(_) => toml_edit::value(f64::from(rate)),
    }
}

/// 设置生效音色 `[tts] voice`（内置音色名或 `custom:<键名>`）。
pub fn set_active_voice(original: &str, voice: &str) -> Result<String> {
    let mut doc = parse(original)?;
    let tts = ensure_table(&mut doc, "tts")?;
    tts["voice"] = toml_edit::value(voice.trim());
    render(&doc, original)
}

/// 设置中文方言 `[tts] dialect`（官方方言名，如 `东北话` / `普通话`）。
pub fn set_tts_dialect(original: &str, dialect: &str) -> Result<String> {
    let mut doc = parse(original)?;
    let tts = ensure_table(&mut doc, "tts")?;
    tts["dialect"] = toml_edit::value(dialect.trim());
    render(&doc, original)
}

/// 写回配置：先确认磁盘原配置仍然合法，再校验新文本，最后备份 `.bak` 并写盘。
///
/// `original` 用来判断是否真的有改动；没有改动直接返回。磁盘原配置非法时
/// **拒绝写回**，避免 GUI 覆盖掉它不认识的配置内容。
pub fn save(path: &Path, original: &str, updated: &str) -> Result<()> {
    if updated == original {
        return Ok(());
    }

    crate::config::AppConfig::from_toml_str(original)
        .with_context(|| "磁盘上的配置已无效，已拒绝写回（请先修正或删除配置文件）")?;
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

/// 确保 `parent[name]` 是表（用于 `[vrchat.osc]` 这样的嵌套段）。
fn ensure_subtable<'a>(parent: &'a mut Table, name: &str) -> Result<&'a mut Table> {
    if !parent.contains_key(name) {
        parent[name] = Item::Table(Table::new());
    }
    parent[name]
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
    // 全部字段删光时保持真正的空文件，不额外补一个只有换行的“非空”文件。
    if !text.is_empty() && original.ends_with('\n') && !text.ends_with('\n') {
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
        let config = AppConfig::from_toml_str(&updated).unwrap();
        assert_eq!(config.api.key, None, "清除 Key 后应回到未配置状态");
    }

    #[test]
    fn set_api_key_none_removes_empty_api_table() {
        let updated = set_api_key("[api]\nkey = \"sk-test\"\n", None).unwrap();
        assert_eq!(updated, "", "只剩 key 时清除后应得到空配置");
        assert!(
            AppConfig::from_toml_str(&updated)
                .unwrap()
                .api
                .key
                .is_none()
        );
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
    fn save_refuses_to_overwrite_invalid_disk_config() {
        let path = temp_path("invalid-original");
        let invalid = "[api]\nkey = \"\"\n";
        fs::write(&path, invalid).unwrap();

        let updated = "[api]\nkey = \"sk-new\"\n";
        let err = save(&path, invalid, updated).unwrap_err();
        assert!(
            format!("{err:#}").contains("磁盘上的配置已无效"),
            "应拒绝覆盖非法原配置: {err:#}"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), invalid);

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
                            rate: 1.3,
                        },
                        crate::config::CustomVoiceSample {
                            language: "ja".to_string(),
                            audio: "voices/a-ja.wav".into(),
                            rate: crate::config::DEFAULT_SAMPLE_RATE,
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
                        rate: crate::config::DEFAULT_SAMPLE_RATE,
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
        // 非默认语速写回为最简十进制，默认语速不写
        assert!(updated.contains("rate = 1.3"), "{updated}");
        assert_eq!(
            updated.matches("rate = ").count(),
            1,
            "默认语速 1.0 不应写进配置: {updated}"
        );
        // 旧音色被整体替换、新音色结构可被正确解析
        assert!(!updated.contains("custom_voices.old"));
        let config = AppConfig::from_toml_str(&updated).unwrap();
        assert_eq!(config.tts.custom_voices.len(), 2);
        assert_eq!(config.tts.voice, "custom:alpha");
        let alpha = &config.tts.custom_voices["alpha"];
        assert_eq!(alpha.name.as_deref(), Some("甲"));
        assert_eq!(alpha.samples.len(), 2);
        assert_eq!(alpha.samples[0].language, "zh");
        assert!((alpha.samples[0].rate - 1.3).abs() < 1e-6);
        assert_eq!(
            alpha.samples[1].rate,
            crate::config::DEFAULT_SAMPLE_RATE,
            "默认语速读取时应回到 1.0"
        );
        assert_eq!(
            config.tts.custom_voices["beta"].samples[0]
                .audio
                .to_string_lossy(),
            "voices/b-en.wav"
        );
    }

    #[test]
    fn set_asr_writes_model_languages_and_hotwords_preserving_other_fields() {
        let updated = set_asr(
            SAMPLE,
            " qwen-audio-3.1-asr-flash-streaming ",
            &["ja".to_string(), "zh".to_string()],
            &["原神".to_string(), "Kaoet".to_string()],
        )
        .unwrap();

        assert!(updated.contains("# 顶部注释"));
        assert!(updated.contains("workspace_host"));
        assert!(
            updated.contains(r#"[asr]"#),
            "原配置已有 [asr] 时不应重复创建: {updated}"
        );
        assert_eq!(updated.matches("[asr]").count(), 1, "{updated}");

        let config = AppConfig::from_toml_str(&updated).unwrap();
        assert_eq!(config.asr.model, "qwen-audio-3.1-asr-flash-streaming");
        assert_eq!(
            config.asr.languages,
            vec!["ja".to_string(), "zh".to_string()],
            "语言顺序应原样写回"
        );
        assert_eq!(
            config.asr.hotwords,
            vec!["原神".to_string(), "Kaoet".to_string()]
        );
    }

    #[test]
    fn set_asr_removes_hotwords_key_when_empty() {
        // SAMPLE 以 [asr] 段结尾，追加的 hotwords 行属于 [asr]
        let original = format!("{SAMPLE}hotwords = [\"旧词\"]\n");
        assert!(AppConfig::from_toml_str(&original).is_ok());

        let updated = set_asr(&original, "asr-model", &["zh".to_string()], &[]).unwrap();
        assert!(
            !updated.contains("hotwords"),
            "热词为空时应删除 hotwords 键: {updated}"
        );
        let config = AppConfig::from_toml_str(&updated).unwrap();
        assert!(config.asr.hotwords.is_empty());
    }

    #[test]
    fn set_asr_creates_missing_asr_table() {
        let original = "[api]\nkey = \"sk-test\"\n";
        let updated = set_asr(original, "asr-model", &["zh".to_string()], &[]).unwrap();
        assert!(updated.contains("[asr]"), "{updated}");
        let config = AppConfig::from_toml_str(&updated).unwrap();
        assert_eq!(config.asr.model, "asr-model");
        assert_eq!(config.asr.languages, vec!["zh".to_string()]);
    }

    #[test]
    fn set_active_voice_updates_tts_voice() {
        let original = format!("{SAMPLE}\n[tts]\nvoice = \"Cherry\"\n");
        let updated = set_active_voice(&original, "Ethan").unwrap();

        let config = AppConfig::from_toml_str(&updated).unwrap();
        assert_eq!(config.tts.voice, "Ethan");
        assert!(updated.contains("# 顶部注释"));
    }

    #[test]
    fn set_tts_dialect_creates_table_and_keeps_other_fields() {
        // 原配置没有 [tts] 表，写回时新建；注释与其它段保留
        let updated = set_tts_dialect(SAMPLE, " 东北话 ").unwrap();

        assert!(updated.contains("[tts]"), "{updated}");
        assert!(updated.contains(r#"dialect = "东北话""#), "{updated}");
        assert!(updated.contains("# 顶部注释"));
        assert!(updated.contains(r#"languages = ["zh", "ja"]"#));
        assert_eq!(
            AppConfig::from_toml_str(&updated).unwrap().tts.dialect,
            "东北话"
        );

        // 改回默认值也照写（GUI 只在草稿与配置不同时才调用本函数）
        let updated = set_tts_dialect(&updated, "普通话").unwrap();
        assert_eq!(
            AppConfig::from_toml_str(&updated).unwrap().tts.dialect,
            "普通话"
        );
    }

    #[test]
    fn save_rejects_unsupported_dialect_without_touching_file() {
        let path = temp_path("dialect");
        fs::write(&path, SAMPLE).unwrap();

        // 官方写法是「广东话」，不接受「粤语」别名
        let updated = set_tts_dialect(SAMPLE, "粤语").unwrap();
        let err = save(&path, SAMPLE, &updated).unwrap_err();
        assert!(format!("{err:#}").contains("[tts] dialect"), "{err:#}");
        assert_eq!(fs::read_to_string(&path).unwrap(), SAMPLE);

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn set_translate_creates_table_and_writes_gui_fields() {
        let updated = set_translate(
            SAMPLE,
            true,
            " ja ",
            crate::config::TranslateGranularity::Sentence,
            false,
        )
        .unwrap();

        // 原配置没有 [translate] 表，写回时新建；其它段与注释保留
        assert!(updated.contains("[translate]"), "{updated}");
        assert!(updated.contains("enabled = true"));
        assert!(updated.contains(r#"target = "ja""#));
        assert!(updated.contains(r#"granularity = "sentence""#));
        assert!(updated.contains("show_source = false"));
        assert!(updated.contains("# 顶部注释"));

        let config = AppConfig::from_toml_str(&updated).unwrap();
        assert!(config.translate.enabled);
        assert_eq!(config.translate.target, "ja");
        assert_eq!(
            config.translate.granularity,
            crate::config::TranslateGranularity::Sentence
        );
        assert!(!config.translate.show_source);
        // model 不在当前 GUI 写回范围里，保持配置文件里的原值
        assert_eq!(
            config.translate.model,
            crate::config::DEFAULT_TRANSLATE_MODEL
        );
    }

    #[test]
    fn set_translate_keeps_manual_model_value() {
        let original = format!("{SAMPLE}\n[translate]\nmodel = \"qwen-mt-lite\"\n");
        let updated = set_translate(
            &original,
            false,
            "en",
            crate::config::TranslateGranularity::Clause,
            true,
        )
        .unwrap();

        assert!(updated.contains(r#"model = "qwen-mt-lite""#), "{updated}");
        assert!(updated.contains("enabled = false"));
        assert!(updated.contains(r#"target = "en""#));
        assert!(updated.contains(r#"granularity = "clause""#));
        assert!(updated.contains("show_source = true"));
    }

    #[test]
    fn set_vrchat_osc_creates_nested_table_and_keeps_other_fields() {
        let original =
            format!("{SAMPLE}\n# 未来 [vrchat.avatar] 等子表应原样保留（当前配置模型尚未定义）\n");
        let updated = set_vrchat_osc(&original, true, 9001, VrchatDisplay::Translation).unwrap();

        assert!(updated.contains("# 顶部注释"));
        assert!(updated.contains(r#"[vrchat.osc]"#), "{updated}");
        assert!(updated.contains(r#"enabled = true"#));
        assert!(updated.contains(r#"port = 9001"#));
        assert!(updated.contains(r#"display = "translation""#));
        assert!(
            updated.contains("未来 [vrchat.avatar] 等子表应原样保留"),
            "{updated}"
        );

        let config = AppConfig::from_toml_str(&updated).unwrap();
        assert!(config.vrchat.osc.enabled);
        assert_eq!(config.vrchat.osc.port, 9001);
        assert_eq!(config.vrchat.osc.display, VrchatDisplay::Translation);

        // 再次写回只改字段，不重复创建 [vrchat.osc]
        let updated2 = set_vrchat_osc(&updated, false, 9000, VrchatDisplay::Bilingual).unwrap();
        assert_eq!(updated2.matches("[vrchat.osc]").count(), 1, "{updated2}");
        let config = AppConfig::from_toml_str(&updated2).unwrap();
        assert!(!config.vrchat.osc.enabled);
        assert_eq!(config.vrchat.osc.port, 9000);
        assert_eq!(config.vrchat.osc.display, VrchatDisplay::Bilingual);
    }
}
