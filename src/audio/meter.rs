//! 顶栏电平条：PCM16 RMS → dBFS → 0..1，以及 GUI 侧的快起慢落平滑。
//!
//! 只做纯计算，不依赖 egui 或音频设备，方便单测（D43）。
//!
//! - 输入是 16 kHz / 单声道 / PCM16 的 100 ms 块；
//! - `-60 dBFS` 映射为 0，`0 dBFS` 映射为 1；
//! - GUI 用 [`MeterSmoother`] 做快起慢落，避免 10 Hz 更新一格一格跳。

/// 归一化下限：低于该电平一律视为 0。
pub const FLOOR_DBFS: f32 = -60.0;

/// 起音时间常数（秒）：说话后电平条快速亮起。
const ATTACK_TAU: f32 = 0.040;
/// 释放时间常数（秒）：说完后慢慢回落。
const RELEASE_TAU: f32 = 0.220;

/// 计算一块 PCM16 的归一化电平（0..1）。
///
/// 末尾不完整的字节会被忽略；空块返回 0。
pub fn pcm16_percent(bytes: &[u8]) -> f32 {
    let mut sum = 0.0_f64;
    let mut count = 0_u32;
    for chunk in bytes.as_chunks::<2>().0 {
        let sample = i16::from_le_bytes([chunk[0], chunk[1]]) as f64 / 32768.0;
        sum += sample * sample;
        count += 1;
    }
    if count == 0 {
        return 0.0;
    }
    let rms = (sum / f64::from(count)).sqrt() as f32;
    rms_to_percent(rms)
}

/// RMS（0..1 满幅）转归一化电平（0..1）。
pub fn rms_to_percent(rms: f32) -> f32 {
    if !rms.is_finite() || rms <= 0.0 {
        return 0.0;
    }
    let dbfs = 20.0 * rms.log10();
    ((dbfs - FLOOR_DBFS) / -FLOOR_DBFS).clamp(0.0, 1.0)
}

/// 归一化电平转 dBFS（只用于 tooltip；0 电平返回 `None`）。
pub fn percent_to_dbfs(percent: f32) -> Option<f32> {
    (percent > 0.0005).then_some(percent * -FLOOR_DBFS + FLOOR_DBFS)
}

/// GUI 侧电平平滑：起音快、释放慢。
#[derive(Debug, Clone, Copy, Default)]
pub struct MeterSmoother {
    level: f32,
}

impl MeterSmoother {
    pub fn level(&self) -> f32 {
        self.level
    }

    /// 直接归零（会话结束/线路关闭时调用）。
    pub fn reset(&mut self) {
        self.level = 0.0;
    }

    /// 用 `dt` 秒向 `target` 逼近，返回新电平。
    pub fn update(&mut self, target: f32, dt: f32) -> f32 {
        let target = target.clamp(0.0, 1.0);
        let dt = dt.clamp(0.0, 0.25);
        if dt <= 0.0 {
            return self.level;
        }
        let tau = if target > self.level {
            ATTACK_TAU
        } else {
            RELEASE_TAU
        };
        let alpha = 1.0 - (-dt / tau).exp();
        self.level += (target - self.level) * alpha;
        if self.level < 0.0005 {
            self.level = 0.0;
        }
        self.level
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm16(sample: i16, count: usize) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(count * 2);
        for _ in 0..count {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn silence_is_zero() {
        assert_eq!(pcm16_percent(&[]), 0.0);
        assert_eq!(pcm16_percent(&pcm16(0, 1600)), 0.0);
        assert_eq!(pcm16_percent(&[0]), 0.0, "不完整样本应被忽略");
    }

    #[test]
    fn full_scale_square_is_one() {
        let level = pcm16_percent(&pcm16(i16::MAX, 1600));
        assert!(
            (level - 1.0).abs() < 0.001,
            "满幅方波应接近 1，实际 {level}"
        );
    }

    #[test]
    fn half_scale_square_maps_to_about_0_9() {
        let level = pcm16_percent(&pcm16(16384, 1600));
        assert!(
            (level - 0.9).abs() < 0.002,
            "半幅方波应接近 0.9，实际 {level}"
        );
    }

    #[test]
    fn odd_trailing_byte_is_ignored() {
        let mut bytes = pcm16(16384, 100);
        bytes.push(0x7f);
        let level = pcm16_percent(&bytes);
        assert!((level - 0.9).abs() < 0.002);
    }

    #[test]
    fn percent_to_dbfs_round_trips() {
        let level = rms_to_percent(0.1);
        let dbfs = percent_to_dbfs(level).expect("非零电平应有 dBFS");
        assert!(
            (dbfs + 20.0).abs() < 0.01,
            "-20 dBFS 应回算为 -20，实际 {dbfs}"
        );
    }

    #[test]
    fn smoother_attacks_faster_than_it_releases() {
        let mut smoother = MeterSmoother::default();
        let up = smoother.update(1.0, 0.04);
        assert!(up > 0.5, "40ms 起音应超过一半，实际 {up}");
        let down = smoother.update(0.0, 0.04);
        assert!(down > 0.0 && down < up, "释放应下降但不立即归零");
        for _ in 0..100 {
            smoother.update(0.0, 0.04);
        }
        assert_eq!(smoother.level(), 0.0);
    }

    #[test]
    fn smoother_handles_zero_dt() {
        let mut smoother = MeterSmoother::default();
        assert_eq!(smoother.update(1.0, 0.0), 0.0);
        smoother.reset();
        assert_eq!(smoother.level(), 0.0);
    }
}
