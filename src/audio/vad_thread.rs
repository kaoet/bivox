//! 独立 VAD 线程：把采集 chunk 送进 Silero 门控，再把应上传的 chunk 交给 ASR 任务。
//!
//! 为什么不放在 cpal 回调线程：回调是实时音频线程，禁止阻塞/推理（否则 Xrun）；
//! 为什么不放在 ASR 任务：VAD 推理会阻塞 ASR 的 socket 读写，并与网络抖动耦合。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use tokio::sync::mpsc::{Receiver, Sender};

use super::vad::{FrameDetector, SpeechGate};

/// VAD 线程本体（可单测）：持有门控并处理 chunk / 重置。
pub struct VadWorker<D: FrameDetector> {
    gate: SpeechGate<D>,
    degraded: Arc<AtomicBool>,
}

impl<D: FrameDetector> VadWorker<D> {
    pub fn new(gate: SpeechGate<D>, degraded: Arc<AtomicBool>) -> Self {
        Self { gate, degraded }
    }

    /// 处理一个采集 chunk，返回需要发往 ASR 的 chunk。
    pub fn on_chunk(&mut self, chunk: Vec<u8>) -> Vec<Vec<u8>> {
        let out = self.gate.process_chunk(chunk);
        if self.gate.is_degraded() {
            self.degraded.store(true, Ordering::Relaxed);
        }
        out
    }

    /// 断线重连：请 VAD/门控重置瞬态。
    pub fn on_reset(&mut self) {
        self.gate.reset_transient();
    }
}

/// 启动独立线程：`rx` 收原始 PCM chunk，`tx` 发门控后的 chunk。
///
/// - `reset` 置位时，线程在下一块音频处理前重置门控瞬态（断线重连）；
/// - `degraded` 在推理失败降级后置位（会话轮询后向 GUI 报 Warning）。
pub fn spawn<D: FrameDetector + Send + 'static>(
    gate: SpeechGate<D>,
    mut rx: Receiver<Vec<u8>>,
    tx: Sender<Vec<u8>>,
    reset: Arc<AtomicBool>,
    degraded: Arc<AtomicBool>,
) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("silero-vad".to_string())
        .spawn(move || {
            let mut worker = VadWorker::new(gate, degraded);
            loop {
                if reset.swap(false, Ordering::SeqCst) {
                    worker.on_reset();
                }
                let Some(chunk) = rx.blocking_recv() else {
                    break;
                };
                for out in worker.on_chunk(chunk) {
                    // ASR 任务已退出时直接结束线程。
                    if tx.blocking_send(out).is_err() {
                        return;
                    }
                }
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::vad::GateConfig;
    use anyhow::Result;
    use std::collections::VecDeque;

    const FRAME_SAMPLES: usize = 512;

    fn silence_chunk() -> Vec<u8> {
        vec![0u8; FRAME_SAMPLES * 2]
    }

    fn voice_chunk() -> Vec<u8> {
        let mut out = Vec::with_capacity(FRAME_SAMPLES * 2);
        for _ in 0..FRAME_SAMPLES {
            out.extend_from_slice(&1000i16.to_le_bytes());
        }
        out
    }

    fn config() -> GateConfig {
        GateConfig {
            threshold: 0.5,
            min_silence_ms: 64,
            pre_roll_ms: 32,
            tail_ms: 0,
            keepalive_ms: 10_000,
            debug: false,
            active_flag: None,
        }
    }

    /// 按预设概率序列返回，用完后回落 fallback。
    struct Scripted {
        probs: VecDeque<f32>,
        fallback: f32,
    }

    impl Scripted {
        fn new(probs: &[f32], fallback: f32) -> Self {
            Self {
                probs: probs.iter().copied().collect(),
                fallback,
            }
        }
    }

    impl FrameDetector for Scripted {
        fn frame_samples(&self) -> usize {
            FRAME_SAMPLES
        }

        fn process_frame(&mut self, _frame: &[i16]) -> Result<f32> {
            Ok(self.probs.pop_front().unwrap_or(self.fallback))
        }

        fn reset(&mut self) {
            self.probs.clear();
        }
    }

    struct Failing;

    impl FrameDetector for Failing {
        fn frame_samples(&self) -> usize {
            FRAME_SAMPLES
        }

        fn process_frame(&mut self, _frame: &[i16]) -> Result<f32> {
            anyhow::bail!("boom")
        }

        fn reset(&mut self) {}
    }

    #[test]
    fn worker_emits_gated_chunks() {
        let degraded = Arc::new(AtomicBool::new(false));
        let gate = SpeechGate::with_detector(config(), Scripted::new(&[0.9, 0.9], 0.0));
        let mut worker = VadWorker::new(gate, degraded.clone());

        assert!(worker.on_chunk(silence_chunk()).is_empty(), "单帧不起音");
        let out = worker.on_chunk(voice_chunk());
        assert_eq!(out.len(), 2, "起音应补 pre-roll + 当前 chunk");
        assert!(!degraded.load(Ordering::Relaxed));
    }

    #[test]
    fn reset_clears_gate_state() {
        let degraded = Arc::new(AtomicBool::new(false));
        let gate = SpeechGate::with_detector(config(), Scripted::new(&[0.9, 0.9], 0.0));
        let mut worker = VadWorker::new(gate, degraded);
        worker.on_chunk(silence_chunk());
        worker.on_chunk(voice_chunk());
        worker.on_reset();
        assert!(
            worker.on_chunk(silence_chunk()).is_empty(),
            "重置后应回到 idle"
        );
    }

    #[test]
    fn failure_sets_degraded_flag() {
        let degraded = Arc::new(AtomicBool::new(false));
        let gate = SpeechGate::with_detector(config(), Failing);
        let mut worker = VadWorker::new(gate, degraded.clone());
        let chunk = voice_chunk();
        assert_eq!(worker.on_chunk(chunk.clone()), vec![chunk]);
        assert!(degraded.load(Ordering::Relaxed));
    }

    #[test]
    fn thread_forwards_chunks_and_exits_on_input_close() {
        let (tx_in, rx_in) = tokio::sync::mpsc::channel(8);
        let (tx_out, mut rx_out) = tokio::sync::mpsc::channel(8);
        let reset = Arc::new(AtomicBool::new(false));
        let degraded = Arc::new(AtomicBool::new(false));
        let gate = SpeechGate::with_detector(config(), Scripted::new(&[0.9, 0.9], 0.0));
        let handle = spawn(gate, rx_in, tx_out, reset, degraded).unwrap();

        tx_in.blocking_send(silence_chunk()).unwrap();
        tx_in.blocking_send(voice_chunk()).unwrap();
        drop(tx_in);
        handle.join().unwrap();

        let mut count = 0;
        while rx_out.try_recv().is_ok() {
            count += 1;
        }
        assert_eq!(count, 2, "起音应输出 pre-roll + 当前 chunk");
    }
}
