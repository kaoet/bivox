//! 进程环回采集调试工具：抓指定进程（默认 `VRChat.exe`）渲染出来的音频，逐秒打印电平，
//! 可选导出 WAV，用于确认「对方语音」这条采集链路是否真的抓到了声音。
//!
//! 用法见 `CONTRIBUTING.md` 第 11 节。这里只用采集层，不联网、不送 ASR。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use bivox::audio::process_capture::{ProcessCapture, find_pid, process_alive};
use clap::Parser;

/// 每秒应有的音频块数（每块 100ms）。
const CHUNKS_PER_SECOND: u32 = 10;
/// 电平条的范围：-60 dBFS 为空，0 dBFS 为满。
const BAR_MIN_DBFS: f64 = -60.0;
/// 峰值低于这个电平的块算「静音块」（采集层补的零与极低底噪都算）。
const SILENT_DBFS: f64 = -80.0;

#[derive(Parser)]
#[command(
    name = "loopback_debug",
    about = "进程环回采集调试：抓 VRChat 等进程的音频并打印电平"
)]
struct Args {
    /// 目标进程名（exe 文件名，不区分大小写）
    #[arg(long, default_value = "VRChat.exe")]
    process: String,

    /// 直接指定 PID（优先于 --process）
    #[arg(long)]
    pid: Option<u32>,

    /// 采集时长（秒）
    #[arg(long, default_value_t = 10)]
    seconds: u64,

    /// 同时导出 WAV（16 kHz / 单声道 / PCM16）
    #[arg(long)]
    wav: Option<PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let pid = match args.pid {
        Some(pid) => pid,
        None => find_pid(&args.process)?
            .ok_or_else(|| anyhow!("未找到进程 {}；请先启动它，或用 --pid 指定", args.process))?,
    };
    match args.pid {
        Some(pid) => println!("目标进程：PID {pid}（--pid 指定）"),
        None => println!("目标进程：{}（PID {pid}）", args.process),
    }
    println!(
        "采集 {} 秒，格式 16 kHz / 单声道 / PCM16；安静期由采集层补静音。",
        args.seconds
    );

    let mut capture = ProcessCapture::start(pid)?;
    let ready = capture
        .ready
        .take()
        .ok_or_else(|| anyhow!("采集句柄缺少激活结果通道"))?;
    match ready.blocking_recv() {
        Ok(Ok(())) => println!("进程环回已激活。"),
        Ok(Err(err)) => bail!("进程环回激活失败：{err:#}"),
        Err(_) => bail!("采集线程在激活前退出"),
    }

    let mut wav = match args.wav.as_deref() {
        Some(path) => Some(WavSink::create(path)?),
        None => None,
    };

    let mut chunks = capture
        .take_chunks()
        .ok_or_else(|| anyhow!("采集句柄缺少音频块接收端"))?;

    let deadline = Instant::now() + Duration::from_secs(args.seconds);
    let mut second = 0u32;
    let mut level = Level::default();
    let mut chunks_in_second = 0u32;
    let mut total_chunks = 0u64;
    let mut silent_chunks = 0u64;
    let mut loudest = f64::NEG_INFINITY;

    while Instant::now() < deadline {
        let Some(chunk) = chunks.blocking_recv() else {
            break;
        };
        total_chunks += 1;
        if chunk_peak_dbfs(&chunk) < SILENT_DBFS {
            silent_chunks += 1;
        }
        if let Some(sink) = wav.as_mut() {
            sink.append(&chunk)?;
        }
        level.accumulate(&chunk);
        chunks_in_second += 1;

        if chunks_in_second == CHUNKS_PER_SECOND {
            second += 1;
            let rms = level.rms_dbfs();
            loudest = loudest.max(rms);
            println!(
                "[{second:02}s] RMS {rms:7.1} dBFS  峰值 {:7.1} dBFS  块 {chunks_in_second}  {}",
                level.peak_dbfs(),
                bar(rms)
            );
            level = Level::default();
            chunks_in_second = 0;
        }
    }

    // 采集线程可能因为激活后的错误（进程退出、音频不连续、下游积压）提前结束
    if let Some(done) = capture.done.as_mut()
        && let Ok(Err(err)) = done.try_recv()
    {
        println!("采集线程提前结束：{err:#}");
    }

    capture.request_stop();
    capture.join(Duration::from_secs(2));

    if let Some(sink) = wav.take() {
        sink.finalize()?;
    }

    println!();
    println!(
        "共收到 {total_chunks} 块（{:.1} 秒音频），其中静音块 {silent_chunks} 块。",
        total_chunks as f64 * 0.1
    );
    if total_chunks == 0 {
        bail!("一块音频都没收到：检查目标进程是否真的在播放声音");
    }
    if loudest < BAR_MIN_DBFS {
        println!(
            "提示：全程最大值只有 {loudest:.1} dBFS。若目标进程正在出声，说明采集链路可能没抓到它。"
        );
    }
    println!(
        "目标进程现在{}。",
        if process_alive(pid) {
            "仍在运行"
        } else {
            "已退出"
        }
    );
    Ok(())
}

/// 一秒内的电平统计（PCM16 → 归一化浮点）。
#[derive(Debug, Default)]
struct Level {
    sum_squares: f64,
    peak: f64,
    samples: u64,
}

impl Level {
    fn accumulate(&mut self, pcm: &[u8]) {
        for frame in pcm.chunks_exact(2) {
            let value = f64::from(i16::from_le_bytes([frame[0], frame[1]])) / 32768.0;
            self.sum_squares += value * value;
            self.peak = self.peak.max(value.abs());
            self.samples += 1;
        }
    }

    fn rms_dbfs(&self) -> f64 {
        if self.samples == 0 {
            return -120.0;
        }
        let rms = (self.sum_squares / self.samples as f64).sqrt();
        20.0 * rms.max(1e-6).log10()
    }

    fn peak_dbfs(&self) -> f64 {
        20.0 * self.peak.max(1e-6).log10()
    }
}

/// 单个音频块的峰值电平（dBFS），用于判断这一块是不是静音。
fn chunk_peak_dbfs(pcm: &[u8]) -> f64 {
    let mut level = Level::default();
    level.accumulate(pcm);
    level.peak_dbfs()
}

/// 把 dBFS 画成 24 格电平条，方便肉眼判断有没有声音。
fn bar(dbfs: f64) -> String {
    const WIDTH: usize = 24;
    let filled = ((dbfs - BAR_MIN_DBFS) / -BAR_MIN_DBFS * WIDTH as f64).clamp(0.0, WIDTH as f64);
    let filled = filled.round() as usize;
    let mut out = String::with_capacity(WIDTH * 3);
    out.push('▏');
    for _ in 0..filled {
        out.push('█');
    }
    out
}

/// WAV 导出：固定 16 kHz / 单声道 / PCM16，与送 ASR 的格式一致。
struct WavSink {
    writer: hound::WavWriter<std::io::BufWriter<std::fs::File>>,
}

impl WavSink {
    fn create(path: &Path) -> Result<Self> {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: bivox::audio::process_loopback::SAMPLE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let writer = hound::WavWriter::create(path, spec)
            .with_context(|| format!("创建 WAV 失败：{}", path.display()))?;
        println!("导出 WAV：{}", path.display());
        Ok(Self { writer })
    }

    fn append(&mut self, pcm: &[u8]) -> Result<()> {
        for frame in pcm.chunks_exact(2) {
            self.writer
                .write_sample(i16::from_le_bytes([frame[0], frame[1]]))
                .context("写 WAV 失败")?;
        }
        Ok(())
    }

    fn finalize(self) -> Result<()> {
        self.writer.finalize().context("收尾 WAV 文件失败")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_reports_silence_floor() {
        let level = Level::default();
        assert_eq!(level.rms_dbfs(), -120.0);
        assert!(level.peak_dbfs() < -100.0);
    }

    #[test]
    fn level_measures_full_scale() {
        let mut level = Level::default();
        level.accumulate(&[0x00, 0x80]);
        assert!((level.rms_dbfs() - 0.0).abs() < 0.001);
        assert!((level.peak_dbfs() - 0.0).abs() < 0.001);
    }

    #[test]
    fn bar_saturates_at_both_ends() {
        assert_eq!(bar(-120.0), "▏");
        assert_eq!(bar(0.0).chars().filter(|c| *c == '█').count(), 24);
    }

    #[test]
    fn silent_chunk_is_classified_by_peak() {
        assert!(chunk_peak_dbfs(&[0u8; 3200]) < SILENT_DBFS);
        assert!(chunk_peak_dbfs(&2000i16.to_le_bytes()) > SILENT_DBFS);
    }
}
