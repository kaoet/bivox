mod audio;
mod client;
mod config;
mod language;
mod pipeline;
mod voice;

use anyhow::Result;
use clap::Parser;
use config::{AppConfig, CliArgs, Command, VoicesAction};
use pipeline::orchestrator::PipelineOrchestrator;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = CliArgs::parse();

    match cli.command.clone().unwrap_or(Command::Run) {
        // 默认行为：读取配置，启动实时语音识别 -> 合成流水线
        Command::Run => {
            let config = AppConfig::load(&cli.config)?;
            PipelineOrchestrator::new(config).run().await
        }

        // 设备列表是一次性诊断动作：不读取配置文件，也不需要 API Key。
        Command::Devices => audio::device::AudioDeviceManager::new().list_devices(),

        // 自定义音色管理：需要配置里的 API Key 与业务空间域名
        Command::Voices { action } => {
            let config = AppConfig::load(&cli.config)?;
            match action {
                VoicesAction::List => voice::cli::list(&config).await,
                VoicesAction::Delete {
                    voice_id,
                    voice,
                    language,
                    yes,
                } => {
                    voice::cli::delete(
                        &config,
                        voice_id.as_deref(),
                        voice.as_deref(),
                        language.as_deref(),
                        yes,
                    )
                    .await
                }
                VoicesAction::DeleteAll { yes } => voice::cli::delete_all(&config, yes).await,
                VoicesAction::Enroll { name, language } => {
                    voice::cli::enroll(&config, name.as_deref(), language.as_deref()).await
                }
            }
        }
    }
}
