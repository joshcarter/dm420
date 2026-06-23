//! Offline call-sign → country resolver.
//!
//! Pure data + functions, no async and no I/O — the Tier-1 lookup behind the
//! Call Sign panel (`docs/call_sign_lookup_panel.md`). Given a callsign it
//! resolves the operating **prefix** to a country name plus an ISO 3166-1
//! alpha-2 code (which keys the flag icon). The prefix table is generated at
//! compile time from the DXCC `cty.dat` country file (BigCty, AD1C), covering
//! the full ~340-entity list.
//!
//! Resolution is **longest-prefix match**: the table holds prefixes of varying
//! length and the longest one that leads the callsign wins (so `KH6` beats `K`,
//! `MM` beats `M`). Portable annotations (`G3ABC/P`, `F/W1AW`, `W1AW/4`) are
//! reduced to the operating prefix first.
#![forbid(unsafe_code)]

/// A resolved country: a display name plus the ISO 3166-1 alpha-2 code used to
/// pick the flag glyph. Note several DXCC entities share an ISO code (the four
/// UK home nations all key the `GB` flag) — the *name* keeps them distinct.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Country {
    pub name: &'static str,
    pub iso: &'static str,
}

/// Resolve a callsign to its country, or `None` when the prefix isn't in the
/// table (odd/compound calls fall through to the panel's neutral state).
pub fn lookup(call: &str) -> Option<Country> {
    let token = operating_prefix(call)?;
    resolve_prefix(&token)
}

/// Reduce a raw callsign to the single token whose prefix names the operating
/// location. By convention a reassigned prefix leads (`F/W1AW` → operating in
/// France) and operating-condition annotations trail (`G3ABC/P`, `W1AW/4`,
/// `DL1ABC/MM`). So: split on `/`, strip trailing suffix segments, and take the
/// first of what remains.
fn operating_prefix(call: &str) -> Option<String> {
    let up = call.trim().to_ascii_uppercase();
    if up.is_empty() {
        return None;
    }
    if !up.contains('/') {
        return Some(up);
    }
    let mut segs: Vec<&str> = up.split('/').filter(|s| !s.is_empty()).collect();
    if segs.is_empty() {
        return None;
    }
    // Trailing annotations (a bare digit/letter, `/P`, `/MM`, …) carry no
    // location; peel them off but never drop the only remaining segment.
    while segs.len() > 1 && is_suffix(segs[segs.len() - 1]) {
        segs.pop();
    }
    Some(segs[0].to_owned())
}

/// A trailing segment that never carries location: a bare single char or pure
/// digits (`/4`, `/A`) or an operating-condition annotation (`/P`, `/MM`).
fn is_suffix(seg: &str) -> bool {
    seg.len() == 1
        || seg.chars().all(|c| c.is_ascii_digit())
        || matches!(seg, "MM" | "AM" | "QRP" | "LH")
}

/// Longest-prefix match against the table.
fn resolve_prefix(token: &str) -> Option<Country> {
    let max = token.len().min(MAX_PREFIX_LEN);
    for n in (1..=max).rev() {
        let head = &token[..n];
        if let Some(&(_, name, iso)) = TABLE.iter().find(|(p, _, _)| *p == head) {
            return Some(Country { name, iso });
        }
    }
    None
}

// Longest prefix in cty.dat is 4 chars (e.g. KH7K, 3D2/c stripped to 3D2, etc.)
// Use 6 as a safe ceiling.
const MAX_PREFIX_LEN: usize = 6;

// Generated at compile time from data/cty.dat by build.rs.
include!(concat!(env!("OUT_DIR"), "/table.rs"));

#[cfg(test)]
mod tests {
    use super::*;

    fn iso(call: &str) -> Option<&'static str> {
        lookup(call).map(|c| c.iso)
    }

    #[test]
    fn us_calls() {
        assert_eq!(iso("W4LL"), Some("US"));
        assert_eq!(iso("N0JDC"), Some("US"));
        assert_eq!(iso("K1ABC"), Some("US"));
        assert_eq!(iso("AA1AA"), Some("US"));
    }

    #[test]
    fn longest_prefix_wins() {
        assert_eq!(lookup("KH6XYZ").map(|c| c.name), Some("Hawaii"));
        assert_eq!(lookup("KL7AB").map(|c| c.name), Some("Alaska"));
        assert_eq!(lookup("MM0XYZ").map(|c| c.name), Some("Scotland"));
        assert_eq!(lookup("M0ABC").map(|c| c.name), Some("England"));
    }

    #[test]
    fn international() {
        assert_eq!(iso("DL1ABC"), Some("DE"));
        assert_eq!(iso("G3XYZ"), Some("GB"));
        assert_eq!(iso("JA1XYZ"), Some("JP"));
        assert_eq!(iso("VK2DEF"), Some("AU"));
        assert_eq!(iso("F5ABC"), Some("FR"));
    }

    #[test]
    fn portable_annotations() {
        assert_eq!(iso("G3ABC/P"), Some("GB")); // suffix dropped
        assert_eq!(iso("W1AW/4"), Some("US")); // digit suffix dropped
        assert_eq!(lookup("F/W1AW").map(|c| c.name), Some("France")); // reassigned prefix
        assert_eq!(
            lookup("DL/N0JDC/M").map(|c| c.name),
            Some("Fed. Rep. of Germany")
        );
    }

    #[test]
    fn unknown_returns_none() {
        assert_eq!(lookup(""), None);
        assert_eq!(lookup("///"), None);
    }

    #[test]
    fn previously_missing_prefixes() {
        assert_eq!(iso("XQ3SK"), Some("CL")); // Chile via XQ
        assert_eq!(iso("JO1LVZ"), Some("JP")); // Japan via JO
    }

    #[test]
    fn logbook_calls() {
        let cases: &[(&str, &str)] = &[
            ("5Z4VJ", "KE"), ("AA7VG", "US"), ("AD9GE", "US"), ("D2UY", "AO"),
            ("HB9EFK", "CH"), ("JO1LVZ", "JP"), ("K3UCQ", "US"), ("K6GBZ", "US"),
            ("K6LUM", "US"), ("K7CTV", "US"), ("K7IOC", "US"), ("K7MHI", "US"),
            ("K9RRW", "US"), ("KB6JFL", "US"), ("KB7RUQ", "US"), ("KB9ELS", "US"),
            ("KE0KUL", "US"), ("KE8ZKN", "US"), ("KF9UG", "US"), ("KG8FM", "US"),
            ("KJ5MRD", "US"), ("KO6DGV", "US"), ("KV1F", "US"), ("N1FAM", "US"),
            ("N3FMC", "US"), ("N5IF", "US"), ("N6ACA", "US"), ("N7PAW", "US"),
            ("N7XAK", "US"), ("N8QA", "US"), ("N8WRC", "US"), ("N9OZ", "US"),
            ("NA6JD", "US"), ("NC7I", "US"), ("NI5B", "US"), ("PI4DX", "NL"),
            ("RW0AR", "RU"), ("SP3QDM", "PL"), ("VA3WL", "CA"), ("VE7SAY", "CA"),
            ("W0HU", "US"), ("W1ABC", "US"), ("W6ACT", "US"), ("W7AIA", "US"),
            ("W7LDE", "US"), ("W7OTV", "US"), ("W7RPS", "US"), ("W7WHO", "US"),
            ("WA0LXY", "US"), ("WH6S", "US"), ("WR3X", "US"), ("WY7AA", "US"),
            ("XQ3SK", "CL"),
        ];
        for (call, expected_iso) in cases {
            let result = lookup(call).map(|c| c.iso);
            assert_eq!(result, Some(*expected_iso), "failed for {call}");
        }
    }
}
