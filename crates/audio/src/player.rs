//! Audio output. `load_wav_mono` (loading/decoding) is pure and tested. The live
//! output is a **persistent** stream: [`OutputStream`] opens the device once and
//! keeps a cpal stream running (playing silence when idle), so starting a
//! transmission is immediate — no per-over device open. The output thread owns the
//! cpal stream (cpal streams are `!Send`); the handle drives it via a command
//! channel plus progress/done atomics.

use crate::AudioError;
use crate::device::{DeviceKind, open_cpal_device};
use crate::dsp::{Resampler, downmix_to_mono};
use cpal::traits::{DeviceTrait, StreamTrait};
use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tracing::{info, warn};

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

/// Write mono f32 samples to an uncompressed 16-bit PCM WAV at `sample_rate` — the
/// inverse of [`load_wav_mono`]. Used by tools that synthesize audio (e.g. the
/// `encode_wav` example); samples are clamped to [-1, 1] by [`crate::dsp::f32_to_i16`].
pub fn save_wav_mono(path: &Path, samples: &[f32], sample_rate: u32) -> Result<(), AudioError> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)?;
    for &s in samples {
        writer.write_sample(crate::dsp::f32_to_i16(s))?;
    }
    writer.finalize()?;
    Ok(())
}

/// A command to the output stream's audio callback.
enum OutCmd {
    /// Play these device-rate samples from the start; `ratio` (file_rate/device_rate)
    /// maps playback position back to file frames for `progress`.
    Play { samples: Vec<f32>, ratio: f64 },
    /// Drop to silence immediately (operator Stop).
    Silence,
}

/// A persistently-open output stream. The device is opened once (here) and the
/// cpal stream runs continuously, emitting silence until [`load`](Self::load)ed
/// with a waveform — so a transmission starts on the next audio callback rather
/// than after a cold device open. Re-open (a fresh `OutputStream`) only when the
/// selected output device changes or the stream dies.
pub struct OutputStream {
    /// The device name as requested by the caller (`None` = system default), kept
    /// so the owner can tell whether a settings change needs a re-open.
    requested: Option<String>,
    device_name: String,
    device_rate: u32,
    cmd_tx: Sender<OutCmd>,
    /// Playback position of the current waveform, in *file* frames.
    progress: Arc<AtomicU64>,
    /// True when the current waveform has finished (or none is loaded) — i.e. the
    /// stream is emitting silence.
    done: Arc<AtomicBool>,
    /// Set by cpal's error callback if the device drops out; the owner re-opens.
    dead: Arc<AtomicBool>,
    shutdown_tx: Sender<()>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl OutputStream {
    /// Open `name` (`None` = system default) and start a silent stream. Blocks only
    /// until the stream is running (the one cold open), then stays warm.
    pub fn open(requested: Option<String>) -> Result<OutputStream, AudioError> {
        let (cmd_tx, cmd_rx) = unbounded::<OutCmd>();
        let (ready_tx, ready_rx) = bounded::<Result<(String, u32), AudioError>>(1);
        let (shutdown_tx, shutdown_rx) = bounded::<()>(1);
        let progress = Arc::new(AtomicU64::new(0));
        let done = Arc::new(AtomicBool::new(true)); // idle == "done" (emitting silence)
        let dead = Arc::new(AtomicBool::new(false));

        let (progress_t, done_t, dead_t, req_t) =
            (progress.clone(), done.clone(), dead.clone(), requested.clone());
        let join = std::thread::Builder::new()
            .name("audio-out".into())
            .spawn(move || {
                output_thread(req_t, cmd_rx, progress_t, done_t, dead_t, ready_tx, shutdown_rx)
            })
            .expect("spawn audio-out");

        match ready_rx.recv() {
            Ok(Ok((device_name, device_rate))) => Ok(OutputStream {
                requested,
                device_name,
                device_rate,
                cmd_tx,
                progress,
                done,
                dead,
                shutdown_tx,
                join: Some(join),
            }),
            Ok(Err(e)) => {
                let _ = join.join();
                Err(e)
            }
            Err(_) => Err(AudioError::Stream("output thread died on startup".into())),
        }
    }

    /// The device name as requested at open (`None` = system default).
    pub fn requested(&self) -> Option<&str> {
        self.requested.as_deref()
    }

    /// The resolved output device name.
    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    /// The current playback position counter (file frames), shared with the audio
    /// callback — used to pace anything synced to playback (e.g. the TX waterfall).
    pub fn progress(&self) -> Arc<AtomicU64> {
        self.progress.clone()
    }

    /// True once the loaded waveform has finished (or none is loaded).
    pub fn is_done(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }

    /// True if the device dropped out and the stream needs re-opening.
    pub fn is_dead(&self) -> bool {
        self.dead.load(Ordering::Acquire)
    }

    /// Load a mono waveform (at `file_rate`) and start playing it immediately,
    /// resampling to the device rate as needed.
    pub fn load(&self, mono: &[f32], file_rate: u32) {
        let ratio = file_rate as f64 / self.device_rate as f64;
        let samples = if file_rate == self.device_rate {
            mono.to_vec()
        } else {
            let mut r = Resampler::new(file_rate, self.device_rate);
            let mut out = r.push(mono);
            out.extend(r.flush());
            out
        };
        // Reset synchronously so a waiter polling `is_done`/`progress` right after
        // this never sees the previous over's finished state.
        self.progress.store(0, Ordering::Release);
        self.done.store(false, Ordering::Release);
        let _ = self.cmd_tx.send(OutCmd::Play { samples, ratio });
    }

    /// Cut any in-progress playback to silence now (operator Stop / abort).
    pub fn silence(&self) {
        let _ = self.cmd_tx.send(OutCmd::Silence);
        self.done.store(true, Ordering::Release);
    }
}

impl Drop for OutputStream {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(());
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn output_thread(
    requested: Option<String>,
    cmd_rx: Receiver<OutCmd>,
    progress: Arc<AtomicU64>,
    done: Arc<AtomicBool>,
    dead: Arc<AtomicBool>,
    ready_tx: Sender<Result<(String, u32), AudioError>>,
    shutdown_rx: Receiver<()>,
) {
    let setup = (|| {
        let device = open_cpal_device(DeviceKind::Output, requested.as_deref())?;
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

    // Callback-owned playback state (cpal data callbacks are `FnMut`, so the closure
    // carries it across invocations). Starts empty == silence. New waveforms / Stop
    // arrive over `cmd_rx`, drained at the top of each callback. `playing` gates the
    // done/progress updates to a single play→finished transition, so a callback that
    // runs on an already-finished buffer can't re-assert `done` after the handle has
    // cleared it for the next over.
    let mut current: Vec<f32> = Vec::new();
    let mut pos: usize = 0;
    let mut ratio: f64 = 1.0;
    let mut playing = false;
    let progress_cb = progress.clone();
    let done_cb = done.clone();
    let err_dead = dead.clone();
    let err_name = name.clone();

    let stream = device.build_output_stream(
        &stream_config,
        move |out: &mut [f32], _| {
            while let Ok(cmd) = cmd_rx.try_recv() {
                match cmd {
                    OutCmd::Play { samples, ratio: r } => {
                        current = samples;
                        pos = 0;
                        ratio = r;
                        playing = true;
                        done_cb.store(false, Ordering::Relaxed);
                    }
                    OutCmd::Silence => {
                        current.clear();
                        pos = 0;
                        playing = false;
                    }
                }
            }
            for frame in out.chunks_mut(channels) {
                let v = current.get(pos).copied().unwrap_or(0.0);
                for slot in frame.iter_mut() {
                    *slot = v;
                }
                if pos < current.len() {
                    pos += 1;
                }
            }
            if playing {
                progress_cb.store((pos as f64 * ratio) as u64, Ordering::Relaxed);
                if pos >= current.len() {
                    done_cb.store(true, Ordering::Relaxed);
                    playing = false; // emit silence until the next Play
                }
            }
        },
        move |e| {
            warn!(device = %err_name, error = %e, "audio-out: output stream error (device dropout?)");
            err_dead.store(true, Ordering::Release);
        },
        None,
    );

    let stream = match stream {
        Ok(s) => s,
        Err(e) => {
            warn!(device = %name, error = %e, "audio-out: failed to build output stream");
            let _ = ready_tx.send(Err(AudioError::Stream(e.to_string())));
            return;
        }
    };
    if let Err(e) = stream.play() {
        warn!(device = %name, error = %e, "audio-out: failed to start output stream");
        let _ = ready_tx.send(Err(AudioError::Stream(e.to_string())));
        return;
    }
    info!(device = %name, device_rate, channels, "audio-out: output stream open (warm)");
    let _ = ready_tx.send(Ok((name, device_rate)));

    // Hold the stream alive until the handle is dropped. Pause before drop —
    // macOS coreaudio has been observed to keep callbacks running after a bare
    // drop (see recorder.rs).
    let _ = shutdown_rx.recv();
    if let Err(e) = stream.pause() {
        warn!(error = %e, "failed to pause output stream");
    }
    drop(stream);
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
    fn save_wav_mono_round_trips_through_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rt.wav");
        // A short sine in [-1, 1].
        let samples: Vec<f32> = (0..480).map(|i| ((i as f32) * 0.05).sin() * 0.5).collect();
        save_wav_mono(&path, &samples, 12_000).unwrap();
        let (loaded, rate) = load_wav_mono(&path).unwrap();
        assert_eq!(rate, 12_000);
        assert_eq!(loaded.len(), samples.len());
        // 16-bit quantization: each sample survives within one quantum.
        for (a, b) in samples.iter().zip(&loaded) {
            assert!((a - b).abs() < 2.0 / 32_767.0, "{a} vs {b}");
        }
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
