//! macOS App Nap suppression — a transmitter-safety measure.
//!
//! DM420 keys a real transmitter, and the keying lifecycle does **not** run on the
//! egui paint loop. Key-up, playing the waveform, and the all-important key-**down**
//! at the end live on background threads: the tokio workers in [`core::tx`] (whose
//! "playback done?" poll is a `tokio::time::sleep`) and the dedicated `audio-out`
//! and `rig-actor` OS threads (the latter holding the 15 s `PTT_WATCHDOG` backstop).
//!
//! When the window is backgrounded, macOS **App Nap** throttles the *whole process*:
//! it coalesces timers out by seconds-to-minutes and drops thread QoS to near-idle.
//! That stretches both the playback-done poll and the watchdog, so a TX started just
//! before you tab away can leave the carrier keyed until you tab back (and the
//! process un-naps). It's also why the waterfall "pauses" when unfocused — same
//! throttle, visible (see `JOELS_ROADMAP.md`). A frozen paint loop alone could never
//! hold PTT; the background threads getting throttled is what does.
//!
//! The fix is to declare, for the whole life of the process, that we're doing
//! user-initiated, latency-critical work — via an `NSProcessInfo` *activity
//! assertion*. Holding it disables App Nap **and** timer coalescing process-wide, so
//! the unkey path, the PTT watchdog, and FT8/FT4 slot timing all keep running at full
//! speed in the background. The returned [`NapGuard`] must be held for the program's
//! lifetime (like the logging guard); the activity ends when it drops — or, since
//! both quit paths `std::process::exit` (bypassing Drop), simply when the process
//! dies. Either way the transmitter is never left throttled.
//!
//! Off macOS this is a no-op (App Nap is a macOS feature) and the objc2 crates are
//! not even compiled in — the dependency is `cfg(target_os = "macos")`-gated.

#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(not(target_os = "macos"))]
pub use other::*;

#[cfg(target_os = "macos")]
mod macos {
    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2_foundation::{NSActivityOptions, NSObjectProtocol, NSProcessInfo, NSString};

    /// Owns the live `NSProcessInfo` activity assertion. While this is alive, macOS
    /// will not nap or coalesce timers on us; dropping it ends the activity. Keep it
    /// for the whole session.
    pub struct NapGuard(#[allow(dead_code)] Retained<ProtocolObject<dyn NSObjectProtocol>>);

    /// Take a process-lifetime activity assertion that keeps macOS from throttling
    /// the app when it's backgrounded.
    ///
    /// `UserInitiated` opts out of App Nap (and idle *system* sleep, so the Mac won't
    /// sleep mid-QSO); `LatencyCritical` disables timer coalescing. Both matter here:
    /// the PTT key-down and the slot timing ride on background timers that App Nap
    /// would otherwise stretch, leaving the rig keyed past the end of an over.
    pub fn prevent_app_nap() -> NapGuard {
        let options = NSActivityOptions::UserInitiated | NSActivityOptions::LatencyCritical;
        let reason = NSString::from_str(
            "DM420 keys a live transmitter; App Nap / timer coalescing must not throttle PTT and slot timing",
        );
        // `processInfo` and `beginActivityWithOptions:reason:` are both safe in the
        // objc2 bindings; the call never returns nil, so the token is always valid.
        let token = NSProcessInfo::processInfo().beginActivityWithOptions_reason(options, &reason);
        tracing::info!(
            "macOS App Nap suppressed for the session (NSProcessInfo activity assertion held)"
        );
        NapGuard(token)
    }
}

#[cfg(not(target_os = "macos"))]
mod other {
    /// No-op guard off macOS (App Nap is macOS-only).
    pub struct NapGuard;

    /// No-op off macOS: there is no App Nap to suppress.
    pub fn prevent_app_nap() -> NapGuard {
        NapGuard
    }
}
