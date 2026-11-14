//! 本地 Silero VAD 门控与发送状态机。
//!
//! - [`SileroDetector`](crate::audio::silero::SileroDetector)：16k / 32ms 帧级语音概率；
//! - [`SpeechGate`]：按采集 chunk 驱动的发送状态机，决定每个 chunk 该不该发给服务端。
//!
//! 设计约束（来自实测，见 docs/architecture.md §10.3）：
//! - 服务端 ASR run-task 在**没有任何音频**约 23s 后会被 `task-failed`，
//!   因此静音期必须每 `keepalive_ms` 补一小段静音；
//! - 服务端 VAD 需要约 800ms 静音才会吐出 `sentence_end`，
//!   所以语音结束后要补足尾部静音（`tail_ms`）；
//! - Silero 每帧固定 512 采样（16k / 32ms）、模型内部需要 64 采样 context；
//!   采集仍是 100ms chunk，由本模块把 chunk 拆成 32ms 帧并跨 chunk 携带余量。

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;

use crate::audio::silero::SileroDetector;

/// 16kHz 单声道 PCM16：每毫秒 32 字节。
const BYTES_PER_MS: usize = 32;
/// 起音需要连续多少帧概率 ≥ `threshold`（2 帧 = 64ms，过滤咔哒声）。
const ONSET_FRAMES: u32 = 2;
/// 负阈值相对起音阈值的回差（Silero 官方默认：`threshold - 0.15`）。
const NEG_MARGIN: f32 = 0.15;

/// 帧级语音概率检测器（生产实现是 Silero；单测用假实现）。
pub trait FrameDetector {
    /// 每帧采样数（Silero 固定 512）。
    fn frame_samples(&self) -> usize;
    /// 处理一帧 PCM16，返回该帧为语音的概率（0.0–1.0）。
    fn process_frame(&mut self, frame: &[i16]) -> Result<f32>;
    /// 重置瞬态（断线重连时调用）。
    fn reset(&mut self);
}

impl FrameDetector for Box<dyn FrameDetector + Send> {
    fn frame_samples(&self) -> usize {
        (**self).frame_samples()
    }

    fn process_frame(&mut self, frame: &[i16]) -> Result<f32> {
        (**self).process_frame(frame)
    }

    fn reset(&mut self) {
        (**self).reset();
    }
}

/// 门控状态机参数
#[derive(Debug, Clone)]
pub struct GateConfig {
    /// 起音概率阈值（0–1）。负阈值自动取 `threshold - 0.15`。
    pub threshold: f32,
    /// 低于负阈值持续多久判定语音结束（毫秒）。
    pub min_silence_ms: u32,
    /// 起音时向前补发的历史音频（毫秒，按整块向上取整，保证实际补发不短于该值）。
    pub pre_roll_ms: u32,
    /// 语音结束后额外补发的静音（毫秒），用于触发服务端 `sentence_end`。
    pub tail_ms: u32,
    /// 完全空闲时补发保活静音的间隔（毫秒）；必须小于任务约 23s 的音频空闲超时。
    pub keepalive_ms: u32,
    /// 打印每 chunk 的检测/发送决策，用于现场排障。
    pub debug: bool,
    /// 可选：把「当前是否处于语音段」发布到共享标志（GUI 顶栏的 VAD 指示器用）。
    pub active_flag: Option<Arc<AtomicBool>>,
}

impl Default for GateConfig {
    fn default() -> Self {
        Self {
            threshold: 0.5,
            min_silence_ms: 500,
            pre_roll_ms: 300,
            tail_ms: 1000,
            keepalive_ms: 10_000,
            debug: false,
            active_flag: None,
        }
    }
}

/// 生产用门控：Silero 检测器（具体类型，便于会话直接持有）。
pub type SileroGate = SpeechGate<SileroDetector>;
/// 类型擦除门控：会话可注入 Silero，单测/ASR 集成测试可注入假检测器。
pub type AnyGate = SpeechGate<Box<dyn FrameDetector + Send>>;

/// 采集 chunk 的发送门控状态机。
///
/// 输入是采集端 100ms 的 PCM16 chunk（也兼容其它长度），输出是需要实际发往
/// ASR 的 chunk 列表：
/// - 语音段：原样发送（起音时额外补发 pre-roll）
/// - 语音刚结束：发送 `tail_ms` 的零静音（触发服务端 `sentence_end`）
/// - 完全空闲：每 `keepalive_ms` 发送一个与输入等长的零静音
pub struct SpeechGate<D: FrameDetector> {
    config: GateConfig,
    detector: D,
    frame_samples: usize,
    min_silence_frames: u32,
    neg_threshold: f32,
    /// 跨 chunk 的采样余量（不足一帧时留到下一个 chunk）。
    carry: Vec<i16>,
    active: bool,
    above_frames: u32,
    below_frames: u32,
    pre_roll: VecDeque<Vec<u8>>,
    pre_roll_bytes: usize,
    /// `pre_roll_ms` 对应的字节数；实际保留量按整块向上取整（见 `push_pre_roll`）。
    pre_roll_target_bytes: usize,
    tail_ms_left: u32,
    idle_ms_since_send: u32,
    /// 推理失败后降级为「全量上传」，避免静默丢音频。
    degraded: bool,
    last_prob: f32,
}

impl SpeechGate<SileroDetector> {
    /// 创建生产用门控（加载内嵌的 Silero 模型）。
    pub fn new(config: GateConfig) -> Result<Self> {
        Ok(Self::with_detector(config, SileroDetector::new()?))
    }
}

impl<D: FrameDetector> SpeechGate<D> {
    pub fn with_detector(config: GateConfig, detector: D) -> Self {
        let frame_samples = detector.frame_samples().max(1);
        let frame_ms = ((frame_samples as u64 * 1000) / 16_000).max(1) as u32;
        let min_silence_frames = config.min_silence_ms.div_ceil(frame_ms).max(1);
        let neg_threshold = (config.threshold - NEG_MARGIN).max(0.01);
        let pre_roll_target_bytes = config.pre_roll_ms.max(1) as usize * BYTES_PER_MS;
        Self {
            config,
            detector,
            frame_samples,
            min_silence_frames,
            neg_threshold,
            carry: Vec::new(),
            active: false,
            above_frames: 0,
            below_frames: 0,
            pre_roll: VecDeque::new(),
            pre_roll_bytes: 0,
            pre_roll_target_bytes,
            tail_ms_left: 0,
            idle_ms_since_send: 0,
            degraded: false,
            last_prob: 0.0,
        }
    }

    /// 处理一个采集 chunk，返回需要发送给 ASR 的 chunk 列表（可能为空）。
    pub fn process_chunk(&mut self, chunk: Vec<u8>) -> Vec<Vec<u8>> {
        if chunk.is_empty() {
            return Vec::new();
        }
        if self.degraded {
            return vec![chunk];
        }
        let chunk_ms = (chunk.len() / BYTES_PER_MS).max(1) as u32;

        // 1. 拆成 32ms 帧，逐帧更新概率状态机
        let mut onset = false;
        let mut any_active = false;
        for pair in chunk.chunks_exact(2) {
            self.carry.push(i16::from_le_bytes([pair[0], pair[1]]));
            if self.carry.len() < self.frame_samples {
                continue;
            }
            let frame: Vec<i16> = self.carry.drain(..self.frame_samples).collect();
            let prob = match self.detector.process_frame(&frame) {
                Ok(p) => p,
                Err(err) => {
                    log::error!("本地 VAD 推理失败，降级为全量上传: {err:#}");
                    self.degraded = true;
                    self.carry.clear();
                    self.publish_active(false);
                    return vec![chunk];
                }
            };
            self.last_prob = prob;
            let was_active = self.active;
            self.update_state(prob);
            if self.active && !was_active {
                onset = true;
            }
            any_active |= self.active;
        }
        self.publish_active(self.active);

        // 2. 决定本 chunk 是否发送
        let mut out = Vec::new();
        if any_active {
            if onset {
                // 起音：补发 pre-roll（历史 chunk），再发当前 chunk
                out.extend(self.pre_roll.iter().cloned());
            }
            out.push(chunk.clone());
            self.tail_ms_left = self.config.tail_ms;
            self.idle_ms_since_send = 0;
        } else if self.tail_ms_left > 0 {
            // 语音刚结束：补静音，帮助服务端 VAD 触发 sentence_end
            out.push(vec![0u8; chunk.len()]);
            self.tail_ms_left = self.tail_ms_left.saturating_sub(chunk_ms);
        } else {
            // 完全空闲：按间隔补保活静音，避免 run-task 任务音频超时
            self.idle_ms_since_send += chunk_ms;
            if self.idle_ms_since_send >= self.config.keepalive_ms {
                self.idle_ms_since_send = 0;
                out.push(vec![0u8; chunk.len()]);
            }
        }

        if self.config.debug {
            let state = if any_active {
                "speech"
            } else if self.tail_ms_left > 0 {
                "tail"
            } else {
                "idle"
            };
            log::warn!(
                "🎚️ VAD p={:.3} thr={:.2} neg={:.2} {} -> emitted {}chunk(s), tail_left={}ms",
                self.last_prob,
                self.config.threshold,
                self.neg_threshold,
                state,
                out.len(),
                self.tail_ms_left
            );
        }

        // 3. 原始音频始终进 pre-roll，便于起音补发和 tail 期间重新起音时恢复
        self.push_pre_roll(chunk);
        out
    }

    /// 断线重连后重置瞬态（模型状态、起音/拖尾、pre-roll 等）。
    pub fn reset_transient(&mut self) {
        self.detector.reset();
        self.carry.clear();
        self.active = false;
        self.above_frames = 0;
        self.below_frames = 0;
        self.pre_roll.clear();
        self.pre_roll_bytes = 0;
        self.tail_ms_left = 0;
        self.idle_ms_since_send = 0;
        self.last_prob = 0.0;
        self.publish_active(false);
    }

    /// 推理是否已失败并降级为全量上传（供会话上报 GUI 告警）。
    pub fn is_degraded(&self) -> bool {
        self.degraded
    }

    /// 当前是否处于语音段（只读；供调参工具 `vad_debug` 标注发送决策）。
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// 剩余需要补发的 tail 静音毫秒数（只读；供调参工具区分 tail 与空闲保活）。
    pub fn tail_ms_left(&self) -> u32 {
        self.tail_ms_left
    }

    fn update_state(&mut self, prob: f32) {
        if self.active {
            if prob >= self.neg_threshold {
                self.below_frames = 0;
            } else {
                self.below_frames += 1;
                if self.below_frames >= self.min_silence_frames {
                    self.active = false;
                    self.above_frames = 0;
                    self.below_frames = 0;
                }
            }
        } else if prob >= self.config.threshold {
            self.above_frames += 1;
            if self.above_frames >= ONSET_FRAMES {
                self.active = true;
                self.below_frames = 0;
            }
        } else {
            self.above_frames = 0;
        }
    }

    fn publish_active(&self, active: bool) {
        if let Some(flag) = &self.config.active_flag {
            flag.store(active, Ordering::Relaxed);
        }
    }

    /// 保留最近音频供起音补发。
    ///
    /// 采集块大小由声卡回调与重采样决定（常见 100–107ms，不一定整除配置的毫秒数），
    /// 所以按“整块向上取整”保留：只在“丢掉最旧一块后仍然 ≥ `pre_roll_ms`”时才丢。
    /// 否则配置 300ms、块长 102ms 时只会保留 2 块（204ms），起音延迟稍大就会切掉首音。
    fn push_pre_roll(&mut self, chunk: Vec<u8>) {
        self.pre_roll_bytes += chunk.len();
        self.pre_roll.push_back(chunk);
        while self.pre_roll.len() > 1 {
            let front_len = self.pre_roll.front().map_or(0, Vec::len);
            if self.pre_roll_bytes.saturating_sub(front_len) < self.pre_roll_target_bytes {
                break;
            }
            if let Some(front) = self.pre_roll.pop_front() {
                self.pre_roll_bytes -= front.len();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc as StdArc;
    use std::sync::atomic::AtomicUsize;

    const FRAME_SAMPLES: usize = 512; // 32ms

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

    fn is_silent(chunk: &[u8]) -> bool {
        chunk.iter().all(|byte| *byte == 0)
    }

    fn gate_config(pre_roll_ms: u32, tail_ms: u32, keepalive_ms: u32) -> GateConfig {
        GateConfig {
            threshold: 0.5,
            min_silence_ms: 64, // 2 帧
            pre_roll_ms,
            tail_ms,
            keepalive_ms,
            debug: false,
            active_flag: None,
        }
    }

    /// 按预设概率序列返回（用完后回落到 fallback）。
    struct Scripted {
        probs: VecDeque<f32>,
        fallback: f32,
    }

    impl Scripted {
        fn new(probs: &[f32]) -> Self {
            Self {
                probs: probs.iter().copied().collect(),
                fallback: 0.0,
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

    struct Counting {
        calls: StdArc<AtomicUsize>,
        prob: f32,
    }

    impl FrameDetector for Counting {
        fn frame_samples(&self) -> usize {
            FRAME_SAMPLES
        }

        fn process_frame(&mut self, _frame: &[i16]) -> Result<f32> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(self.prob)
        }

        fn reset(&mut self) {}
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
    fn onset_requires_two_frames_and_sends_pre_roll() {
        let flag = StdArc::new(AtomicBool::new(false));
        let mut config = gate_config(32, 0, 10_000);
        config.active_flag = Some(flag.clone());
        let mut gate = SpeechGate::with_detector(config, Scripted::new(&[0.9, 0.9]));

        assert!(
            gate.process_chunk(silence_chunk()).is_empty(),
            "单帧不应起音"
        );
        assert!(!flag.load(Ordering::Relaxed));

        let out = gate.process_chunk(voice_chunk());
        assert_eq!(out.len(), 2, "起音应补 pre-roll + 当前 chunk");
        assert!(is_silent(&out[0]), "pre-roll 是上一个静音 chunk");
        assert!(!is_silent(&out[1]));
        assert!(flag.load(Ordering::Relaxed));
    }

    #[test]
    fn min_silence_ends_speech() {
        let mut gate = SpeechGate::with_detector(
            gate_config(32, 0, 10_000),
            Scripted::new(&[0.9, 0.9, 0.0, 0.0, 0.0]),
        );
        assert!(gate.process_chunk(silence_chunk()).is_empty());
        assert_eq!(gate.process_chunk(voice_chunk()).len(), 2);
        assert_eq!(
            gate.process_chunk(silence_chunk()).len(),
            1,
            "第一帧静音仍在保持期"
        );
        assert!(
            gate.process_chunk(silence_chunk()).is_empty(),
            "超过 min_silence 后应结束"
        );
        assert!(!gate.active);
    }

    #[test]
    fn tail_sends_zero_chunks_after_speech() {
        let mut gate = SpeechGate::with_detector(
            gate_config(32, 64, 10_000),
            Scripted::new(&[0.9, 0.9, 0.0, 0.0, 0.0, 0.0]),
        );
        assert!(gate.process_chunk(silence_chunk()).is_empty());
        assert_eq!(gate.process_chunk(voice_chunk()).len(), 2);
        assert_eq!(gate.process_chunk(silence_chunk()).len(), 1);
        let tail1 = gate.process_chunk(silence_chunk());
        assert_eq!(tail1.len(), 1);
        assert!(is_silent(&tail1[0]), "tail 应是零静音");
        let tail2 = gate.process_chunk(silence_chunk());
        assert_eq!(tail2.len(), 1);
        assert!(is_silent(&tail2[0]));
        assert!(gate.process_chunk(silence_chunk()).is_empty(), "tail 结束");
    }

    #[test]
    fn idle_sends_keepalive_every_interval() {
        let mut gate = SpeechGate::with_detector(gate_config(32, 0, 96), Scripted::new(&[]));
        let mut emitted = Vec::new();
        for _ in 0..6 {
            emitted.push(gate.process_chunk(silence_chunk()).len());
        }
        assert_eq!(emitted, vec![0, 0, 1, 0, 0, 1]);
        assert!(gate.process_chunk(silence_chunk()).len() == 0);
    }

    #[test]
    fn chunks_are_framed_across_boundaries() {
        let calls = StdArc::new(AtomicUsize::new(0));
        let mut gate = SpeechGate::with_detector(
            gate_config(32, 0, 10_000),
            Counting {
                calls: calls.clone(),
                prob: 0.0,
            },
        );
        // 1600 采样 = 3×512 帧 + 64 采样余量；下一 chunk 继续消费余量
        gate.process_chunk(vec![0u8; 1600 * 2]);
        assert_eq!(calls.load(Ordering::Relaxed), 3);
        gate.process_chunk(vec![0u8; 1600 * 2]);
        assert_eq!(calls.load(Ordering::Relaxed), 6);
    }

    #[test]
    fn pre_roll_keeps_enough_whole_chunks() {
        // 32ms 一块时 300ms 应保留 10 块（320ms）；按“字节上限”只会留 9 块（288ms）
        let mut gate = SpeechGate::with_detector(gate_config(300, 0, 10_000), Scripted::new(&[]));
        for _ in 0..20 {
            assert!(gate.process_chunk(silence_chunk()).is_empty());
        }
        assert_eq!(
            gate.pre_roll.len(),
            10,
            "应按整块向上取整保留 10 块（320ms ≥ 300ms）"
        );
        assert_eq!(gate.pre_roll_bytes, 10 * 1024);
    }

    #[test]
    fn reset_transient_clears_state() {
        let mut gate =
            SpeechGate::with_detector(gate_config(32, 0, 10_000), Scripted::new(&[0.9, 0.9]));
        gate.process_chunk(voice_chunk());
        gate.process_chunk(voice_chunk());
        assert!(gate.active);
        gate.reset_transient();
        assert!(!gate.active);
        assert!(gate.process_chunk(silence_chunk()).is_empty());
    }

    #[test]
    fn inference_failure_degrades_to_passthrough() {
        let mut gate = SpeechGate::with_detector(gate_config(32, 0, 10_000), Failing);
        let chunk = voice_chunk();
        let out = gate.process_chunk(chunk.clone());
        assert_eq!(out, vec![chunk.clone()], "失败后应原样上传");
        let out = gate.process_chunk(chunk.clone());
        assert_eq!(out, vec![chunk], "降级后持续全量上传");
    }
}
