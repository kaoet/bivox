//! 进程环回采集线程：按进程名找 PID → 开 WASAPI 进程环回 → 按 100ms 攒块 → 有界 channel。
//!
//! 与麦克风采集（`audio/capture.rs`）的区别：
//! - 音源是另一个进程的渲染输出（见 `process_loopback.rs` 与 D36），不是输入设备；
//! - 目标进程随时可能退出/重启，所以这里把采集做成**一次性会话**：线程结束即返回错误，
//!   由上层退避重连（`pipeline` 里的对方线路）；
//! - WASAPI 在安静时可能长时间不送包，而 ASR 的 run-task 约 23 秒没有音频就会失败，
//!   因此这里按**墙钟**补静音块，保证「送出去的采样数 ≈ 经过的时间 × 采样率」。
//!
//! 线程是 `std::thread`（不是 tokio 任务）：采集里要做 50ms 粒度的阻塞等待，不能占运行时。

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};

use super::process_loopback::{ProcessLoopback, SAMPLE_RATE};

/// 单个音频块的采样数（100ms）。
pub const CHUNK_SAMPLES: usize = 1600;
/// 单个音频块的字节数（PCM16 单声道）。
pub const CHUNK_BYTES: usize = CHUNK_SAMPLES * 2;
/// 采集 channel 容量：最多积压 1 秒音频。
pub const QUEUE_CHUNKS: usize = 10;
/// 目标进程存活检查间隔。
const LIVENESS_INTERVAL: Duration = Duration::from_secs(1);
/// 单次最多补多少静音（防止系统睡眠后一次性灌进大量静音）。
const MAX_SILENCE_SAMPLES: usize = SAMPLE_RATE as usize;

/// 查找进程名对应的 PID；找不到返回 `None`，同名多个时取第一个（并记 warning）。
pub fn find_pid(process_name: &str) -> Result<Option<u32>> {
    let target = process_name.trim();
    if target.is_empty() {
        return Ok(None);
    }
    let matches: Vec<u32> = list_processes()?
        .into_iter()
        .filter(|(_, name)| name.eq_ignore_ascii_case(target))
        .map(|(pid, _)| pid)
        .collect();
    match matches.as_slice() {
        [] => Ok(None),
        [pid] => Ok(Some(*pid)),
        [pid, rest @ ..] => {
            log::warn!(
                "找到 {} 个名为 {target} 的进程（PID {} 等），使用 PID {pid}",
                rest.len() + 1,
                rest[0]
            );
            Ok(Some(*pid))
        }
    }
}

/// 目标进程是否还在（用进程快照判断，不受跨完整性级别的访问限制影响）。
pub fn process_alive(pid: u32) -> bool {
    match list_processes() {
        Ok(list) => list.iter().any(|(candidate, _)| *candidate == pid),
        // 快照失败极罕见：按「还活着」处理，避免误触发重连（真正的采集错误会从 WASAPI 报出来）
        Err(err) => {
            log::debug!("枚举进程失败，暂按目标进程存活处理：{err:#}");
            true
        }
    }
}

/// 一次采集会话句柄。
///
/// 上层拿到 `chunks` 后要**持续消费**：积压超过 1 秒时采集线程会自行结束（返回错误）。
/// `ready` / `done` 是一次性接收端（`take()` 后为空），异步调用方 `.await`、同步调用方
/// `blocking_recv()`。
pub struct ProcessCapture {
    /// 门控前的 100ms PCM16 块。
    pub chunks: mpsc::Receiver<Vec<u8>>,
    /// 采集线程是否已成功启动环回；`Err` 表示激活/初始化失败。
    pub ready: Option<oneshot::Receiver<Result<()>>>,
    /// 采集线程的最终结果（正常停止为 `Ok`）。
    pub done: Option<oneshot::Receiver<Result<()>>>,
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl ProcessCapture {
    /// 为 `pid` 启动采集线程（进程树）。线程启动失败只可能是 `std::thread` 创建失败。
    pub fn start(pid: u32) -> Result<Self> {
        let (chunk_tx, chunk_rx) = mpsc::channel::<Vec<u8>>(QUEUE_CHUNKS);
        let (ready_tx, ready_rx) = oneshot::channel();
        let (done_tx, done_rx) = oneshot::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = stop.clone();
        let join = std::thread::Builder::new()
            .name("process-loopback".to_string())
            .spawn(move || {
                let result = capture_loop(pid, chunk_tx, stop_flag, ready_tx);
                // best-effort: 上层可能已经放弃等待这次采集
                let _ = done_tx.send(result);
            })
            .context("创建进程环回采集线程失败")?;
        Ok(Self {
            chunks: chunk_rx,
            ready: Some(ready_rx),
            done: Some(done_rx),
            stop,
            join: Some(join),
        })
    }

    /// 请求采集线程退出（线程最多再等 50ms 的音频事件）。
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
    }

    /// 等待采集线程结束，最多 `timeout`；返回是否在超时前退出。
    pub fn join(&mut self, timeout: Duration) -> bool {
        let Some(join) = self.join.take() else {
            return true;
        };
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if join.is_finished() {
                // best-effort: 线程 panic 的 payload 无关紧要，结果已经通过 done 上报
                let _ = join.join();
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        // std 线程无法强杀：线程看到 stop 标志后会自己退出，这里只是不再等
        false
    }
}

impl Drop for ProcessCapture {
    fn drop(&mut self) {
        self.request_stop();
    }
}

/// 采集线程主体：成功建立环回后先通知上层，再循环「等事件 → 收包 → 补静音 → 出块」。
fn capture_loop(
    pid: u32,
    tx: mpsc::Sender<Vec<u8>>,
    stop: Arc<AtomicBool>,
    ready: oneshot::Sender<Result<()>>,
) -> Result<()> {
    let client = ProcessLoopback::new(pid)?;
    // best-effort: 上层可能已经超时放弃这次尝试
    let _ = ready.send(Ok(()));

    let mut queue: VecDeque<u8> = VecDeque::new();
    let mut pacer = Pacer::new(SAMPLE_RATE);
    let mut started = Instant::now();
    let mut last_liveness = Instant::now();
    let mut silence_warned = false;

    loop {
        if stop.load(Ordering::Acquire) {
            return Ok(());
        }
        // 超时（安静期没有新数据）是正常情况，继续往下走补静音。
        client.wait()?;

        loop {
            let before = queue.len();
            let Some(discontinuity) = client.read(&mut queue)? else {
                break;
            };
            pacer.note_produced((queue.len() - before) / 2);
            if discontinuity && pacer.produced() > 0 {
                bail!("Windows 报告音频不连续，需要重建采集");
            }
        }

        if last_liveness.elapsed() >= LIVENESS_INTERVAL {
            last_liveness = Instant::now();
            if !process_alive(pid) {
                bail!("目标进程（PID {pid}）已退出");
            }
        }

        if queue.len() > CHUNK_BYTES * QUEUE_CHUNKS {
            bail!("采集积压超过 1 秒，下游消费太慢");
        }

        // 墙钟补静音：WASAPI 安静时可能完全不送包，靠它维持 ASR 的心跳。
        if queue.len() < CHUNK_BYTES {
            let deficit = pacer.deficit(started.elapsed());
            let padded = deficit.min(MAX_SILENCE_SAMPLES as u64) as usize;
            if padded > 0 {
                queue.extend(std::iter::repeat_n(0u8, padded * 2));
                pacer.note_produced(padded);
            }
            if deficit > padded as u64 {
                // 落后超过 1 秒（系统睡眠 / 线程被长时间挂起）：重置基准，不追补旧时间
                started = Instant::now();
                pacer.rebase(queue.len() / 2);
                if !silence_warned {
                    silence_warned = true;
                    log::warn!("进程环回采集落后超过 1 秒，已重置时间基准");
                }
            }
        }

        while queue.len() >= CHUNK_BYTES {
            let pcm: Vec<u8> = queue.drain(..CHUNK_BYTES).collect();
            tx.try_send(pcm)
                .map_err(|_| anyhow!("音频消费端已停止或积压超过 1 秒，终止本次采集"))?;
        }
    }
}

/// 墙钟配速器：保证「已产生的采样数」跟上经过的时间。
#[derive(Debug)]
struct Pacer {
    rate: u32,
    produced: u64,
}

impl Pacer {
    fn new(rate: u32) -> Self {
        Self { rate, produced: 0 }
    }

    /// 已产生的采样数。
    fn produced(&self) -> u64 {
        self.produced
    }

    /// 到 `elapsed` 为止还差多少采样没有产生。
    fn deficit(&self, elapsed: Duration) -> u64 {
        let expected = (elapsed.as_secs_f64() * f64::from(self.rate)) as u64;
        expected.saturating_sub(self.produced)
    }

    fn note_produced(&mut self, samples: usize) {
        self.produced += samples as u64;
    }

    /// 重置基准：把「已产生」直接对齐到当前缓冲里的采样数（调用方同时重置计时起点）。
    fn rebase(&mut self, buffered_samples: usize) {
        self.produced = buffered_samples as u64;
    }
}

/// 枚举当前所有进程的 `(PID, exe 文件名)`。
fn list_processes() -> Result<Vec<(u32, String)>> {
    let mut out = Vec::new();
    // SAFETY: 快照句柄在下面对称关闭；PROCESSENTRY32W 先填 dwSize 再交给系统写入
    unsafe {
        let snapshot =
            CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).context("创建进程快照失败")?;
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        // 快照里一个进程都没有时 Process32FirstW 会失败，按空列表处理
        let mut has_entry = Process32FirstW(snapshot, &mut entry).is_ok();
        while has_entry {
            let len = entry
                .szExeFile
                .iter()
                .position(|&unit| unit == 0)
                .unwrap_or(entry.szExeFile.len());
            out.push((
                entry.th32ProcessID,
                String::from_utf16_lossy(&entry.szExeFile[..len]),
            ));
            has_entry = Process32NextW(snapshot, &mut entry).is_ok();
        }
        // best-effort: 关闭快照失败不影响已取到的结果
        let _ = CloseHandle(snapshot);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pacer_fills_elapsed_time_with_silence() {
        let pacer = Pacer::new(SAMPLE_RATE);
        assert_eq!(pacer.deficit(Duration::from_millis(100)), 1600);
        assert_eq!(pacer.deficit(Duration::from_secs(1)), 16_000);
    }

    #[test]
    fn pacer_counts_produced_samples() {
        let mut pacer = Pacer::new(SAMPLE_RATE);
        pacer.note_produced(16_000);
        assert_eq!(pacer.deficit(Duration::from_secs(1)), 0, "已产生足够采样");
        assert_eq!(pacer.deficit(Duration::from_secs(2)), 16_000);
    }

    #[test]
    fn pacer_rebase_aligns_to_buffered_samples() {
        let mut pacer = Pacer::new(SAMPLE_RATE);
        pacer.note_produced(48_000);
        pacer.rebase(1600);
        assert_eq!(pacer.produced(), 1600);
        assert_eq!(pacer.deficit(Duration::from_millis(100)), 0);
    }

    #[test]
    fn find_pid_locates_current_process() {
        let exe = std::env::current_exe().expect("当前测试进程应有 exe 路径");
        let name = exe
            .file_name()
            .expect("exe 路径应有文件名")
            .to_string_lossy()
            .to_string();
        assert_eq!(find_pid(&name).unwrap(), Some(std::process::id()));
    }

    #[test]
    fn find_pid_returns_none_for_unknown_process() {
        assert_eq!(
            find_pid("kaols-不存在这个进程.exe").unwrap(),
            None,
            "未知进程名应返回 None 而不是报错"
        );
        assert_eq!(find_pid("   ").unwrap(), None, "空白进程名视为未配置");
    }

    #[test]
    fn current_process_is_alive_and_bogus_pid_is_not() {
        assert!(process_alive(std::process::id()));
        assert!(!process_alive(u32::MAX - 1), "不存在的 PID 应判定为已退出");
    }
}
