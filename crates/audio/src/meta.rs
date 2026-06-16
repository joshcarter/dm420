//! Sidecar metadata for recordings: `<recording>.wav` gets a `<recording>.wav.json`
//! describing when it was made and what the radio was doing — this is what makes
//! a recording replayable in context (and is the seed of the Phase 3/4 session
//! manifests).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Radio state at recording start, as supplied by the caller (kenctl fetches it
/// from the rig actor; all optional so recording works with no radio attached).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RadioSnapshot {
    pub freq_hz: Option<u64>,
    pub mode: Option<String>,
    pub data_mode: Option<bool>,
    pub radio_id: Option<String>,
}

/// Sidecar metadata written next to each WAV.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecordingMeta {
    pub utc_start: DateTime<Utc>,
    pub utc_end: Option<DateTime<Utc>>,
    pub sample_rate: u32,
    pub channels: u16,
    pub frames: u64,
    pub duration_secs: f64,
    pub peak_dbfs: f32,
    pub input_device: String,
    #[serde(default)]
    pub radio: RadioSnapshot,
}

impl RecordingMeta {
    /// The sidecar path for a WAV path: `foo.wav` -> `foo.wav.json`.
    pub fn sidecar_path(wav_path: &Path) -> std::path::PathBuf {
        let mut s = wav_path.as_os_str().to_os_string();
        s.push(".json");
        std::path::PathBuf::from(s)
    }

    pub fn save(&self, wav_path: &Path) -> std::io::Result<std::path::PathBuf> {
        let path = Self::sidecar_path(wav_path);
        let json = serde_json::to_string_pretty(self).expect("meta serializes");
        std::fs::write(&path, json)?;
        Ok(path)
    }

    pub fn load(wav_path: &Path) -> std::io::Result<RecordingMeta> {
        let json = std::fs::read_to_string(Self::sidecar_path(wav_path))?;
        serde_json::from_str(&json).map_err(|e| std::io::Error::other(e.to_string()))
    }
}

/// Default recording filename: `rec_20260611_193000Z.wav` in the working dir.
pub fn default_recording_path(now: DateTime<Utc>) -> std::path::PathBuf {
    std::path::PathBuf::from(format!("rec_{}.wav", now.format("%Y%m%d_%H%M%SZ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidecar_path_appends_json() {
        assert_eq!(
            RecordingMeta::sidecar_path(Path::new("/tmp/a.wav")),
            Path::new("/tmp/a.wav.json")
        );
    }

    #[test]
    fn default_path_format() {
        let t = DateTime::parse_from_rfc3339("2026-06-11T19:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            default_recording_path(t),
            Path::new("rec_20260611_193000Z.wav")
        );
    }

    #[test]
    fn meta_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("x.wav");
        let meta = RecordingMeta {
            utc_start: Utc::now(),
            utc_end: Some(Utc::now()),
            sample_rate: 48_000,
            channels: 1,
            frames: 480_000,
            duration_secs: 10.0,
            peak_dbfs: -12.5,
            input_device: "USB Audio CODEC".into(),
            radio: RadioSnapshot {
                freq_hz: Some(14_074_000),
                mode: Some("USB".into()),
                data_mode: Some(true),
                radio_id: Some("ID021".into()),
            },
        };
        meta.save(&wav).unwrap();
        let loaded = RecordingMeta::load(&wav).unwrap();
        assert_eq!(loaded, meta);
    }
}
