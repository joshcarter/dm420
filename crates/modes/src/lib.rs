//! From-scratch FT8/FT4 decoder and encoder (Phase 3 of poc-cli-plan.md).
//!
//! This is a pure-Rust implementation of the FT8/FT4 digital modes — no C, no
//! FFI. The sync/demod/LDPC/unpack chain is ours; the forward FFT is the
//! `realfft`/`rustfft` crate (see `fft.rs`). The algorithms were reverse-
//! engineered / ported from the MIT-licensed C reference decoder `ft8_lib`
//! (Kārlis Goba); the
//! fixed protocol constant tables are the WSJT-X (Franke/Taylor) on-air spec.
//! **See [ATTRIBUTION.md](../ATTRIBUTION.md) — we have MIT and spec attribution
//! obligations to keep.**
//!
//! Pipeline (see the modules): audio → STFT waterfall → Costas sync + soft-symbol
//! LLRs → LDPC belief-propagation → CRC check → message unpack to text. The
//! encoder runs it backwards to synthesize test signals so the whole chain is
//! self-verifying without a radio.

mod cohere;
mod constants;
mod crc;
mod decode;
mod encode;
mod fft;
mod ldpc;
mod message;
mod osd;
mod slot;
mod text;
mod waterfall;

pub use decode::{Decode, decode, decode_streaming};
pub use encode::synth_ft8;
pub use message::MessageType;

/// Synthesize a full FT8 slot of audio for a text message (e.g.
/// `"CQ K1ABC FN42"`) at the given audio frequency and sample rate. Returns None
/// if the message can't be encoded. Useful for generating test/known signals.
pub fn synth_message(text: &str, audio_freq_hz: f32, sample_rate: u32) -> Option<Vec<f32>> {
    let mut hash = message::CallHash::new();
    let payload = message::encode_message(text, &mut hash)?;
    Some(encode::synth_ft8(&payload, audio_freq_hz, sample_rate))
}
pub use slot::{current_slot_start, seconds_until_next_slot, slot_period, time_into_slot};
pub use waterfall::Protocol;
