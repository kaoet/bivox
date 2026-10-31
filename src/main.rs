mod audio;
mod client;
mod config;
mod pipeline;

use anyhow::Result;
use clap::Parser;
use config::{AppConfig, CliArgs};
use pipeline::orchestrator::PipelineOrchestrator;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = CliArgs::parse();

    // 设备列表是一次性诊断动作：不读取配置文件，也不需要 API Key。
    if cli.list_devices {
        audio::device::AudioDeviceManager::new().list_devices()?;
        return Ok(());
    }

    let config = AppConfig::load(&cli.config)?;
    let orchestrator = PipelineOrchestrator::new(config);
    orchestrator.run().await
}
