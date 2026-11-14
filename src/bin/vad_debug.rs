//! `vad_debug`：本地 VAD 调参 / 排障工具（不随发布包分发）。
//!
//! 同一路音频同时导出两份 WAV，用来判断「句首丢字」是不是 VAD 切的：
//! - `raw.wav`：麦克风原始录音（= VAD 输入，采集块原样拼接）；
//! - `gated.wav`：经过 [`SpeechGate`] 之后**真实发给 ASR 的字节流**（含起音补发的
//!   pre-roll、句末 tail 与空闲 keepalive 静音，逐块拼接；线上 ASR 客户端就是把这些
//!   块原样 `Message::Binary` 上传）；
//! - `report.csv`：每个采集块内 32ms 帧的概率、门控状态与发送决策；
//! - `chunks.txt`：每个采集块的采样数，`--replay` 用它复现与原始录音相同的切块边界
//!   （采集块大小由声卡回调与重采样决定，不一定正好 100ms）。
//!
//! 复用生产的采集与门控代码（`AudioCapture` + `SpeechGate<SileroDetector>`），因此听感
//! 与线上一致；`--replay` 可对同一段 `raw.wav` 反复换参数重跑，免去反复说话。工具不连
//! ASR、不需要 API Key，也不改动生产 VAD 算法（`pre_roll_ms` / `keepalive_ms` 在
//! `GateConfig` 里但 `config.toml` 尚未暴露，这里先开放给调参用）。

use std::cell::RefCell;
use std::fs::File;
use std::io::{BufWriter, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use chrono::Local;
use clap::Parser;
use colored::Colorize;
use hound::{SampleFormat, WavReader, WavSpec, WavWriter};

use kaols::audio::capture::AudioCapture;
use kaols::audio::device::{AudioDeviceManager, device_name};
use kaols::audio::silero::SileroDetector;
use kaols::audio::vad::{FrameDetector, GateConfig, SpeechGate};

/// 采集与输出统一为 16kHz 单声道 PCM16（与服务端一致）。
const SAMPLE_RATE: u32 = 16_000;
/// 名义采集块：1600 采样 = 100ms；实际块长由声卡回调/重采样决定，见 `chunks.txt`。
const CHUNK_SAMPLES: usize = 1_600;
/// 16kHz 下每毫秒 16 个采样。
const SAMPLES_PER_MS: usize = 16;

#[derive(Debug, Parser)]
#[command(
    name = "vad_debug",
    about = "本地 VAD 调参工具：监听麦克风（或重放录音），导出 raw.wav、gated.wav 与逐块报告（不随发布包分发）"
)]
struct Args {
    /// 录音时长（秒）
    #[arg(long, default_value_t = 15)]
    seconds: u64,

    /// 开录前倒计时（秒），0 表示立即开始
    #[arg(long, default_value_t = 3)]
    countdown: u64,

    /// 输入设备完整名（默认系统默认输入设备）
    #[arg(long, value_name = "name")]
    device: Option<String>,

    /// 列出输入设备后退出
    #[arg(long)]
    list_devices: bool,

    /// 起音概率阈值（0–1，越大越不敏感）
    #[arg(long, default_value_t = 0.5)]
    threshold: f32,

    /// 低于负阈值（threshold - 0.15）持续多久判定语音结束（毫秒）
    #[arg(long, default_value_t = 500)]
    min_silence_ms: u32,

    /// 起音时补发的历史音频（毫秒，按整块向上取整）
    #[arg(long, default_value_t = 300)]
    pre_roll_ms: u32,

    /// 语音结束后补发的尾部静音（毫秒）
    #[arg(long, default_value_t = 1000)]
    tail_ms: u32,

    /// 完全空闲时补发保活静音的间隔（毫秒）
    #[arg(long, default_value_t = 10_000)]
    keepalive_ms: u32,

    /// 输出目录（默认 target\vad-debug\<时间戳>）
    #[arg(long, value_name = "dir")]
    out: Option<PathBuf>,

    /// 离线重放已有 raw.wav（16kHz / 单声道 / PCM16），不再监听麦克风
    #[arg(long, value_name = "raw.wav")]
    replay: Option<PathBuf>,

    /// 不写 report.csv 与 chunks.txt
    #[arg(long)]
    no_report: bool,
}

/// 调参用的门控参数副本（汇总输出用）。
#[derive(Debug, Clone, Copy)]
struct Params {
    threshold: f32,
    min_silence_ms: u32,
    pre_roll_ms: u32,
    tail_ms: u32,
    keepalive_ms: u32,
}

/// 音频来源：现场采集（含 VAD 之前的丢块计数）或离线重放。
#[derive(Debug, Clone, Copy)]
enum Source {
    Live(u64),
    Replay,
}

/// 每个采集块的处理结果（同时用于控制台、CSV 与汇总）。
#[derive(Clone)]
struct ChunkRow {
    chunk: usize,
    t_ms: u64,
    probs: Vec<f32>,
    state: &'static str,
    emitted: usize,
    sent_ms: u64,
    chunk_samples: usize,
    note: String,
}

impl ChunkRow {
    fn probs_joined(&self, sep: &str) -> String {
        join_probs(&self.probs, sep)
    }
}

fn join_probs(probs: &[f32], sep: &str) -> String {
    probs
        .iter()
        .map(|prob| format!("{prob:.3}"))
        .collect::<Vec<_>>()
        .join(sep)
}

/// 一段语音（起音块 → 语音结束块）。
struct Segment {
    /// 起音发生的采集块起点（原始音频时间轴）。
    onset_ms: u64,
    /// 本段实际开始发送的时间轴（起音块起点 - 补发的历史音频）。
    sent_start_ms: u64,
    /// 补发的历史音频块数与毫秒数。
    preroll_blocks: usize,
    preroll_ms: u64,
    end_ms: Option<u64>,
}

/// 包一层 Silero 检测器，顺手记录每帧概率供报告使用；门控逻辑仍是生产实现。
struct ProbeDetector {
    inner: SileroDetector,
    probs: Rc<RefCell<Vec<f32>>>,
}

impl FrameDetector for ProbeDetector {
    fn frame_samples(&self) -> usize {
        self.inner.frame_samples()
    }

    fn process_frame(&mut self, frame: &[i16]) -> Result<f32> {
        let prob = self.inner.process_frame(frame)?;
        self.probs.borrow_mut().push(prob);
        Ok(prob)
    }

    fn reset(&mut self) {
        self.inner.reset();
    }
}

/// 采集/重放 chunk → 门控 → 两份音频缓冲 + 逐块报告。
struct Processor {
    gate: SpeechGate<ProbeDetector>,
    probs: Rc<RefCell<Vec<f32>>>,
    params: Params,
    write_report: bool,
    raw: Vec<i16>,
    gated: Vec<i16>,
    rows: Vec<ChunkRow>,
    segments: Vec<Segment>,
    chunk_sizes: Vec<usize>,
    // 以下按采样数累计，避免按块取整累积误差
    sent_speech_samples: usize,
    sent_preroll_samples: usize,
    sent_tail_samples: usize,
    sent_keepalive_samples: usize,
}

impl Processor {
    fn new(config: GateConfig, write_report: bool) -> Result<Self> {
        let params = Params {
            threshold: config.threshold,
            min_silence_ms: config.min_silence_ms,
            pre_roll_ms: config.pre_roll_ms,
            tail_ms: config.tail_ms,
            keepalive_ms: config.keepalive_ms,
        };
        let probs = Rc::new(RefCell::new(Vec::new()));
        let detector = ProbeDetector {
            inner: SileroDetector::new().context("加载内嵌 Silero 模型失败")?,
            probs: probs.clone(),
        };
        Ok(Self {
            gate: SpeechGate::with_detector(config, detector),
            probs,
            params,
            write_report,
            raw: Vec::new(),
            gated: Vec::new(),
            rows: Vec::new(),
            segments: Vec::new(),
            chunk_sizes: Vec::new(),
            sent_speech_samples: 0,
            sent_preroll_samples: 0,
            sent_tail_samples: 0,
            sent_keepalive_samples: 0,
        })
    }

    /// 处理一个采集块，返回该块的可视化行。
    fn push_chunk(&mut self, chunk: Vec<u8>) -> Result<ChunkRow> {
        if chunk.is_empty() || !chunk.len().is_multiple_of(2) {
            bail!("音频块长度非法：{} 字节", chunk.len());
        }
        let chunk_samples = chunk.len() / 2;
        let t_ms = self.raw.len() as u64 / SAMPLES_PER_MS as u64;
        self.raw.extend(bytes_to_samples(&chunk));

        let active_before = self.gate.is_active();
        let tail_before = self.gate.tail_ms_left();
        self.probs.borrow_mut().clear();
        let emitted = self.gate.process_chunk(chunk);
        if self.gate.is_degraded() {
            bail!("Silero 推理失败，门控已降级为全量上传；本次数据不能用于调参");
        }
        let probs: Vec<f32> = self.probs.borrow_mut().drain(..).collect();
        let active_after = self.gate.is_active();

        let onset = !active_before && active_after;
        let ended = active_before && !active_after;
        let preroll_blocks = if onset {
            emitted.len().saturating_sub(1)
        } else {
            0
        };
        let preroll_samples: usize = emitted
            .iter()
            .take(preroll_blocks)
            .map(|chunk| chunk.len() / 2)
            .sum();
        let state = if active_after {
            "speech"
        } else if active_before || tail_before > 0 {
            "tail"
        } else if emitted.is_empty() {
            "idle"
        } else {
            "keepalive"
        };

        for chunk in &emitted {
            self.gated.extend(bytes_to_samples(chunk));
        }
        let emitted_samples: usize = emitted.iter().map(|chunk| chunk.len() / 2).sum();
        if active_after {
            self.sent_speech_samples += chunk_samples;
            self.sent_preroll_samples += preroll_samples;
        } else if state == "tail" {
            self.sent_tail_samples += emitted_samples;
        } else if state == "keepalive" {
            self.sent_keepalive_samples += emitted_samples;
        }

        let preroll_ms = samples_to_ms(preroll_samples);
        if onset {
            let sent_start_ms = t_ms.saturating_sub(preroll_ms);
            self.segments.push(Segment {
                onset_ms: t_ms,
                sent_start_ms,
                preroll_blocks,
                preroll_ms,
                end_ms: None,
            });
        }
        if ended {
            if let Some(segment) = self.segments.last_mut() {
                segment.end_ms = Some(t_ms);
            }
        }

        let note = if onset {
            format!("起音：补发 {preroll_blocks} 块历史音频（{preroll_ms}ms）")
        } else if ended {
            "语音结束，开始补 tail 静音".to_string()
        } else if state == "keepalive" {
            "保活静音".to_string()
        } else {
            String::new()
        };

        self.chunk_sizes.push(chunk_samples);
        let row = ChunkRow {
            chunk: self.rows.len(),
            t_ms,
            probs,
            state,
            emitted: emitted.len(),
            sent_ms: samples_to_ms(self.gated.len()),
            chunk_samples,
            note,
        };
        self.rows.push(row.clone());
        Ok(row)
    }

    /// 落盘两份 WAV（+ 可选报告）并打印汇总。
    fn finish(&self, out_dir: &Path, source: Source) -> Result<()> {
        let raw_path = out_dir.join("raw.wav");
        let gated_path = out_dir.join("gated.wav");
        write_wav(&raw_path, &self.raw)?;
        write_wav(&gated_path, &self.gated)?;
        if self.write_report {
            write_report(&out_dir.join("report.csv"), &self.rows)?;
            write_chunk_sizes(&out_dir.join("chunks.txt"), &self.chunk_sizes)?;
        }
        self.print_summary(out_dir, source, &raw_path, &gated_path)
    }

    fn print_summary(
        &self,
        out_dir: &Path,
        source: Source,
        raw_path: &Path,
        gated_path: &Path,
    ) -> Result<()> {
        let raw_ms = samples_to_ms(self.raw.len());
        let gated_ms = samples_to_ms(self.gated.len());
        let sent_speech_ms = samples_to_ms(self.sent_speech_samples);
        let sent_preroll_ms = samples_to_ms(self.sent_preroll_samples);
        let sent_tail_ms = samples_to_ms(self.sent_tail_samples);
        let sent_keepalive_ms = samples_to_ms(self.sent_keepalive_samples);
        let discarded_ms = raw_ms.saturating_sub(sent_speech_ms + sent_preroll_ms);

        println!();
        println!("{}", "━━━ 结果 ━━━".bold());
        println!(
            "参数：threshold={:.2}  min_silence={}ms  pre_roll={}ms  tail={}ms  keepalive={}ms",
            self.params.threshold,
            self.params.min_silence_ms,
            self.params.pre_roll_ms,
            self.params.tail_ms,
            self.params.keepalive_ms
        );
        println!(
            "原始音频：{:.2}s（{} 块，{}）",
            raw_ms as f64 / 1000.0,
            self.rows.len(),
            describe_chunk_sizes(&self.chunk_sizes)
        );
        println!(
            "发送给 ASR：{:.2}s = 语音 {:.2}s + pre-roll {:.2}s + tail {:.2}s + keepalive {:.2}s",
            gated_ms as f64 / 1000.0,
            sent_speech_ms as f64 / 1000.0,
            sent_preroll_ms as f64 / 1000.0,
            sent_tail_ms as f64 / 1000.0,
            sent_keepalive_ms as f64 / 1000.0
        );
        println!("被 VAD 丢弃：{:.2}s", discarded_ms as f64 / 1000.0);

        match source {
            Source::Live(0) => println!("VAD 之前丢块：0 块"),
            Source::Live(dropped) => println!(
                "{}",
                format!(
                    "⚠️ VAD 之前丢块：{dropped} 块（AudioCapture 通道满，约 {}ms 音频未进 VAD）",
                    dropped * 100
                )
                .yellow()
            ),
            Source::Replay => println!("VAD 之前丢块：0（重放模式不采集）"),
        }

        if self.segments.is_empty() {
            println!("语音段：未检测到（可试着降低 --threshold，或检查设备/增益）");
        } else {
            println!("语音段：{} 段", self.segments.len());
            for (index, segment) in self.segments.iter().enumerate() {
                let end = match segment.end_ms {
                    Some(ms) => format!("{:.2}s", ms as f64 / 1000.0),
                    None => "录音结束时仍在说话".to_string(),
                };
                println!(
                    "  #{} 起音 {:.2}s（从 {:.2}s 开始发送，补发 {} 块 / {}ms）→ 结束 {}",
                    index + 1,
                    segment.onset_ms as f64 / 1000.0,
                    segment.sent_start_ms as f64 / 1000.0,
                    segment.preroll_blocks,
                    segment.preroll_ms,
                    end
                );
            }
        }

        if let Some(segment) = self
            .segments
            .iter()
            .find(|segment| segment.preroll_blocks > 0)
        {
            println!(
                "{}",
                format!(
                    "pre-roll 实际补发 {} 块 / {}ms（配置 {}ms，按整块向上取整）",
                    segment.preroll_blocks, segment.preroll_ms, self.params.pre_roll_ms
                )
                .dimmed()
            );
        }

        println!("输出目录：{}", out_dir.display());
        println!(
            "  {:<10} {:>10}  {:.2}s",
            "raw.wav",
            format_file_size(raw_path),
            raw_ms as f64 / 1000.0
        );
        println!(
            "  {:<10} {:>10}  {:.2}s",
            "gated.wav",
            format_file_size(gated_path),
            gated_ms as f64 / 1000.0
        );
        if self.write_report {
            println!(
                "  {:<10} {:>10}  {} 行",
                "report.csv",
                format_file_size(&out_dir.join("report.csv")),
                self.rows.len()
            );
            println!(
                "  {:<10} {:>10}  replay 用切块边界",
                "chunks.txt",
                format_file_size(&out_dir.join("chunks.txt"))
            );
        }
        println!();
        println!(
            "{}",
            "提示：先对比两份 WAV 的开头——gated.wav 里首字还在，说明丢字不是 VAD 切的；\
             可用 --replay 对同一段 raw.wav 换参数重跑。"
                .dimmed()
        );
        Ok(())
    }
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
    validate(&args)?;

    if args.list_devices {
        return list_devices();
    }

    let out_dir = prepare_out_dir(&args)?;
    println!(
        "{} {}",
        "输出目录：".dimmed(),
        out_dir.display().to_string().cyan()
    );
    print_params(&args);

    let config = GateConfig {
        threshold: args.threshold,
        min_silence_ms: args.min_silence_ms,
        pre_roll_ms: args.pre_roll_ms,
        tail_ms: args.tail_ms,
        keepalive_ms: args.keepalive_ms,
        debug: false,
        active_flag: None,
    };
    let mut processor = Processor::new(config, !args.no_report)?;

    let source = match &args.replay {
        Some(path) => {
            replay(path, &mut processor, args.threshold)?;
            Source::Replay
        }
        None => Source::Live(record(&args, &mut processor).await?),
    };
    processor.finish(&out_dir, source)
}

/// 现场采集：倒计时 → 开始采集 → 门控 → 到时长结束。
async fn record(args: &Args, processor: &mut Processor) -> Result<u64> {
    let manager = AudioDeviceManager::new();
    let device = manager
        .get_input_device(args.device.as_deref())
        .context("打开输入设备失败（可用 --list-devices 查看设备名）")?;
    println!("{} {}", "输入设备：".dimmed(), device_name(&device).cyan());

    if args.countdown > 0 {
        for remaining in (1..=args.countdown).rev() {
            print!("{} ", format!("{remaining}…").yellow());
            std::io::stdout().flush().context("刷新终端输出失败")?;
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        println!();
    }

    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(100);
    let capture = AudioCapture::start(&device, tx).context("启动麦克风采集失败")?;
    println!(
        "{}",
        format!("开始录音 {} 秒，请说话…", args.seconds).green()
    );

    let live = LivePrinter::new(args.threshold);
    let deadline = Instant::now() + Duration::from_secs(args.seconds);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        // 超时只用于检查录音时长，不丢数据（正常情况下每 100ms 左右就有一块）
        match tokio::time::timeout(remaining.min(Duration::from_millis(500)), rx.recv()).await {
            Ok(Some(chunk)) => {
                let row = processor.push_chunk(chunk)?;
                live.print(&row);
            }
            // 采集通道已关闭（设备被拔出等）
            Ok(None) => break,
            Err(_) => {}
        }
    }
    live.finish();

    let dropped = capture.dropped_chunks();
    drop(capture);
    println!("{}", "录音结束。".green());
    Ok(dropped)
}

/// 离线重放：按原始录音的切块边界（`chunks.txt`）把 raw.wav 喂给同一套门控。
fn replay(path: &Path, processor: &mut Processor, threshold: f32) -> Result<()> {
    let samples = read_wav(path)?;
    println!(
        "{} {}（{:.2}s）",
        "重放：".dimmed(),
        path.display().to_string().cyan(),
        samples.len() as f64 / SAMPLE_RATE as f64
    );

    let chunk_sizes = read_chunk_sizes(path);
    if chunk_sizes.is_none() {
        println!(
            "{}",
            "（同目录没有 chunks.txt：按固定 100ms 切块，pre-roll 的整块取整会与原始录音略有差别）"
                .dimmed()
        );
    }

    let live = LivePrinter::new(threshold);
    let mut offset = 0;
    let mut index = 0;
    while offset < samples.len() {
        let size = chunk_sizes
            .as_ref()
            .and_then(|sizes| sizes.get(index).copied())
            .unwrap_or(CHUNK_SAMPLES)
            .clamp(1, samples.len() - offset);
        let end = offset + size;
        let mut bytes = Vec::with_capacity(size * 2);
        for &sample in &samples[offset..end] {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        let row = processor.push_chunk(bytes)?;
        live.print(&row);
        offset = end;
        index += 1;
    }
    live.finish();
    Ok(())
}

/// 控制台实时进度行：终端里原地刷新，重定向输出时逐行打印。
struct LivePrinter {
    tty: bool,
    threshold: f32,
}

impl LivePrinter {
    fn new(threshold: f32) -> Self {
        Self {
            tty: std::io::stdout().is_terminal(),
            threshold,
        }
    }

    fn print(&self, row: &ChunkRow) {
        let probs = row.probs_joined("/");
        // 先补齐宽度再上色，避免 ANSI 转义序列打乱对齐
        let padded = format!("{probs:<23}");
        let peak = row.probs.iter().copied().fold(0.0_f32, f32::max);
        let colored = if row.probs.is_empty() {
            padded.dimmed().to_string()
        } else if peak >= self.threshold {
            padded.green().to_string()
        } else if peak >= self.threshold - 0.15 {
            padded.yellow().to_string()
        } else {
            padded.dimmed().to_string()
        };
        let line = format!(
            "[t={:6.2}s] p={colored} {:9} sent={}",
            row.t_ms as f64 / 1000.0,
            row.state,
            row.emitted
        );
        if self.tty {
            print!("\r{line}");
            // best-effort: 进度行刷新失败不影响录音
            let _ = std::io::stdout().flush();
        } else {
            println!("{line}");
        }
    }

    fn finish(&self) {
        if self.tty {
            println!();
        }
    }
}

fn validate(args: &Args) -> Result<()> {
    if !args.threshold.is_finite() || args.threshold <= 0.0 || args.threshold >= 1.0 {
        bail!("--threshold 必须是 (0, 1) 内的有效数字");
    }
    if args.min_silence_ms > 5_000 {
        bail!("--min-silence-ms 不能超过 5000");
    }
    if args.tail_ms > 5_000 {
        bail!("--tail-ms 不能超过 5000");
    }
    if args.pre_roll_ms > 2_000 {
        bail!("--pre-roll-ms 不能超过 2000");
    }
    if args.keepalive_ms < 100 {
        bail!("--keepalive-ms 至少 100（每块 100ms 左右）");
    }
    if args.seconds == 0 || args.seconds > 300 {
        bail!("--seconds 必须在 1–300 之间");
    }
    if args.countdown > 60 {
        bail!("--countdown 不能超过 60");
    }
    if args.list_devices && args.replay.is_some() {
        bail!("--list-devices 与 --replay 不能同时使用");
    }
    Ok(())
}

fn print_params(args: &Args) {
    println!(
        "参数：threshold={:.2}  min_silence={}ms  pre_roll={}ms  tail={}ms  keepalive={}ms",
        args.threshold, args.min_silence_ms, args.pre_roll_ms, args.tail_ms, args.keepalive_ms
    );
}

fn prepare_out_dir(args: &Args) -> Result<PathBuf> {
    let dir = match &args.out {
        Some(dir) => dir.clone(),
        None => {
            let stamp = Local::now().format("%Y%m%d-%H%M%S").to_string();
            let name = if args.replay.is_some() {
                format!("{stamp}-replay")
            } else {
                stamp
            };
            PathBuf::from("target").join("vad-debug").join(name)
        }
    };
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("创建输出目录失败：{}", dir.display()))?;
    // 不用 canonicalize：Windows 下会带上 `\\?\` 前缀，打印出来不好看
    if dir.is_absolute() {
        Ok(dir)
    } else {
        Ok(std::env::current_dir()
            .context("读取当前工作目录失败")?
            .join(dir))
    }
}

fn list_devices() -> Result<()> {
    let manager = AudioDeviceManager::new();
    let names = manager.input_device_names().context("枚举输入设备失败")?;
    let default = manager.default_input_name();
    println!("{}", "输入设备（* 为系统默认）：".bold());
    if names.is_empty() {
        println!("  （没有可用的输入设备）");
        return Ok(());
    }
    for name in &names {
        if default.as_deref() == Some(name.as_str()) {
            println!("  {} {name}", "*".green());
        } else {
            println!("    {name}");
        }
    }
    Ok(())
}

fn describe_chunk_sizes(chunk_sizes: &[usize]) -> String {
    match (chunk_sizes.iter().min(), chunk_sizes.iter().max()) {
        (Some(min), Some(max)) if min == max => {
            format!(
                "每块 {min} 采样 = {:.2}ms",
                *min as f64 / SAMPLES_PER_MS as f64
            )
        }
        (Some(min), Some(max)) => format!("每块 {min}–{max} 采样"),
        _ => "无采集块".to_string(),
    }
}

/// 采样数 → 毫秒（整数截断，只用于展示）。
fn samples_to_ms(samples: usize) -> u64 {
    (samples as u64 * 1000) / SAMPLE_RATE as u64
}

/// PCM16 小端字节 → 采样。
fn bytes_to_samples(bytes: &[u8]) -> impl Iterator<Item = i16> + '_ {
    bytes
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
}

fn write_wav(path: &Path, samples: &[i16]) -> Result<()> {
    let spec = WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    let mut writer = WavWriter::create(path, spec)
        .with_context(|| format!("创建 WAV 失败：{}", path.display()))?;
    for &sample in samples {
        writer
            .write_sample(sample)
            .with_context(|| format!("写入 WAV 失败：{}", path.display()))?;
    }
    writer
        .finalize()
        .with_context(|| format!("收尾 WAV 失败：{}", path.display()))?;
    Ok(())
}

fn read_wav(path: &Path) -> Result<Vec<i16>> {
    let mut reader =
        WavReader::open(path).with_context(|| format!("打开 WAV 失败：{}", path.display()))?;
    let spec = reader.spec();
    if spec.sample_format != SampleFormat::Int
        || spec.bits_per_sample != 16
        || spec.channels != 1
        || spec.sample_rate != SAMPLE_RATE
    {
        bail!(
            "--replay 需要 16kHz / 单声道 / PCM16 WAV（当前 {}Hz / {} 声道 / {}bit / {:?}）",
            spec.sample_rate,
            spec.channels,
            spec.bits_per_sample,
            spec.sample_format
        );
    }
    reader
        .samples::<i16>()
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("读取 WAV 采样失败：{}", path.display()))
}

fn write_report(path: &Path, rows: &[ChunkRow]) -> Result<()> {
    let file = File::create(path).with_context(|| format!("创建报告失败：{}", path.display()))?;
    let mut writer = BufWriter::new(file);
    // BOM：让 Excel 按 UTF-8 打开
    writer.write_all("\u{feff}".as_bytes())?;
    writeln!(
        writer,
        "块,起始ms,块采样数,帧概率,状态,发送块数,已发送ms,备注"
    )?;
    for row in rows {
        writeln!(
            writer,
            "{},{},{},{},{},{},{},\"{}\"",
            row.chunk,
            row.t_ms,
            row.chunk_samples,
            row.probs_joined(";"),
            row.state,
            row.emitted,
            row.sent_ms,
            row.note
        )?;
    }
    writer.flush()?;
    Ok(())
}

/// replay 用：记录每个采集块的采样数，保证重放时切块边界与原始录音一致。
fn write_chunk_sizes(path: &Path, chunk_sizes: &[usize]) -> Result<()> {
    let mut text = String::new();
    for size in chunk_sizes {
        text.push_str(&format!("{size}\n"));
    }
    std::fs::write(path, text).with_context(|| format!("写入 chunks.txt 失败：{}", path.display()))
}

fn read_chunk_sizes(raw_path: &Path) -> Option<Vec<usize>> {
    let path = raw_path.parent()?.join("chunks.txt");
    let text = std::fs::read_to_string(path).ok()?;
    let sizes: Vec<usize> = text
        .lines()
        .filter_map(|line| line.trim().parse().ok())
        .collect();
    if sizes.is_empty() { None } else { Some(sizes) }
}

fn format_file_size(path: &Path) -> String {
    match std::fs::metadata(path) {
        Ok(metadata) => format!("{:.1} KB", metadata.len() as f64 / 1024.0),
        Err(err) => format!("（读不到大小：{err}）"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_and_samples_roundtrip() {
        let samples: Vec<i16> = vec![0, 1, -1, i16::MAX, i16::MIN];
        let mut bytes = Vec::new();
        for sample in &samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        assert_eq!(bytes_to_samples(&bytes).collect::<Vec<_>>(), samples);
    }

    #[test]
    fn wav_roundtrip_preserves_pcm() -> Result<()> {
        let dir = test_dir("wav");
        let path = dir.join("roundtrip.wav");
        let samples: Vec<i16> = (0..CHUNK_SAMPLES).map(|i| (i as i16) - 800).collect();
        write_wav(&path, &samples)?;
        assert_eq!(read_wav(&path)?, samples);
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn chunk_sizes_roundtrip() -> Result<()> {
        let dir = test_dir("chunks");
        let sizes = [1634usize, 1634, 1634, 1584];
        write_chunk_sizes(&dir.join("chunks.txt"), &sizes)?;
        let raw = dir.join("raw.wav");
        assert_eq!(read_chunk_sizes(&raw), Some(sizes.to_vec()));
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    fn test_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("kaols-vad-debug-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("创建临时目录失败");
        dir
    }
}
