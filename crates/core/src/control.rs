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
///
/// The device and mode live under a *single* `Mutex` (mirroring [`RigControl`]'s
/// `Mutex<SerialConfig>`) so [`snapshot`](Self::snapshot) always observes a
/// consistent `(input, proto)` pair. Splitting them across two mutexes would let
/// a concurrent [`set`](Self::set) interleave between the supervisor's two reads
/// and hand back a torn `(new_input, old_proto)` pairing at the *old* generation
/// — slipping past the generation check and running a session on the wrong
/// device/mode combination.
pub struct AudioControl {
    cfg: Mutex<(Option<String>, Protocol)>,
    generation: AtomicU64,
}

impl AudioControl {
    pub(crate) fn new(input: Option<String>, proto: Protocol) -> Self {
        Self {
            cfg: Mutex::new((input, proto)),
            generation: AtomicU64::new(0),
        }
    }

    /// Replace the capture device and/or mode. The capture session restarts with
    /// them promptly.
    pub fn set(&self, input: Option<String>, proto: Protocol) {
        *self.cfg.lock().unwrap() = (input, proto);
        self.generation.fetch_add(1, Ordering::Release);
    }

    pub(crate) fn snapshot(&self) -> (Option<String>, Protocol) {
        self.cfg.lock().unwrap().clone()
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
}

/// Shared, live-editable TX audio output device. The audio-TX service reads it
/// before each over, so a device change made in the UI applies on re-lock without
/// a restart (mirrors [`AudioControl`], but TX output is independent of capture so
/// it gets its own control).
pub struct TxControl {
    output: Mutex<Option<String>>,
}

impl TxControl {
    pub(crate) fn new(output: Option<String>) -> Self {
        Self {
            output: Mutex::new(output),
        }
    }

    /// Replace the TX output device; picked up on the next over.
    pub fn set(&self, output: Option<String>) {
        *self.output.lock().unwrap() = output;
    }

    pub(crate) fn snapshot(&self) -> Option<String> {
        self.output.lock().unwrap().clone()
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
    pub tx: Option<std::sync::Arc<TxControl>>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;
    use std::time::Instant;

    #[test]
    fn sleep_or_changed_returns_early_when_generation_already_moved() {
        // gen_now != start_gen on the very first check, so it must return before
        // the first sleep step — well under the (deliberately huge) duration.
        let start = Instant::now();
        sleep_or_changed(Duration::from_secs(10), || 1, 0);
        assert!(
            start.elapsed() < Duration::from_millis(200),
            "expected immediate return on generation mismatch, took {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn sleep_or_changed_returns_early_mid_sleep() {
        // Generation moves after the first ~100ms step; the loop must notice and
        // bail well before the full 10s duration.
        let counter = Arc::new(AtomicU64::new(0));
        let bump = Arc::clone(&counter);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(120));
            bump.store(1, Ordering::Release);
        });
        let start = Instant::now();
        sleep_or_changed(
            Duration::from_secs(10),
            || counter.load(Ordering::Acquire),
            0,
        );
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_secs(1),
            "expected early return after generation moved, took {elapsed:?}"
        );
    }

    #[test]
    fn sleep_or_changed_sleeps_full_duration_when_generation_stable() {
        let start = Instant::now();
        sleep_or_changed(Duration::from_millis(150), || 0, 0);
        assert!(
            start.elapsed() >= Duration::from_millis(150),
            "expected full sleep when generation is stable, took {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn audio_control_set_bumps_generation_and_snapshots_atomically() {
        let c = AudioControl::new(Some("alpha".into()), Protocol::Ft8);
        assert_eq!(c.generation(), 0);
        assert_eq!(c.snapshot(), (Some("alpha".into()), Protocol::Ft8));

        c.set(Some("bravo".into()), Protocol::Ft4);
        assert_eq!(c.generation(), 1);
        assert_eq!(c.snapshot(), (Some("bravo".into()), Protocol::Ft4));
    }

    /// Regression test for the torn-snapshot race: a reader must never observe a
    /// mixed `(input, proto)` pair while a writer flips between two known-good
    /// pairings. With the single-mutex design this holds by construction; the
    /// previous split-mutex layout could surface `(alpha, Ft4)` / `(bravo, Ft8)`.
    #[test]
    fn audio_control_snapshot_is_never_torn_under_concurrent_set() {
        let c = Arc::new(AudioControl::new(Some("alpha".into()), Protocol::Ft8));
        let writer = Arc::clone(&c);
        let w = std::thread::spawn(move || {
            for i in 0..50_000 {
                if i % 2 == 0 {
                    writer.set(Some("alpha".into()), Protocol::Ft8);
                } else {
                    writer.set(Some("bravo".into()), Protocol::Ft4);
                }
            }
        });
        for _ in 0..50_000 {
            let snap = c.snapshot();
            assert!(
                snap == (Some("alpha".into()), Protocol::Ft8)
                    || snap == (Some("bravo".into()), Protocol::Ft4),
                "torn snapshot observed: {snap:?}"
            );
        }
        w.join().unwrap();
    }
}
