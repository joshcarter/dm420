//! The logbook.
//!
//! Stores QSOs, distinguishes own vs. peer origin, exposes the `logbook/entries`
//! stream and `logbook/query` command, and merges gossiped peer contacts as a
//! G-set keyed by `QsoId`. ADIF import/export is the amateur-radio lingua franca
//! (OVERVIEW §7 open decision #4).
//!
//! Specs: `docs/log_book.md`, `docs/message-catalog.md` §7.
