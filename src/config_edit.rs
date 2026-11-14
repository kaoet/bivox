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

/// 写回配置。
///
/// `original` 用来判断是否真的有改动，并确认磁盘配置仍然合法；磁盘原配置非法时
/// **拒绝写回**，避免 GUI 覆盖掉它不认识的内容。
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
    fs::write(path, updated).with_context(|| format!("写入配置文件 {} 失败", path.display()))?;
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
            "kaols-config-edit-{tag}-{}-{}.toml",
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
            "[api]\nkey = \"sk-test\"\n[translate]\nenabled = false\n[vad]\nenabled = true\n",
        )
        .unwrap();
        save(&path, original, &config).unwrap();

        let written = fs::read_to_string(&path).unwrap();
        assert!(written.contains("[api]"));
        assert!(written.contains("key = \"sk-test\""));
        assert!(!written.contains("[translate]"), "{written}");
        assert!(!written.contains("[vad]"), "{written}");

        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(supplement_extension(&path, "bak"));
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
