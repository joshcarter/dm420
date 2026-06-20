//! Short-time Fourier transform "waterfall" (port of ft8_lib `monitor.c`).
//!
//! Slides a Hann-windowed analysis frame across the slot audio, FFTs each
//! sub-block with [`crate::fft::Fft`] (realfft), and stores per-bin magnitudes as
//! `u8` (0.5 dB steps, like the reference) for the sync/demod stages. Layout of
//! `mag`: `[block][time_sub][freq_sub][bin]`, flattened.

use crate::fft::Fft;

/// FT8 or FT4 — selects the symbol/slot timing throughout the pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Ft8,
    Ft4,
}

impl Protocol {
    pub fn symbol_period(self) -> f32 {
        match self {
            Protocol::Ft8 => 0.160,
            Protocol::Ft4 => 0.048,
        }
    }
    pub fn slot_time(self) -> f32 {
        match self {
            Protocol::Ft8 => 15.0,
            Protocol::Ft4 => 7.5,
        }
    }
    /// Number of FSK tones (8 for FT8, 4 for FT4).
    pub fn num_tones(self) -> usize {
        match self {
            Protocol::Ft8 => 8,
            Protocol::Ft4 => 4,
        }
    }
}

/// Magnitude in dB recovered from the stored u8 (inverse of the 2·db+240 scale).
#[inline]
pub fn mag_db(v: u8) -> f32 {
    v as f32 * 0.5 - 120.0
}

/// Accumulated time/frequency magnitude grid for one slot.
pub struct Waterfall {
    pub protocol: Protocol,
    pub time_osr: usize,
    pub freq_osr: usize,
    pub num_bins: usize,
    pub block_stride: usize,
    pub num_blocks: usize,
    pub max_blocks: usize,
    pub min_bin: usize,
    pub symbol_period: f32,
    pub mag: Vec<u8>,
}

/// Streaming STFT processor — feed it `block_size` samples at a time.
pub struct Monitor {
    nfft: usize,
    pub block_size: usize,
    subblock_size: usize,
    min_bin: usize,
    max_bin: usize,
    window: Vec<f32>,
    last_frame: Vec<f32>,
    fft: Fft,
    pub wf: Waterfall,
    pub max_mag: f32,
}

fn hann(i: usize, n: usize) -> f32 {
    let x = (std::f32::consts::PI * i as f32 / n as f32).sin();
    x * x
}

impl Monitor {
    pub fn new(
        sample_rate: u32,
        protocol: Protocol,
        time_osr: usize,
        freq_osr: usize,
        f_min: f32,
        f_max: f32,
    ) -> Monitor {
        let symbol_period = protocol.symbol_period();
        let block_size = (sample_rate as f32 * symbol_period) as usize;
        let subblock_size = block_size / time_osr;
        let nfft = block_size * freq_osr;
        let fft_norm = 2.0 / nfft as f32;
        let window: Vec<f32> = (0..nfft).map(|i| fft_norm * hann(i, nfft)).collect();

        let max_blocks = (protocol.slot_time() / symbol_period) as usize;
        let min_bin = (f_min * symbol_period) as usize;
        let max_bin = (f_max * symbol_period) as usize + 1;
        let num_bins = max_bin - min_bin;
        let block_stride = time_osr * freq_osr * num_bins;

        Monitor {
            nfft,
            block_size,
            subblock_size,
            min_bin,
            max_bin,
            window,
            last_frame: vec![0.0; nfft],
            fft: Fft::new(nfft),
            wf: Waterfall {
                protocol,
                time_osr,
                freq_osr,
                num_bins,
                block_stride,
                num_blocks: 0,
                max_blocks,
                min_bin,
                symbol_period,
                mag: vec![0u8; max_blocks * block_stride],
            },
            max_mag: -120.0,
        }
    }

    /// Process one symbol's worth (`block_size`) of audio.
    pub fn process(&mut self, frame: &[f32]) {
        if self.wf.num_blocks >= self.wf.max_blocks {
            return;
        }
        let mut offset = self.wf.num_blocks * self.wf.block_stride;
        let mut frame_pos = 0;
        let mut timedata = vec![0.0f32; self.nfft];
        let mut re = vec![0.0f32; self.nfft];
        let mut im = vec![0.0f32; self.nfft];

        for _time_sub in 0..self.wf.time_osr {
            // Shift in the next sub-block of samples.
            self.last_frame
                .copy_within(self.subblock_size..self.nfft, 0);
            for pos in (self.nfft - self.subblock_size)..self.nfft {
                self.last_frame[pos] = frame[frame_pos];
                frame_pos += 1;
            }
            for (pos, td) in timedata.iter_mut().enumerate() {
                *td = self.window[pos] * self.last_frame[pos];
            }
            self.fft.forward_real(&timedata, &mut re, &mut im);

            for freq_sub in 0..self.wf.freq_osr {
                for bin in self.min_bin..self.max_bin {
                    let src = bin * self.wf.freq_osr + freq_sub;
                    let mag2 = re[src] * re[src] + im[src] * im[src];
                    let db = 10.0 * (1e-12 + mag2).log10();
                    let scaled = (2.0 * db + 240.0) as i32;
                    self.wf.mag[offset] = scaled.clamp(0, 255) as u8;
                    offset += 1;
                    if db > self.max_mag {
                        self.max_mag = db;
                    }
                }
            }
        }
        self.wf.num_blocks += 1;
    }
}
