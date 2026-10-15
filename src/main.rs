mod audio;
mod client;
mod config;
mod pipeline;

use anyhow::Result;
use config::AppConfig;
use pipeline::orchestrator::PipelineOrchestrator;

#[tokio::main]
async fn main() -> Result<()> {
    let config = AppConfig::parse_with_env();
    let orchestrator = PipelineOrchestrator::new(config);
    orchestrator.run().await
}
