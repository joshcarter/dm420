//! Audio device enumeration and selection. The selection logic is a pure
//! function over [`DeviceInfo`] so it's testable without hardware; only the
//! enumeration itself touches cpal.

use crate::AudioError;
use cpal::traits::{DeviceTrait, HostTrait};
use serde::{Deserialize, Serialize};
use tracing::debug;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceKind {
    Input,
    Output,
}

/// A discovered audio device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub name: String,
    pub kind: DeviceKind,
    pub sample_rate: u32,
    pub channels: u16,
    pub is_default: bool,
    /// True if this looks like the TS-590's built-in codec ("USB Audio CODEC").
    pub looks_like_radio: bool,
}

/// True if a device name looks like the Kenwood USB codec.
pub fn is_radio_codec(name: &str) -> bool {
    name.to_lowercase().contains("usb audio codec")
}

/// Enumerate input and output devices on the default host.
pub fn list_devices() -> Result<Vec<DeviceInfo>, AudioError> {
    let host = cpal::default_host();
    let default_in = host.default_input_device().and_then(|d| d.name().ok());
    let default_out = host.default_output_device().and_then(|d| d.name().ok());

    let mut out = Vec::new();
    for device in host
        .devices()
        .map_err(|e| AudioError::Device(e.to_string()))?
    {
        let name = device.name().unwrap_or_else(|_| "<unknown>".into());
        // TEMP diagnostic (remove once enumeration is sorted): dump exactly what
        // cpal reports per device, so we can see why a duplex device may not
        // surface as an output.
        eprintln!(
            "dm420 audio-probe: {name:?}  in(default={} ranges={})  out(default={} ranges={})",
            device.default_input_config().is_ok(),
            device
                .supported_input_configs()
                .map(|mut c| c.next().is_some())
                .unwrap_or(false),
            device.default_output_config().is_ok(),
            device
                .supported_output_configs()
                .map(|mut c| c.next().is_some())
                .unwrap_or(false),
        );
        if let Some((rate, channels)) = probe_config(&device, DeviceKind::Input) {
            out.push(DeviceInfo {
                name: name.clone(),
                kind: DeviceKind::Input,
                sample_rate: rate,
                channels,
                is_default: Some(&name) == default_in.as_ref(),
                looks_like_radio: is_radio_codec(&name),
            });
        }
        if let Some((rate, channels)) = probe_config(&device, DeviceKind::Output) {
            out.push(DeviceInfo {
                name: name.clone(),
                kind: DeviceKind::Output,
                sample_rate: rate,
                channels,
                is_default: Some(&name) == default_out.as_ref(),
                looks_like_radio: is_radio_codec(&name),
            });
        }
    }
    for d in &out {
        debug!(?d.kind, name = %d.name, rate = d.sample_rate, ch = d.channels, "found audio device");
    }
    Ok(out)
}

/// A device's `(sample_rate, channels)` for `kind`: its default config if the
/// device answers, else the first supported config range. Some USB codecs (the
/// TS-590 "USB Audio CODEC" among them) reject the default-config query on one
/// direction while enumerating ranges fine — without this fallback a duplex codec
/// vanishes from one side of the device list (e.g. shows as input but not output).
fn probe_config(device: &cpal::Device, kind: DeviceKind) -> Option<(u32, u16)> {
    match kind {
        DeviceKind::Input => {
            if let Ok(cfg) = device.default_input_config() {
                return Some((cfg.sample_rate().0, cfg.channels()));
            }
            let r = device.supported_input_configs().ok()?.next()?;
            Some((r.max_sample_rate().0, r.channels()))
        }
        DeviceKind::Output => {
            if let Ok(cfg) = device.default_output_config() {
                return Some((cfg.sample_rate().0, cfg.channels()));
            }
            let r = device.supported_output_configs().ok()?.next()?;
            Some((r.max_sample_rate().0, r.channels()))
        }
    }
}

/// Pick a device from `devices` of the given `kind` by `selector`:
/// a number selects by position among devices of that kind (as printed by
/// `audio devices`), otherwise case-insensitive substring match on the name.
pub fn select<'a>(
    devices: &'a [DeviceInfo],
    kind: DeviceKind,
    selector: &str,
) -> Option<&'a DeviceInfo> {
    let of_kind: Vec<&DeviceInfo> = devices.iter().filter(|d| d.kind == kind).collect();
    if let Ok(idx) = selector.trim().parse::<usize>() {
        return of_kind.get(idx).copied();
    }
    let needle = selector.to_lowercase();
    of_kind
        .into_iter()
        .find(|d| d.name.to_lowercase().contains(&needle))
}

/// The preferred default *input*: the radio codec if present, else the system
/// default input, else the first input.
pub fn preferred_input(devices: &[DeviceInfo]) -> Option<&DeviceInfo> {
    let inputs: Vec<&DeviceInfo> = devices
        .iter()
        .filter(|d| d.kind == DeviceKind::Input)
        .collect();
    inputs
        .iter()
        .find(|d| d.looks_like_radio)
        .or_else(|| inputs.iter().find(|d| d.is_default))
        .copied()
        .or_else(|| inputs.first().copied())
}

/// True if the device actually supports streams of the given kind. macOS can
/// expose a USB codec as two same-named sibling devices (one input-only, one
/// output-only), so a name match alone is not enough.
fn supports_kind(device: &cpal::Device, kind: DeviceKind) -> bool {
    match kind {
        DeviceKind::Input => {
            device.default_input_config().is_ok()
                || device
                    .supported_input_configs()
                    .map(|mut c| c.next().is_some())
                    .unwrap_or(false)
        }
        DeviceKind::Output => {
            device.default_output_config().is_ok()
                || device
                    .supported_output_configs()
                    .map(|mut c| c.next().is_some())
                    .unwrap_or(false)
        }
    }
}

/// Resolve a cpal input or output device by name (or the default when `name` is
/// None). Matching is forgiving so a config value need not be the device's exact
/// OS string: an exact (case-insensitive) name wins, else the first
/// case-insensitive substring match. Candidates that lack the required
/// capability are skipped, so a same-named output-only sibling can't shadow the
/// input device.
pub(crate) fn open_cpal_device(
    kind: DeviceKind,
    name: Option<&str>,
) -> Result<cpal::Device, AudioError> {
    let host = cpal::default_host();
    match name {
        Some(n) => {
            let needle = n.to_lowercase();
            let mut name_matched = 0u32;
            let mut substring: Option<cpal::Device> = None;
            for d in host
                .devices()
                .map_err(|e| AudioError::Device(e.to_string()))?
            {
                let Some(dn) = d.name().ok() else { continue };
                let dn_lc = dn.to_lowercase();
                let exact = dn_lc == needle;
                let contains = dn_lc.contains(&needle);
                if !exact && !contains {
                    continue;
                }
                name_matched += 1;
                if !supports_kind(&d, kind) {
                    debug!(name = %dn, ?kind, "skipping matched device without {kind:?} support");
                    continue;
                }
                if exact {
                    return Ok(d);
                }
                // Remember the first capable substring match; prefer an exact one
                // if a later device provides it.
                if substring.is_none() {
                    substring = Some(d);
                }
            }
            substring.ok_or_else(|| {
                AudioError::Device(if name_matched > 0 {
                    format!("audio device matching '{n}' exists but has no {kind:?} streams")
                } else {
                    format!("audio device '{n}' not found")
                })
            })
        }
        None => match kind {
            DeviceKind::Input => host
                .default_input_device()
                .ok_or_else(|| AudioError::Device("no default input device".into())),
            DeviceKind::Output => host
                .default_output_device()
                .ok_or_else(|| AudioError::Device("no default output device".into())),
        },
    }
}

/// A summary of one supported-config range, decoupled from cpal types so the
/// preference logic is testable without hardware.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ConfigRange {
    pub channels: u16,
    pub min_rate: u32,
    pub max_rate: u32,
    /// 2 = f32 (preferred), 1 = i16, 0 = u16, -1 = unsupported format.
    pub format_pref: i8,
}

/// Pick the best range and sample rate: prefer ranges that contain
/// `preferred_rate` (else highest max rate), then the friendliest sample
/// format, then the fewest channels (avoids 8-ch aggregate weirdness).
/// Returns `(index, rate_to_use)`.
pub(crate) fn choose_config(ranges: &[ConfigRange], preferred_rate: u32) -> Option<(usize, u32)> {
    ranges
        .iter()
        .enumerate()
        .filter(|(_, r)| r.format_pref >= 0)
        .max_by_key(|(_, r)| {
            let contains = r.min_rate <= preferred_rate && preferred_rate <= r.max_rate;
            (
                contains,
                r.format_pref,
                std::cmp::Reverse(r.channels),
                r.max_rate,
            )
        })
        .map(|(i, r)| {
            let rate = if r.min_rate <= preferred_rate && preferred_rate <= r.max_rate {
                preferred_rate
            } else {
                r.max_rate
            };
            (i, rate)
        })
}

fn format_pref(f: cpal::SampleFormat) -> i8 {
    match f {
        cpal::SampleFormat::F32 => 2,
        cpal::SampleFormat::I16 => 1,
        cpal::SampleFormat::U16 => 0,
        _ => -1,
    }
}

/// Resolve a usable *input* stream config: try the device default first, then
/// fall back to scanning `supported_input_configs()` (some USB codecs reject
/// the default-config query but enumerate ranges fine).
pub(crate) fn resolve_input_config(
    device: &cpal::Device,
) -> Result<cpal::SupportedStreamConfig, AudioError> {
    if let Ok(cfg) = device.default_input_config() {
        return Ok(cfg);
    }
    let ranges: Vec<cpal::SupportedStreamConfigRange> = device
        .supported_input_configs()
        .map_err(|e| AudioError::Device(format!("querying input configs: {e}")))?
        .collect();
    let summaries: Vec<ConfigRange> = ranges
        .iter()
        .map(|r| ConfigRange {
            channels: r.channels(),
            min_rate: r.min_sample_rate().0,
            max_rate: r.max_sample_rate().0,
            format_pref: format_pref(r.sample_format()),
        })
        .collect();
    debug!(
        ?summaries,
        "default input config unavailable; choosing from supported ranges"
    );
    let (idx, rate) = choose_config(&summaries, 48_000).ok_or_else(|| {
        AudioError::Device("device reports no usable input stream configs".into())
    })?;
    Ok(ranges[idx].with_sample_rate(cpal::SampleRate(rate)))
}

/// Resolve a usable *output* stream config: the device default first, then fall
/// back to scanning `supported_output_configs()` — the mirror of
/// [`resolve_input_config`], for codecs that reject the default-output query.
pub(crate) fn resolve_output_config(
    device: &cpal::Device,
) -> Result<cpal::SupportedStreamConfig, AudioError> {
    if let Ok(cfg) = device.default_output_config() {
        return Ok(cfg);
    }
    let ranges: Vec<cpal::SupportedStreamConfigRange> = device
        .supported_output_configs()
        .map_err(|e| AudioError::Device(format!("querying output configs: {e}")))?
        .collect();
    let summaries: Vec<ConfigRange> = ranges
        .iter()
        .map(|r| ConfigRange {
            channels: r.channels(),
            min_rate: r.min_sample_rate().0,
            max_rate: r.max_sample_rate().0,
            format_pref: format_pref(r.sample_format()),
        })
        .collect();
    debug!(
        ?summaries,
        "default output config unavailable; choosing from supported ranges"
    );
    let (idx, rate) = choose_config(&summaries, 48_000).ok_or_else(|| {
        AudioError::Device("device reports no usable output stream configs".into())
    })?;
    Ok(ranges[idx].with_sample_rate(cpal::SampleRate(rate)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(name: &str, kind: DeviceKind, is_default: bool) -> DeviceInfo {
        DeviceInfo {
            name: name.to_string(),
            kind,
            sample_rate: 48_000,
            channels: 2,
            is_default,
            looks_like_radio: is_radio_codec(name),
        }
    }

    fn fixture() -> Vec<DeviceInfo> {
        vec![
            dev("MacBook Pro Microphone", DeviceKind::Input, true),
            dev("USB Audio CODEC", DeviceKind::Input, false),
            dev("MacBook Pro Speakers", DeviceKind::Output, true),
            dev("USB Audio CODEC", DeviceKind::Output, false),
        ]
    }

    #[test]
    fn radio_codec_detection() {
        assert!(is_radio_codec("USB Audio CODEC"));
        assert!(is_radio_codec("usb audio codec "));
        assert!(!is_radio_codec("MacBook Pro Microphone"));
    }

    #[test]
    fn select_by_index_within_kind() {
        let devices = fixture();
        let d = select(&devices, DeviceKind::Input, "1").unwrap();
        assert_eq!(d.name, "USB Audio CODEC");
        let d = select(&devices, DeviceKind::Output, "0").unwrap();
        assert_eq!(d.name, "MacBook Pro Speakers");
        assert!(select(&devices, DeviceKind::Input, "9").is_none());
    }

    #[test]
    fn select_by_substring_case_insensitive() {
        let devices = fixture();
        let d = select(&devices, DeviceKind::Input, "codec").unwrap();
        assert_eq!(d.name, "USB Audio CODEC");
        let d = select(&devices, DeviceKind::Output, "speakers").unwrap();
        assert_eq!(d.name, "MacBook Pro Speakers");
        assert!(select(&devices, DeviceKind::Input, "nonexistent").is_none());
    }

    #[test]
    fn choose_config_prefers_containing_preferred_rate() {
        let ranges = [
            // 44.1k-only f32
            ConfigRange {
                channels: 2,
                min_rate: 44_100,
                max_rate: 44_100,
                format_pref: 2,
            },
            // contains 48k but i16
            ConfigRange {
                channels: 2,
                min_rate: 8_000,
                max_rate: 48_000,
                format_pref: 1,
            },
        ];
        // Containing the preferred rate beats a nicer sample format.
        assert_eq!(choose_config(&ranges, 48_000), Some((1, 48_000)));
    }

    #[test]
    fn choose_config_prefers_format_then_fewer_channels() {
        let ranges = [
            ConfigRange {
                channels: 8,
                min_rate: 48_000,
                max_rate: 48_000,
                format_pref: 2,
            },
            ConfigRange {
                channels: 2,
                min_rate: 48_000,
                max_rate: 48_000,
                format_pref: 2,
            },
            ConfigRange {
                channels: 2,
                min_rate: 48_000,
                max_rate: 48_000,
                format_pref: 1,
            },
        ];
        // Both contain 48k; f32 wins over i16; 2ch wins over 8ch.
        assert_eq!(choose_config(&ranges, 48_000), Some((1, 48_000)));
    }

    #[test]
    fn choose_config_falls_back_to_max_rate() {
        let ranges = [ConfigRange {
            channels: 1,
            min_rate: 8_000,
            max_rate: 44_100,
            format_pref: 1,
        }];
        // 48k not available -> use the range's max.
        assert_eq!(choose_config(&ranges, 48_000), Some((0, 44_100)));
    }

    #[test]
    fn choose_config_skips_unsupported_formats() {
        let ranges = [ConfigRange {
            channels: 2,
            min_rate: 48_000,
            max_rate: 48_000,
            format_pref: -1,
        }];
        assert_eq!(choose_config(&ranges, 48_000), None);
        assert_eq!(choose_config(&[], 48_000), None);
    }

    #[test]
    fn preferred_input_prefers_radio_codec() {
        let devices = fixture();
        assert_eq!(preferred_input(&devices).unwrap().name, "USB Audio CODEC");
        // Without the codec, falls back to the system default.
        let no_radio: Vec<DeviceInfo> = devices
            .iter()
            .filter(|d| !d.looks_like_radio)
            .cloned()
            .collect();
        assert_eq!(
            preferred_input(&no_radio).unwrap().name,
            "MacBook Pro Microphone"
        );
        assert!(preferred_input(&[]).is_none());
    }
}
