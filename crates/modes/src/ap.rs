//! A-priori (AP) decoding — CRC-gated hypothesis retries after a blind miss.
//!
//! On a crowded / contest band, WSJT-X's `jt9 -d3` recovers far more than its
//! blind decoder by *injecting a-priori information* into the LDPC decode: when a
//! candidate fails to decode blind, it re-runs the decoder with the bits of a
//! **hypothesized field** clamped to strong log-likelihoods, then lets the 14-bit
//! CRC accept or reject the result. We proved (see `decoder-sensitivity` notes)
//! that DM420's *blind* decoder is already at jt9's blind ceiling; this module is
//! the separate AP capability that accounts for the rest of jt9's contest yield.
//!
//! Mechanism (mirrors `ft8b.f90`'s `iaptype`/`apmask`/`apmag` logic):
//!   1. Blind decode runs first and unchanged. AP only runs on a blind *miss*, so
//!      it can only ADD decodes — never change or regress a blind result.
//!   2. Each [`Hypothesis`] fixes a known **bit range** of the 77-bit payload (e.g.
//!      "the addressed call is CQ FD", or "...is my callsign") to values read from
//!      DM420's *own* encoder — so the fix is self-consistent with our decoder and
//!      needs no transcription of WSJT-X's bit tables.
//!   3. For a hypothesis we overlay `apmag = 1.1·max|llr|` (sign per bit: a `1`
//!      bit → +apmag, a `0` bit → −apmag, matching `bp_decode`'s `s>0 ⇒ bit 1`)
//!      onto those positions of the candidate's LLR vector, re-run `bp_decode`, and
//!      accept only if `verify_codeword`'s CRC passes. The CRC is the false-decode
//!      gate, exactly as on the blind path.
//!
//! Gated behind `DM420_AP` (default OFF) — opt-in until measured and trusted.
//! `DM420_AP_MYCALL=<call>` enables the directed (MyCall) hypotheses.

use crate::decode::verify_codeword;
use crate::ldpc::{N, bp_decode};
use crate::message::{CallHash, encode_message};
use crate::waterfall::Protocol;
use std::sync::{OnceLock, RwLock};

/// LDPC iterations for an AP retry. A touch higher than the blind path's 25: the
/// strong a-priori clamps give belief propagation a firm anchor, so the extra
/// sweeps reliably converge the remaining free bits.
const LDPC_ITERS_AP: usize = 30;

/// Whether AP decoding runs at all (default ON). `DM420_AP=0` is the explicit off
/// switch (A/B testing / a fallback to the exact blind path).
pub(crate) fn ap_enabled() -> bool {
    static EN: OnceLock<bool> = OnceLock::new();
    *EN.get_or_init(|| std::env::var("DM420_AP").map(|v| v != "0").unwrap_or(true))
}

/// The operator's callsign pushed from the app (via `core::CoreControl::set_mycall`,
/// sourced from the GUI-committed `StationConfig`). `DM420_AP_MYCALL` overrides it
/// (a test/CLI escape hatch). Absent ⇒ only the context-free CQ hypotheses run.
static MYCALL: RwLock<Option<String>> = RwLock::new(None);

/// Set the operator callsign for the directed (MyCall) hypothesis. Idempotent;
/// normalizes case/whitespace and treats empty as unset. The per-protocol
/// hypothesis caches are keyed on the active call, so they pick this up on next use.
pub fn set_mycall(call: Option<String>) {
    let norm = call.map(|c| c.trim().to_uppercase()).filter(|s| !s.is_empty());
    *MYCALL.write().unwrap() = norm;
}

/// The `DM420_AP_MYCALL` override, read once. Wins over the pushed call so a test
/// or CLI run can pin a callsign regardless of app state.
fn env_mycall() -> Option<&'static str> {
    static E: OnceLock<Option<String>> = OnceLock::new();
    E.get_or_init(|| {
        std::env::var("DM420_AP_MYCALL")
            .ok()
            .map(|c| c.trim().to_uppercase())
            .filter(|s| !s.is_empty())
    })
    .as_deref()
}

/// The callsign the MyCall hypothesis should use right now: env override, else the
/// value pushed via [`set_mycall`].
fn current_mycall() -> Option<String> {
    match env_mycall() {
        Some(c) => Some(c.to_string()),
        None => MYCALL.read().unwrap().clone(),
    }
}

/// One a-priori hypothesis: a set of payload bit positions clamped to known values.
struct Hypothesis {
    /// Codeword bit positions (0..77, the systematic message bits) that this
    /// hypothesis fixes.
    mask: Vec<usize>,
    /// Target value (0/1) at each masked position, in codeword (whitened) space.
    bits: Vec<u8>,
    #[allow(dead_code)]
    label: &'static str,
}

/// The 77 systematic message bits of a payload in **codeword space** (whitened for
/// FT4, identity for FT8), MSB-first — i.e. exactly the bits `bp_decode` produces
/// and `verify_codeword` consumes, so `log174[i]` drives bit `i` here.
fn payload_to_codeword_bits(payload: &[u8; 10], protocol: Protocol) -> [u8; 77] {
    let mut a = *payload;
    if let Some(xor) = protocol.whitening() {
        for (b, x) in a.iter_mut().zip(xor.iter()) {
            *b ^= *x;
        }
    }
    let mut bits = [0u8; 77];
    for (i, slot) in bits.iter_mut().enumerate() {
        *slot = (a[i / 8] >> (7 - (i % 8))) & 1;
    }
    bits
}

/// Build a hypothesis by encoding `template` with DM420's own encoder and fixing
/// the payload bit positions in `ranges` to the encoded values. Reading the values
/// from our encoder (not a hardcoded table) keeps the clamp self-consistent with
/// our decoder and correct under FT4 whitening.
fn from_template(template: &str, ranges: &[(usize, usize)], protocol: Protocol, label: &'static str) -> Option<Hypothesis> {
    let mut hash = CallHash::new();
    let payload = encode_message(template, &mut hash)?;
    let bits = payload_to_codeword_bits(&payload, protocol);
    let mut mask = Vec::new();
    let mut vals = Vec::new();
    for &(lo, hi) in ranges {
        mask.extend(lo..hi);
        vals.extend(bits[lo..hi].iter().copied());
    }
    Some(Hypothesis { mask, bits: vals, label })
}

// Payload bit ranges (0-indexed into the 77 systematic bits), matching DM420's
// layout: `decode_std` reads `n29a` (call_to + its flag) from bits [0,29); the i3
// message-type bits are the last three, [74,77). These mirror WSJT-X's
// apmask(1:29) and apmask(75:77).
const CALL_TO: (usize, usize) = (0, 29); // addressed call (28) + R/hash flag (1)
const I3: (usize, usize) = (74, 77); // message-type bits

/// The hypothesis set for `protocol` and operator call `mycall` (`None` ⇒ CQ only).
/// Ordered cheapest/most-likely first: the context-free CQ hypotheses always run;
/// the directed MyCall hypothesis is added when a callsign is known.
fn hypotheses(protocol: Protocol, mycall: Option<&str>) -> Vec<Hypothesis> {
    let mut hs = Vec::new();
    // CQ FD — the dominant context-free case on a Field Day band. Fixes the
    // "CQ FD" addressed-call marker and the standard message type; the caller and
    // grid stay free for the LDPC decoder to recover.
    if let Some(h) = from_template("CQ FD K1ABC FN42", &[CALL_TO, I3], protocol, "cq-fd") {
        hs.push(h);
    }
    // Plain CQ — non-FD callers on the same band.
    if let Some(h) = from_template("CQ K1ABC FN42", &[CALL_TO, I3], protocol, "cq") {
        hs.push(h);
    }
    // Directed: messages addressed to the operator (replies to our CQ, exchanges,
    // RR73). Fix only the addressed call (not the type), so one hypothesis covers
    // both standard replies and FD exchanges sent to us.
    if let Some(mycall) = mycall {
        if let Some(h) = from_template(&format!("{mycall} K1ABC FN42"), &[CALL_TO], protocol, "mycall") {
            hs.push(h);
        }
    }
    hs
}

/// Try every AP hypothesis against a candidate's LLR vector. Returns the first
/// CRC-valid payload, or `None`. `log174` is the candidate's normalized blind LLRs
/// (left unmodified — each hypothesis works on a clamped copy).
pub(crate) fn try_ap(log174: &[f32; N], protocol: Protocol) -> Option<[u8; 10]> {
    // A clamp magnitude that dominates the observed soft information without
    // wholly erasing it (WSJT-X uses 1.1·max|llr|).
    let apmag = 1.1 * log174.iter().fold(0.0f32, |m, &v| m.max(v.abs()));
    if apmag <= 0.0 {
        return None;
    }
    with_hypotheses(protocol, |hyps| {
        for h in hyps {
            let mut llr = *log174;
            for (&pos, &bit) in h.mask.iter().zip(h.bits.iter()) {
                llr[pos] = if bit == 1 { apmag } else { -apmag };
            }
            let (plain, errors) = bp_decode(&llr, LDPC_ITERS_AP);
            if errors == 0 {
                if let Some(p) = verify_codeword(protocol, &plain) {
                    return Some(p);
                }
            }
        }
        None
    })
}

/// Per-protocol hypothesis cache, keyed on the MyCall it was built for.
type HypCache = RwLock<Option<(Option<String>, Vec<Hypothesis>)>>;

/// Run `f` over the per-protocol hypothesis set for the *current* MyCall. The CQ
/// hypotheses never change, but the directed one tracks [`current_mycall`], so the
/// cache is keyed on the active call and rebuilt (cheaply — a couple
/// `encode_message` calls) only when the operator's callsign changes.
fn with_hypotheses<R>(protocol: Protocol, f: impl FnOnce(&[Hypothesis]) -> R) -> R {
    let want = current_mycall();
    let cell = hyp_cell(protocol);
    {
        let r = cell.read().unwrap();
        if let Some((have, hyps)) = r.as_ref() {
            if *have == want {
                return f(hyps);
            }
        }
    }
    let hyps = hypotheses(protocol, want.as_deref());
    let mut w = cell.write().unwrap();
    *w = Some((want, hyps));
    f(&w.as_ref().unwrap().1)
}

fn hyp_cell(protocol: Protocol) -> &'static HypCache {
    static FT8: OnceLock<HypCache> = OnceLock::new();
    static FT4: OnceLock<HypCache> = OnceLock::new();
    match protocol {
        Protocol::Ft8 => FT8.get_or_init(|| RwLock::new(None)),
        Protocol::Ft4 => FT4.get_or_init(|| RwLock::new(None)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crc;
    use crate::ldpc::{N_BYTES, encode174};
    use crate::message::CallHash;

    /// The CQ-FD hypothesis recovers a `CQ FD` message whose addressed-call LLRs
    /// have been overwritten with the *wrong* sign (as a co-channel collision
    /// would) — exercising both the sign convention (a `1` bit clamps to +apmag)
    /// and the `CALL_TO` bit range. A blind decode of the same corrupted LLRs must
    /// NOT recover it; clamping the call back to "CQ FD" is what closes the gap.
    #[test]
    fn ap_cq_fd_recovers_corrupted_addressed_call() {
        let mut hash = CallHash::new();
        let payload = crate::message::encode_message("CQ FD K1ABC FN42", &mut hash).unwrap();
        // True 174-bit FT8 codeword (no whitening).
        let mut a91 = [0u8; 12];
        crc::add_crc(&payload, &mut a91);
        let mut cw = [0u8; N_BYTES];
        encode174(&a91, &mut cw);
        let truebit = |i: usize| (cw[i / 8] >> (7 - (i % 8))) & 1;

        // Strong, correct LLRs everywhere; then overwrite the addressed-call field
        // (CALL_TO) with high-confidence WRONG-sign values.
        let mut llr = [0.0f32; N];
        for (i, v) in llr.iter_mut().enumerate() {
            *v = if truebit(i) == 1 { 2.0 } else { -2.0 };
        }
        #[allow(clippy::needless_range_loop)] // index drives both llr and truebit
        for i in CALL_TO.0..CALL_TO.1 {
            llr[i] = if truebit(i) == 1 { -6.0 } else { 6.0 };
        }

        // Decode a payload to text (the 3 padding bits past bit 77 are don't-cares,
        // so compare the message, not the raw bytes).
        let text = |p: &[u8; 10]| crate::message::decode(p, &mut CallHash::new()).map(|(t, _)| t);

        // Blind: the confidently-wrong call field defeats it.
        let (plain, _) = bp_decode(&llr, LDPC_ITERS_AP);
        assert_ne!(
            verify_codeword(Protocol::Ft8, &plain).as_ref().and_then(text),
            Some("CQ FD K1ABC FN42".to_string()),
            "blind decode should not survive a corrupted addressed-call field"
        );
        // AP: clamping CALL_TO back to the CQ-FD pattern recovers the message.
        assert_eq!(
            try_ap(&llr, Protocol::Ft8).as_ref().and_then(text),
            Some("CQ FD K1ABC FN42".to_string()),
            "AP CQ-FD hypothesis should recover the message"
        );
    }

    /// AP hypotheses are non-empty, only fix bits within the 77-bit payload, and a
    /// known operator call adds exactly the directed MyCall hypothesis (guards the
    /// bit ranges / whitening and the mycall plumbing).
    #[test]
    fn hypotheses_are_well_formed() {
        for protocol in [Protocol::Ft8, Protocol::Ft4] {
            let cq = hypotheses(protocol, None);
            assert!(!cq.is_empty(), "{protocol:?} should have CQ hypotheses");
            for h in &cq {
                assert!(!h.mask.is_empty());
                assert_eq!(h.mask.len(), h.bits.len());
                assert!(h.mask.iter().all(|&i| i < 77), "{protocol:?} masks within payload");
            }
            let directed = hypotheses(protocol, Some("N0JDC"));
            assert_eq!(directed.len(), cq.len() + 1, "{protocol:?} mycall adds one hypothesis");
            assert!(directed.last().unwrap().mask.iter().all(|&i| i < 77));
        }
    }

    /// `set_mycall` normalizes the call and feeds `current_mycall` (with no
    /// `DM420_AP_MYCALL` override in the test env). Resets the global afterward.
    #[test]
    fn set_mycall_normalizes_and_feeds_current() {
        set_mycall(Some("  n0jdc ".to_string()));
        assert_eq!(current_mycall().as_deref(), Some("N0JDC"));
        set_mycall(Some(String::new()));
        assert_eq!(current_mycall(), None, "empty call is unset");
        set_mycall(None);
    }
}
