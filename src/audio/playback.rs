use super::resampler::MonoResampler;
use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{Device, SampleFormat, Stream};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct AudioPlaybackHandle {
    sample_queue: Arc<Mutex<VecDeque<i16>>>,
    resampler: Option<Arc<Mutex<MonoResampler>>>,
    interrupt_flag: Arc<AtomicBool>,
}

impl AudioPlaybackHandle {
    pub fn push_pcm16(&self, bytes: &[u8]) {
        if let Some(ref resampler_mutex) = self.resampler {
            let mut resampler = resampler_mutex.lock().unwrap();
            for chunk in bytes.as_chunks::<2>().0 {
                let sample = i16::from_le_bytes(*chunk);
                resampler.input_fifo.push(sample as f32 / 32768.0);
            }

            let mut produced = Vec::new();
            resampler.process_available(|s16| produced.push(s16));
            drop(resampler);

            if !produced.is_empty() {
                let mut queue = self.sample_queue.lock().unwrap();
                queue.extend(produced);
            }
        } else {
            let mut queue = self.sample_queue.lock().unwrap();
            for chunk in bytes.as_chunks::<2>().0 {
                let sample = i16::from_le_bytes(*chunk);
                queue.push_back(sample);
            }
        }
    }

    #[allow(dead_code)] // 预留：播放中断；当前由 drop 播放引擎停止，暂无调用方
    pub fn interrupt(&self) {
        self.interrupt_flag.store(true, Ordering::Relaxed);
        if let Ok(mut queue) = self.sample_queue.lock() {
            queue.clear();
        }
        if let Some(ref resampler_mutex) = self.resampler {
            if let Ok(mut resampler) = resampler_mutex.lock() {
                resampler.input_fifo.clear();
            }
        }
    }

    pub fn flush(&self) {
        if let Some(ref resampler_mutex) = self.resampler {
            let mut resampler = resampler_mutex.lock().unwrap();
            if !resampler.input_fifo.is_empty() {
                let in_frames = resampler.in_frames;
                resampler.input_fifo.resize(in_frames, 0.0);
                let mut produced = Vec::new();
                resampler.process_available(|s16| produced.push(s16));
                drop(resampler);

                if !produced.is_empty() {
                    let mut queue = self.sample_queue.lock().unwrap();
                    queue.extend(produced);
                }
            }
        }
    }
}

#[cfg(test)]
impl AudioPlaybackHandle {
    /// 测试用：不依赖声卡的空句柄
    pub fn for_test() -> Self {
        Self {
            sample_queue: Arc::new(Mutex::new(VecDeque::new())),
            resampler: None,
            interrupt_flag: Arc::new(AtomicBool::new(false)),
        }
    }

    /// 测试用：当前播放队列中已排队的样本数
    pub fn queued_samples_len(&self) -> usize {
        self.sample_queue.lock().unwrap().len()
    }
}

pub struct AudioPlaybackEngine {
    _stream: Stream,
    handle: AudioPlaybackHandle,
}

impl AudioPlaybackEngine {
    pub fn new(device: &Device, input_sample_rate: u32) -> Result<Self> {
        let supported_config = device
            .default_output_config()
            .context("获取默认输出音频配置失败")?;

        let native_sample_rate = supported_config.sample_rate();
        let channels = supported_config.channels() as usize;
        let sample_format = supported_config.sample_format();

        let sample_queue = Arc::new(Mutex::new(VecDeque::<i16>::with_capacity(48000 * 2)));
        let queue_clone = sample_queue.clone();

        let interrupt_flag = Arc::new(AtomicBool::new(false));
        let interrupt_clone = interrupt_flag.clone();

        let resampler = MonoResampler::new(input_sample_rate, native_sample_rate)?
            .map(|r| Arc::new(Mutex::new(r)));

        let err_fn = |err| log::warn!("扬声器输出流异常: {:?}", err);

        let stream = match sample_format {
            SampleFormat::F32 => device.build_output_stream(
                supported_config.config(),
                move |data: &mut [f32], _| {
                    let mut queue = queue_clone.lock().unwrap();

                    if interrupt_clone.swap(false, Ordering::Relaxed) {
                        queue.clear();
                    }

                    for frame in data.chunks_mut(channels) {
                        let sample_f32 = if let Some(s) = queue.pop_front() {
                            s as f32 / 32768.0
                        } else {
                            0.0
                        };
                        for channel_sample in frame.iter_mut() {
                            *channel_sample = sample_f32;
                        }
                    }
                },
                err_fn,
                None,
            )?,
            SampleFormat::I16 => device.build_output_stream(
                supported_config.config(),
                move |data: &mut [i16], _| {
                    let mut queue = queue_clone.lock().unwrap();

                    if interrupt_clone.swap(false, Ordering::Relaxed) {
                        queue.clear();
                    }

                    for frame in data.chunks_mut(channels) {
                        let s16_sample = queue.pop_front().unwrap_or(0);
                        for channel_sample in frame.iter_mut() {
                            *channel_sample = s16_sample;
                        }
                    }
                },
                err_fn,
                None,
            )?,
            _ => {
                return Err(anyhow::anyhow!(
                    "暂不支持的输出音频格式: {:?}",
                    sample_format
                ))
            }
        };

        stream.play().context("启动扬声器输出流失败")?;

        let handle = AudioPlaybackHandle {
            sample_queue,
            resampler,
            interrupt_flag,
        };

        Ok(Self {
            _stream: stream,
            handle,
        })
    }

    pub fn handle(&self) -> AudioPlaybackHandle {
        self.handle.clone()
    }
}
