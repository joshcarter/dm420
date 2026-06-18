//! Uncompressed WAV playback. Loading/decoding is pure and tested; only the
//! actual output stream needs hardware. The player thread owns the cpal output
//! stream (cpal streams are `!Send`); the caller gets a handle with progress
//! counters and a stop channel.

use crate::AudioError;
use crate::device::{DeviceKind, open_cpal_device};
use crate::dsp::{Resampler, downmix_to_mono};
use cpal::traits::{DeviceTrait, StreamTrait};
use crossbeam_channel::{Receiver, Sender, bounded};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{debug, info, warn};

/// Load a WAV file as mono f32 samples plus its sample rate. Handles 16/24/32-bit
/// integer and 32-bit float, any channel count (downmixed by averaging).
pub fn load_wav_mono(path: &Path) -> Result<(Vec<f32>, u32), AudioError> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    let interleaved: Vec<f32> = match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Float, 32) => reader.samples::<f32>().collect::<Result<_, _>>()?,
        (hound::SampleFormat::Int, bits) if bits <= 32 => {
            let scale = (1i64 << (bits - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 / scale))
                .collect::<Result<_, _>>()?
        }
        (fmt, bits) => {
            return Err(AudioError::Unsupported(format!(
                "WAV format {fmt:?}/{bits}-bit"
            )));
        }
    };
    let mono = downmix_to_mono(&interleaved, spec.channels.max(1));
    Ok((mono, spec.sample_rate))
}

/// Handle to an in-progress playback.
pub struct Playback {
    /// Frames (at the *file* rate) played so far.
    pub progress: Arc<AtomicU64>,
    /// Total frames at the file rate.
    pub total_frames: u64,
    pub file_rate: u32,
    pub device_name: String,
    stop_tx: Sender<()>,
    done_rx: Receiver<()>,
    join: std::thread::JoinHandle<()>,
}

impl Playback {
    /// True once the file has finished playing (does not block).
    pub fn is_done(&self) -> bool {
        self.done_rx.try_recv().is_ok()
    }

    /// Block until playback completes or `stop()` is called elsewhere.
    pub fn wait(self) {
        let _ = self.done_rx.recv();
        let _ = self.stop_tx.send(());
        let _ = self.join.join();
    }

    /// A clone of the done-channel for select loops.
    pub fn done_receiver(&self) -> Receiver<()> {
        self.done_rx.clone()
    }

    /// Stop playback now.
    pub fn stop(self) {
        let _ = self.stop_tx.send(());
        let _ = self.join.join();
    }
}

/// Start playing mono samples (at `file_rate`) on the output device
/// (`None` = system default). Resamples to the device rate as needed and fans
/// the mono signal out to all device channels.
pub fn play(
    output_name: Option<String>,
    mono: Vec<f32>,
    file_rate: u32,
) -> Result<Playback, AudioError> {
    let total_frames = mono.len() as u64;
    let progress = Arc::new(AtomicU64::new(0));
    let progress2 = Arc::clone(&progress);
    let (stop_tx, stop_rx) = bounded::<()>(1);
    let (done_tx, done_rx) = bounded::<()>(1);
    let (ready_tx, ready_rx) = bounded::<Result<String, AudioError>>(1);

    let join = std::thread::Builder::new()
        .name("audio-play".into())
        .spawn(move || {
            playback_thread(
                output_name,
                mono,
                file_rate,
                progress2,
                stop_rx,
                done_tx,
                ready_tx,
            )
        })
        .expect("spawn audio-play");

    let device_name = match ready_rx.recv() {
        Ok(Ok(name)) => name,
        Ok(Err(e)) => {
            let _ = join.join();
            return Err(e);
        }
        Err(_) => return Err(AudioError::Stream("playback thread died on startup".into())),
    };

    Ok(Playback {
        progress,
        total_frames,
        file_rate,
        device_name,
        stop_tx,
        done_rx,
        join,
    })
}

fn playback_thread(
    output_name: Option<String>,
    mono: Vec<f32>,
    file_rate: u32,
    progress: Arc<AtomicU64>,
    stop_rx: Receiver<()>,
    done_tx: Sender<()>,
    ready_tx: Sender<Result<String, AudioError>>,
) {
    let setup = (|| {
        let device = open_cpal_device(DeviceKind::Output, output_name.as_deref())?;
        let name = device.name().unwrap_or_else(|_| "<unknown>".into());
        // Prefer the device's reported config; fall back to a common f32 layout for a
        // USB codec whose output configs cpal can't enumerate (it still streams fine).
        let (rate, channels) = match crate::device::resolve_output_config(&device) {
            Ok(cfg) => {
                if cfg.sample_format() != cpal::SampleFormat::F32 {
                    return Err(AudioError::Unsupported(format!(
                        "output sample format {:?} (only f32 supported)",
                        cfg.sample_format()
                    )));
                }
                (cfg.sample_rate().0, cfg.channels())
            }
            Err(_) => (48_000, 2),
        };
        Ok((device, name, rate, channels))
    })();

    let (device, name, device_rate, channels) = match setup {
        Ok(v) => v,
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            return;
        }
    };

    let stream_config = cpal::StreamConfig {
        channels,
        sample_rate: cpal::SampleRate(device_rate),
        buffer_size: cpal::BufferSize::Default,
    };
    let channels = channels as usize;
    debug!(device = %name, rate = device_rate, channels, "audio-tx: opening output stream");

    // Resample the whole file up front (memory is cheap at these sizes, and it
    // keeps the audio callback trivial). Track progress in *file* frames so the
    // UI can show position against the file duration.
    let ratio = file_rate as f64 / device_rate as f64;
    let samples: Vec<f32> = if file_rate == device_rate {
        mono
    } else {
        let mut r = Resampler::new(file_rate, device_rate);
        let mut out = r.push(&mono);
        out.extend(r.flush());
        out
    };

    let pos = Arc::new(AtomicU64::new(0));
    let pos_cb = Arc::clone(&pos);
    let samples = Arc::new(samples);
    let samples_cb = Arc::clone(&samples);
    let done_cb = done_tx.clone();
    let progress_cb = Arc::clone(&progress);
    let err_name = name.clone(); // for the stream-error callback (catches device dropouts)

    let stream = device.build_output_stream(
        &stream_config,
        move |out: &mut [f32], _| {
            let mut p = pos_cb.load(Ordering::Relaxed) as usize;
            for frame in out.chunks_mut(channels) {
                let v = samples_cb.get(p).copied().unwrap_or(0.0);
                for slot in frame.iter_mut() {
                    *slot = v;
                }
                if p < samples_cb.len() {
                    p += 1;
                }
            }
            pos_cb.store(p as u64, Ordering::Relaxed);
            progress_cb.store((p as f64 * ratio) as u64, Ordering::Relaxed);
            if p >= samples_cb.len() {
                let _ = done_cb.try_send(());
            }
        },
        move |e| warn!(device = %err_name, error = %e, "audio-tx: output stream error (device dropout?)"),
        None,
    );

    let stream = match stream {
        Ok(s) => s,
        Err(e) => {
            warn!(device = %name, error = %e, "audio-tx: failed to build output stream");
            let _ = ready_tx.send(Err(AudioError::Stream(e.to_string())));
            return;
        }
    };
    if let Err(e) = stream.play() {
        warn!(device = %name, error = %e, "audio-tx: failed to start output stream");
        let _ = ready_tx.send(Err(AudioError::Stream(e.to_string())));
        return;
    }
    info!(device = %name, device_rate, channels, frames = samples.len(), "audio-tx: playback started");
    let _ = ready_tx.send(Ok(name));

    // Hold the stream until stopped (wait() sends stop after done arrives).
    let _ = stop_rx.recv();
    // Pause explicitly before dropping — macOS coreaudio has been observed to
    // keep callbacks running after a bare drop (see recorder.rs).
    if let Err(e) = stream.pause() {
        warn!(error = %e, "failed to pause output stream");
    }
    drop(stream);
    let _ = done_tx.try_send(());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::f32_to_i16;

    fn write_wav(path: &Path, spec: hound::WavSpec, samples: &[f32]) {
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        match spec.sample_format {
            hound::SampleFormat::Int => {
                for &s in samples {
                    w.write_sample(f32_to_i16(s)).unwrap();
                }
            }
            hound::SampleFormat::Float => {
                for &s in samples {
                    w.write_sample(s).unwrap();
                }
            }
        }
        w.finalize().unwrap();
    }

    #[test]
    fn load_mono_i16_wav() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        write_wav(&path, spec, &[0.5, -0.25, 0.0]);
        let (mono, rate) = load_wav_mono(&path).unwrap();
        assert_eq!(rate, 48_000);
        assert_eq!(mono.len(), 3);
        assert!((mono[0] - 0.5).abs() < 0.001);
        assert!((mono[1] + 0.25).abs() < 0.001);
    }

    #[test]
    fn load_stereo_f32_wav_downmixes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 44_100,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        // L=1.0 R=0.0 -> 0.5; L=-0.5 R=-0.5 -> -0.5
        write_wav(&path, spec, &[1.0, 0.0, -0.5, -0.5]);
        let (mono, rate) = load_wav_mono(&path).unwrap();
        assert_eq!(rate, 44_100);
        assert_eq!(mono, vec![0.5, -0.5]);
    }

    #[test]
    fn load_missing_file_errors() {
        assert!(load_wav_mono(Path::new("/nonexistent/x.wav")).is_err());
    }
}
