//! UTC time-slot clock for FT8/FT4.
//!
//! FT8 transmits in 15-second slots aligned to the UTC minute (:00/:15/:30/:45);
//! FT4 uses 7.5-second slots. Both divide 60 s evenly, so slot boundaries are
//! `floor(t / period) * period` in Unix time. We take time as an `f64` of Unix
//! seconds to keep this crate free of a time dependency (kenctl passes
//! `chrono::Utc::now()` in).

use crate::waterfall::Protocol;

/// Slot length in seconds.
pub fn slot_period(p: Protocol) -> f64 {
    p.slot_time() as f64
}

/// Unix time of the start of the slot containing `now_unix`.
pub fn current_slot_start(now_unix: f64, p: Protocol) -> f64 {
    let period = slot_period(p);
    (now_unix / period).floor() * period
}

/// Seconds elapsed since the current slot boundary (0 .. period).
pub fn time_into_slot(now_unix: f64, p: Protocol) -> f64 {
    now_unix - current_slot_start(now_unix, p)
}

/// Seconds remaining until the next slot boundary.
pub fn seconds_until_next_slot(now_unix: f64, p: Protocol) -> f64 {
    slot_period(p) - time_into_slot(now_unix, p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ft8_slots_align_to_quarter_minutes() {
        // 1_500_000_000 is an exact multiple of 15 (a slot boundary); +7.3 s in.
        let base = 1_500_000_000.0;
        assert_eq!(base % 15.0, 0.0, "test base must be a slot boundary");
        let t = base + 7.3;
        assert!((time_into_slot(t, Protocol::Ft8) - 7.3).abs() < 1e-6);
        assert!((seconds_until_next_slot(t, Protocol::Ft8) - 7.7).abs() < 1e-6);
        assert_eq!(current_slot_start(t, Protocol::Ft8), base);
    }

    #[test]
    fn ft4_slots_are_half_as_long() {
        assert_eq!(slot_period(Protocol::Ft4), 7.5);
        // 1_500_000_000 is also a multiple of 7.5; +3.0 s into the FT4 slot.
        let t = 1_500_000_000.0 + 3.0;
        assert!((time_into_slot(t, Protocol::Ft4) - 3.0).abs() < 1e-6);
        assert!(time_into_slot(t, Protocol::Ft4) < 7.5);
    }
}
