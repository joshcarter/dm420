//! Ordered-statistics decoding (OSD) backstop for the LDPC(174, 91) FT8/FT4 code.
//!
//! Runs when belief-propagation fails to converge. Instead of discarding a
//! near-miss, OSD re-derives a *valid* codeword from the most-reliable bits:
//! it picks the 91 most-reliable independent codeword positions (the
//! "most-reliable basis", MRB), takes their hard decisions as the information,
//! and re-encodes. Order-1 reprocessing then flips each MRB bit singly and keeps
//! the re-encodings closest (in reliability-weighted Hamming distance) to the
//! soft input. This is the single biggest weak-signal win that plain `ft8_lib`
//! lacks — see `docs/decoder_sensitivity_plan.md`.
//!
//! Correctness is still gated by CRC at the call site: every codeword OSD returns
//! satisfies parity by construction, so CRC is the only thing distinguishing a
//! real decode from a confident-but-wrong one. We hand back the few best
//! candidates for the caller to CRC-check.

use crate::constants::LDPC_GENERATOR;
use std::sync::OnceLock;

const K: usize = 91; // systematic message bits
const N: usize = 174; // codeword bits
const WORDS: usize = 3; // ceil(174 / 64)

/// How many best (lowest soft-distance) candidates to return for CRC testing.
/// Each extra try multiplies the (tiny) CRC-14 false-accept probability, so keep
/// it small.
const CRC_TRIES: usize = 4;

/// Order-2 reprocessing: also try flipping *pairs* drawn from the `LAMBDA`
/// least-reliable basis bits (where hard decisions are most likely wrong). The
/// full order-2 search over all 91 basis bits is ~4000 re-encodings; restricting
/// to the least-reliable tail captures almost all of the gain for a fraction of
/// the cost (≈`LAMBDA²/2` extra candidates).
const LAMBDA: usize = 20;

/// A 174-bit row (a codeword or a working generator row), LSB-first within words.
#[derive(Clone, Copy)]
struct Row([u64; WORDS]);

impl Row {
    fn zero() -> Self {
        Row([0; WORDS])
    }
    #[inline]
    fn get(&self, n: usize) -> u8 {
        ((self.0[n >> 6] >> (n & 63)) & 1) as u8
    }
    #[inline]
    fn set(&mut self, n: usize) {
        self.0[n >> 6] |= 1u64 << (n & 63);
    }
    #[inline]
    fn xor(&mut self, o: &Row) {
        for i in 0..WORDS {
            self.0[i] ^= o.0[i];
        }
    }
}

/// Coefficient of message bit `i` (0..K) in codeword position `n` (0..N). The
/// code is systematic: positions 0..K are the message bits themselves; positions
/// K.. are parity, with row `n-K` of `LDPC_GENERATOR` (MSB-first per byte, the
/// same layout `encode174` consumes).
fn gen_bit(i: usize, n: usize) -> u8 {
    if n < K {
        (i == n) as u8
    } else {
        let row = &LDPC_GENERATOR[n - K];
        (row[i >> 3] >> (7 - (i & 7))) & 1
    }
}

/// The K generator rows as 174-bit codewords (message `e_i` → its codeword),
/// computed once. Row reduction of these preserves codeword-ness, which is what
/// lets OSD re-encode by XORing a subset of (reduced) rows.
fn generator_rows() -> &'static [Row; K] {
    static ROWS: OnceLock<[Row; K]> = OnceLock::new();
    ROWS.get_or_init(|| {
        let mut rows = [Row::zero(); K];
        for (i, row) in rows.iter_mut().enumerate() {
            for n in 0..N {
                if gen_bit(i, n) != 0 {
                    row.set(n);
                }
            }
        }
        rows
    })
}

/// OSD-1 decode of 174 normalized LLRs (same convention as `bp_decode`: positive
/// favors a 1). Returns up to [`CRC_TRIES`] candidate codewords (174 hard bits
/// each), best-first by soft distance, for the caller to CRC-check. Empty only if
/// the generator is rank-deficient (never, for this code).
pub fn osd_decode(llr: &[f32; N]) -> Vec<[u8; N]> {
    // Hard decisions and reliabilities.
    let mut hard = Row::zero();
    let mut rel = [0.0f32; N];
    for n in 0..N {
        if llr[n] > 0.0 {
            hard.set(n);
        }
        rel[n] = llr[n].abs();
    }

    // Positions, most-reliable first.
    let mut perm: [usize; N] = std::array::from_fn(|i| i);
    perm.sort_by(|&a, &b| rel[b].partial_cmp(&rel[a]).unwrap_or(std::cmp::Ordering::Equal));

    // Gauss-Jordan over GF(2): pull in the most-reliable independent positions as
    // pivots, reducing the working rows so each has a 1 at its own pivot and 0 at
    // every other pivot. Each working row stays a valid codeword throughout.
    let mut rows = *generator_rows();
    let mut pivot_pos = [0usize; K];
    let mut r = 0;
    for &col in perm.iter() {
        let Some(p) = (r..K).find(|&rr| rows[rr].get(col) == 1) else {
            continue; // this position is dependent on the pivots already chosen
        };
        rows.swap(r, p);
        let pivot = rows[r];
        for (rr, row) in rows.iter_mut().enumerate() {
            if rr != r && row.get(col) == 1 {
                row.xor(&pivot);
            }
        }
        pivot_pos[r] = col;
        r += 1;
        if r == K {
            break;
        }
    }
    if r < K {
        return Vec::new(); // unreachable for the FT8 generator (full rank)
    }

    // Order-0: the codeword whose MRB bits equal their hard decisions is the XOR
    // of the reduced rows whose pivot's hard bit is 1.
    let mut base = Row::zero();
    for t in 0..K {
        if hard.get(pivot_pos[t]) == 1 {
            base.xor(&rows[t]);
        }
    }

    let dist = |cw: &Row| -> f32 {
        let mut d = 0.0;
        for (n, &r) in rel.iter().enumerate() {
            if cw.get(n) != hard.get(n) {
                d += r;
            }
        }
        d
    };

    // Order-0, every order-1 single flip, and order-2 pair flips among the
    // least-reliable basis bits (the tail of `rows`, since pivots were selected
    // most-reliable first).
    let mut cands: Vec<(f32, Row)> = Vec::with_capacity(K + 1 + LAMBDA * LAMBDA / 2);
    cands.push((dist(&base), base));
    for row in &rows {
        let mut cw = base;
        cw.xor(row);
        cands.push((dist(&cw), cw));
    }
    let lo = K - LAMBDA;
    for a in lo..K {
        for b in (a + 1)..K {
            let mut cw = base;
            cw.xor(&rows[a]);
            cw.xor(&rows[b]);
            cands.push((dist(&cw), cw));
        }
    }
    cands.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    cands
        .iter()
        .take(CRC_TRIES)
        .map(|(_, cw)| std::array::from_fn(|n| cw.get(n)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ldpc::{K_BYTES, N_BYTES, encode174};

    fn codeword_bits(message: &[u8; K_BYTES]) -> [u8; N] {
        let mut cw = [0u8; N_BYTES];
        encode174(message, &mut cw);
        std::array::from_fn(|i| (cw[i / 8] >> (7 - (i % 8))) & 1)
    }

    #[test]
    fn osd_recovers_true_codeword() {
        let msg = [
            0x83u8, 0x29, 0xCE, 0x11, 0xBF, 0x31, 0xEA, 0xF5, 0x09, 0xF2, 0x7F, 0xC0,
        ];
        let bits = codeword_bits(&msg);

        // The regime OSD exploits: the 91 systematic positions are decided with
        // high confidence and correct (so the most-reliable basis is clean), while
        // a few low-reliability parity bits are wrong-sign. Here the true codeword
        // is provably the soft-distance minimum — flipping any basis bit costs ≥5.0
        // and can recoup at most 12·0.3 = 3.6 — so OSD must return it first. (The
        // real "beats BP on weak signals" proof is the ab_jt9 recall measurement,
        // not a hand-built vector.)
        let mut llr: [f32; N] = std::array::from_fn(|i| if bits[i] == 1 { 5.0 } else { -5.0 });
        for i in K..N {
            llr[i] = if bits[i] == 1 { 0.3 } else { -0.3 }; // parity: low confidence
        }
        for i in (K..N).filter(|i| i % 7 == 0) {
            llr[i] = -llr[i]; // 12 parity bits wrong-sign, still low reliability
        }

        let cands = osd_decode(&llr);
        assert_eq!(cands[0], bits, "OSD's best candidate should be the true codeword");
    }

    #[test]
    fn osd_candidates_are_valid_codewords() {
        // Pure noise in → OSD still returns parity-valid codewords (CRC at the
        // call site rejects them); just confirm the re-encoding is consistent.
        let llr: [f32; N] = std::array::from_fn(|i| if i % 3 == 0 { 0.4 } else { -0.3 });
        for cw in osd_decode(&llr) {
            let msg: [u8; K_BYTES] =
                std::array::from_fn(|b| (0..8).fold(0u8, |acc, k| acc | (cw[b * 8 + k] << (7 - k))));
            assert_eq!(codeword_bits(&msg), cw, "returned bits must be a codeword");
        }
    }
}
