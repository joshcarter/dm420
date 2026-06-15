//! Digital signal processing.
//!
//! Produces the `SpectrumRow` stream that drives the waterslide's rotated FFT.
//! Pure compute — no async, no I/O — so it stays trivially testable and free of
//! the bus/tokio dependency.
//!
//! Specs: `docs/waterslide_panel.md`, `docs/message-catalog.md` §2.
