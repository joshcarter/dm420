//! ARRL/RAC Field Day exchange — the human-text ⇄ semantic-fields layer.
//!
//! This module is **pure**: it owns the Field Day vocabulary (the canonical
//! section table and the `<count><class>` token grammar) and knows nothing about
//! callsign hashing or the 77-bit wire layout. `message.rs` owns that second half
//! — it maps a [`FieldDayExchange`] to/from the packed payload with `pack28` /
//! `unpack28`.
//!
//! Splitting it this way keeps the token parsing and the (interop-critical)
//! section table testable without touching bits, and keeps the bit math next to
//! the other message types in `message.rs`. See
//! `docs/joel/fd-exchange-encode-bug.md` for the why.

/// Canonical WSJT-X ARRL/RAC Field Day section list, in the **exact order** of the
/// `csec` array in WSJT-X `lib/77bit/unpack77.f90`. The array index *is* the 7-bit
/// `isec` value carried on the air, so this ordering is the interop contract: a
/// station transmits the index and the receiver names the section by looking the
/// index back up here.
///
/// Do **not** re-order this — not to alphabetical, and not to match
/// `gui::panel_data::SECTIONS` (that one is a *geographic* table for the map, in a
/// different order). A re-order silently decodes every section as a different one.
/// Transcribe from WSJT-X verbatim; the membership quirks (`GH`, `KP4`, `PR`,
/// `VI`, `MAR`, the RAC sections, trailing `DX`) are part of the contract.
const SECTIONS: [&str; 85] = [
    "AB", "AK", "AL", "AR", "AZ", "BC", "CO", "CT", "DE", "EB", // 0..9
    "EMA", "ENY", "EPA", "EWA", "GA", "GH", "IA", "ID", "IL", "IN", // 10..19
    "KP4", "KS", "KY", "LA", "LAX", "MAR", "MB", "MDC", "ME", "MI", // 20..29
    "MN", "MO", "MS", "MT", "NC", "ND", "NE", "NFL", "NH", "NL", // 30..39
    "NLI", "NM", "NNJ", "NNY", "NT", "NV", "OH", "OK", "ONE", "ONN", // 40..49
    "ONS", "OR", "ORG", "PAC", "PR", "QC", "RI", "SB", "SC", "SCV", // 50..59
    "SD", "SDG", "SF", "SFL", "SJV", "SK", "SNJ", "STX", "SV", "TN", // 60..69
    "TX", "UT", "VA", "VI", "VT", "WCF", "WI", "WMA", "WNY", "WPA", // 70..79
    "WTX", "WV", "WWA", "WY", "DX", // 80..84
];

/// Highest transmitter count representable (4-bit `intx` + the `n3` 3/4 split).
const MAX_NTX: u8 = 32;

/// Resolve a section abbreviation to its wire index (`isec`). Case-insensitive,
/// trims surrounding space. `None` for anything not in [`SECTIONS`] — callers use
/// that to let non-Field-Day text fall through to the other packers.
pub(crate) fn section_index(sec: &str) -> Option<u8> {
    let s = sec.trim();
    SECTIONS
        .iter()
        .position(|name| name.eq_ignore_ascii_case(s))
        .map(|i| i as u8)
}

/// Name a section from its wire index. `None` if out of range (the 7-bit field can
/// hold 0..127, but only 0..84 are assigned).
pub(crate) fn section_name(isec: u8) -> Option<&'static str> {
    SECTIONS.get(isec as usize).copied()
}

/// Parse a Field Day class token — `<count><letter>`, e.g. `"3A"`, `"12E"` — into
/// `(ntx, class_idx)`, where `ntx` is the transmitter count (1..=32) and
/// `class_idx` is `0..=5` for letters `A`..`F`. `None` unless it matches WSJT-X's
/// `bFieldDay_msg` shape: 1–2 leading digits forming a count in 1..=32 and a single
/// trailing letter `A`–`F`, nothing else.
pub(crate) fn parse_class(token: &str) -> Option<(u8, u8)> {
    // 1–2 digits + 1 class letter ⇒ length 2 or 3.
    if !(2..=3).contains(&token.len()) {
        return None;
    }
    let letter = token.as_bytes()[token.len() - 1];
    if !(b'A'..=b'F').contains(&letter) {
        return None;
    }
    let digits = &token[..token.len() - 1];
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let ntx: u8 = digits.parse().ok()?;
    if !(1..=MAX_NTX).contains(&ntx) {
        return None;
    }
    Some((ntx, letter - b'A'))
}

/// Render `(ntx, class_idx)` back to a class token (`3`, `0` → `"3A"`). The count
/// is a plain decimal with no leading zero, matching WSJT-X (`"6A"`, `"12E"`).
fn format_class(ntx: u8, class_idx: u8) -> String {
    format!("{ntx}{}", (b'A' + class_idx) as char)
}

/// The parsed meaning of a Field Day exchange over — the semantic layer between
/// the on-screen string and the 77-bit payload. Built by [`FieldDayExchange::parse`]
/// from message tokens, rendered back by [`FieldDayExchange::to_text`], and mapped
/// to/from wire bits by `message::{encode_arrl_fd, decode_arrl_fd}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FieldDayExchange {
    /// The addressed station (`call_to`) and the sender (`call_de`).
    pub call_to: String,
    pub call_de: String,
    /// `true` for the rogered form (`R <class> <section>`, the combined Tx3 that
    /// both acknowledges the partner's exchange and sends ours).
    pub rogered: bool,
    /// Transmitter count, 1..=32.
    pub ntx: u8,
    /// Class letter as an index, `0..=5` ⇒ `A`..`F`.
    pub class_idx: u8,
    /// Section as its [`SECTIONS`] wire index, `0..=84`.
    pub section_idx: u8,
}

impl FieldDayExchange {
    /// Parse message tokens into an exchange, or `None` if they aren't a
    /// well-formed Field Day over. Recognized shapes (the two calls, then the
    /// exchange, optionally rogered):
    ///
    /// ```text
    /// <to> <de> <count><class> <section>          e.g.  K1ABC N0JDC 3A CO
    /// <to> <de> R <count><class> <section>        e.g.  K1ABC N0JDC R 3A CO
    /// ```
    ///
    /// Requiring *both* a parseable class token and a known section is what keeps
    /// an ordinary four-word free-text line from being mistaken for an exchange.
    /// Callsign validity is deliberately *not* checked here — that is the packer's
    /// job (`pack28`), so this layer stays free of hashing concerns.
    pub fn parse(toks: &[&str]) -> Option<Self> {
        let (call_to, call_de, rogered, class_tok, sec_tok) = match toks {
            [to, de, class, sec] => (*to, *de, false, *class, *sec),
            [to, de, r, class, sec] if *r == "R" => (*to, *de, true, *class, *sec),
            _ => return None,
        };
        let (ntx, class_idx) = parse_class(class_tok)?;
        let section_idx = section_index(sec_tok)?;
        Some(Self {
            call_to: call_to.to_string(),
            call_de: call_de.to_string(),
            rogered,
            ntx,
            class_idx,
            section_idx,
        })
    }

    /// Render the canonical on-air string, e.g. `"K1ABC N0JDC R 3A CO"`.
    pub fn to_text(&self) -> String {
        let class = format_class(self.ntx, self.class_idx);
        let section = section_name(self.section_idx).unwrap_or("");
        let roger = if self.rogered { "R " } else { "" };
        format!("{} {} {roger}{class} {section}", self.call_to, self.call_de)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_class_accepts_valid_and_rejects_invalid() {
        assert_eq!(parse_class("1A"), Some((1, 0)));
        assert_eq!(parse_class("3A"), Some((3, 0)));
        assert_eq!(parse_class("12E"), Some((12, 4)));
        assert_eq!(parse_class("32F"), Some((32, 5)));
        // count 0, count > 32, bad/again letter, missing parts, junk.
        for bad in ["0A", "33A", "3G", "3", "A", "R", "ABC", "123A", ""] {
            assert_eq!(parse_class(bad), None, "{bad:?} should not parse");
        }
    }

    #[test]
    fn section_table_is_well_formed() {
        // 85 unique, uppercase entries; the ends are pinned where WSJT-X puts them.
        assert_eq!(SECTIONS.len(), 85);
        assert_eq!(SECTIONS[0], "AB");
        assert_eq!(SECTIONS[6], "CO");
        assert_eq!(SECTIONS[76], "WI");
        assert_eq!(SECTIONS[84], "DX");
        let unique: std::collections::HashSet<_> = SECTIONS.iter().collect();
        assert_eq!(unique.len(), SECTIONS.len(), "no duplicate sections");
        assert!(
            SECTIONS
                .iter()
                .all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())),
            "sections are upper-case alphanumeric"
        );
    }

    #[test]
    fn section_index_name_roundtrip_and_folding() {
        for (i, &name) in SECTIONS.iter().enumerate() {
            assert_eq!(section_index(name), Some(i as u8), "index of {name}");
            assert_eq!(section_name(i as u8), Some(name), "name of {i}");
        }
        // Case-insensitive and space-trimmed.
        assert_eq!(section_index(" co "), Some(6));
        assert_eq!(section_index("Co"), Some(6));
        // Unknown / out of range.
        assert_eq!(section_index("ZZ"), None);
        assert_eq!(section_name(85), None);
        assert_eq!(section_name(200), None);
    }

    #[test]
    fn exchange_parse_and_render_roundtrip() {
        let plain = FieldDayExchange::parse(&["K1ABC", "N0JDC", "3A", "CO"]).unwrap();
        assert!(!plain.rogered);
        assert_eq!((plain.ntx, plain.class_idx), (3, 0));
        assert_eq!(plain.section_idx, section_index("CO").unwrap());
        assert_eq!(plain.to_text(), "K1ABC N0JDC 3A CO");

        let rogered = FieldDayExchange::parse(&["K1ABC", "N0JDC", "R", "3A", "CO"]).unwrap();
        assert!(rogered.rogered);
        assert_eq!(rogered.to_text(), "K1ABC N0JDC R 3A CO");
    }

    #[test]
    fn exchange_parse_rejects_non_field_day() {
        // Wrong arity, unknown section, a grid/report where the class goes, a plain
        // standard report — none are Field Day exchanges.
        assert!(FieldDayExchange::parse(&["K1ABC", "N0JDC", "3A"]).is_none());
        assert!(FieldDayExchange::parse(&["K1ABC", "N0JDC", "3A", "ZZ"]).is_none());
        assert!(FieldDayExchange::parse(&["K1ABC", "N0JDC", "FN42", "CO"]).is_none());
        assert!(FieldDayExchange::parse(&["W9XYZ", "K1ABC", "R-09"]).is_none());
        // The "R" form still requires a valid class + section.
        assert!(FieldDayExchange::parse(&["K1ABC", "N0JDC", "R", "XX", "CO"]).is_none());
    }
}
