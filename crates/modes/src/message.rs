//! FT8/FT4 message packing/unpacking — 77-bit payload <-> human text.
//!
//! Ported from ft8_lib `message.c` (algorithm ours, bit layouts are the spec).
//! We implement the message types that make up essentially all normal traffic:
//! Standard (i3 = 1/2: CQ / calls / grids / reports), Free text (0.0),
//! Non-standard calls (i3 = 4), and Telemetry (0.5). Callsign hashing uses a
//! session-lived table so hashed `<CALL>` references resolve across slots.

use crate::crc;
use crate::text::{charn, dd_to_int, int_to_dd, nchar, trim, Table};
use std::collections::HashMap;

const MAX22: u32 = 4_194_304;
const NTOKENS: u32 = 2_063_592;
const MAXGRID4: u16 = 32_400;

/// Message category (from the i3/n3 type bits).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum MessageType {
    FreeText,
    DxPedition,
    EuVhf,
    ArrlFd,
    Telemetry,
    Contesting,
    Standard,
    ArrlRtty,
    NonStdCall,
    Wwrof,
    Unknown,
}

/// Session callsign hash table: maps the 22-bit hash to the resolved callsign so
/// later `<...>`/hashed references can be filled in. Cheap and small.
#[derive(Default)]
pub struct CallHash {
    by_n22: HashMap<u32, String>,
}

impl CallHash {
    pub fn new() -> CallHash {
        CallHash::default()
    }

    /// Hash a (trimmed) callsign and remember it. Returns (n22, n12, n10), or
    /// None if it contains characters outside the callsign alphabet.
    fn save(&mut self, callsign: &str) -> Option<(u32, u16, u16)> {
        let mut n58: u64 = 0;
        let mut count = 0;
        for c in callsign.chars().take(11) {
            let j = nchar(c, Table::AlphanumSpaceSlash);
            if j < 0 {
                return None;
            }
            n58 = 38u64.wrapping_mul(n58).wrapping_add(j as u64);
            count += 1;
        }
        while count < 11 {
            n58 = 38u64.wrapping_mul(n58); // pad with spaces (index 0)
            count += 1;
        }
        // NB: this multiply overflows u64 by design — must wrap.
        let n22 = ((47_055_833_459u64.wrapping_mul(n58) >> (64 - 22)) & 0x3F_FFFF) as u32;
        let n12 = (n22 >> 10) as u16;
        let n10 = (n22 >> 12) as u16;
        if !callsign.is_empty() {
            self.by_n22.insert(n22, callsign.to_string());
        }
        Some((n22, n12, n10))
    }

    /// Look up a callsign by a truncated hash. `shift` selects the width:
    /// 0 = 22-bit, 10 = 12-bit, 12 = 10-bit.
    fn lookup(&self, hash: u32, shift: u32) -> Option<&str> {
        self.by_n22
            .iter()
            .find(|(n22, _)| (*n22 >> shift) == hash)
            .map(|(_, call)| call.as_str())
    }
}

fn lookup_bracketed(hash: &CallHash, hashval: u32, shift: u32) -> String {
    match hash.lookup(hashval, shift) {
        Some(call) => format!("<{call}>"),
        None => "<...>".to_string(),
    }
}

pub fn get_i3(p: &[u8; 10]) -> u8 {
    (p[9] >> 3) & 0x07
}

pub fn get_n3(p: &[u8; 10]) -> u8 {
    ((p[8] << 2) & 0x04) | ((p[9] >> 6) & 0x03)
}

pub fn get_type(p: &[u8; 10]) -> MessageType {
    use MessageType::*;
    match get_i3(p) {
        0 => match get_n3(p) {
            0 => FreeText,
            1 => DxPedition,
            2 => EuVhf,
            3 | 4 => ArrlFd,
            5 => Telemetry,
            6 => Contesting,
            _ => Unknown,
        },
        1 | 2 => Standard,
        3 => ArrlRtty,
        4 => NonStdCall,
        5 => Wwrof,
        _ => Unknown,
    }
}

/// Decode a payload to display text and its type. Returns None for message types
/// we don't unpack. Updates the hash table with any resolved callsigns.
pub fn decode(p: &[u8; 10], hash: &mut CallHash) -> Option<(String, MessageType)> {
    let msg_type = get_type(p);
    let text = match msg_type {
        MessageType::Standard => {
            let (to, de, extra) = decode_std(p, hash)?;
            join_fields(&to, &de, &extra)
        }
        MessageType::NonStdCall => {
            let (to, de, extra) = decode_nonstd(p, hash);
            join_fields(&to, &de, &extra)
        }
        MessageType::FreeText => decode_free(p),
        MessageType::Telemetry => decode_telemetry_hex(p),
        _ => return None,
    };
    Some((text, msg_type))
}

fn join_fields(f1: &str, f2: &str, f3: &str) -> String {
    let mut s = String::from(f1);
    if !f2.is_empty() {
        s.push(' ');
        s.push_str(f2);
        if !f3.is_empty() {
            s.push(' ');
            s.push_str(f3);
        }
    }
    s
}

// ---- decode side -----------------------------------------------------------

fn decode_std(p: &[u8; 10], hash: &mut CallHash) -> Option<(String, String, String)> {
    let pu: [u32; 10] = std::array::from_fn(|i| p[i] as u32);
    let n29a = (pu[0] << 21) | (pu[1] << 13) | (pu[2] << 5) | (pu[3] >> 3);
    let n29b = ((pu[3] & 0x07) << 26) | (pu[4] << 18) | (pu[5] << 10) | (pu[6] << 2) | (pu[7] >> 6);
    let ir = (p[7] & 0x20) >> 5;
    let igrid4 = (((pu[7] & 0x1F) << 10) | (pu[8] << 2) | (pu[9] >> 6)) as u16;
    let i3 = get_i3(p);

    let call_to = unpack28(n29a >> 1, (n29a & 1) as u8, i3, hash)?;
    let call_de = unpack28(n29b >> 1, (n29b & 1) as u8, i3, hash)?;
    let extra = unpackgrid(igrid4, ir);
    Some((call_to, call_de, extra))
}

fn unpack28(n28: u32, ip: u8, i3: u8, hash: &mut CallHash) -> Option<String> {
    // Special tokens and CQ variants.
    if n28 < NTOKENS {
        if n28 <= 2 {
            return Some(["DE", "QRZ", "CQ"][n28 as usize].to_string());
        }
        if n28 <= 1002 {
            return Some(format!("CQ {}", int_to_dd((n28 - 3) as i32, 3, false)));
        }
        if n28 <= 532_443 {
            let mut n = n28 - 1003;
            let mut aaaa = [b' '; 4];
            for i in (0..4).rev() {
                aaaa[i] = charn((n % 27) as i32, Table::LettersSpace) as u8;
                n /= 27;
            }
            let s: String = aaaa.iter().map(|&b| b as char).collect();
            return Some(format!("CQ {}", s.trim_start_matches(' ')));
        }
        return None;
    }

    let n28 = n28 - NTOKENS;
    if n28 < MAX22 {
        // 22-bit hashed callsign.
        return Some(lookup_bracketed(hash, n28, 0));
    }

    // Standard base callsign.
    let mut n = n28 - MAX22;
    let mut call = [0u8; 6];
    call[5] = charn((n % 27) as i32, Table::LettersSpace) as u8;
    n /= 27;
    call[4] = charn((n % 27) as i32, Table::LettersSpace) as u8;
    n /= 27;
    call[3] = charn((n % 27) as i32, Table::LettersSpace) as u8;
    n /= 27;
    call[2] = charn((n % 10) as i32, Table::Numeric) as u8;
    n /= 10;
    call[1] = charn((n % 36) as i32, Table::Alphanum) as u8;
    n /= 36;
    call[0] = charn((n % 37) as i32, Table::AlphanumSpace) as u8;
    let raw: String = call.iter().map(|&b| b as char).collect();

    // Prefix work-arounds (Swaziland 3D0->3DA0, Guinea Qx->3X), then trim.
    let mut result = if raw.starts_with("3D0") && call[3] != b' ' {
        format!("3DA0{}", raw[3..].trim())
    } else if call[0] == b'Q' && (call[1] as char).is_ascii_uppercase() {
        format!("3X{}", raw[1..].trim())
    } else {
        raw.trim().to_string()
    };

    if result.len() < 3 {
        return None;
    }
    if ip != 0 {
        match i3 {
            1 => result.push_str("/R"),
            2 => result.push_str("/P"),
            _ => return None,
        }
    }
    hash.save(&result);
    Some(result)
}

fn unpackgrid(igrid4: u16, ir: u8) -> String {
    if igrid4 <= MAXGRID4 {
        let mut s = String::new();
        if ir > 0 {
            s.push_str("R ");
        }
        let mut n = igrid4;
        let d3 = (b'0' + (n % 10) as u8) as char;
        n /= 10;
        let d2 = (b'0' + (n % 10) as u8) as char;
        n /= 10;
        let l1 = (b'A' + (n % 18) as u8) as char;
        n /= 18;
        let l0 = (b'A' + (n % 18) as u8) as char;
        s.push(l0);
        s.push(l1);
        s.push(d2);
        s.push(d3);
        s
    } else {
        let irpt = (igrid4 - MAXGRID4) as i32;
        match irpt {
            1 => String::new(),
            2 => "RRR".to_string(),
            3 => "RR73".to_string(),
            4 => "73".to_string(),
            _ => {
                let mut s = String::new();
                if ir > 0 {
                    s.push('R');
                }
                s.push_str(&int_to_dd(irpt - 35, 2, true));
                s
            }
        }
    }
}

fn decode_nonstd(p: &[u8; 10], hash: &mut CallHash) -> (String, String, String) {
    let n12 = ((p[0] as u16) << 4) | ((p[1] as u16) >> 4);
    let mut n58: u64 = ((p[1] & 0x0F) as u64) << 54;
    n58 |= (p[2] as u64) << 46;
    n58 |= (p[3] as u64) << 38;
    n58 |= (p[4] as u64) << 30;
    n58 |= (p[5] as u64) << 22;
    n58 |= (p[6] as u64) << 14;
    n58 |= (p[7] as u64) << 6;
    n58 |= (p[8] as u64) >> 2;
    let iflip = (p[8] >> 1) & 0x01;
    let nrpt = ((p[8] & 0x01) << 1) | (p[9] >> 7);
    let icq = (p[9] >> 6) & 0x01;

    let call_decoded = unpack58(n58, hash);
    let call_3 = lookup_bracketed(hash, n12 as u32, 10);

    let (call_1, call_2) = if iflip != 0 {
        (call_decoded.clone(), call_3)
    } else {
        (call_3, call_decoded.clone())
    };

    let (call_to, extra) = if icq == 0 {
        let extra = match nrpt {
            1 => "RRR",
            2 => "RR73",
            3 => "73",
            _ => "",
        };
        (call_1, extra.to_string())
    } else {
        ("CQ".to_string(), String::new())
    };
    (call_to, call_2, extra)
}

fn unpack58(mut n58: u64, hash: &mut CallHash) -> String {
    let mut c11 = [0u8; 11];
    for i in (0..11).rev() {
        c11[i] = charn((n58 % 38) as i32, Table::AlphanumSpaceSlash) as u8;
        n58 /= 38;
    }
    let raw: String = c11.iter().map(|&b| b as char).collect();
    let call = trim(&raw).to_string();
    if call.len() >= 3 {
        hash.save(&call);
    }
    call
}

fn decode_telemetry(p: &[u8; 10]) -> [u8; 9] {
    let mut t = [0u8; 9];
    let mut carry = 0u8;
    for i in 0..9 {
        t[i] = (carry << 7) | (p[i] >> 1);
        carry = p[i] & 0x01;
    }
    t
}

fn decode_free(p: &[u8; 10]) -> String {
    let mut b71 = decode_telemetry(p);
    let mut c14 = [0u8; 13];
    for slot in c14.iter_mut().rev() {
        let mut rem: u16 = 0;
        for b in b71.iter_mut() {
            rem = (rem << 8) | (*b as u16);
            *b = (rem / 42) as u8;
            rem %= 42;
        }
        *slot = charn(rem as i32, Table::Full) as u8;
    }
    let raw: String = c14.iter().map(|&b| b as char).collect();
    trim(&raw).to_string()
}

fn decode_telemetry_hex(p: &[u8; 10]) -> String {
    let b71 = decode_telemetry(p);
    let mut s = String::with_capacity(18);
    for b in b71 {
        s.push_str(&format!("{b:02X}"));
    }
    s
}

// ---- encode side (used to synthesize test signals) -------------------------

fn parse_cq_modifier(s: &str) -> i32 {
    let bytes = s.as_bytes();
    let (mut nnum, mut nlet, mut m) = (0, 0, 0i32);
    for i in 3..8 {
        match bytes.get(i) {
            None | Some(b' ') => break,
            Some(&c) if c.is_ascii_digit() => nnum += 1,
            Some(&c) if c.is_ascii_uppercase() => {
                nlet += 1;
                m = 27 * m + (c - b'A' + 1) as i32;
            }
            _ => return -1,
        }
    }
    if nnum == 3 && nlet == 0 {
        s[3..].parse::<i32>().unwrap_or(-1)
    } else if nnum == 0 && nlet <= 4 {
        1000 + m
    } else {
        -1
    }
}

fn pack_basecall(callsign: &str, length: usize) -> i32 {
    let cb = callsign.as_bytes();
    if length <= 2 {
        return -1;
    }
    let mut c6 = [b' '; 6];
    let is_letter = |b: u8| (b as char).is_ascii_alphabetic();
    let is_digit = |b: u8| (b as char).is_ascii_digit();
    if callsign.starts_with("3DA0") && length > 4 && length <= 7 {
        c6[..3].copy_from_slice(b"3D0");
        c6[3..3 + (length - 4)].copy_from_slice(&cb[4..length]);
    } else if callsign.starts_with("3X") && cb.len() > 2 && is_letter(cb[2]) && length <= 7 {
        c6[0] = b'Q';
        c6[1..1 + (length - 2)].copy_from_slice(&cb[2..length]);
    } else if cb.len() > 2 && is_digit(cb[2]) && length <= 6 {
        c6[..length].copy_from_slice(&cb[..length]);
    } else if cb.len() > 1 && is_digit(cb[1]) && length <= 5 {
        c6[1..1 + length].copy_from_slice(&cb[..length]);
    }

    let i0 = nchar(c6[0] as char, Table::AlphanumSpace);
    let i1 = nchar(c6[1] as char, Table::Alphanum);
    let i2 = nchar(c6[2] as char, Table::Numeric);
    let i3 = nchar(c6[3] as char, Table::LettersSpace);
    let i4 = nchar(c6[4] as char, Table::LettersSpace);
    let i5 = nchar(c6[5] as char, Table::LettersSpace);
    if i0 < 0 || i1 < 0 || i2 < 0 || i3 < 0 || i4 < 0 || i5 < 0 {
        return -1;
    }
    let mut n = i0;
    n = n * 36 + i1;
    n = n * 10 + i2;
    n = n * 27 + i3;
    n = n * 27 + i4;
    n * 27 + i5
}

/// Returns (n28, ip) where n28 < 0 signals failure.
fn pack28(callsign: &str, hash: &mut CallHash) -> (i32, u8) {
    match callsign {
        "DE" => return (0, 0),
        "QRZ" => return (1, 0),
        "CQ" => return (2, 0),
        _ => {}
    }
    let length = callsign.len();
    if callsign.starts_with("CQ ") && length < 8 {
        let v = parse_cq_modifier(callsign);
        if v < 0 {
            return (-1, 0);
        }
        return (3 + v, 0);
    }

    let mut ip = 0u8;
    let mut length_base = length;
    if callsign.ends_with("/P") || callsign.ends_with("/R") {
        ip = 1;
        length_base = length - 2;
    }
    let n28 = pack_basecall(callsign, length_base);
    if n28 >= 0 {
        if hash.save(callsign).is_none() {
            return (-1, 0);
        }
        return ((NTOKENS + MAX22) as i32 + n28, ip);
    }
    if (3..=11).contains(&length) {
        if let Some((n22, _, _)) = hash.save(callsign) {
            return ((NTOKENS + n22) as i32, 0);
        }
    }
    (-1, 0)
}

fn packgrid(extra: &str) -> u16 {
    if extra.is_empty() {
        return MAXGRID4 + 1;
    }
    match extra {
        "RRR" => return MAXGRID4 + 2,
        "RR73" => return MAXGRID4 + 3,
        "73" => return MAXGRID4 + 4,
        _ => {}
    }
    let b = extra.as_bytes();
    if b.len() >= 4
        && (b'A'..=b'R').contains(&b[0])
        && (b'A'..=b'R').contains(&b[1])
        && b[2].is_ascii_digit()
        && b[3].is_ascii_digit()
    {
        let mut g = (b[0] - b'A') as u16;
        g = g * 18 + (b[1] - b'A') as u16;
        g = g * 10 + (b[2] - b'0') as u16;
        g = g * 10 + (b[3] - b'0') as u16;
        return g;
    }
    if let Some(rest) = extra.strip_prefix('R') {
        let dd = dd_to_int(rest, 3);
        ((MAXGRID4 as i32 + 35 + dd) as u16) | 0x8000
    } else {
        let dd = dd_to_int(extra, 3);
        (MAXGRID4 as i32 + 35 + dd) as u16
    }
}

/// Encode a standard (type 1/2) message into a 77-bit payload. None on failure.
pub fn encode_std(
    call_to: &str,
    call_de: &str,
    extra: &str,
    hash: &mut CallHash,
) -> Option<[u8; 10]> {
    let (n28a, ipa) = pack28(call_to, hash);
    let (n28b, ipb) = pack28(call_de, hash);
    if n28a < 0 || n28b < 0 {
        return None;
    }
    let mut i3 = 1u8;
    if call_to.ends_with("/P") || call_de.ends_with("/P") {
        i3 = 2;
        if call_to.ends_with("/R") || call_de.ends_with("/R") {
            return None;
        }
    }
    let igrid4 = packgrid(extra);

    let mut n29a = ((n28a as u32) << 1) | ipa as u32;
    let n29b = ((n28b as u32) << 1) | ipb as u32;
    if call_to.ends_with("/R") {
        n29a |= 1;
    } else if call_to.ends_with("/P") {
        n29a |= 1;
        i3 = 2;
    }

    let mut p = [0u8; 10];
    p[0] = (n29a >> 21) as u8;
    p[1] = (n29a >> 13) as u8;
    p[2] = (n29a >> 5) as u8;
    p[3] = ((n29a << 3) as u8) | (n29b >> 26) as u8;
    p[4] = (n29b >> 18) as u8;
    p[5] = (n29b >> 10) as u8;
    p[6] = (n29b >> 2) as u8;
    p[7] = ((n29b << 6) as u8) | (igrid4 >> 10) as u8;
    p[8] = (igrid4 >> 2) as u8;
    p[9] = ((igrid4 << 6) as u8) | (i3 << 3);
    Some(p)
}

/// Encode free text (up to 13 chars from the Full alphabet). None on failure.
pub fn encode_free(textmsg: &str) -> Option<[u8; 10]> {
    if textmsg.len() > 13 {
        return None;
    }
    let bytes = textmsg.as_bytes();
    let mut b71 = [0u8; 9];
    for idx in 0..13 {
        let c = if idx < bytes.len() {
            bytes[idx] as char
        } else {
            ' '
        };
        let cid = nchar(c, Table::Full);
        if cid < 0 {
            return None;
        }
        let mut rem = cid as u16;
        for i in (0..9).rev() {
            rem += b71[i] as u16 * 42;
            b71[i] = (rem & 0xff) as u8;
            rem >>= 8;
        }
    }
    let mut p = encode_telemetry(&b71);
    p[9] = 0; // i3.n3 = 0.0
    Some(p)
}

fn encode_telemetry(telemetry: &[u8; 9]) -> [u8; 10] {
    let mut p = [0u8; 10];
    let mut carry = 0u8;
    for i in (0..9).rev() {
        p[i] = (telemetry[i] << 1) | (carry >> 7);
        carry = telemetry[i] & 0x80;
    }
    p
}

fn tok(toks: &[&str], i: usize) -> String {
    toks.get(i).copied().unwrap_or("").to_string()
}

fn is_cq_modifier_tok(t: &str) -> bool {
    t == "DX"
        || (t.len() == 3 && t.bytes().all(|b| b.is_ascii_digit()))
        || (!t.is_empty() && t.len() <= 4 && t.bytes().all(|b| b.is_ascii_uppercase()))
}

/// Parse a message string and encode it to a 77-bit payload: try the standard
/// type (CQ / call to / call de / grid|report), falling back to free text.
/// Input is upper-cased. None if it can't be encoded at all.
pub fn encode_message(text: &str, hash: &mut CallHash) -> Option<[u8; 10]> {
    let up = text.to_uppercase();
    let toks: Vec<&str> = up.split_whitespace().collect();
    if !toks.is_empty() {
        let (to, de, extra) = if toks[0] == "CQ" {
            if toks.len() >= 2 && is_cq_modifier_tok(toks[1]) {
                (format!("CQ {}", toks[1]), tok(&toks, 2), tok(&toks, 3))
            } else {
                ("CQ".to_string(), tok(&toks, 1), tok(&toks, 2))
            }
        } else {
            (tok(&toks, 0), tok(&toks, 1), tok(&toks, 2))
        };
        if let Some(p) = encode_std(&to, &de, &extra, hash) {
            return Some(p);
        }
    }
    encode_free(&up)
}

/// Build the 91-bit (12-byte) message = payload + CRC, for the LDPC encoder.
pub fn payload_with_crc(payload: &[u8; 10]) -> [u8; 12] {
    let mut a91 = [0u8; 12];
    crc::add_crc(payload, &mut a91);
    a91
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip_std(to: &str, de: &str, extra: &str, expect: &str) {
        let mut h = CallHash::new();
        let p = encode_std(to, de, extra, &mut h).expect("encode");
        assert_eq!(get_type(&p), MessageType::Standard);
        let mut h2 = CallHash::new();
        let (text, _ty) = decode(&p, &mut h2).expect("decode");
        assert_eq!(text, expect);
    }

    #[test]
    fn standard_messages_roundtrip() {
        roundtrip_std("CQ", "K1ABC", "FN42", "CQ K1ABC FN42");
        roundtrip_std("W9XYZ", "K1ABC", "FN42", "W9XYZ K1ABC FN42");
        roundtrip_std("W9XYZ", "K1ABC", "-09", "W9XYZ K1ABC -09");
        roundtrip_std("W9XYZ", "K1ABC", "R-09", "W9XYZ K1ABC R-09");
        roundtrip_std("W9XYZ", "K1ABC", "RR73", "W9XYZ K1ABC RR73");
        roundtrip_std("W9XYZ", "K1ABC", "", "W9XYZ K1ABC");
    }

    #[test]
    fn cq_dx_modifier_roundtrips() {
        roundtrip_std("CQ DX", "K1ABC", "FN42", "CQ DX K1ABC FN42");
    }

    #[test]
    fn free_text_roundtrips() {
        let p = encode_free("HELLO WORLD").expect("encode");
        assert_eq!(get_type(&p), MessageType::FreeText);
        let mut h = CallHash::new();
        let (text, ty) = decode(&p, &mut h).unwrap();
        assert_eq!(ty, MessageType::FreeText);
        assert_eq!(text, "HELLO WORLD");
    }

    #[test]
    fn crc_is_stable() {
        let mut h = CallHash::new();
        let p = encode_std("CQ", "K1ABC", "FN42", &mut h).unwrap();
        let a91 = payload_with_crc(&p);
        let stored = crc::extract_crc(&a91);
        let mut chk = a91;
        chk[9] &= 0xF8;
        chk[10] = 0;
        chk[11] = 0;
        assert_eq!(stored, crc::compute_crc(&chk, 82));
    }
}
