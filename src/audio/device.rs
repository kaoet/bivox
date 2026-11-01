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

impl AudioDeviceManager {
    pub fn new() -> Self {
        Self {
            host: cpal::default_host(),
        }
    }

    /// 打印可用设备（M1 会给 GUI 提供返回 `Vec<String>` 的枚举 API）。
    pub fn list_devices(&self) -> Result<()> {
        println!("\n=== 可用音频输入设备 (Microphones) ===");
        let default_in = self
            .host
            .default_input_device()
            .map(|device| device_name(&device));
        for (i, dev) in self.host.input_devices()?.enumerate() {
            let name = device_name(&dev);
            let is_default = default_in.as_deref() == Some(name.as_str());
            println!(
                "  [{}] {}{}",
                i + 1,
                name,
                if is_default { " (默认)" } else { "" }
            );
        }

        println!("\n=== 可用音频输出设备 (Speakers / Virtual Cables) ===");
        let default_out = self
            .host
            .default_output_device()
            .map(|device| device_name(&device));
        for (i, dev) in self.host.output_devices()?.enumerate() {
            let name = device_name(&dev);
            let is_default = default_out.as_deref() == Some(name.as_str());
            println!(
                "  [{}] {}{}",
                i + 1,
                name,
                if is_default { " (默认)" } else { "" }
            );
        }
        println!();
        Ok(())
    }

    pub fn get_input_device(&self, name_filter: Option<&str>) -> Result<Device> {
        if let Some(target) = name_filter {
            for dev in self.host.input_devices()? {
                if device_name(&dev)
                    .to_lowercase()
                    .contains(&target.to_lowercase())
                {
                    return Ok(dev);
                }
            }
            return Err(anyhow!("未找到匹配的输入音频设备: '{}'", target));
        }

        self.host
            .default_input_device()
            .ok_or_else(|| anyhow!("系统未检测到默认输入音频设备"))
    }

    pub fn get_output_device(&self, name_filter: Option<&str>) -> Result<Device> {
        if let Some(target) = name_filter {
            for dev in self.host.output_devices()? {
                if device_name(&dev)
                    .to_lowercase()
                    .contains(&target.to_lowercase())
                {
                    return Ok(dev);
                }
            }
            return Err(anyhow!("未找到匹配的输出音频设备: '{}'", target));
        }

        self.host
            .default_output_device()
            .ok_or_else(|| anyhow!("系统未检测到默认输出音频设备"))
    }
}
