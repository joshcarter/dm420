//! Type translation between Joel's vendored `rig` crate and dm420's catalog
//! (`bus::types`). Kept in one place so the seam is easy to audit.

use bus::types as t;
use rig::{Mode, RigState as RigCatState};

/// dm420 `RigMode` → Joel's CAT `Mode`, plus whether the data sub-mode is implied.
/// FT8/FT4 run in the `*Data` sidebands; Joel's rig models the data flag
/// separately (`DA`), so the bool is returned for the adapter to apply if it ever
/// drives the data menu. For now the sideband is what we set.
pub fn to_rig_mode(m: t::RigMode) -> (Mode, bool) {
    match m {
        t::RigMode::Usb => (Mode::Usb, false),
        t::RigMode::Lsb => (Mode::Lsb, false),
        t::RigMode::UsbData => (Mode::Usb, true),
        t::RigMode::LsbData => (Mode::Lsb, true),
        t::RigMode::Cw => (Mode::Cw, false),
    }
}

/// Joel's CAT `Mode` → dm420 `RigMode`. The catalog has no FM/AM/FSK variants;
/// they map to the nearest sideband (FT8 operation never sees them). The data
/// flag is not present in `IF;` state, so sideband modes report non-data here.
pub fn from_rig_mode(m: Mode) -> t::RigMode {
    match m {
        Mode::Usb => t::RigMode::Usb,
        Mode::Lsb => t::RigMode::Lsb,
        Mode::Cw | Mode::CwR => t::RigMode::Cw,
        Mode::Fm | Mode::Am | Mode::Fsk | Mode::FskR => t::RigMode::Usb,
    }
}

/// Joel's `RigState` (from one `IF;` snapshot) → dm420 `RigState`.
///
/// `Meters{s_unit,alc,swr}` has no source in `IF;`, so it starts empty; reading
/// the S-meter/SWR/ALC needs extra CAT queries (`SM`, etc.) — a follow-up.
pub fn to_bus_rig_state(radio: t::RadioId, s: &RigCatState) -> t::RigState {
    t::RigState {
        radio,
        vfo: t::AbsHz(s.freq_hz),
        rig_mode: s.mode.map(from_rig_mode).unwrap_or(t::RigMode::Usb),
        ptt: s.tx,
        meters: t::Meters::default(),
    }
}
