//! Cross-platform audio I/O (cpal).
//!
//! Captures the receiver passband for the decoder and renders generated TX audio
//! for the rig side. Co-located conceptually with `rig` (the codec lives next to
//! the CAT driver) but split out so the device handling is testable on its own.
//!
//! Add `cpal.workspace = true` when wiring real devices.
//!
//! Specs: `OVERVIEW.md` §3.2, `docs/message-catalog.md` §2 / §6.
