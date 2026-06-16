//! WAV recording. A capture thread owns the cpal input stream (cpal streams are
//! `!Send`, so the stream never crosses threads); its callback only forwards raw
//! interleaved samples over a channel. A drain thread downmixes to mono, tracks
//! levels, and writes uncompressed 16-bit PCM via hound. The drain loop is a free
//! function so tests can drive it with synthetic buffers — no hardware needed.

use crate::device::{open_cpal_device, DeviceKind};
use crate::dsp;
use crate::meta::{RadioSnapshot, RecordingMeta};
use crate::AudioError;
use chrono::{DateTime, Utc};
use cpal::traits::{DeviceTrait, StreamTrait};
use crossbeam_channel::{bounded, unbounded, Receiver, Sender};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use tracing::{debug, info, warn};

/// Message from the capture callback to the drain thread.
///
/// The drain ends on an explicit `End` rather than on channel disconnect:
/// on macOS, dropping a cpal input stream does not reliably stop the callbacks
/// (observed live — the closure keeps firing after `drop(stream)`), so the
/// sender inside the callback can never be trusted to hang up. The callback is
/// also gated by an `AtomicBool` so a leaked stream sends nothing after stop.
pub(crate) enum Chunk {
    Samples(Vec<f32>),
    End,
}

/// Shared live stats, updated by the drain thread, readable from the REPL.
#[derive(Default)]
pub struct LevelTracker {
    frames: AtomicU64,
    /// f32 bits of the overall peak since start.
    peak_overall: AtomicU32,
    /// f32 bits of the peak since the last `take_recent_peak` call.
    peak_recent: AtomicU32,
    /// Set once any sample is non-zero. Distinguishes "real but quiet" from
    /// "exact digital silence" (a muted source or denied mic permission), which
    /// peak alone can't tell apart at the dBFS floor.
    saw_nonzero: AtomicBool,
}

impl LevelTracker {
    pub fn frames(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }

    pub fn peak_overall(&self) -> f32 {
        f32::from_bits(self.peak_overall.load(Ordering::Relaxed))
    }

    /// True if at least one captured sample was non-zero.
    pub fn saw_nonzero(&self) -> bool {
        self.saw_nonzero.load(Ordering::Relaxed)
    }

    /// True if frames were captured but every sample was exact digital zero —
    /// i.e. the device delivered silence (muted source or denied permission),
    /// not merely a quiet signal.
    pub fn is_digital_silence(&self) -> bool {
        self.frames() > 0 && !self.saw_nonzero()
    }

    /// Peak since the last call (resets the recent-peak window).
    pub fn take_recent_peak(&self) -> f32 {
        f32::from_bits(self.peak_recent.swap(0, Ordering::Relaxed))
    }

    fn update(&self, mono: &[f32]) {
        self.frames.fetch_add(mono.len() as u64, Ordering::Relaxed);
        let p = dsp::peak(mono);
        if p > 0.0 {
            self.saw_nonzero.store(true, Ordering::Relaxed);
        }
        for slot in [&self.peak_overall, &self.peak_recent] {
            // Monotonic-max via CAS on the f32 bit pattern.
            let mut cur = slot.load(Ordering::Relaxed);
            while p > f32::from_bits(cur) {
                match slot.compare_exchange_weak(
                    cur,
                    p.to_bits(),
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(actual) => cur = actual,
                }
            }
        }
    }
}

/// Summary returned by [`Recorder::stop`].
#[derive(Debug, Clone)]
pub struct RecordingSummary {
    pub wav_path: PathBuf,
    pub meta_path: PathBuf,
    pub frames: u64,
    pub sample_rate: u32,
    pub duration_secs: f64,
    pub peak_dbfs: f32,
    /// True if frames were captured but every sample was exact digital zero.
    pub silent: bool,
}

/// Drain interleaved f32 chunks from `rx` into a mono 16-bit WAV writer,
/// updating `tracker`. Returns total mono frames written. Runs until an
/// explicit [`Chunk::End`] (or, as a safety net, channel disconnect). Factored
/// out of the thread so tests can feed it directly.
pub(crate) fn drain_to_wav<W: std::io::Write + std::io::Seek>(
    rx: &Receiver<Chunk>,
    writer: &mut hound::WavWriter<W>,
    channels: u16,
    tracker: &LevelTracker,
) -> Result<u64, AudioError> {
    let mut total: u64 = 0;
    for msg in rx.iter() {
        let chunk = match msg {
            Chunk::Samples(s) => s,
            Chunk::End => break,
        };
        let mono = dsp::downmix_to_mono(&chunk, channels);
        tracker.update(&mono);
        for &s in &mono {
            writer.write_sample(dsp::f32_to_i16(s))?;
        }
        total += mono.len() as u64;
    }
    writer.flush()?;
    Ok(total)
}

/// An in-progress recording. One per `rec start`.
pub struct Recorder {
    pub wav_path: PathBuf,
    pub device_name: String,
    pub sample_rate: u32,
    pub started: DateTime<Utc>,
    pub tracker: Arc<LevelTracker>,
    radio: RadioSnapshot,
    stop_tx: Sender<()>,
    capture_join: JoinHandle<()>,
    drain_join: JoinHandle<Result<u64, AudioError>>,
}

impl Recorder {
    /// Start recording from `input_name` (None = preferred/default input) to
    /// `wav_path`. `radio` is whatever rig state the caller could snapshot.
    pub fn start(
        input_name: Option<String>,
        wav_path: PathBuf,
        radio: RadioSnapshot,
    ) -> Result<Recorder, AudioError> {
        let (stop_tx, stop_rx) = bounded::<()>(1);
        let (sample_tx, sample_rx) = unbounded::<Chunk>();
        // Startup handshake: the capture thread reports the negotiated config
        // (or the open error) before we return.
        let (ready_tx, ready_rx) = bounded::<Result<(String, u32, u16), AudioError>>(1);

        let capture_join = std::thread::Builder::new()
            .name("audio-capture".into())
            .spawn(move || capture_thread(input_name, sample_tx, stop_rx, ready_tx))
            .expect("spawn audio-capture");

        let (device_name, sample_rate, channels) = match ready_rx.recv() {
            Ok(Ok(cfg)) => cfg,
            Ok(Err(e)) => {
                let _ = capture_join.join();
                return Err(e);
            }
            Err(_) => return Err(AudioError::Stream("capture thread died on startup".into())),
        };

        let spec = hound::WavSpec {
            channels: 1,
            sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let writer = hound::WavWriter::create(&wav_path, spec)?;
        let tracker = Arc::new(LevelTracker::default());
        let tracker2 = Arc::clone(&tracker);
        let drain_join = std::thread::Builder::new()
            .name("audio-drain".into())
            .spawn(move || {
                let mut writer = writer;
                let result = drain_to_wav(&sample_rx, &mut writer, channels, &tracker2);
                writer.finalize()?;
                result
            })
            .expect("spawn audio-drain");

        info!(device = %device_name, sample_rate, channels, path = %wav_path.display(), "recording started");
        Ok(Recorder {
            wav_path,
            device_name,
            sample_rate,
            started: Utc::now(),
            tracker,
            radio,
            stop_tx,
            capture_join,
            drain_join,
        })
    }

    pub fn duration_secs(&self) -> f64 {
        self.tracker.frames() as f64 / self.sample_rate as f64
    }

    /// Stop recording, finalize the WAV, and write the sidecar metadata.
    pub fn stop(self) -> Result<RecordingSummary, AudioError> {
        let _ = self.stop_tx.send(());
        let _ = self.capture_join.join();
        let frames = self
            .drain_join
            .join()
            .map_err(|_| AudioError::Stream("drain thread panicked".into()))??;

        let duration_secs = frames as f64 / self.sample_rate as f64;
        let peak_dbfs = dsp::dbfs(self.tracker.peak_overall());
        let silent = self.tracker.is_digital_silence();
        let meta = RecordingMeta {
            utc_start: self.started,
            utc_end: Some(Utc::now()),
            sample_rate: self.sample_rate,
            channels: 1,
            frames,
            duration_secs,
            peak_dbfs,
            input_device: self.device_name.clone(),
            radio: self.radio.clone(),
        };
        let meta_path = meta.save(&self.wav_path)?;
        info!(frames, duration_secs, peak_dbfs, silent, path = %self.wav_path.display(), "recording stopped");
        Ok(RecordingSummary {
            wav_path: self.wav_path,
            meta_path,
            frames,
            sample_rate: self.sample_rate,
            duration_secs,
            peak_dbfs,
            silent,
        })
    }
}

/// Thread that owns the cpal input stream for the life of the recording.
///
/// Shutdown is belt-and-braces because macOS coreaudio has been observed to
/// keep firing callbacks after `drop(stream)`: (1) the `active` flag silences
/// the callback, (2) `pause()` asks coreaudio to stop, (3) the stream is
/// dropped, (4) an explicit `Chunk::End` releases the drain thread regardless.
fn capture_thread(
    input_name: Option<String>,
    sample_tx: Sender<Chunk>,
    stop_rx: Receiver<()>,
    ready_tx: Sender<Result<(String, u32, u16), AudioError>>,
) {
    let active = Arc::new(AtomicBool::new(true));
    let end_tx = sample_tx.clone();
    let result = (|| {
        let device = open_cpal_device(DeviceKind::Input, input_name.as_deref())?;
        let name = device.name().unwrap_or_else(|_| "<unknown>".into());
        let config = crate::device::resolve_input_config(&device)
            .map_err(|e| AudioError::Device(format!("'{name}': {e}")))?;
        debug!(device = %name, ?config, "input stream config resolved");
        let rate = config.sample_rate().0;
        let channels = config.channels();
        let stream = build_input_stream(&device, &config, sample_tx, Arc::clone(&active))
            .map_err(|e| AudioError::Stream(format!("'{name}': {e}")))?;
        stream
            .play()
            .map_err(|e| AudioError::Stream(format!("'{name}': {e}")))?;
        Ok((name, rate, channels, stream))
    })();

    match result {
        Ok((name, rate, channels, stream)) => {
            let _ = ready_tx.send(Ok((name, rate, channels)));
            let _ = stop_rx.recv();
            active.store(false, Ordering::Relaxed); // gate the callback first
            if let Err(e) = stream.pause() {
                warn!(error = %e, "failed to pause input stream");
            }
            drop(stream);
            let _ = end_tx.send(Chunk::End); // release the drain unconditionally
            debug!("capture stream closed");
        }
        Err(e) => {
            let _ = ready_tx.send(Err(e));
        }
    }
}

/// Build an input stream for the device's native sample format, forwarding
/// interleaved f32 chunks. The callback only converts and sends, and is gated
/// by `active` so a leaked stream goes quiet after stop.
fn build_input_stream(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    tx: Sender<Chunk>,
    active: Arc<AtomicBool>,
) -> Result<cpal::Stream, AudioError> {
    let err_fn = |e| warn!(error = %e, "audio input stream error");
    let stream_config: cpal::StreamConfig = config.config();
    macro_rules! input_stream {
        ($ty:ty, $convert:expr) => {{
            let convert: fn(&[$ty]) -> Vec<f32> = $convert;
            device.build_input_stream(
                &stream_config,
                move |data: &[$ty], _| {
                    if active.load(Ordering::Relaxed) {
                        let _ = tx.send(Chunk::Samples(convert(data)));
                    }
                },
                err_fn,
                None,
            )
        }};
    }
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => input_stream!(f32, |d| d.to_vec()),
        cpal::SampleFormat::I16 => {
            input_stream!(i16, |d| d.iter().map(|&s| s as f32 / 32768.0).collect())
        }
        cpal::SampleFormat::U16 => {
            input_stream!(u16, |d| d
                .iter()
                .map(|&s| (s as f32 - 32768.0) / 32768.0)
                .collect())
        }
        other => return Err(AudioError::Unsupported(format!("sample format {other:?}"))),
    }
    .map_err(|e| AudioError::Stream(e.to_string()))?;
    Ok(stream)
}

/// One-shot level measurement: capture `duration` from the input and return
/// (peak, rms, sample_rate). Used by the `level` command when not recording.
/// Result of a one-shot level measurement.
#[derive(Debug, Clone, Copy)]
pub struct LevelMeasurement {
    pub peak: f32,
    pub rms: f32,
    pub sample_rate: u32,
    /// Mono frames captured during the window.
    pub frames: usize,
    /// True if frames arrived but every sample was exact zero (muted source or
    /// denied mic permission), as opposed to a quiet-but-real signal.
    pub digital_silence: bool,
}

pub fn measure_level(
    input_name: Option<String>,
    duration: std::time::Duration,
) -> Result<LevelMeasurement, AudioError> {
    let (stop_tx, stop_rx) = bounded::<()>(1);
    let (sample_tx, sample_rx) = unbounded::<Chunk>();
    let (ready_tx, ready_rx) = bounded(1);

    let join = std::thread::Builder::new()
        .name("audio-level".into())
        .spawn(move || capture_thread(input_name, sample_tx, stop_rx, ready_tx))
        .expect("spawn audio-level");

    let (_name, rate, channels) = match ready_rx.recv() {
        Ok(Ok(cfg)) => cfg,
        Ok(Err(e)) => {
            let _ = join.join();
            return Err(e);
        }
        Err(_) => return Err(AudioError::Stream("level capture died on startup".into())),
    };

    std::thread::sleep(duration);
    let _ = stop_tx.send(());
    let _ = join.join();

    let mut mono = Vec::new();
    for msg in sample_rx.try_iter() {
        if let Chunk::Samples(chunk) = msg {
            mono.extend(dsp::downmix_to_mono(&chunk, channels));
        }
    }
    let peak = dsp::peak(&mono);
    Ok(LevelMeasurement {
        peak,
        rms: dsp::rms(&mono),
        sample_rate: rate,
        frames: mono.len(),
        digital_silence: !mono.is_empty() && peak == 0.0,
    })
}

/// Capture `duration` of audio from the input into memory and return the mono
/// samples plus the device sample rate. Like [`measure_level`] but keeps the
/// samples — used by FT8/FT4 decoding to grab one slot of receiver audio. Reuses
/// the same macOS-safe capture thread.
pub fn capture_window(
    input_name: Option<String>,
    duration: std::time::Duration,
) -> Result<(Vec<f32>, u32), AudioError> {
    let (stop_tx, stop_rx) = bounded::<()>(1);
    let (sample_tx, sample_rx) = unbounded::<Chunk>();
    let (ready_tx, ready_rx) = bounded(1);

    let join = std::thread::Builder::new()
        .name("audio-capture-window".into())
        .spawn(move || capture_thread(input_name, sample_tx, stop_rx, ready_tx))
        .expect("spawn audio-capture-window");

    let (_name, rate, channels) = match ready_rx.recv() {
        Ok(Ok(cfg)) => cfg,
        Ok(Err(e)) => {
            let _ = join.join();
            return Err(e);
        }
        Err(_) => return Err(AudioError::Stream("capture died on startup".into())),
    };

    std::thread::sleep(duration);
    let _ = stop_tx.send(());
    let _ = join.join();

    let mut mono = Vec::new();
    for msg in sample_rx.try_iter() {
        if let Chunk::Samples(chunk) = msg {
            mono.extend(dsp::downmix_to_mono(&chunk, channels));
        }
    }
    Ok((mono, rate))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive the drain loop with synthetic stereo chunks and verify the WAV.
    #[test]
    fn drain_writes_mono_wav_and_tracks_levels() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        let tracker = LevelTracker::default();
        let (tx, rx) = unbounded::<Chunk>();

        // Two stereo chunks: [L=0.5, R=0.5] -> mono 0.5; [L=1.0, R=0.0] -> 0.5;
        // then a louder frame [L=-0.8, R=-0.8] -> -0.8.
        tx.send(Chunk::Samples(vec![0.5, 0.5, 1.0, 0.0])).unwrap();
        tx.send(Chunk::Samples(vec![-0.8, -0.8])).unwrap();
        tx.send(Chunk::End).unwrap(); // explicit end -> drain returns

        let frames = drain_to_wav(&rx, &mut writer, 2, &tracker).unwrap();
        writer.finalize().unwrap();

        assert_eq!(frames, 3);
        assert_eq!(tracker.frames(), 3);
        assert!((tracker.peak_overall() - 0.8).abs() < 1e-6);

        let mut reader = hound::WavReader::open(&path).unwrap();
        assert_eq!(reader.spec().channels, 1);
        assert_eq!(reader.spec().sample_rate, 48_000);
        let samples: Vec<i16> = reader.samples::<i16>().map(|s| s.unwrap()).collect();
        assert_eq!(samples.len(), 3);
        assert!((samples[0] as f32 / 32767.0 - 0.5).abs() < 0.001);
        assert!((samples[1] as f32 / 32767.0 - 0.5).abs() < 0.001);
        assert!((samples[2] as f32 / 32767.0 + 0.8).abs() < 0.001);
    }

    #[test]
    fn tracker_distinguishes_silence_from_quiet() {
        // Exact digital silence: frames captured, every sample zero.
        let silent = LevelTracker::default();
        silent.update(&[0.0, 0.0, 0.0]);
        assert!(silent.is_digital_silence());
        assert!(!silent.saw_nonzero());

        // Quiet but real: a tiny non-zero sample is NOT digital silence.
        let quiet = LevelTracker::default();
        quiet.update(&[0.0, 0.0001, 0.0]);
        assert!(!quiet.is_digital_silence());
        assert!(quiet.saw_nonzero());

        // No frames at all is not "digital silence" (nothing was captured).
        let empty = LevelTracker::default();
        assert!(!empty.is_digital_silence());
    }

    #[test]
    fn tracker_recent_peak_resets() {
        let t = LevelTracker::default();
        t.update(&[0.3]);
        assert!((t.take_recent_peak() - 0.3).abs() < 1e-6);
        assert_eq!(t.take_recent_peak(), 0.0); // window reset
        t.update(&[0.1]);
        assert!((t.take_recent_peak() - 0.1).abs() < 1e-6);
        // Overall peak unaffected by recent resets.
        assert!((t.peak_overall() - 0.3).abs() < 1e-6);
    }

    #[test]
    fn drain_handles_clipping_input() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 12_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        let tracker = LevelTracker::default();
        let (tx, rx) = unbounded::<Chunk>();
        tx.send(Chunk::Samples(vec![2.0, -2.0])).unwrap(); // out-of-range input
        drop(tx); // disconnect (no End) must also terminate the drain
        drain_to_wav(&rx, &mut writer, 1, &tracker).unwrap();
        writer.finalize().unwrap();

        let mut reader = hound::WavReader::open(&path).unwrap();
        let samples: Vec<i16> = reader.samples::<i16>().map(|s| s.unwrap()).collect();
        assert_eq!(samples, vec![i16::MAX, -i16::MAX]); // clamped, not wrapped
    }
}
