//! FT8/FT4 CRC-14 (port of ft8_lib `crc.c`; algorithm ours, polynomial is spec).
//!
//! The 14-bit CRC is computed over the 77-bit payload zero-extended to 82 bits.

const CRC_WIDTH: u32 = 14;
const CRC_POLY: u16 = 0x2757; // FT8/FT4 CRC-14 polynomial (spec)
const TOPBIT: u16 = 1 << (CRC_WIDTH - 1);

/// Compute the 14-bit CRC over the first `num_bits` of `message` (MSB first).
pub fn compute_crc(message: &[u8], num_bits: usize) -> u16 {
    let mut remainder: u16 = 0;
    let mut idx_byte = 0usize;
    for idx_bit in 0..num_bits {
        if idx_bit % 8 == 0 {
            remainder ^= (message[idx_byte] as u16) << (CRC_WIDTH - 8);
            idx_byte += 1;
        }
        if remainder & TOPBIT != 0 {
            remainder = (remainder << 1) ^ CRC_POLY;
        } else {
            remainder <<= 1;
        }
    }
    remainder & ((TOPBIT << 1) - 1)
}

/// Extract the stored CRC from a packed 91-bit message (12 bytes).
pub fn extract_crc(a91: &[u8]) -> u16 {
    (((a91[9] & 0x07) as u16) << 11) | ((a91[10] as u16) << 3) | ((a91[11] >> 5) as u16)
}

/// Copy `payload` (10 bytes / 77 bits) into `a91` (12 bytes) and append the CRC,
/// producing the 91-bit message fed to the LDPC encoder.
pub fn add_crc(payload: &[u8], a91: &mut [u8; 12]) {
    a91[..10].copy_from_slice(&payload[..10]);
    a91[9] &= 0xF8;
    a91[10] = 0;
    a91[11] = 0;
    let checksum = compute_crc(a91, 96 - 14); // 82 bits
    a91[9] |= (checksum >> 11) as u8;
    a91[10] = (checksum >> 3) as u8;
    a91[11] = (checksum << 5) as u8;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_then_extract_roundtrips() {
        let payload = [0x12u8, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0, 0x11, 0x22];
        let mut a91 = [0u8; 12];
        add_crc(&payload, &mut a91);
        let stored = extract_crc(&a91);
        // Recompute the way the decoder does.
        let mut check = a91;
        check[9] &= 0xF8;
        check[10] = 0;
        check[11] = 0;
        let calc = compute_crc(&check, 96 - 14);
        assert_eq!(stored, calc, "stored CRC must match recomputed CRC");
        assert_ne!(stored, 0);
    }

    #[test]
    fn detects_bit_flip() {
        let payload = [0u8, 1, 2, 3, 4, 5, 6, 7, 8, 9];
        let mut a91 = [0u8; 12];
        add_crc(&payload, &mut a91);
        let good = extract_crc(&a91);
        let mut p2 = payload;
        p2[3] ^= 0x10;
        let mut b91 = [0u8; 12];
        add_crc(&p2, &mut b91);
        assert_ne!(good, extract_crc(&b91));
    }
}
