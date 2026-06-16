//! Live, thread-shared producer configuration.
//!
//! [`spawn`](crate::spawn) returns a [`CoreControl`] the UI holds. The rig and
//! audio supervisors run forever on their own threads; rather than tearing them
//! down to reconfigure, each one reads a shared config snapshot on every
//! (re)connect and watches a *generation* counter. The UI edits the config and
//! bumps the generation (via [`RigControl::set`] / [`AudioControl::set`]); the
//! supervisor notices and reconnects with the new settings — promptly, because
//! the generation also cuts the reconnect backoff short (see [`sleep_or_changed`]).

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use modes::Protocol;

use crate::SerialConfig;

/// Shared, live-editable rig connection settings.
pub struct RigControl {
    cfg: Mutex<SerialConfig>,
    generation: AtomicU64,
}

impl RigControl {
    pub(crate) fn new(cfg: SerialConfig) -> Self {
        Self {
            cfg: Mutex::new(cfg),
            generation: AtomicU64::new(0),
        }
    }

    /// Replace the rig settings. The supervisor reconnects with them promptly.
    pub fn set(&self, cfg: SerialConfig) {
        *self.cfg.lock().unwrap() = cfg;
        self.generation.fetch_add(1, Ordering::Release);
    }

    pub(crate) fn snapshot(&self) -> SerialConfig {
        self.cfg.lock().unwrap().clone()
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
}

/// Shared, live-editable audio capture settings (device + on-air mode).
pub struct AudioControl {
    input: Mutex<Option<String>>,
    proto: Mutex<Protocol>,
    generation: AtomicU64,
}

impl AudioControl {
    pub(crate) fn new(input: Option<String>, proto: Protocol) -> Self {
        Self {
            input: Mutex::new(input),
            proto: Mutex::new(proto),
            generation: AtomicU64::new(0),
        }
    }

    /// Replace the capture device and/or mode. The capture session restarts with
    /// them promptly.
    pub fn set(&self, input: Option<String>, proto: Protocol) {
        *self.input.lock().unwrap() = input;
        *self.proto.lock().unwrap() = proto;
        self.generation.fetch_add(1, Ordering::Release);
    }

    pub(crate) fn snapshot(&self) -> (Option<String>, Protocol) {
        (self.input.lock().unwrap().clone(), *self.proto.lock().unwrap())
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
}

/// Handle for live reconfiguration of the running producers from the UI. Each
/// field is present only when that producer is running (e.g. `audio` is `None`
/// for WAV replay or rig-only setups). Cheap to clone (the controls live behind
/// `Arc`).
#[derive(Clone, Default)]
pub struct CoreControl {
    pub rig: Option<std::sync::Arc<RigControl>>,
    pub audio: Option<std::sync::Arc<AudioControl>>,
}

/// Why a supervisor's connected session ended — distinguishes a real fault from
/// a user-requested reconfigure so the supervisor can report the right health.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum StopReason {
    /// The device stopped responding / disconnected.
    LinkLost,
    /// The shared config changed; reconnect with the new settings.
    Reconfigured,
}

/// Sleep up to `dur`, returning early if `gen_now()` moves off `start_gen`. Lets
/// a config edit interrupt reconnect backoff so changes apply without waiting out
/// a long backoff while a device is absent.
pub(crate) fn sleep_or_changed(dur: Duration, gen_now: impl Fn() -> u64, start_gen: u64) {
    let step = Duration::from_millis(100);
    let mut slept = Duration::ZERO;
    while slept < dur {
        if gen_now() != start_gen {
            return;
        }
        let nap = step.min(dur - slept);
        std::thread::sleep(nap);
        slept += nap;
    }
}
