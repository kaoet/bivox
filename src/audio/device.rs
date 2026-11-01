//! 音频设备枚举与查找。
//!
//! 设备名规则（GUI_MIGRATION.md D12）：GUI 只保存**完整设备名**，查找时按字符串
//! **精确相等**匹配；找不到就报错让用户在设置页重选，不做模糊匹配、也不静默回退到默认设备。

use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait};
use cpal::Device;

pub struct AudioDeviceManager {
    host: cpal::Host,
}

/// 设备可读名称。
///
/// cpal 0.18 起 `DeviceTrait::name()` 被 `description()` 取代，名称在
/// [`cpal::DeviceDescription::name`] 里；读不到时返回占位名，避免 UI 空白。
pub fn device_name(device: &Device) -> String {
    device
        .description()
        .map(|description| description.name().to_string())
        .unwrap_or_else(|_| "未知设备".to_string())
}

impl Default for AudioDeviceManager {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioDeviceManager {
    pub fn new() -> Self {
        Self {
            host: cpal::default_host(),
        }
    }

    /// 全部输入设备的完整名称（顺序由 cpal 决定）。
    pub fn input_device_names(&self) -> Result<Vec<String>> {
        let devices = self
            .host
            .input_devices()
            .map_err(|err| anyhow!("枚举输入设备失败：{err}"))?;
        Ok(devices.map(|device| device_name(&device)).collect())
    }

    /// 全部输出设备的完整名称。
    pub fn output_device_names(&self) -> Result<Vec<String>> {
        let devices = self
            .host
            .output_devices()
            .map_err(|err| anyhow!("枚举输出设备失败：{err}"))?;
        Ok(devices.map(|device| device_name(&device)).collect())
    }

    /// 系统默认输入设备名（无默认设备时为 `None`）。
    pub fn default_input_name(&self) -> Option<String> {
        self.host
            .default_input_device()
            .map(|device| device_name(&device))
    }

    /// 系统默认输出设备名（无默认设备时为 `None`）。
    pub fn default_output_name(&self) -> Option<String> {
        self.host
            .default_output_device()
            .map(|device| device_name(&device))
    }

    /// 按完整设备名精确查找输入设备；`None` = 系统默认输入设备。
    pub fn get_input_device(&self, exact_name: Option<&str>) -> Result<Device> {
        if let Some(target) = exact_name {
            for device in self
                .host
                .input_devices()
                .map_err(|err| anyhow!("枚举输入设备失败：{err}"))?
            {
                if device_name(&device) == target {
                    return Ok(device);
                }
            }
            return Err(anyhow!(
                "未找到输入设备 {target:?}；请在设置页重新选择（设备可能已改名或被拔掉）"
            ));
        }

        self.host
            .default_input_device()
            .ok_or_else(|| anyhow!("系统未检测到默认输入音频设备"))
    }

    /// 按完整设备名精确查找输出设备；`None` = 系统默认输出设备。
    pub fn get_output_device(&self, exact_name: Option<&str>) -> Result<Device> {
        if let Some(target) = exact_name {
            for device in self
                .host
                .output_devices()
                .map_err(|err| anyhow!("枚举输出设备失败：{err}"))?
            {
                if device_name(&device) == target {
                    return Ok(device);
                }
            }
            return Err(anyhow!(
                "未找到输出设备 {target:?}；请在设置页重新选择（设备可能已改名或被拔掉）"
            ));
        }

        self.host
            .default_output_device()
            .ok_or_else(|| anyhow!("系统未检测到默认输出音频设备"))
    }
}
