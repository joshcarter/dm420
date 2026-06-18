//! FT8 character tables and small text helpers (port of ft8_lib `text.c`).
//!
//! Callsigns, grids and free text are packed using a handful of restricted
//! alphabets. `charn` maps an index to a character within a table; `nchar` is the
//! inverse. These must match the spec exactly.

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Table {
    /// space + 0-9 + A-Z + "+-./?" (42 symbols) — free text
    Full,
    /// space + 0-9 + A-Z (37 symbols)
    AlphanumSpace,
    /// 0-9 + A-Z (36 symbols)
    Alphanum,
    /// 0-9 (10 symbols)
    Numeric,
    /// space + A-Z (27 symbols)
    LettersSpace,
    /// space + 0-9 + A-Z + "/" (38 symbols) — callsign hashing
    AlphanumSpaceSlash,
}

const FULL_EXTRA: &[u8] = b"+-./?";

/// Index -> character within `table`.
pub fn charn(mut c: i32, table: Table) -> char {
    use Table::*;
    if table != Alphanum && table != Numeric {
        if c == 0 {
            return ' ';
        }
        c -= 1;
    }
    if table != LettersSpace {
        if c < 10 {
            return (b'0' + c as u8) as char;
        }
        c -= 10;
    }
    if table != Numeric {
        if c < 26 {
            return (b'A' + c as u8) as char;
        }
        c -= 26;
    }
    if table == Full {
        if (c as usize) < FULL_EXTRA.len() {
            return FULL_EXTRA[c as usize] as char;
        }
    } else if table == AlphanumSpaceSlash && c == 0 {
        return '/';
    }
    '_' // unknown; should not happen for valid input
}

/// Character -> index within `table`, or -1 if not representable.
pub fn nchar(c: char, table: Table) -> i32 {
    use Table::*;
    let mut n = 0i32;
    if table != Alphanum && table != Numeric {
        if c == ' ' {
            return n;
        }
        n += 1;
    }
    if table != LettersSpace {
        if c.is_ascii_digit() {
            return n + (c as i32 - '0' as i32);
        }
        n += 10;
    }
    if table != Numeric {
        if c.is_ascii_uppercase() {
            return n + (c as i32 - 'A' as i32);
        }
        n += 26;
    }
    if table == Full {
        if let Some(pos) = FULL_EXTRA.iter().position(|&b| b == c as u8) {
            return n + pos as i32;
        }
    } else if table == AlphanumSpaceSlash && c == '/' {
        return n;
    }
    -1
}

/// Trim leading and trailing spaces.
pub fn trim(s: &str) -> &str {
    s.trim_matches(' ')
}

/// Format `value` as a zero-padded `width`-digit number, optionally with a
/// leading '+' for non-negatives (matches ft8_lib `int_to_dd`).
pub fn int_to_dd(value: i32, width: usize, full_sign: bool) -> String {
    let mut out = String::new();
    let mut v = value;
    if v < 0 {
        out.push('-');
        v = -v;
    } else if full_sign {
        out.push('+');
    }
    let mut divisor = 1i32;
    for _ in 0..width.saturating_sub(1) {
        divisor *= 10;
    }
    while divisor >= 1 {
        let digit = v / divisor;
        out.push((b'0' + digit as u8) as char);
        v -= digit * divisor;
        divisor /= 10;
    }
    out
}

/// Parse a signed integer from the start of `s` (matches ft8_lib `dd_to_int`),
/// reading at most `max_len` characters and stopping at the first non-digit.
pub fn dd_to_int(s: &str, max_len: usize) -> i32 {
    let bytes = s.as_bytes();
    let mut i = 0usize;
    let negative = matches!(bytes.first(), Some(b'-'));
    if negative || matches!(bytes.first(), Some(b'+')) {
        i = 1;
    }
    let mut result = 0i32;
    while i < max_len && i < bytes.len() {
        let c = bytes[i];
        if !c.is_ascii_digit() {
            break;
        }
        result = result * 10 + (c - b'0') as i32;
        i += 1;
    }
    if negative { -result } else { result }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charn_nchar_roundtrip() {
        for (table, count) in [
            (Table::Full, 42),
            (Table::AlphanumSpace, 37),
            (Table::Alphanum, 36),
            (Table::Numeric, 10),
            (Table::LettersSpace, 27),
            (Table::AlphanumSpaceSlash, 38),
        ] {
            for i in 0..count {
                let c = charn(i, table);
                assert_ne!(c, '_', "table has a hole at {i}");
                assert_eq!(nchar(c, table), i, "roundtrip failed for '{c}'");
            }
        }
    }

    #[test]
    fn int_to_dd_formats() {
        assert_eq!(int_to_dd(-7, 2, true), "-07");
        assert_eq!(int_to_dd(5, 2, true), "+05");
        assert_eq!(int_to_dd(123, 3, false), "123");
    }
}
