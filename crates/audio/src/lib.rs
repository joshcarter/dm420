//! Audio capture, playback, and DSP for kenctl (Phase 2 of poc-cli-plan.md).
//!
//! Layers:
//! - [`dsp`] — pure math: downmix, levels/dBFS, and the streaming windowed-sinc
//!   [`dsp::Resampler`] (the 48k→12k groundwork for Phase 3 FT8 decode).
//! - [`device`] — cpal device enumeration + pure selection logic.
//! - [`recorder`] — capture thread (owns the cpal stream) + drain thread writing
//!   uncompressed 16-bit PCM WAV, with live level tracking.
//! - [`player`] — WAV loading (pure) + playback through any output device, with
//!   automatic rate conversion.
//! - [`meta`] — sidecar JSON metadata written next to each recording.
//!
//! This crate deliberately does not depend on `rig`: kenctl snapshots the radio
//! state and passes it in as a plain [`meta::RadioSnapshot`].

pub mod device;
pub mod dsp;
pub mod meta;
pub mod player;
pub mod recorder;

pub use device::{list_devices, preferred_input, select, DeviceInfo, DeviceKind};
pub use dsp::{dbfs, level_bar, Resampler};
pub use meta::{default_recording_path, RadioSnapshot, RecordingMeta};
pub use player::{load_wav_mono, play, Playback};
pub use recorder::{capture_window, measure_level, LevelMeasurement, Recorder, RecordingSummary};

/// Errors from audio operations.
#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("audio device error: {0}")]
    Device(String),
    #[error("audio stream error: {0}")]
    Stream(String),
    #[error("WAV error: {0}")]
    Wav(#[from] hound::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("unsupported: {0}")]
    Unsupported(String),
}
