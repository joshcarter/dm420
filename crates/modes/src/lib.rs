//! On-air protocol implementations ("modes").
//!
//! FT8/FT4 encode + decode integration (decoder strategy is OVERVIEW §7 open
//! decision #1), the `OverAirMode` -> `RigMode` mapping, and the mode-owned
//! calling-frequency reference data (`calling_freq(mode, band)`). The decode
//! family split (`Slotted` vs `Streaming`) keeps PSK31/RTTY a sibling, not a
//! rewrite.
//!
//! Specs: `docs/message-catalog.md` §3–§4, `OVERVIEW.md` §7. Owner: Joel (W4LL).
