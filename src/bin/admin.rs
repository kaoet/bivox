//! `admin`：百炼自定义音色调试工具（M4，见 GUI_MIGRATION.md D19）。
//!
//! 只做两件事：列出账号下的自定义音色、删除音色（单条或全部）。
//!
//! - **读 `config.toml`**：默认当前工作目录的 `./config.toml`，可用 `-c/--config` 指定；
//!   只取 `[api] key` 与 `[api] workspace_host`（其余字段沿用同一套校验）。
//! - **不读写 `voice_cache.json`、不提供 enroll、不做任何 prefix 过滤**。
//! - 该工具不随发布包分发，只用于本地调试。

use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use colored::Colorize;

use kaols::config::AppConfig;
use kaols::voice::admin::{self, DeleteOutcome};
use kaols::voice::api::{BailianVoiceApi, RemoteVoice};

#[derive(Debug, Parser)]
#[command(
    name = "admin",
    about = "百炼自定义音色调试工具：列出 / 删除账号下的自定义音色（不随主程序分发）"
)]
struct Args {
    /// config.toml 路径（默认当前工作目录下的 config.toml）
    #[arg(
        short = 'c',
        long,
        value_name = "path",
        default_value = "config.toml",
        global = true
    )]
    config: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// 列出账号下的全部自定义音色
    List,

    /// 删除指定 voice_id（可重复传入）
    Delete {
        /// 百炼原生 voice_id，可重复传入
        #[arg(long = "voice-id", value_name = "id", required = true)]
        voice_ids: Vec<String>,

        /// 跳过交互确认
        #[arg(short = 'y', long)]
        yes: bool,
    },

    /// 删除账号下全部自定义音色
    DeleteAll {
        /// 跳过交互确认
        #[arg(short = 'y', long)]
        yes: bool,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{} {err:#}", "❌".red());
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<()> {
    let args = Args::parse();
    let config = AppConfig::load(&args.config)
        .with_context(|| format!("读取配置失败：{}", args.config.display()))?;
    let api = kaols::voice::api_from_config(&config)?;

    println!(
        "{} {}",
        "配置：".dimmed(),
        args.config.display().to_string().cyan()
    );
    println!(
        "{} {}",
        "接入域名：".dimmed(),
        config.api.workspace_host.cyan()
    );

    match args.command {
        Command::List => list(&api).await,
        Command::Delete { voice_ids, yes } => delete(&api, &voice_ids, yes).await,
        Command::DeleteAll { yes } => delete_all(&api, yes).await,
    }
}

async fn list(api: &BailianVoiceApi) -> Result<()> {
    let voices = admin::list_remote_voices(api).await?;
    if voices.is_empty() {
        println!("📭 账号下没有自定义音色。");
        return Ok(());
    }

    print_voices(&voices);
    println!("\n共 {} 个自定义音色。", voices.len());
    Ok(())
}

async fn delete(api: &BailianVoiceApi, voice_ids: &[String], yes: bool) -> Result<()> {
    let ids: Vec<&str> = voice_ids
        .iter()
        .map(|id| id.trim())
        .filter(|id| !id.is_empty())
        .collect();
    if ids.is_empty() {
        bail!("--voice-id 不能为空");
    }

    if !yes {
        let prompt = format!("即将删除 {} 个音色：{}", ids.len(), ids.join(", "));
        if !confirm(&prompt)? {
            println!("{}", "已取消。".yellow());
            return Ok(());
        }
    }

    let mut failed = Vec::new();
    for voice_id in ids {
        match admin::delete_remote_voice(api, voice_id).await {
            Ok(DeleteOutcome::Deleted) => println!("{} 已删除 {voice_id}", "✅".green()),
            Ok(DeleteOutcome::AlreadyMissing) => {
                println!("{} {voice_id} 在百炼中已不存在", "⚠️".yellow())
            }
            Err(err) => {
                println!("{} 删除 {voice_id} 失败：{err:#}", "❌".red());
                failed.push(voice_id.to_string());
            }
        }
    }

    if !failed.is_empty() {
        bail!("{} 个音色删除失败，详见上方输出", failed.len());
    }
    Ok(())
}

async fn delete_all(api: &BailianVoiceApi, yes: bool) -> Result<()> {
    let voices = admin::list_remote_voices(api).await?;
    if voices.is_empty() {
        println!("📭 账号下没有自定义音色，无需删除。");
        return Ok(());
    }

    print_voices(&voices);
    if !yes {
        let prompt = format!("即将删除全部 {} 个自定义音色，删除后不可恢复", voices.len());
        if !confirm(&prompt)? {
            println!("{}", "已取消。".yellow());
            return Ok(());
        }
    }

    let report = admin::delete_all_remote_voices(api, &voices).await;

    for voice_id in &report.deleted {
        println!("{} 已删除 {voice_id}", "✅".green());
    }
    for voice_id in &report.already_missing {
        println!("{} {voice_id} 在百炼中已不存在，已跳过", "⚠️".yellow());
    }
    for (voice_id, detail) in &report.failed {
        println!("{} 删除 {voice_id} 失败：{detail}", "❌".red());
    }

    println!(
        "\n🎉 批量删除完成：成功 {} 个，已不存在 {} 个，失败 {} 个。",
        report.deleted.len(),
        report.already_missing.len(),
        report.failed.len()
    );

    if !report.failed.is_empty() {
        bail!("{} 个音色删除失败，详见上方输出", report.failed.len());
    }
    Ok(())
}

/// 打印音色表格（只展示云端字段，不涉及本地缓存）。
fn print_voices(voices: &[RemoteVoice]) {
    let id_width = voices
        .iter()
        .map(|voice| voice.voice_id.chars().count())
        .max()
        .unwrap_or(8)
        .max("voice_id".len());

    println!(
        "\n{}  {}  {}  {}",
        format!("{:<id_width$}", "voice_id").bold(),
        format!("{:<32}", "target_model").bold(),
        format!("{:<10}", "status").bold(),
        "就绪".bold(),
    );
    println!("{}", "-".repeat(id_width + 60).dimmed());

    for voice in voices {
        println!(
            "{}  {:<32}  {:<10}  {}",
            format!("{:<id_width$}", voice.voice_id).cyan(),
            voice.target_model,
            voice.status,
            if voice.is_ready() {
                "是".green().to_string()
            } else {
                "否".yellow().to_string()
            },
        );
    }
}

/// 交互式确认：需要输入 `YES` 才算确认；非交互终端（管道/脚本）直接报错。
fn confirm(prompt: &str) -> Result<bool> {
    if !std::io::stdin().is_terminal() {
        bail!("当前不是交互式终端，无法确认；请加 -y/--yes 跳过确认");
    }

    println!("\n{}", format!("⚠️ {prompt}").red());
    print!("请输入 YES 确认: ");
    std::io::stdout().flush().context("刷新终端输出失败")?;

    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .context("读取确认输入失败")?;
    Ok(line.trim() == "YES")
}
