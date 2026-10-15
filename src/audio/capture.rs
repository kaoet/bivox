use anyhow::{Context, Result};
use audioadapter_buffers::direct::InterleavedSlice;
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{Device, SampleFormat, Stream};
use rubato::{Fft, FixedSync, Resampler};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc::Sender;

#[allow(dead_code)]
pub struct AudioCapture {
    _stream: Stream,
    is_capturing: Arc<AtomicBool>,
}

struct CaptureResampler {
    resampler: Fft<f32>,
    in_frames: usize,
    out_frames: usize,
    input_fifo: Vec<f32>,
    out_buf: Vec<f32>,
}

fn create_capture_resampler(
    native_sample_rate: u32,
    target_sample_rate: u32,
) -> Result<Option<CaptureResampler>> {
    if native_sample_rate != target_sample_rate {
        let resampler = Fft::<f32>::new(
            native_sample_rate as usize,
            target_sample_rate as usize,
            512,
            1,
            FixedSync::Both,
        )
        .context("初始化麦克风音频重采样器失败")?;
        let in_frames = resampler.input_frames_next();
        let out_frames = resampler.output_frames_next();
        Ok(Some(CaptureResampler {
            resampler,
            in_frames,
            out_frames,
            input_fifo: Vec::with_capacity(in_frames * 4),
            out_buf: vec![0.0f32; out_frames],
        }))
    } else {
        Ok(None)
    }
}

impl AudioCapture {
    pub fn start(device: &Device, tx_pcm_16k: Sender<Vec<u8>>) -> Result<Self> {
        let supported_config = device
            .default_input_config()
            .context("获取默认输入配置失败")?;

        let native_sample_rate = supported_config.sample_rate().0;
        let channels = supported_config.channels();
        let sample_format = supported_config.sample_format();

        let is_capturing = Arc::new(AtomicBool::new(true));
        let capturing_flag = is_capturing.clone();

        // 目标为 16000Hz 16-bit PCM 单声道
        let target_sample_rate = 16000u32;
        let mut pcm_buffer = Vec::<i16>::with_capacity(3200);

        let err_fn = |err| eprintln!("麦克风输入流异常: {:?}", err);

        let stream = match sample_format {
            SampleFormat::F32 => {
                let mut channel_sum = 0.0f32;
                let mut channel_count = 0u16;
                let mut resampler_state =
                    create_capture_resampler(native_sample_rate, target_sample_rate)?;

                device.build_input_stream(
                    &supported_config.into(),
                    move |data: &[f32], _| {
                        if !capturing_flag.load(Ordering::Relaxed) {
                            return;
                        }

                        for &sample in data {
                            channel_sum += sample;
                            channel_count += 1;

                            if channel_count == channels {
                                let mono_sample = channel_sum / channels as f32;
                                channel_sum = 0.0;
                                channel_count = 0;

                                if let Some(ref mut state) = resampler_state {
                                    state.input_fifo.push(mono_sample);
                                    while state.input_fifo.len() >= state.in_frames {
                                        if let (Ok(in_adapter), Ok(mut out_adapter)) = (
                                            InterleavedSlice::new(
                                                &state.input_fifo[..state.in_frames],
                                                1,
                                                state.in_frames,
                                            ),
                                            InterleavedSlice::new_mut(
                                                &mut state.out_buf[..state.out_frames],
                                                1,
                                                state.out_frames,
                                            ),
                                        ) {
                                            if state
                                                .resampler
                                                .process_into_buffer(
                                                    &in_adapter,
                                                    &mut out_adapter,
                                                    None,
                                                )
                                                .is_ok()
                                            {
                                                for &s in &state.out_buf[..state.out_frames] {
                                                    let clamped = s.clamp(-1.0, 1.0);
                                                    let s16 = (clamped * 32767.0) as i16;
                                                    pcm_buffer.push(s16);
                                                }
                                            }
                                        }
                                        state.input_fifo.drain(0..state.in_frames);
                                    }
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
                                    let _ = tx_pcm_16k.try_send(bytes);
                                    pcm_buffer.clear();
                                }
                            }
                        }
                    },
                    err_fn,
                    None,
                )?
            }
            SampleFormat::I16 => {
                let mut channel_sum = 0i32;
                let mut channel_count = 0u16;
                let mut resampler_state =
                    create_capture_resampler(native_sample_rate, target_sample_rate)?;

                device.build_input_stream(
                    &supported_config.into(),
                    move |data: &[i16], _| {
                        if !capturing_flag.load(Ordering::Relaxed) {
                            return;
                        }

                        for &sample in data {
                            channel_sum += sample as i32;
                            channel_count += 1;

                            if channel_count == channels {
                                let mono_sample = channel_sum as f32 / (channels as f32 * 32768.0);
                                channel_sum = 0;
                                channel_count = 0;

                                if let Some(ref mut state) = resampler_state {
                                    state.input_fifo.push(mono_sample);
                                    while state.input_fifo.len() >= state.in_frames {
                                        if let (Ok(in_adapter), Ok(mut out_adapter)) = (
                                            InterleavedSlice::new(
                                                &state.input_fifo[..state.in_frames],
                                                1,
                                                state.in_frames,
                                            ),
                                            InterleavedSlice::new_mut(
                                                &mut state.out_buf[..state.out_frames],
                                                1,
                                                state.out_frames,
                                            ),
                                        ) {
                                            if state
                                                .resampler
                                                .process_into_buffer(
                                                    &in_adapter,
                                                    &mut out_adapter,
                                                    None,
                                                )
                                                .is_ok()
                                            {
                                                for &s in &state.out_buf[..state.out_frames] {
                                                    let clamped = s.clamp(-1.0, 1.0);
                                                    let s16 = (clamped * 32767.0) as i16;
                                                    pcm_buffer.push(s16);
                                                }
                                            }
                                        }
                                        state.input_fifo.drain(0..state.in_frames);
                                    }
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
                                    let _ = tx_pcm_16k.try_send(bytes);
                                    pcm_buffer.clear();
                                }
                            }
                        }
                    },
                    err_fn,
                    None,
                )?
            }
            _ => {
                return Err(anyhow::anyhow!(
                    "暂不支持的声卡采样格式: {:?}",
                    sample_format
                ))
            }
        };

        stream.play().context("启动麦克风输入流失败")?;

        Ok(Self {
            _stream: stream,
            is_capturing,
        })
    }

    #[allow(dead_code)]
    pub fn stop(&self) {
        self.is_capturing.store(false, Ordering::Relaxed);
    }
}
