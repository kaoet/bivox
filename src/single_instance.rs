//! 单实例（GUI_MIGRATION.md D17）。
//!
//! 以「exe 路径（小写、规范化）的哈希」作命名 Mutex 名：同一份便携目录只能开一个实例，
//! 不同目录的两份副本可以同时运行。第二个实例不会弹错误，而是尝试把已有窗口激活到前台后退出。

use std::hash::{Hash, Hasher};
use std::path::Path;

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    FindWindowW, SetForegroundWindow, ShowWindow, SW_RESTORE,
};

/// 第一个实例持有的守卫：进程存活期间必须一直持有（drop 时关闭 Mutex）。
pub struct SingleInstance {
    handle: HANDLE,
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        // SAFETY: handle 来自 CreateMutexW，且只在本结构 drop 时关闭一次
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

/// 尝试成为唯一实例。
///
/// - `Ok(Some(guard))`：当前是第一个实例（guard 要活到进程退出）；
/// - `Ok(None)`：已有实例在运行（已尽力激活其窗口，调用方直接退出即可）；
/// - `Err(_)`：创建 Mutex 失败（极罕见，调用方可选择继续启动）。
pub fn acquire(exe_path: &Path) -> std::io::Result<Option<SingleInstance>> {
    let name = mutex_name(exe_path);
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: 传入合法的空 attributes 与以 NUL 结尾的 UTF-16 字符串
    let handle = unsafe { CreateMutexW(std::ptr::null(), 1, wide.as_ptr()) };
    if handle.is_null() {
        return Err(std::io::Error::last_os_error());
    }

    // SAFETY: CreateMutexW 之后读 last error 是标准做法
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        // SAFETY: 已确认 handle 非空
        unsafe {
            CloseHandle(handle);
        }
        activate_existing_window();
        return Ok(None);
    }

    Ok(Some(SingleInstance { handle }))
}

/// 命名 Mutex 名：`Local\kaols-<hash>`。
///
/// 用 `Local\`（当前登录会话内唯一）即可：不同用户会话互不影响，且不需要管理员权限。
fn mutex_name(exe_path: &Path) -> String {
    let normalized = exe_path
        .canonicalize()
        .unwrap_or_else(|_| exe_path.to_path_buf())
        .to_string_lossy()
        .to_lowercase();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    normalized.hash(&mut hasher);
    format!("Local\\kaols-{:016x}", hasher.finish())
}

/// 找到标题匹配的已有窗口，恢复并置前台。窗口可能还在创建中，短暂重试。
fn activate_existing_window() {
    let title: Vec<u16> = crate::app::WINDOW_TITLE
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    for _ in 0..10 {
        // SAFETY: 传入以 NUL 结尾的窗口标题
        let hwnd = unsafe { FindWindowW(std::ptr::null(), title.as_ptr()) };
        if !hwnd.is_null() {
            // SAFETY: hwnd 来自 FindWindowW
            unsafe {
                ShowWindow(hwnd, SW_RESTORE);
                SetForegroundWindow(hwnd);
            }
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn mutex_name_is_stable_and_path_specific() {
        let a = PathBuf::from(r"C:\Apps\kaols\kaols.exe");
        let b = PathBuf::from(r"D:\Portable\kaols\kaols.exe");

        assert_eq!(mutex_name(&a), mutex_name(&a));
        assert_ne!(mutex_name(&a), mutex_name(&b));
        assert!(mutex_name(&a).starts_with("Local\\kaols-"));
    }
}
