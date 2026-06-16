//! LDPC(174,91) encoder and soft-decision belief-propagation decoder.
//!
//! Ported from ft8_lib (`encode.c`, `ldpc.c`); the sum-product algorithm and bit
//! layout are reproduced in Rust, the parity/generator matrices live in
//! [`crate::constants`]. LLR convention (matching our demodulator): a codeword
//! value is log(p(bit=1) / p(bit=0)), so a positive value favors a 1.

use crate::constants::{LDPC_GENERATOR, LDPC_MN, LDPC_NM, LDPC_NUM_ROWS};

pub const M: usize = 83; // parity checks
pub const N: usize = 174; // codeword bits
pub const K: usize = 91; // payload+CRC bits
pub const K_BYTES: usize = 12;
pub const N_BYTES: usize = 22;

/// Encode a 91-bit message (12 bytes, MSB first) into a 174-bit codeword
/// (22 bytes). Systematic: the first 91 bits are the message, the next 83 are
/// LDPC parity.
pub fn encode174(message: &[u8; K_BYTES], codeword: &mut [u8; N_BYTES]) {
    for (j, slot) in codeword.iter_mut().enumerate() {
        *slot = if j < K_BYTES { message[j] } else { 0 };
    }

    let mut col_mask: u8 = 0x80 >> (K % 8); // first parity bit position (bit 91)
    let mut col_idx = K_BYTES - 1;

    for row in LDPC_GENERATOR.iter() {
        let mut nsum: u8 = 0;
        for j in 0..K_BYTES {
            nsum ^= (message[j] & row[j]).count_ones() as u8 & 1;
        }
        if nsum & 1 != 0 {
            codeword[col_idx] |= col_mask;
        }
        col_mask >>= 1;
        if col_mask == 0 {
            col_mask = 0x80;
            col_idx += 1;
        }
    }
}

/// Count parity-check failures for a hard-decision codeword (0 = valid).
fn ldpc_check(plain: &[u8; N]) -> i32 {
    let mut errors = 0;
    for m in 0..M {
        let mut x = 0u8;
        for i in 0..LDPC_NUM_ROWS[m] as usize {
            x ^= plain[LDPC_NM[m][i] as usize - 1];
        }
        if x != 0 {
            errors += 1;
        }
    }
    errors
}

/// Belief-propagation decode of 174 LLRs. Returns the hard-decision bits and the
/// minimum parity-error count reached (0 means a valid codeword was found).
pub fn bp_decode(codeword: &[f32; N], max_iters: usize) -> ([u8; N], i32) {
    let mut tov = [[0.0f32; 3]; N];
    let mut toc = [[0.0f32; 7]; M];
    let mut plain = [0u8; N];
    let mut min_errors = M as i32;

    for _ in 0..max_iters {
        // Hard decision from current beliefs.
        let mut plain_sum = 0i32;
        for n in 0..N {
            let s = codeword[n] + tov[n][0] + tov[n][1] + tov[n][2];
            plain[n] = (s > 0.0) as u8;
            plain_sum += plain[n] as i32;
        }
        if plain_sum == 0 {
            break; // all-zeros is prohibited
        }

        let errors = ldpc_check(&plain);
        if errors < min_errors {
            min_errors = errors;
            if errors == 0 {
                break;
            }
        }

        // Variable -> check messages.
        for m in 0..M {
            for n_idx in 0..LDPC_NUM_ROWS[m] as usize {
                let n = LDPC_NM[m][n_idx] as usize - 1;
                let mut tnm = codeword[n];
                for m_idx in 0..3 {
                    if LDPC_MN[n][m_idx] as usize - 1 != m {
                        tnm += tov[n][m_idx];
                    }
                }
                toc[m][n_idx] = fast_tanh(-tnm / 2.0);
            }
        }

        // Check -> variable messages.
        for n in 0..N {
            for m_idx in 0..3 {
                let m = LDPC_MN[n][m_idx] as usize - 1;
                let mut tmn = 1.0f32;
                for n_idx in 0..LDPC_NUM_ROWS[m] as usize {
                    if LDPC_NM[m][n_idx] as usize - 1 != n {
                        tmn *= toc[m][n_idx];
                    }
                }
                tov[n][m_idx] = -2.0 * fast_atanh(tmn);
            }
        }
    }

    (plain, min_errors)
}

fn fast_tanh(x: f32) -> f32 {
    if x < -4.97 {
        return -1.0;
    }
    if x > 4.97 {
        return 1.0;
    }
    let x2 = x * x;
    let a = x * (945.0 + x2 * (105.0 + x2));
    let b = 945.0 + x2 * (420.0 + x2 * 15.0);
    a / b
}

fn fast_atanh(x: f32) -> f32 {
    let x2 = x * x;
    let a = x * (945.0 + x2 * (-735.0 + x2 * 64.0));
    let b = 945.0 + x2 * (-1050.0 + x2 * 225.0);
    a / b
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codeword_bits(message: &[u8; K_BYTES]) -> [u8; N] {
        let mut cw = [0u8; N_BYTES];
        encode174(message, &mut cw);
        let mut bits = [0u8; N];
        for (i, bit) in bits.iter_mut().enumerate() {
            *bit = (cw[i / 8] >> (7 - (i % 8))) & 1;
        }
        bits
    }

    #[test]
    fn encoded_codeword_passes_parity() {
        let msg = [
            0x83u8, 0x29, 0xCE, 0x11, 0xBF, 0x31, 0xEA, 0xF5, 0x09, 0xF2, 0x7F, 0xC0,
        ];
        let bits = codeword_bits(&msg);
        assert_eq!(ldpc_check(&bits), 0);
    }

    #[test]
    fn bp_decodes_noisy_codeword() {
        let msg = [
            0x11u8, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0x10,
        ];
        let bits = codeword_bits(&msg);
        // Strong LLRs, then corrupt a handful of bits with wrong-sign LLRs.
        let mut llr = [0.0f32; N];
        for i in 0..N {
            llr[i] = if bits[i] == 1 { 4.0 } else { -4.0 };
        }
        for &flip in &[3usize, 20, 57, 99, 140, 173] {
            llr[flip] = -llr[flip];
        }
        let (plain, errors) = bp_decode(&llr, 50);
        assert_eq!(errors, 0, "should converge to a valid codeword");
        assert_eq!(plain, bits, "should recover the original codeword");
    }
}
