//! 便携路径解析（决策 D4/D18，见 docs/decisions.md）。
//!
//! 规则很简单：**所有可写文件都在 exe 同目录**，不读 `%APPDATA%`、不读当前工作目录、
//! 不读任何环境变量。这样整个程序目录可以整体拷贝/备份，即插即用。

use std::fs;
use std::path::PathBuf;

/// 首次启动写入的默认配置模板。
///
/// 内嵌自仓库根 `config.example.toml`（编译期 `include_str!`），因此发布包不需要
/// 再附带该文件；模板里的 `[api] key` 保持注释，首次启动后是合法的“未配置”
/// 状态，开始实时会话时再由 GUI 引导填写。
pub const DEFAULT_CONFIG_TEMPLATE: &str = include_str!("../config.example.toml");

/// 程序用到的全部路径。
#[derive(Debug, Clone)]
pub struct AppPaths {
    /// exe 所在目录（便携根目录）。
    pub exe_dir: PathBuf,
    /// `<exe_dir>\config.toml`
    pub config_file: PathBuf,
    /// `<exe_dir>\logs`
    pub log_dir: PathBuf,
    /// `<exe_dir>\crash.log`
    pub crash_log: PathBuf,
}

impl AppPaths {
    /// 以当前进程的 exe 路径解析全部路径。
    pub fn resolve() -> std::io::Result<Self> {
        let exe = std::env::current_exe()?;
        let exe_dir = match exe.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
            // 极罕见：拿不到父目录时退回当前目录，至少能跑起来。
            _ => std::env::current_dir()?,
        };
        Ok(Self::from_exe_dir(exe_dir))
    }

    /// 从指定目录推导路径（便于测试与将来复用）。
    pub fn from_exe_dir(exe_dir: PathBuf) -> Self {
        Self {
            config_file: exe_dir.join("config.toml"),
            log_dir: exe_dir.join("logs"),
            crash_log: exe_dir.join("crash.log"),
            exe_dir,
        }
    }

    /// 配置不存在时写入内置模板；返回是否新建。
    pub fn ensure_default_config(&self) -> std::io::Result<bool> {
        if self.config_file.exists() {
            return Ok(false);
        }
        fs::write(&self.config_file, DEFAULT_CONFIG_TEMPLATE)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derived_paths_are_next_to_exe() {
        let dir = PathBuf::from(r"C:\Apps\kaols");
        let paths = AppPaths::from_exe_dir(dir.clone());

        assert_eq!(paths.exe_dir, dir);
        assert_eq!(
            paths.config_file,
            PathBuf::from(r"C:\Apps\kaols\config.toml")
        );
        assert_eq!(paths.log_dir, PathBuf::from(r"C:\Apps\kaols\logs"));
        assert_eq!(paths.crash_log, PathBuf::from(r"C:\Apps\kaols\crash.log"));
    }

    /// 模板里的 key 必须保持注释：避免把示例 key 当真实 key 写进用户配置；
    /// 缺省 Key 现在是合法的“未配置”状态，不影响首次启动。
    #[test]
    fn default_template_has_commented_api_key() {
        assert!(DEFAULT_CONFIG_TEMPLATE.contains("[api]"));
        assert!(DEFAULT_CONFIG_TEMPLATE.contains("# key ="));
        assert!(!DEFAULT_CONFIG_TEMPLATE.contains("\nkey = "));
    }
}
