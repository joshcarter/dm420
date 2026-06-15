//! The band scanner.
//!
//! A *strategy for single-receiver hardware*: on demand it blocks TX and
//! time-slices the receiver across selected bands, decoding one interval per band
//! and reporting per-band heard/unworked counts. Behind the same capability
//! abstraction, a multi-receiver SDR back-end answers "what's open" with parallel
//! receivers instead.
//!
//! Specs: `docs/band_scanner.md`, `docs/message-catalog.md` §8. Owner: Josh (N0JDC).
