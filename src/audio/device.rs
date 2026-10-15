use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait};
use cpal::Device;

pub struct AudioDeviceManager {
    host: cpal::Host,
}

impl AudioDeviceManager {
    pub fn new() -> Self {
        Self {
            host: cpal::default_host(),
        }
    }

    pub fn list_devices(&self) -> Result<()> {
        println!("\n=== 可用音频输入设备 (Microphones) ===");
        let default_in = self.host.default_input_device().and_then(|d| d.name().ok());
        for (i, dev) in self.host.input_devices()?.enumerate() {
            let name = dev.name().unwrap_or_else(|_| "Unknown".into());
            let is_default = default_in.as_deref() == Some(&name);
            println!(
                "  [{}] {}{}",
                i + 1,
                name,
                if is_default { " (默认)" } else { "" }
            );
        }

        println!("\n=== 可用音频输出设备 (Speakers / Virtual Cables) ===");
        let default_out = self.host.default_output_device().and_then(|d| d.name().ok());
        for (i, dev) in self.host.output_devices()?.enumerate() {
            let name = dev.name().unwrap_or_else(|_| "Unknown".into());
            let is_default = default_out.as_deref() == Some(&name);
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
                if let Ok(name) = dev.name() {
                    if name.to_lowercase().contains(&target.to_lowercase()) {
                        return Ok(dev);
                    }
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
                if let Ok(name) = dev.name() {
                    if name.to_lowercase().contains(&target.to_lowercase()) {
                        return Ok(dev);
                    }
                }
            }
            return Err(anyhow!("未找到匹配的输出音频设备: '{}'", target));
        }

        self.host
            .default_output_device()
            .ok_or_else(|| anyhow!("系统未检测到默认输出音频设备"))
    }
}
