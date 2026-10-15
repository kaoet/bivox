use anyhow::{Context, Result};
use audioadapter_buffers::direct::InterleavedSlice;
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{Device, SampleFormat, Stream};
use rubato::{Fft, FixedSync, Resampler};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

struct PlaybackResampler {
    resampler: Fft<f32>,
    in_frames: usize,
    out_frames: usize,
    input_fifo: Vec<f32>,
    out_buf: Vec<f32>,
}

#[derive(Clone)]
pub struct AudioPlaybackHandle {
    sample_queue: Arc<Mutex<VecDeque<i16>>>,
    resampler: Arc<Mutex<Option<PlaybackResampler>>>,
    interrupt_flag: Arc<Mutex<bool>>,
}

impl AudioPlaybackHandle {
    pub fn push_pcm16(&self, bytes: &[u8]) {
        let mut resampler_guard = self.resampler.lock().unwrap();
        if let Some(ref mut resampler) = *resampler_guard {
            for chunk in bytes.chunks_exact(2) {
                let sample = i16::from_le_bytes([chunk[0], chunk[1]]);
                resampler.input_fifo.push(sample as f32 / 32768.0);
            }

            let mut produced = Vec::new();
            while resampler.input_fifo.len() >= resampler.in_frames {
                if let (Ok(in_adapter), Ok(mut out_adapter)) = (
                    InterleavedSlice::new(&resampler.input_fifo[..resampler.in_frames], 1, resampler.in_frames),
                    InterleavedSlice::new_mut(&mut resampler.out_buf[..resampler.out_frames], 1, resampler.out_frames),
                ) {
                    if resampler.resampler.process_into_buffer(&in_adapter, &mut out_adapter, None).is_ok() {
                        for &s in &resampler.out_buf[..resampler.out_frames] {
                            let clamped = s.clamp(-1.0, 1.0);
                            produced.push((clamped * 32767.0) as i16);
                        }
                    }
                }
                resampler.input_fifo.drain(0..resampler.in_frames);
            }
            drop(resampler_guard);

            if !produced.is_empty() {
                let mut queue = self.sample_queue.lock().unwrap();
                queue.extend(produced);
            }
        } else {
            drop(resampler_guard);
            let mut queue = self.sample_queue.lock().unwrap();
            for chunk in bytes.chunks_exact(2) {
                let sample = i16::from_le_bytes([chunk[0], chunk[1]]);
                queue.push_back(sample);
            }
        }
    }

    #[allow(dead_code)]
    pub fn interrupt(&self) {
        if let Ok(mut flag) = self.interrupt_flag.lock() {
            *flag = true;
        }
        if let Ok(mut queue) = self.sample_queue.lock() {
            queue.clear();
        }
        if let Ok(mut resampler_guard) = self.resampler.lock() {
            if let Some(ref mut resampler) = *resampler_guard {
                resampler.input_fifo.clear();
            }
        }
    }

    #[allow(dead_code)]
    pub fn flush(&self) {
        let mut resampler_guard = self.resampler.lock().unwrap();
        if let Some(ref mut resampler) = *resampler_guard {
            if !resampler.input_fifo.is_empty() {
                resampler.input_fifo.resize(resampler.in_frames, 0.0);
                let mut produced = Vec::new();
                if let (Ok(in_adapter), Ok(mut out_adapter)) = (
                    InterleavedSlice::new(&resampler.input_fifo[..resampler.in_frames], 1, resampler.in_frames),
                    InterleavedSlice::new_mut(&mut resampler.out_buf[..resampler.out_frames], 1, resampler.out_frames),
                ) {
                    if resampler.resampler.process_into_buffer(&in_adapter, &mut out_adapter, None).is_ok() {
                        for &s in &resampler.out_buf[..resampler.out_frames] {
                            let clamped = s.clamp(-1.0, 1.0);
                            produced.push((clamped * 32767.0) as i16);
                        }
                    }
                }
                resampler.input_fifo.clear();
                drop(resampler_guard);

                if !produced.is_empty() {
                    let mut queue = self.sample_queue.lock().unwrap();
                    queue.extend(produced);
                }
            }
        }
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

        let native_sample_rate = supported_config.sample_rate().0;
        let channels = supported_config.channels() as usize;
        let sample_format = supported_config.sample_format();

        let sample_queue = Arc::new(Mutex::new(VecDeque::<i16>::with_capacity(48000 * 2)));
        let queue_clone = sample_queue.clone();

        let interrupt_flag = Arc::new(Mutex::new(false));
        let interrupt_clone = interrupt_flag.clone();

        let resampler_state = if input_sample_rate != native_sample_rate {
            let resampler = Fft::<f32>::new(
                input_sample_rate as usize,
                native_sample_rate as usize,
                512,
                1,
                FixedSync::Both,
            )
            .context("初始化扬声器重采样器失败")?;
            let in_frames = resampler.input_frames_next();
            let out_frames = resampler.output_frames_next();
            Some(PlaybackResampler {
                resampler,
                in_frames,
                out_frames,
                input_fifo: Vec::with_capacity(in_frames * 4),
                out_buf: vec![0.0f32; out_frames],
            })
        } else {
            None
        };
        let resampler = Arc::new(Mutex::new(resampler_state));

        let err_fn = |err| eprintln!("扬声器输出流异常: {:?}", err);

        let stream = match sample_format {
            SampleFormat::F32 => device.build_output_stream(
                &supported_config.into(),
                move |data: &mut [f32], _| {
                    let mut queue = queue_clone.lock().unwrap();
                    let mut interrupted = interrupt_clone.lock().unwrap();

                    if *interrupted {
                        queue.clear();
                        *interrupted = false;
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
                &supported_config.into(),
                move |data: &mut [i16], _| {
                    let mut queue = queue_clone.lock().unwrap();
                    let mut interrupted = interrupt_clone.lock().unwrap();

                    if *interrupted {
                        queue.clear();
                        *interrupted = false;
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
