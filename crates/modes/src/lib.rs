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

mod ap;
mod arrl_fd;
mod cohere;
mod cohere_ft4;
mod constants;
mod crc;
mod decode;
mod encode;
mod fft;
mod ldpc;
mod message;
mod osd;
mod subtract;
mod slot;
mod text;
mod waterfall;

pub use decode::{Decode, decode, decode_streaming};
pub use encode::{synth_ft4, synth_ft8};
pub use message::{CallHash, MessageType};

/// Synthesize a full slot of audio for a text message (e.g. `"CQ K1ABC FN42"`) in
/// the given `protocol`, at the given audio frequency and sample rate. Returns
/// `None` if the message can't be encoded. Drives the live TX path and generates
/// known signals for tests.
pub fn synth_message(
    text: &str,
    protocol: Protocol,
    audio_freq_hz: f32,
    sample_rate: u32,
) -> Option<Vec<f32>> {
    let mut hash = message::CallHash::new();
    let payload = message::encode_message(text, &mut hash)?;
    Some(encode::synth(&payload, protocol, audio_freq_hz, sample_rate))
}
pub use slot::{current_slot_start, seconds_until_next_slot, slot_period, time_into_slot};
pub use waterfall::Protocol;

#[cfg(test)]
mod tests {
    use super::*;

    /// `synth_message` is the public TX entry (`core::tx` calls it): same text,
    /// a different waveform per protocol, and each round-trips under its own mode.
    #[test]
    fn synth_message_is_mode_aware_and_round_trips() {
        let text = "CQ K1ABC FN42";
        let ft8 = synth_message(text, Protocol::Ft8, 1500.0, 12000).expect("ft8 synth");
        let ft4 = synth_message(text, Protocol::Ft4, 1500.0, 12000).expect("ft4 synth");
        // Mode-aware lengths: 15 s vs 7.5 s @ 12 kHz.
        assert_eq!(ft8.len(), 180_000, "FT8 slot length");
        assert_eq!(ft4.len(), 90_000, "FT4 slot length");
        // Each decodes back under its own protocol.
        assert!(
            decode(&ft8, 12000, Protocol::Ft8).iter().any(|d| d.message == text),
            "FT8 synth_message should round-trip"
        );
        assert!(
            decode(&ft4, 12000, Protocol::Ft4).iter().any(|d| d.message == text),
            "FT4 synth_message should round-trip"
        );
    }

    /// The ARRL Field Day exchange (WSJT-X message type 0.3/0.4) survives the whole
    /// TX path — encode → synth → decode — in both protocols. Regression guard for
    /// the "FD exchange won't encode" bug, where it used to pack as a standard
    /// signal report and silently drop the section.
    #[test]
    fn field_day_exchange_synth_round_trips() {
        let text = "K1ABC N0JDC 3A CO";
        for protocol in [Protocol::Ft8, Protocol::Ft4] {
            let audio = synth_message(text, protocol, 1500.0, 12000)
                .unwrap_or_else(|| panic!("{protocol:?} FD synth"));
            assert!(
                decode(&audio, 12000, protocol).iter().any(|d| d.message == text),
                "{protocol:?} FD synth_message should round-trip"
            );
        }
    }
}
