//! 配置读写（决策 D3，见 docs/decisions.md）。
//!
//! 写回流程：读磁盘原文 → 调用方基于 `AppConfig` 做类型化修改 →
//! [`AppConfig::to_toml_string`] 只序列化非默认值 → 校验磁盘原配置和新文本 →
//! 备份 `config.toml.bak` → 写盘。
//!
//! `config.toml` 由 GUI 维护，不再保留注释、字段顺序或用户手写格式。

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::AppConfig;

/// 读取配置原文。
pub fn read(path: &Path) -> Result<String> {
    fs::read_to_string(path).with_context(|| format!("读取配置文件 {} 失败", path.display()))
}

/// 读盘 → 解析 → 类型化修改 → 写回，一次完成，返回**写好的**配置。
///
/// 调用方拿到返回值直接更新内存状态即可，不必再读一次盘（这是单字段保存时读 3 次
/// 文件的来源）。写入仍是「校验 → 备份 → 原子替换」。
pub fn update(path: &Path, edit: impl FnOnce(&mut AppConfig) -> Result<()>) -> Result<AppConfig> {
    let original = read(path)?;
    let mut config = AppConfig::from_toml_str_in(&original, path)?;
    edit(&mut config)?;
    save(path, &original, &config)?;
    Ok(config)
}

/// 写回配置。
///
/// `original` 用来判断是否真的有改动，并确认磁盘配置仍然合法；磁盘原配置非法时
/// **拒绝写回**，避免 GUI 覆盖掉它不认识的内容。有改动时先写临时文件再 rename
/// （原子替换），避免写到一半崩溃留下截断的 `config.toml`。
pub fn save(path: &Path, original: &str, config: &AppConfig) -> Result<()> {
    let updated = config.to_toml_string()?;
    if updated == original {
        return Ok(());
    }

    AppConfig::from_toml_str(original)
        .with_context(|| "磁盘上的配置已无效，已拒绝写回（请先修正或删除配置文件）")?;
    AppConfig::from_toml_str(&updated)
        .with_context(|| "新配置校验失败，已放弃写回（配置文件未被修改）")?;

    let backup = supplement_extension(path, "bak");
    if path.exists() {
        fs::copy(path, &backup)
            .with_context(|| format!("备份配置文件到 {} 失败", backup.display()))?;
    }
    let temp = supplement_extension(path, "tmp");
    fs::write(&temp, updated)
        .with_context(|| format!("写入临时配置文件 {} 失败", temp.display()))?;
    // `fs::rename` 在 Windows 上会替换已存在的目标文件
    fs::rename(&temp, path).with_context(|| format!("替换配置文件 {} 失败", path.display()))?;
    Ok(())
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

    fn temp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "bivox-config-edit-{tag}-{}-{}.toml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn config_with_key(key: &str) -> AppConfig {
        AppConfig::from_toml_str(&format!("[api]\nkey = \"{key}\"\n")).unwrap()
    }

    #[test]
    fn save_backs_up_and_writes_minimal_toml() {
        let path = temp_path("write");
        let original = "[api]\nkey = \"sk-old\"\n";
        fs::write(&path, original).unwrap();

        let config = config_with_key("sk-new");
        save(&path, original, &config).unwrap();

        let written = fs::read_to_string(&path).unwrap();
        assert!(written.contains("[api]"));
        assert!(written.contains("key = \"sk-new\""));
        assert_eq!(
            fs::read_to_string(supplement_extension(&path, "bak")).unwrap(),
            original
        );

        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(supplement_extension(&path, "bak"));
    }

    #[test]
    fn save_is_noop_when_unchanged() {
        let path = temp_path("noop");
        let config = config_with_key("sk-test");
        let original = config.to_toml_string().unwrap();
        fs::write(&path, &original).unwrap();

        save(&path, &original, &config).unwrap();
        assert!(!supplement_extension(&path, "bak").exists());

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn save_refuses_to_overwrite_invalid_disk_config() {
        let path = temp_path("invalid-original");
        let invalid = "[api]\nkey = \"\"\n";
        fs::write(&path, invalid).unwrap();

        let config = config_with_key("sk-new");
        let err = save(&path, invalid, &config).unwrap_err();
        assert!(
            format!("{err:#}").contains("磁盘上的配置已无效"),
            "应拒绝覆盖非法原配置: {err:#}"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), invalid);

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn save_removes_cleared_api_key_and_empty_api_table() {
        let path = temp_path("clear-key");
        let original = "[api]\nkey = \"sk-old\"\n";
        fs::write(&path, original).unwrap();

        let config = AppConfig::from_toml_str("").unwrap();
        save(&path, original, &config).unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "");

        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(supplement_extension(&path, "bak"));
    }

    #[test]
    fn save_omits_explicit_defaults() {
        let path = temp_path("defaults");
        let original = "";
        fs::write(&path, original).unwrap();

        let config = AppConfig::from_toml_str(
            "[api]\nkey = \"sk-test\"\n[self]\nmic = true\n[translate]\nshow_source = true\n[vad]\nenabled = true\n",
        )
        .unwrap();
        save(&path, original, &config).unwrap();

        let written = fs::read_to_string(&path).unwrap();
        assert!(written.contains("[api]"));
        assert!(written.contains("key = \"sk-test\""));
        assert!(!written.contains("[self]"), "{written}");
        assert!(!written.contains("[translate]"), "{written}");
        assert!(!written.contains("[vad]"), "{written}");

        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(supplement_extension(&path, "bak"));
    }

    #[test]
    fn save_leaves_no_temp_file_behind() {
        let path = temp_path("atomic");
        let original = "[api]\nkey = \"sk-old\"\n";
        fs::write(&path, original).unwrap();

        update(&path, |config| {
            config.asr.model = "asr-custom".to_string();
            Ok(())
        })
        .unwrap();

        assert!(
            !supplement_extension(&path, "tmp").exists(),
            "原子写不应留下临时文件"
        );
        assert!(fs::read_to_string(&path).unwrap().contains("asr-custom"));
        // 备份仍是写之前的原文
        assert_eq!(
            fs::read_to_string(supplement_extension(&path, "bak")).unwrap(),
            original
        );

        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(supplement_extension(&path, "bak"));
    }

    #[test]
    fn update_reads_modifies_and_returns_the_written_config() {
        let path = temp_path("update");
        fs::write(&path, "[api]\nkey = \"sk-old\"\n").unwrap();

        let returned = update(&path, |config| {
            config.api.workspace_host = "ws.example.com".to_string();
            Ok(())
        })
        .unwrap();

        // 返回值就是写盘的那份，可直接用于更新内存状态（不必再读一次盘）
        assert_eq!(returned.api.workspace_host, "ws.example.com");
        assert_eq!(returned.api.key.as_deref(), Some("sk-old"));
        assert_eq!(
            returned.to_toml_string().unwrap(),
            fs::read_to_string(&path).unwrap()
        );

        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(supplement_extension(&path, "bak"));
    }

    #[test]
    fn update_propagates_edit_error_without_touching_the_file() {
        let path = temp_path("update-error");
        let original = "[api]\nkey = \"sk-old\"\n";
        fs::write(&path, original).unwrap();

        let err = update(&path, |config| {
            config.api.key = Some("sk-new".to_string());
            anyhow::bail!("故意失败")
        })
        .unwrap_err();

        assert!(format!("{err:#}").contains("故意失败"));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            original,
            "失败时不应写盘"
        );
        assert!(!supplement_extension(&path, "bak").exists());

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn save_rejects_invalid_original_toml() {
        let path = temp_path("broken");
        let config = config_with_key("sk-test");
        let err = save(&path, "[api\nkey = 1", &config).unwrap_err();
        assert!(!format!("{err:#}").is_empty());

        let _ = fs::remove_file(&path);
    }
}
