//! Pure Kenwood CAT encoding/decoding. No I/O — every function here is a total,
//! synchronous transform over strings, so the whole module is exhaustively unit
//! tested without a radio. The serial and mock layers build commands and parse
//! responses exclusively through these functions.
//!
//! CAT framing: ASCII commands terminated by `;`, no CR/LF, no echo. A *set*
//! command (parameters present) gets no acknowledgement; a *get* command (bare
//! mnemonic) returns a `;`-terminated response echoing the mnemonic. The strings
//! handled here are already stripped of the trailing `;`.

use serde::{Deserialize, Serialize};

/// Errors from encoding user input or decoding radio responses.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CodecError {
    #[error("invalid frequency '{0}' (try 14074000, 14.074, or 14074k)")]
    InvalidFrequency(String),
    #[error("frequency out of range: {0} Hz (must be < 100 GHz / 11 digits)")]
    FreqRange(u64),
    #[error("invalid mode '{0}' (lsb|usb|cw|cwr|fm|am|fsk|fskr)")]
    InvalidMode(String),
    #[error("malformed response '{got}' (expected {expected})")]
    BadResponse { expected: String, got: String },
    #[error("response too short: '{0}'")]
    Short(String),
}

/// Operating mode. Kenwood digit mapping per the TS-590/TS-480 CAT spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    Lsb,
    Usb,
    Cw,
    Fm,
    Am,
    Fsk,
    CwR,
    FskR,
}

impl Mode {
    /// The Kenwood `MD` digit. Note digit 8 is unused; FSK-R is 9.
    pub fn to_digit(self) -> char {
        match self {
            Mode::Lsb => '1',
            Mode::Usb => '2',
            Mode::Cw => '3',
            Mode::Fm => '4',
            Mode::Am => '5',
            Mode::Fsk => '6',
            Mode::CwR => '7',
            Mode::FskR => '9',
        }
    }

    pub fn from_digit(c: char) -> Option<Mode> {
        Some(match c {
            '1' => Mode::Lsb,
            '2' => Mode::Usb,
            '3' => Mode::Cw,
            '4' => Mode::Fm,
            '5' => Mode::Am,
            '6' => Mode::Fsk,
            '7' => Mode::CwR,
            '9' => Mode::FskR,
            _ => return None,
        })
    }

    /// Parse a user-typed mode name. FSK and RTTY are accepted as synonyms.
    pub fn parse(s: &str) -> Result<Mode, CodecError> {
        Ok(match s.trim().to_lowercase().as_str() {
            "lsb" => Mode::Lsb,
            "usb" => Mode::Usb,
            "cw" => Mode::Cw,
            "cwr" | "cw-r" => Mode::CwR,
            "fm" => Mode::Fm,
            "am" => Mode::Am,
            "fsk" | "rtty" => Mode::Fsk,
            "fskr" | "fsk-r" | "rtty-r" => Mode::FskR,
            _ => return Err(CodecError::InvalidMode(s.to_string())),
        })
    }

    /// Human-readable label for display.
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Lsb => "LSB",
            Mode::Usb => "USB",
            Mode::Cw => "CW",
            Mode::Fm => "FM",
            Mode::Am => "AM",
            Mode::Fsk => "FSK",
            Mode::CwR => "CW-R",
            Mode::FskR => "FSK-R",
        }
    }
}

/// Which VFO a frequency command targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Vfo {
    A,
    B,
}

impl Vfo {
    fn letter(self) -> char {
        match self {
            Vfo::A => 'A',
            Vfo::B => 'B',
        }
    }
}

/// A snapshot of radio state decoded from an `IF;` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RigState {
    pub freq_hz: u64,
    pub mode: Option<Mode>,
    pub tx: bool,
    pub split: bool,
    pub rit_on: bool,
    pub rit_hz: i32,
    /// True if the trailing IF fields (mode/tx/split/rit) decoded at the expected
    /// offsets. If false, only `freq_hz` is trustworthy and `raw_if` holds the
    /// untouched response so the offsets can be corrected from a real capture.
    pub fields_parsed: bool,
    pub raw_if: String,
}

/// Parse a user-typed frequency into Hz.
///
/// Accepted: bare integer = Hz (`14074000`); bare decimal = MHz (`14.074`);
/// suffixes `hz`, `k`/`khz`, `m`/`mhz`. Underscores are ignored (`14_074_000`).
pub fn parse_frequency(input: &str) -> Result<u64, CodecError> {
    let s = input.trim().to_lowercase().replace('_', "");
    if s.is_empty() {
        return Err(CodecError::InvalidFrequency(input.to_string()));
    }
    let bad = || CodecError::InvalidFrequency(input.to_string());

    let hz: f64 = if let Some(n) = s.strip_suffix("mhz").or_else(|| s.strip_suffix('m')) {
        n.trim().parse::<f64>().map_err(|_| bad())? * 1_000_000.0
    } else if let Some(n) = s.strip_suffix("khz").or_else(|| s.strip_suffix('k')) {
        n.trim().parse::<f64>().map_err(|_| bad())? * 1_000.0
    } else if let Some(n) = s.strip_suffix("hz") {
        n.trim().parse::<f64>().map_err(|_| bad())?
    } else if s.contains('.') {
        s.parse::<f64>().map_err(|_| bad())? * 1_000_000.0
    } else {
        s.parse::<u64>().map_err(|_| bad())? as f64
    };

    if !hz.is_finite() || hz < 0.0 {
        return Err(bad());
    }
    let hz = hz.round() as u64;
    if hz >= 100_000_000_000 {
        return Err(CodecError::FreqRange(hz));
    }
    Ok(hz)
}

/// Format Hz for human display, e.g. `14.074000 MHz`.
pub fn format_freq_mhz(hz: u64) -> String {
    format!("{}.{:06} MHz", hz / 1_000_000, hz % 1_000_000)
}

// --- Command builders ------------------------------------------------------

/// `FA00014074000` / `FB...` — set a VFO frequency (11 zero-padded digits).
pub fn set_freq_cmd(vfo: Vfo, hz: u64) -> String {
    format!("F{}{:011}", vfo.letter(), hz)
}

/// `FA` / `FB` — read a VFO frequency.
pub fn get_freq_cmd(vfo: Vfo) -> String {
    format!("F{}", vfo.letter())
}

/// `MD2` — set mode.
pub fn set_mode_cmd(mode: Mode) -> String {
    format!("MD{}", mode.to_digit())
}

// --- Response parsers ------------------------------------------------------

/// Parse an `FA`/`FB` response into Hz.
pub fn parse_freq_response(resp: &str) -> Result<u64, CodecError> {
    if resp.len() < 3 || !(resp.starts_with("FA") || resp.starts_with("FB")) {
        return Err(CodecError::BadResponse {
            expected: "FA/FB + 11 digits".into(),
            got: resp.to_string(),
        });
    }
    resp[2..]
        .trim()
        .parse::<u64>()
        .map_err(|_| CodecError::BadResponse {
            expected: "FA/FB + 11 digits".into(),
            got: resp.to_string(),
        })
}

/// Parse an `MD` response into a [`Mode`].
pub fn parse_mode_response(resp: &str) -> Result<Mode, CodecError> {
    if resp.len() < 3 || !resp.starts_with("MD") {
        return Err(CodecError::BadResponse {
            expected: "MD + digit".into(),
            got: resp.to_string(),
        });
    }
    Mode::from_digit(resp.as_bytes()[2] as char).ok_or_else(|| CodecError::BadResponse {
        expected: "MD + valid mode digit".into(),
        got: resp.to_string(),
    })
}

// IF response field offsets — indices into the `;`-stripped response string.
// VALIDATED against a real Kenwood TS-590S (ID021) on 2026-06-11 — see
// field-notes.md for the captured response and the cross-checks:
//   capture:  "IF00007074930      000000000020010080"
//   freq  (2..13) = 00007074930  -> matched the FA reply
//   tx    (28)    = 0 (RX)
//   mode  (29)    = 2 (USB)       -> matched the MD reply
//   split (32)    = 1             -> toggled exactly with FT1/FT0
// `freq_hz` (2..13) is unambiguous across all Kenwood rigs.
const IF_FREQ: std::ops::Range<usize> = 2..13; // 11-digit frequency
const IF_RIT: std::ops::Range<usize> = 18..23; // sign + 4 digits
const IF_RIT_ON: usize = 23;
const IF_TX: usize = 28; // 0 = RX, 1 = TX
const IF_MODE: usize = 29;
const IF_SPLIT: usize = 32; // 0 = simplex, 1 = split
const IF_MIN_LEN: usize = 33;

/// Parse an `IF` status response. Always recovers `freq_hz` (rock-solid offset);
/// recovers mode/tx/split/rit only if the response reaches the expected length,
/// setting [`RigState::fields_parsed`] accordingly.
pub fn parse_if_response(resp: &str) -> Result<RigState, CodecError> {
    if !resp.starts_with("IF") {
        return Err(CodecError::BadResponse {
            expected: "IF...".into(),
            got: resp.to_string(),
        });
    }
    if resp.len() < IF_FREQ.end {
        return Err(CodecError::Short(resp.to_string()));
    }
    let freq_hz = resp[IF_FREQ]
        .trim()
        .parse::<u64>()
        .map_err(|_| CodecError::BadResponse {
            expected: "IF frequency digits".into(),
            got: resp.to_string(),
        })?;

    let mut state = RigState {
        freq_hz,
        raw_if: resp.to_string(),
        ..Default::default()
    };

    if resp.len() >= IF_MIN_LEN {
        let b = resp.as_bytes();
        state.rit_hz = resp.get(IF_RIT).and_then(parse_signed).unwrap_or(0);
        state.rit_on = b[IF_RIT_ON] == b'1';
        state.tx = b[IF_TX] == b'1';
        state.mode = Mode::from_digit(b[IF_MODE] as char);
        state.split = b[IF_SPLIT] == b'1';
        state.fields_parsed = true;
    }
    Ok(state)
}

/// Parse a signed field like `+0000` / `-0500`.
fn parse_signed(s: &str) -> Option<i32> {
    let s = s.trim();
    let (sign, digits) = match s.strip_prefix('+') {
        Some(rest) => (1, rest),
        None => match s.strip_prefix('-') {
            Some(rest) => (-1, rest),
            None => (1, s),
        },
    };
    digits.parse::<i32>().ok().map(|v| sign * v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_digit_roundtrip() {
        for m in [
            Mode::Lsb,
            Mode::Usb,
            Mode::Cw,
            Mode::Fm,
            Mode::Am,
            Mode::Fsk,
            Mode::CwR,
            Mode::FskR,
        ] {
            assert_eq!(Mode::from_digit(m.to_digit()), Some(m));
        }
        assert_eq!(Mode::from_digit('8'), None);
        assert_eq!(Mode::from_digit('0'), None);
    }

    #[test]
    fn mode_parse_names() {
        assert_eq!(Mode::parse("usb").unwrap(), Mode::Usb);
        assert_eq!(Mode::parse("USB").unwrap(), Mode::Usb);
        assert_eq!(Mode::parse("cw-r").unwrap(), Mode::CwR);
        assert_eq!(Mode::parse("rtty").unwrap(), Mode::Fsk);
        assert_eq!(Mode::parse("rtty-r").unwrap(), Mode::FskR);
        assert!(Mode::parse("ssb").is_err());
    }

    #[test]
    fn parse_frequency_forms() {
        assert_eq!(parse_frequency("14074000").unwrap(), 14_074_000);
        assert_eq!(parse_frequency("14.074").unwrap(), 14_074_000);
        assert_eq!(parse_frequency("14074k").unwrap(), 14_074_000);
        assert_eq!(parse_frequency("14074khz").unwrap(), 14_074_000);
        assert_eq!(parse_frequency("14.074mhz").unwrap(), 14_074_000);
        assert_eq!(parse_frequency("14.074MHz").unwrap(), 14_074_000);
        assert_eq!(parse_frequency("7074000hz").unwrap(), 7_074_000);
        assert_eq!(parse_frequency("14_074_000").unwrap(), 14_074_000);
        assert_eq!(parse_frequency(" 14.074 ").unwrap(), 14_074_000);
        // FT4 / FT8 dial frequencies should round-trip cleanly.
        assert_eq!(parse_frequency("7.074").unwrap(), 7_074_000);
        assert_eq!(parse_frequency("21.140").unwrap(), 21_140_000);
    }

    #[test]
    fn parse_frequency_rejects_garbage() {
        assert!(parse_frequency("").is_err());
        assert!(parse_frequency("abc").is_err());
        assert!(parse_frequency("14.0.7").is_err());
        assert!(matches!(
            parse_frequency("999999999999"),
            Err(CodecError::FreqRange(_))
        ));
    }

    #[test]
    fn freq_command_format() {
        assert_eq!(set_freq_cmd(Vfo::A, 14_074_000), "FA00014074000");
        assert_eq!(set_freq_cmd(Vfo::B, 7_074_000), "FB00007074000");
        assert_eq!(get_freq_cmd(Vfo::A), "FA");
        assert_eq!(get_freq_cmd(Vfo::B), "FB");
    }

    #[test]
    fn parse_freq_response_ok_and_bad() {
        assert_eq!(parse_freq_response("FA00014074000").unwrap(), 14_074_000);
        assert_eq!(parse_freq_response("FB00007074000").unwrap(), 7_074_000);
        assert!(parse_freq_response("XX00014074000").is_err());
        assert!(parse_freq_response("FA").is_err());
        assert!(parse_freq_response("FAxxxxxxxxxxx").is_err());
    }

    #[test]
    fn mode_response_parse() {
        assert_eq!(parse_mode_response("MD2").unwrap(), Mode::Usb);
        assert_eq!(parse_mode_response("MD3").unwrap(), Mode::Cw);
        assert!(parse_mode_response("MD8").is_err());
        assert!(parse_mode_response("XX2").is_err());
    }

    #[test]
    fn format_freq() {
        assert_eq!(format_freq_mhz(14_074_000), "14.074000 MHz");
        assert_eq!(format_freq_mhz(7_074_000), "7.074000 MHz");
        assert_eq!(format_freq_mhz(0), "0.000000 MHz");
    }

    #[test]
    fn signed_field() {
        assert_eq!(parse_signed("+0000"), Some(0));
        assert_eq!(parse_signed("-0500"), Some(-500));
        assert_eq!(parse_signed("+1234"), Some(1234));
        assert_eq!(parse_signed("abcd"), None);
    }

    /// A synthetic 38-char IF response (incl. trailing `;`, stripped here to 37).
    /// freq=14.074 MHz, RX, USB, simplex. Built to the documented offsets; will be
    /// replaced/confirmed by a real capture per Test 1.2.
    fn synthetic_if(freq: u64, mode: Mode, tx: bool, split: bool) -> String {
        format!(
            "IF{freq:011}00000+0000{rit_on}00{mem}{tx}{mode}0{scan}{split}000{p15}",
            rit_on = 0,
            mem = "00",
            tx = tx as u8,
            mode = mode.to_digit(),
            scan = 0,
            split = split as u8,
            p15 = 0,
        )
    }

    #[test]
    fn if_response_parse() {
        let s = synthetic_if(14_074_000, Mode::Usb, false, false);
        // Sanity: the field offsets only make sense if the string is the length
        // we expect. If this assert ever fires, the synthetic builder drifted.
        assert_eq!(
            s.len(),
            37,
            "synthetic IF should be 37 chars (sans ';'): {s}"
        );
        let st = parse_if_response(&s).unwrap();
        assert!(st.fields_parsed);
        assert_eq!(st.freq_hz, 14_074_000);
        assert_eq!(st.mode, Some(Mode::Usb));
        assert!(!st.tx);
        assert!(!st.split);

        let tx = parse_if_response(&synthetic_if(7_074_000, Mode::Cw, true, true)).unwrap();
        assert_eq!(tx.freq_hz, 7_074_000);
        assert_eq!(tx.mode, Some(Mode::Cw));
        assert!(tx.tx);
        assert!(tx.split);
    }

    #[test]
    fn if_response_partial_recovers_freq() {
        // Truncated response: only frequency should be trusted.
        let st = parse_if_response("IF00014074000").unwrap();
        assert_eq!(st.freq_hz, 14_074_000);
        assert!(!st.fields_parsed);
    }

    #[test]
    fn if_response_rejects_non_if() {
        assert!(parse_if_response("FA00014074000").is_err());
        assert!(parse_if_response("IF").is_err());
    }
}
