use anyhow::{Context, Result};
use audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Resampler};

pub(crate) struct MonoResampler {
    pub resampler: Fft<f32>,
    pub in_frames: usize,
    pub out_frames: usize,
    pub input_fifo: Vec<f32>,
    pub out_buf: Vec<f32>,
}

impl MonoResampler {
    pub fn new(in_sample_rate: u32, out_sample_rate: u32) -> Result<Option<Self>> {
        if in_sample_rate == out_sample_rate {
            return Ok(None);
        }

        let resampler = Fft::<f32>::new(
            in_sample_rate as usize,
            out_sample_rate as usize,
            512,
            1,
            FixedSync::Both,
        )
        .context("初始化音频重采样器失败")?;
        let in_frames = resampler.input_frames_next();
        let out_frames = resampler.output_frames_next();

        Ok(Some(Self {
            resampler,
            in_frames,
            out_frames,
            input_fifo: Vec::with_capacity(in_frames * 4),
            out_buf: vec![0.0f32; out_frames],
        }))
    }

    pub fn process_available(&mut self, mut on_sample: impl FnMut(i16)) {
        while self.input_fifo.len() >= self.in_frames {
            if let (Ok(in_adapter), Ok(mut out_adapter)) = (
                InterleavedSlice::new(&self.input_fifo[..self.in_frames], 1, self.in_frames),
                InterleavedSlice::new_mut(&mut self.out_buf[..self.out_frames], 1, self.out_frames),
            ) {
                if self
                    .resampler
                    .process_into_buffer(&in_adapter, &mut out_adapter, None)
                    .is_ok()
                {
                    for &s in &self.out_buf[..self.out_frames] {
                        let clamped = s.clamp(-1.0, 1.0);
                        on_sample((clamped * 32767.0) as i16);
                    }
                }
            }
            self.input_fifo.drain(0..self.in_frames);
        }
    }
}

