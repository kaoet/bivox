//! 本地粗粒度 VAD 与发送门控。
//!
//! 目的：把"静音也全程上传"变成"只上传语音 + 少量保活/尾部静音"，
//! 在保持识别体验不变的前提下降低 ASR 费用。
//!
//! - [`EnergyVad`]：自适应噪声底 + 迟滞的能量 VAD（无第三方依赖，适配近场麦克风）
//! - [`SpeechGate`]：按采集 chunk 驱动的发送状态机
//!   （pre-roll / hangover / tail / keepalive），决定每个 chunk 该不该发给服务端
//!
//! 设计约束（来自实测）：
//! - ASR run-task 任务在**没有任何音频**约 23s 后会被服务端 `task-failed`，
//!   WS Ping 与 heartbeat 参数都救不了，只有音频能续；
//! - 因此静音期必须每 <23s 补一小段静音（默认 10s 补 100ms，约 1% 开销）；
//! - 服务端 VAD 需要看到约 `max_sentence_silence`(默认 800ms) 的静音才会吐出
//!   `sentence_end`，所以语音结束后要补足尾部静音（hangover + tail 合计约 1.5s）。

use std::collections::VecDeque;

/// 16kHz 单声道 PCM16：每毫秒 32 字节
const BYTES_PER_MS: usize = 32;
/// 数字静音/低于该电平时按该值处理，避免 log(0)
const MIN_DBFS: f32 = -90.0;
/// 噪声底向下跟随速度（低于噪声底时）
const NOISE_DOWN_ALPHA: f32 = 0.25;
/// 噪声底向上上浮速度（高于噪声底但当前不是语音时，缓慢上浮）
const NOISE_UP_ALPHA: f32 = 0.002;
/// 语音维持阈值比起音阈值低多少 dB（迟滞，避免句中出现短暂低能量时中断）
const SUSTAIN_HYSTERESIS_DB: f32 = 3.0;
/// 启动后的噪声底标定窗口：前 300ms 只测量不判定，避免未知底噪导致误触发
const WARMUP_MS: u32 = 300;

/// 帧级语音检测器参数
#[derive(Debug, Clone)]
pub struct VadConfig {
    /// 帧长（毫秒）。默认 20ms；起音判定粒度即该值
    pub frame_ms: u32,
    /// 起音阈值 = 噪声底 + margin_db
    pub margin_db: f32,
    /// 阈值绝对下限（dBFS），避免极安静环境下阈值过低
    pub min_threshold_dbfs: f32,
    /// 连续多少帧超过起音阈值才判定为语音（默认 2 帧 = 40ms）
    pub onset_frames: u32,
    /// 语音结束后维持多少毫秒才判定结束
    pub hangover_ms: u32,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            frame_ms: 20,
            margin_db: 9.0,
            min_threshold_dbfs: -55.0,
            onset_frames: 2,
            hangover_ms: 500,
        }
    }
}

/// 门控状态机参数
#[derive(Debug, Clone)]
pub struct GateConfig {
    pub vad: VadConfig,
    /// 起音时向前补发的历史音频（毫秒），避免切掉首字声母
    pub pre_roll_ms: u32,
    /// 语音结束后额外补发的静音（毫秒），用于触发服务端 sentence_end
    pub tail_ms: u32,
    /// 完全空闲时补发保活静音的间隔（毫秒）；必须小于任务约 23s 的音频空闲超时
    pub keepalive_ms: u32,
    /// 打印每 chunk 的检测/发送决策，用于现场标定阈值
    pub debug: bool,
}

impl Default for GateConfig {
    fn default() -> Self {
        Self {
            vad: VadConfig::default(),
            pre_roll_ms: 300,
            tail_ms: 1000,
            keepalive_ms: 10_000,
            debug: false,
        }
    }
}

/// 帧级语音检测器接口（便于将来替换为 webrtc-vad / Silero 等实现）
pub trait VoiceDetector {
    /// 处理一帧 PCM16（LE）音频，返回该帧是否处于语音段（含 hangover 维持）
    fn process_frame(&mut self, frame: &[u8]) -> bool;
    /// 当前是否处于语音段
    fn is_active(&self) -> bool;
    /// 重置瞬态（起音/拖尾状态、pre-roll 等），保留噪声底估计
    fn reset_transient(&mut self);
    /// 最近一帧的电平（dBFS），用于调试
    fn last_frame_dbfs(&self) -> f32;
    /// 当前噪声底估计（dBFS），用于调试
    fn noise_dbfs(&self) -> f32;
    /// 当前起音阈值（dBFS），用于调试
    fn threshold_dbfs(&self) -> f32;
}

/// 自适应能量 VAD：
/// 用"噪声底 + margin"作为动态阈值，连续多帧超过阈值才判定语音，
/// 语音结束后维持 hangover 才判定结束。
pub struct EnergyVad {
    margin_db: f32,
    min_threshold_dbfs: f32,
    onset_frames: u32,
    hangover_frames: u32,
    noise_dbfs: f32,
    last_frame_dbfs: f32,
    active: bool,
    above_frames: u32,
    below_frames: u32,
    /// 启动标定剩余帧数；期间强制返回非语音
    warmup_frames_left: u32,
    /// 标定窗口内的最小电平
    warmup_min_db: f32,
}

impl EnergyVad {
    pub fn new(config: &VadConfig) -> Self {
        let hangover_frames = (config.hangover_ms / config.frame_ms.max(1)).max(1);
        Self {
            margin_db: config.margin_db,
            min_threshold_dbfs: config.min_threshold_dbfs,
            onset_frames: config.onset_frames.max(1),
            hangover_frames,
            // 初始噪声底给一个保守值，标定完成后由输入替换
            noise_dbfs: -60.0,
            last_frame_dbfs: MIN_DBFS,
            active: false,
            above_frames: 0,
            below_frames: 0,
            warmup_frames_left: (WARMUP_MS / config.frame_ms.max(1)).max(1),
            warmup_min_db: f32::MAX,
        }
    }

    /// 一帧 PCM16 的 RMS 电平（dBFS）
    pub fn frame_dbfs(frame: &[u8]) -> f32 {
        let mut sum = 0.0f64;
        let mut count = 0u32;
        for pair in frame.chunks_exact(2) {
            let sample = i16::from_le_bytes([pair[0], pair[1]]) as f64 / 32768.0;
            sum += sample * sample;
            count += 1;
        }
        if count == 0 {
            return MIN_DBFS;
        }
        let rms = (sum / count as f64).sqrt();
        if rms <= 1e-9 {
            MIN_DBFS
        } else {
            ((20.0 * rms.log10()) as f32).clamp(MIN_DBFS, 0.0)
        }
    }

    fn onset_threshold_dbfs(&self) -> f32 {
        (self.noise_dbfs + self.margin_db).max(self.min_threshold_dbfs)
    }

    fn update_noise_floor(&mut self, frame_dbfs: f32) {
        if frame_dbfs < self.noise_dbfs {
            // 环境变安静：快速跟随
            self.noise_dbfs += NOISE_DOWN_ALPHA * (frame_dbfs - self.noise_dbfs);
        } else if !self.active {
            // 语音段内不向上学习，避免把说话声学成噪声底
            self.noise_dbfs += NOISE_UP_ALPHA * (frame_dbfs - self.noise_dbfs);
        }
    }
}

impl VoiceDetector for EnergyVad {
    fn process_frame(&mut self, frame: &[u8]) -> bool {
        if frame.len() < 2 {
            return self.active;
        }
        let frame_dbfs = Self::frame_dbfs(frame);
        self.last_frame_dbfs = frame_dbfs;

        // 启动标定：只记录最小电平，不判定语音
        if self.warmup_frames_left > 0 {
            self.warmup_min_db = self.warmup_min_db.min(frame_dbfs);
            self.warmup_frames_left -= 1;
            if self.warmup_frames_left == 0 {
                self.noise_dbfs = self.warmup_min_db.clamp(MIN_DBFS, -20.0);
            }
            return false;
        }

        self.update_noise_floor(frame_dbfs);

        let onset_threshold = self.onset_threshold_dbfs();
        let sustain_threshold = onset_threshold - SUSTAIN_HYSTERESIS_DB;

        if self.active {
            if frame_dbfs > sustain_threshold {
                self.below_frames = 0;
            } else {
                self.below_frames += 1;
                if self.below_frames >= self.hangover_frames {
                    self.active = false;
                    self.above_frames = 0;
                    self.below_frames = 0;
                }
            }
        } else if frame_dbfs > onset_threshold {
            self.above_frames += 1;
            if self.above_frames >= self.onset_frames {
                self.active = true;
                self.below_frames = 0;
            }
        } else {
            self.above_frames = 0;
        }

        self.active
    }

    fn is_active(&self) -> bool {
        self.active
    }

    fn reset_transient(&mut self) {
        self.active = false;
        self.above_frames = 0;
        self.below_frames = 0;
    }

    fn last_frame_dbfs(&self) -> f32 {
        self.last_frame_dbfs
    }

    fn noise_dbfs(&self) -> f32 {
        self.noise_dbfs
    }

    fn threshold_dbfs(&self) -> f32 {
        self.onset_threshold_dbfs()
    }
}

/// 采集 chunk 的发送门控状态机。
///
/// 输入是采集端 100ms 的 PCM16 chunk（也兼容其它长度），输出是需要实际发往
/// ASR 的 chunk 列表：
/// - 语音段：原样发送（起音时额外补发 pre-roll）
/// - 语音刚结束：发送 hangover 期间的静音 + `tail_ms` 的零静音
/// - 完全空闲：每 `keepalive_ms` 发送一个与输入等长的零静音
pub struct SpeechGate<D: VoiceDetector = EnergyVad> {
    config: GateConfig,
    detector: D,
    pre_roll: VecDeque<Vec<u8>>,
    pre_roll_bytes: usize,
    pre_roll_limit_bytes: usize,
    tail_ms_left: u32,
    idle_ms_since_send: u32,
}

impl SpeechGate<EnergyVad> {
    pub fn new(config: GateConfig) -> Self {
        let detector = EnergyVad::new(&config.vad);
        Self::with_detector(config, detector)
    }
}

impl<D: VoiceDetector> SpeechGate<D> {
    pub fn with_detector(config: GateConfig, detector: D) -> Self {
        let pre_roll_limit_bytes = config.pre_roll_ms.max(1) as usize * BYTES_PER_MS;
        Self {
            config,
            detector,
            pre_roll: VecDeque::new(),
            pre_roll_bytes: 0,
            pre_roll_limit_bytes,
            tail_ms_left: 0,
            idle_ms_since_send: 0,
        }
    }

    /// 处理一个采集 chunk，返回需要发送给 ASR 的 chunk 列表（可能为空）
    pub fn process_chunk(&mut self, chunk: Vec<u8>) -> Vec<Vec<u8>> {
        if chunk.is_empty() {
            return Vec::new();
        }
        let chunk_ms = (chunk.len() / BYTES_PER_MS).max(1) as u32;
        let frame_bytes = (self.config.vad.frame_ms.max(1) as usize) * BYTES_PER_MS;

        // 1. 逐 20ms 帧检测本 chunk 内是否出现语音/起音
        let mut onset = false;
        let mut any_active = false;
        let mut was_active = self.detector.is_active();
        for frame in chunk.chunks(frame_bytes) {
            let active = self.detector.process_frame(frame);
            if active && !was_active {
                onset = true;
            }
            any_active |= active;
            was_active = active;
        }

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
            eprintln!(
                "🎚️ VAD db={:.1} noise={:.1} thr={:.1} {} -> emitted {}chunk(s), tail_left={}ms",
                self.detector.last_frame_dbfs(),
                self.detector.noise_dbfs(),
                self.detector.threshold_dbfs(),
                state,
                out.len(),
                self.tail_ms_left
            );
        }

        // 3. 原始音频始终进 pre-roll，便于起音补发和 tail 期间重新起音时恢复
        self.push_pre_roll(chunk);
        out
    }

    /// 断线重连后重置瞬态（保留噪声底估计）
    pub fn reset_transient(&mut self) {
        self.detector.reset_transient();
        self.pre_roll.clear();
        self.pre_roll_bytes = 0;
        self.tail_ms_left = 0;
        self.idle_ms_since_send = 0;
    }

    fn push_pre_roll(&mut self, chunk: Vec<u8>) {
        self.pre_roll_bytes += chunk.len();
        self.pre_roll.push_back(chunk);
        while self.pre_roll_bytes > self.pre_roll_limit_bytes && self.pre_roll.len() > 1 {
            if let Some(front) = self.pre_roll.pop_front() {
                self.pre_roll_bytes -= front.len();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME_SAMPLES: usize = 320; // 20ms @16k

    fn silence_frame() -> Vec<u8> {
        vec![0u8; FRAME_SAMPLES * 2]
    }

    fn tone_frame(amplitude: i16) -> Vec<u8> {
        let mut out = Vec::with_capacity(FRAME_SAMPLES * 2);
        for i in 0..FRAME_SAMPLES {
            let phase = 2.0 * std::f32::consts::PI * 440.0 * i as f32 / 16000.0;
            let sample = (phase.sin() * amplitude as f32) as i16;
            out.extend_from_slice(&sample.to_le_bytes());
        }
        out
    }

    fn silence_chunk() -> Vec<u8> {
        vec![0u8; 5 * FRAME_SAMPLES * 2] // 100ms
    }

    fn tone_chunk(amplitude: i16) -> Vec<u8> {
        let mut out = Vec::new();
        for _ in 0..5 {
            out.extend_from_slice(&tone_frame(amplitude));
        }
        out
    }

    fn is_silent(chunk: &[u8]) -> bool {
        chunk.iter().all(|byte| *byte == 0)
    }

    fn test_gate_config() -> GateConfig {
        GateConfig {
            vad: VadConfig {
                hangover_ms: 100,
                onset_frames: 2,
                ..Default::default()
            },
            pre_roll_ms: 100,
            tail_ms: 200,
            keepalive_ms: 500,
            debug: false,
        }
    }

    #[test]
    fn energy_vad_detects_tone_and_hangover() {
        let mut vad = EnergyVad::new(&VadConfig {
            hangover_ms: 100,
            onset_frames: 2,
            ..Default::default()
        });

        // 启动标定窗口 300ms（15 帧）内强制非语音
        for _ in 0..15 {
            assert!(!vad.process_frame(&silence_frame()));
        }

        // 起音需要连续 2 帧超过阈值
        assert!(!vad.process_frame(&tone_frame(3000)), "起音需要连续 2 帧");
        assert!(vad.process_frame(&tone_frame(3000)));
        assert!(vad.is_active());

        // hangover 100ms = 5 帧：前 4 帧仍 active，第 5 帧结束
        for _ in 0..4 {
            assert!(vad.process_frame(&silence_frame()));
        }
        assert!(
            !vad.process_frame(&silence_frame()),
            "超过 hangover 后应结束"
        );
        assert!(!vad.is_active());
    }

    #[test]
    fn energy_vad_ignores_short_click() {
        let mut vad = EnergyVad::new(&VadConfig {
            onset_frames: 2,
            ..Default::default()
        });
        for _ in 0..15 {
            assert!(!vad.process_frame(&silence_frame()));
        }
        // 单帧突发（20ms，低于 40ms 起音要求）不应触发
        assert!(!vad.process_frame(&tone_frame(8000)));
        for _ in 0..5 {
            assert!(!vad.process_frame(&silence_frame()));
        }
        assert!(!vad.is_active());
    }

    #[test]
    fn gate_gates_silence_and_sends_keepalive() {
        let mut gate = SpeechGate::new(test_gate_config());
        let mut emitted = 0;
        for _ in 0..20 {
            let out = gate.process_chunk(silence_chunk());
            assert!(out.iter().all(|chunk| is_silent(chunk)));
            emitted += out.len();
        }
        // 500ms 保活间隔：20 个 100ms chunk 应发 4 个保活静音
        assert_eq!(emitted, 4);
    }

    #[test]
    fn gate_sends_pre_roll_speech_hangover_and_tail() {
        let mut gate = SpeechGate::new(test_gate_config());
        let mut per_chunk = Vec::new();

        // 3 个静音 chunk：启动标定，不发送
        for _ in 0..3 {
            per_chunk.push(gate.process_chunk(silence_chunk()).len());
        }

        // 2 个语音 chunk：第一个起音（补 pre-roll），第二个正常发送
        per_chunk.push(gate.process_chunk(tone_chunk(3000)).len());
        per_chunk.push(gate.process_chunk(tone_chunk(3000)).len());

        // 1 个静音 chunk：仍在 hangover 内，按语音发送
        per_chunk.push(gate.process_chunk(silence_chunk()).len());
        // 2 个静音 chunk：进入 tail
        per_chunk.push(gate.process_chunk(silence_chunk()).len());
        per_chunk.push(gate.process_chunk(silence_chunk()).len());
        // 4 个静音 chunk：tail 结束后的空闲，未到 500ms 保活
        for _ in 0..4 {
            per_chunk.push(gate.process_chunk(silence_chunk()).len());
        }
        // 第 5 个空闲 chunk 触发保活
        per_chunk.push(gate.process_chunk(silence_chunk()).len());

        assert_eq!(
            per_chunk,
            vec![0, 0, 0, 2, 1, 1, 1, 1, 0, 0, 0, 0, 1],
            "pre-roll/hangover/tail/keepalive 发送节奏"
        );
    }

    #[test]
    fn gate_recovers_speech_during_tail() {
        let mut gate = SpeechGate::new(test_gate_config());
        // 启动标定
        for _ in 0..3 {
            gate.process_chunk(silence_chunk());
        }
        // 一句语音
        gate.process_chunk(tone_chunk(3000));
        // tail 期间（第 1 个静音 chunk）
        let tail_out = gate.process_chunk(silence_chunk());
        assert_eq!(tail_out.len(), 1);
        assert!(is_silent(&tail_out[0]));

        // tail 期间重新开口：应恢复发送真实音频而不是继续丢弃
        let speech_out = gate.process_chunk(tone_chunk(3000));
        assert!(!speech_out.is_empty());
        assert!(
            speech_out.iter().any(|chunk| !is_silent(chunk)),
            "tail 期间重新起音必须发送真实语音"
        );
    }

    #[test]
    fn reset_transient_clears_pending_state() {
        let mut gate = SpeechGate::new(test_gate_config());
        for _ in 0..3 {
            gate.process_chunk(silence_chunk());
        }
        gate.process_chunk(tone_chunk(3000));
        gate.reset_transient();
        // 重置后第一个静音 chunk 不应再发 tail
        let out = gate.process_chunk(silence_chunk());
        assert!(out.is_empty());
    }
}
