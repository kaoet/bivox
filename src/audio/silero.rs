//! Silero VAD 的 ONNX Runtime 封装。
//!
//! 模型文件在 `assets/silero/silero_vad_16k_op15.onnx`（MIT，来源与校验和见该目录 README），
//! 编译期内嵌进 exe，运行时通过 ONNX Runtime 从内存加载。
//!
//! 接口契约（P0 实测，见 docs/architecture.md §10.3）：
//! - 采样率 16000 Hz；每帧恰好 512 采样（32ms）；
//! - 输入 `input` f32 `[1,576]`（64 采样 context + 512 采样新帧）、`state` f32 `[2,1,128]`、
//!   `sr` i64 标量（16000）；
//! - 输出 `output` f32 `[1,1]`（语音概率）、`stateN` f32 `[2,1,128]`（下一帧 state）。

use std::sync::OnceLock;

use anyhow::{Result, anyhow};
use ort::environment::Environment;
use ort::session::{Session, builder::GraphOptimizationLevel};
use ort::value::Tensor;

use super::vad::FrameDetector;

/// 内嵌的 Silero 16k 模型（1.23MiB）。
const MODEL: &[u8] = include_bytes!("../../assets/silero/silero_vad_16k_op15.onnx");
const SAMPLE_RATE: i64 = 16_000;
const FRAME_SAMPLES: usize = 512;
const CONTEXT_SAMPLES: usize = 64;
const STATE_LEN: usize = 2 * 128;

// 编译期自检：模型文件必须存在且大小合理。
const _: () = assert!(MODEL.len() > 1_000_000);

/// ONNX Runtime 全局环境（进程内只初始化一次）。
static ENV: OnceLock<Environment> = OnceLock::new();

fn environment() -> Result<&'static Environment> {
    if ENV.get().is_none() {
        let env = ort::init()
            .build()
            .map_err(|e| anyhow!("初始化 ONNX Runtime 失败: {e}"))?;
        // 竞态时保留先到的环境即可（应用只在会话启动时创建一次）。
        let _ = ENV.set(env);
    }
    ENV.get().ok_or_else(|| anyhow!("ONNX Runtime 环境不可用"))
}

/// Silero VAD 帧级检测器：维护 LSTM state 与 64 采样 context。
pub struct SileroDetector {
    session: Session,
    state: Vec<f32>,
    context: Vec<f32>,
}

impl SileroDetector {
    /// 加载内嵌模型并创建推断会话。
    pub fn new() -> Result<Self> {
        let env = environment()?;
        let builder =
            Session::builder(env).map_err(|e| anyhow!("创建 ONNX Runtime 会话失败: {e}"))?;
        let builder = builder
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| anyhow!("设置 ONNX Runtime 优化级别失败: {e}"))?;
        let mut builder = builder
            .with_intra_threads(1)
            .map_err(|e| anyhow!("设置 ONNX Runtime 线程数失败: {e}"))?;
        let session = builder
            .commit_from_memory(MODEL)
            .map_err(|e| anyhow!("加载 Silero 模型失败: {e}"))?;
        Ok(Self {
            session,
            state: vec![0.0; STATE_LEN],
            context: vec![0.0; CONTEXT_SAMPLES],
        })
    }

    fn infer(&mut self, frame: &[i16]) -> Result<f32> {
        let mut input = vec![0.0f32; CONTEXT_SAMPLES + FRAME_SAMPLES];
        input[..CONTEXT_SAMPLES].copy_from_slice(&self.context);
        for (dst, &sample) in input[CONTEXT_SAMPLES..].iter_mut().zip(frame.iter()) {
            *dst = sample as f32 / 32768.0;
        }

        let audio_t = Tensor::from_array((
            vec![1i64, (CONTEXT_SAMPLES + FRAME_SAMPLES) as i64],
            input.clone(),
        ))?;
        let state_t = Tensor::from_array((vec![2i64, 1, 128], self.state.clone()))?;
        let sr_t = Tensor::from_array((Vec::<i64>::new(), vec![SAMPLE_RATE]))?;
        let outputs = self.session.run(ort::inputs![
            "input" => audio_t,
            "sr" => sr_t,
            "state" => state_t
        ])?;

        let (_, prob) = outputs["output"].try_extract_tensor::<f32>()?;
        let probability = prob
            .first()
            .copied()
            .ok_or_else(|| anyhow!("Silero 输出为空"))?;

        let (_, next_state) = outputs["stateN"].try_extract_tensor::<f32>()?;
        if next_state.len() != STATE_LEN {
            return Err(anyhow!(
                "Silero state 形状异常：期望 {STATE_LEN}，实际 {}",
                next_state.len()
            ));
        }
        self.state.copy_from_slice(next_state);
        // 下一帧的 context = 本帧最后 64 个采样。
        self.context
            .copy_from_slice(&input[FRAME_SAMPLES..CONTEXT_SAMPLES + FRAME_SAMPLES]);

        Ok(probability.clamp(0.0, 1.0))
    }
}

impl FrameDetector for SileroDetector {
    fn frame_samples(&self) -> usize {
        FRAME_SAMPLES
    }

    fn process_frame(&mut self, frame: &[i16]) -> Result<f32> {
        if frame.len() != FRAME_SAMPLES {
            return Err(anyhow!(
                "Silero 帧长错误：期望 {FRAME_SAMPLES} 采样，实际 {}",
                frame.len()
            ));
        }
        self.infer(frame)
    }

    fn reset(&mut self) {
        self.state.fill(0.0);
        self.context.fill(0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 手动冒烟：真实加载内嵌模型，确认 ONNX Runtime 链路可用。
    /// 运行：`cargo test --lib silero_detector_loads -- --ignored`
    #[test]
    #[ignore = "加载真实模型较慢，属于发布前/P4 手动冒烟"]
    fn silero_detector_loads_and_scores_silence() -> Result<()> {
        let mut detector = SileroDetector::new()?;
        let silence = vec![0i16; FRAME_SAMPLES];
        let mut max = 0.0f32;
        for _ in 0..10 {
            max = max.max(detector.process_frame(&silence)?);
        }
        assert!(max < 0.1, "静音概率应很低，实际最大 {max:.3}");
        Ok(())
    }
}
