use super::resampler::MonoResampler;
use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{Device, SampleFormat, Stream};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::mpsc::Sender;
use tokio::sync::mpsc::error::TrySendError;

pub struct AudioCapture {
    _stream: Stream,
    dropped_chunks: Arc<AtomicU64>,
}

impl AudioCapture {
    pub fn start(device: &Device, tx_pcm_16k: Sender<Vec<u8>>) -> Result<Self> {
        let supported_config = device
            .default_input_config()
            .context("获取默认输入配置失败")?;

        let native_sample_rate = supported_config.sample_rate();
        let channels = supported_config.channels();
        let sample_format = supported_config.sample_format();

        let dropped_chunks = Arc::new(AtomicU64::new(0));
        let dropped_flag = dropped_chunks.clone();

        // 目标为 16000Hz 16-bit PCM 单声道
        let target_sample_rate = 16000u32;
        let mut pcm_buffer = Vec::<i16>::with_capacity(3200);
        let mut resampler_state = MonoResampler::new(native_sample_rate, target_sample_rate)?;

        let mut process_mono = move |mono_sample: f32| {
            if let Some(ref mut state) = resampler_state {
                state.input_fifo.push(mono_sample);
                state.process_available(|s16| pcm_buffer.push(s16));
            } else {
                let clamped = mono_sample.clamp(-1.0, 1.0);
                let s16 = (clamped * 32767.0) as i16;
                pcm_buffer.push(s16);
            }

            if pcm_buffer.len() >= 1600 {
                let mut bytes = Vec::with_capacity(pcm_buffer.len() * 2);
                for &v in &pcm_buffer {
                    bytes.extend_from_slice(&v.to_le_bytes());
                }
                // 通道满说明下游积压已超过容量，这段音频确实丢了，计数供排障查看；
                // 接收端已关闭属于正常收尾，不计数。
                if let Err(TrySendError::Full(_)) = tx_pcm_16k.try_send(bytes) {
                    dropped_flag.fetch_add(1, Ordering::Relaxed);
                }
                pcm_buffer.clear();
            }
        };

        let err_fn = |err| log::warn!("麦克风输入流异常: {:?}", err);

        let stream = match sample_format {
            SampleFormat::F32 => device.build_input_stream(
                supported_config.config(),
                move |data: &[f32], _| {
                    for frame in data.chunks_exact(channels as usize) {
                        let channel_sum: f32 = frame.iter().copied().sum();
                        let mono_sample = channel_sum / channels as f32;
                        process_mono(mono_sample);
                    }
                },
                err_fn,
                None,
            )?,
            SampleFormat::I16 => device.build_input_stream(
                supported_config.config(),
                move |data: &[i16], _| {
                    for frame in data.chunks_exact(channels as usize) {
                        let channel_sum: i32 = frame.iter().map(|&s| s as i32).sum();
                        let mono_sample = channel_sum as f32 / (channels as f32 * 32768.0);
                        process_mono(mono_sample);
                    }
                },
                err_fn,
                None,
            )?,
            _ => {
                return Err(anyhow::anyhow!(
                    "暂不支持的声卡采样格式: {:?}",
                    sample_format
                ));
            }
        };

        stream.play().context("启动麦克风输入流失败")?;

        Ok(Self {
            _stream: stream,
            dropped_chunks,
        })
    }

    /// 因通道满（下游积压）被丢弃的 100ms 音频块数；正常收尾导致的关闭不计入。
    pub fn dropped_chunks(&self) -> u64 {
        self.dropped_chunks.load(Ordering::Relaxed)
    }
}
