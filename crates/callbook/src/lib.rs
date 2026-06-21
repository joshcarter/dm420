//! Offline call-sign → country resolver.
//!
//! Pure data + functions, no async and no I/O — the Tier-1 lookup behind the
//! Call Sign panel (`docs/call_sign_lookup_panel.md`). Given a callsign it
//! resolves the operating **prefix** to a country name plus an ISO 3166-1
//! alpha-2 code (which keys the flag icon). This is a curated Phase-1 subset of
//! the DXCC/ITU prefix allocations — broad enough for everyday FT8/Field Day
//! traffic, not the full ~340-entity list. A fuller table (and online name
//! enrichment) is Tier-2 work; see the panel spec.
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

const MAX_PREFIX_LEN: usize = 3;

/// Curated `(prefix, country, iso2)` table, Phase-1 subset. Longest matching
/// prefix wins, so specific multi-char entries (e.g. `KH6`, `MM`) sit alongside
/// the broad single-letter ones (`K`, `G`) without conflict. Several entities
/// intentionally share an ISO/flag (US territories → `US`, UK home nations →
/// `GB`) since Phase 1 resolves to *country*, not full DXCC entity.
#[rustfmt::skip]
static TABLE: &[(&str, &str, &str)] = &[
    // ---- United States (K / W / N and the AA–AL block; territories → US) ----
    ("K", "United States", "US"), ("W", "United States", "US"), ("N", "United States", "US"),
    ("AA", "United States", "US"), ("AB", "United States", "US"), ("AC", "United States", "US"),
    ("AD", "United States", "US"), ("AE", "United States", "US"), ("AF", "United States", "US"),
    ("AG", "United States", "US"), ("AI", "United States", "US"), ("AJ", "United States", "US"),
    ("AK", "United States", "US"), ("AL", "United States", "US"),
    ("KL", "Alaska", "US"), ("AL7", "Alaska", "US"), ("NL", "Alaska", "US"), ("WL", "Alaska", "US"),
    ("KH", "Hawaii", "US"), ("NH", "Hawaii", "US"), ("WH", "Hawaii", "US"), ("AH", "Hawaii", "US"),
    ("KP4", "Puerto Rico", "PR"), ("NP4", "Puerto Rico", "PR"), ("WP4", "Puerto Rico", "PR"),
    ("KP2", "US Virgin Islands", "VI"),
    // ---- Canada ----
    ("VE", "Canada", "CA"), ("VA", "Canada", "CA"), ("VO", "Canada", "CA"), ("VY", "Canada", "CA"),
    ("CF", "Canada", "CA"), ("CG", "Canada", "CA"), ("CK", "Canada", "CA"),
    // ---- Mexico / Central America / Caribbean ----
    ("XE", "Mexico", "MX"), ("XF", "Mexico", "MX"), ("4A", "Mexico", "MX"),
    ("CO", "Cuba", "CU"), ("CM", "Cuba", "CU"), ("HI", "Dominican Rep.", "DO"), ("HH", "Haiti", "HT"),
    ("TI", "Costa Rica", "CR"), ("HP", "Panama", "PA"), ("YS", "El Salvador", "SV"),
    ("TG", "Guatemala", "GT"), ("HR", "Honduras", "HN"), ("YN", "Nicaragua", "NI"),
    ("V3", "Belize", "BZ"), ("8P", "Barbados", "BB"), ("J3", "Grenada", "GD"), ("J6", "St. Lucia", "LC"),
    ("J7", "Dominica", "DM"), ("J8", "St. Vincent", "VC"), ("VP9", "Bermuda", "BM"),
    ("FG", "Guadeloupe", "FR"), ("FM", "Martinique", "FR"), ("ZF", "Cayman Is.", "KY"),
    // ---- South America ----
    ("PY", "Brazil", "BR"), ("PP", "Brazil", "BR"), ("PT", "Brazil", "BR"), ("PR", "Brazil", "BR"),
    ("PU", "Brazil", "BR"), ("LU", "Argentina", "AR"), ("LW", "Argentina", "AR"),
    ("CE", "Chile", "CL"), ("CA", "Chile", "CL"), ("HK", "Colombia", "CO"), ("HC", "Ecuador", "EC"),
    ("OA", "Peru", "PE"), ("CP", "Bolivia", "BO"), ("CX", "Uruguay", "UY"), ("ZP", "Paraguay", "PY"),
    ("YV", "Venezuela", "VE"), ("8R", "Guyana", "GY"), ("PZ", "Suriname", "SR"), ("HJ", "Colombia", "CO"),
    // ---- Western Europe ----
    ("G", "England", "GB"), ("M", "England", "GB"), ("2E", "England", "GB"),
    ("MM", "Scotland", "GB"), ("GM", "Scotland", "GB"), ("2M", "Scotland", "GB"),
    ("MW", "Wales", "GB"), ("GW", "Wales", "GB"), ("2W", "Wales", "GB"),
    ("MI", "N. Ireland", "GB"), ("GI", "N. Ireland", "GB"), ("2I", "N. Ireland", "GB"),
    ("MD", "Isle of Man", "IM"), ("GD", "Isle of Man", "IM"), ("MJ", "Jersey", "JE"), ("GJ", "Jersey", "JE"),
    ("MU", "Guernsey", "GG"), ("GU", "Guernsey", "GG"),
    ("EI", "Ireland", "IE"), ("EJ", "Ireland", "IE"),
    ("DL", "Germany", "DE"), ("DA", "Germany", "DE"), ("DB", "Germany", "DE"), ("DC", "Germany", "DE"),
    ("DD", "Germany", "DE"), ("DF", "Germany", "DE"), ("DG", "Germany", "DE"), ("DH", "Germany", "DE"),
    ("DJ", "Germany", "DE"), ("DK", "Germany", "DE"), ("DM", "Germany", "DE"), ("DO", "Germany", "DE"),
    ("F", "France", "FR"), ("TM", "France", "FR"),
    ("I", "Italy", "IT"), ("IZ", "Italy", "IT"), ("IK", "Italy", "IT"), ("IW", "Italy", "IT"),
    ("EA", "Spain", "ES"), ("EB", "Spain", "ES"), ("EC", "Spain", "ES"), ("ED", "Spain", "ES"),
    ("EH", "Spain", "ES"), ("CT", "Portugal", "PT"), ("CR", "Portugal", "PT"), ("CQ", "Portugal", "PT"),
    ("PA", "Netherlands", "NL"), ("PB", "Netherlands", "NL"), ("PC", "Netherlands", "NL"),
    ("PD", "Netherlands", "NL"), ("PE", "Netherlands", "NL"), ("PI", "Netherlands", "NL"),
    ("ON", "Belgium", "BE"), ("OO", "Belgium", "BE"), ("OT", "Belgium", "BE"),
    ("LX", "Luxembourg", "LU"), ("HB", "Switzerland", "CH"), ("HB0", "Liechtenstein", "LI"),
    ("OE", "Austria", "AT"),
    // ---- Nordics ----
    ("SM", "Sweden", "SE"), ("SA", "Sweden", "SE"), ("SK", "Sweden", "SE"), ("8S", "Sweden", "SE"),
    ("LA", "Norway", "NO"), ("LB", "Norway", "NO"), ("LN", "Norway", "NO"),
    ("OZ", "Denmark", "DK"), ("OU", "Denmark", "DK"), ("5P", "Denmark", "DK"),
    ("OH", "Finland", "FI"), ("OF", "Finland", "FI"), ("OG", "Finland", "FI"),
    ("TF", "Iceland", "IS"),
    // ---- Central / Eastern Europe ----
    ("SP", "Poland", "PL"), ("SQ", "Poland", "PL"), ("SO", "Poland", "PL"), ("3Z", "Poland", "PL"),
    ("OK", "Czech Rep.", "CZ"), ("OL", "Czech Rep.", "CZ"), ("OM", "Slovakia", "SK"),
    ("HA", "Hungary", "HU"), ("HG", "Hungary", "HU"), ("YO", "Romania", "RO"), ("YP", "Romania", "RO"),
    ("LZ", "Bulgaria", "BG"), ("S5", "Slovenia", "SI"), ("9A", "Croatia", "HR"), ("E7", "Bosnia", "BA"),
    ("YT", "Serbia", "RS"), ("YU", "Serbia", "RS"), ("Z3", "N. Macedonia", "MK"), ("ZA", "Albania", "AL"),
    ("SV", "Greece", "GR"), ("SW", "Greece", "GR"), ("Z6", "Kosovo", "XK"), ("4O", "Montenegro", "ME"),
    ("ES", "Estonia", "EE"), ("YL", "Latvia", "LV"), ("LY", "Lithuania", "LT"),
    ("UR", "Ukraine", "UA"), ("UT", "Ukraine", "UA"), ("UU", "Ukraine", "UA"), ("EW", "Belarus", "BY"),
    ("ER", "Moldova", "MD"),
    // ---- Russia & neighbours ----
    ("UA", "Russia", "RU"), ("UB", "Russia", "RU"), ("RA", "Russia", "RU"), ("RU", "Russia", "RU"),
    ("RV", "Russia", "RU"), ("RW", "Russia", "RU"), ("RK", "Russia", "RU"), ("RN", "Russia", "RU"),
    ("R", "Russia", "RU"), ("UN", "Kazakhstan", "KZ"), ("EX", "Kyrgyzstan", "KG"), ("EY", "Tajikistan", "TJ"),
    ("EZ", "Turkmenistan", "TM"), ("UK", "Uzbekistan", "UZ"), ("4L", "Georgia", "GE"),
    ("EK", "Armenia", "AM"), ("4J", "Azerbaijan", "AZ"), ("4K", "Azerbaijan", "AZ"),
    // ---- Mediterranean / Middle East ----
    ("9H", "Malta", "MT"), ("5B", "Cyprus", "CY"), ("TA", "Turkey", "TR"), ("TB", "Turkey", "TR"),
    ("YM", "Turkey", "TR"), ("4X", "Israel", "IL"), ("4Z", "Israel", "IL"), ("JY", "Jordan", "JO"),
    ("OD", "Lebanon", "LB"), ("YK", "Syria", "SY"), ("YI", "Iraq", "IQ"), ("EP", "Iran", "IR"),
    ("A4", "Oman", "OM"), ("A6", "United Arab Emirates", "AE"), ("A7", "Qatar", "QA"),
    ("A9", "Bahrain", "BH"), ("9K", "Kuwait", "KW"), ("HZ", "Saudi Arabia", "SA"), ("7Z", "Saudi Arabia", "SA"),
    ("A2", "Botswana", "BW"),
    // ---- Africa ----
    ("ZS", "South Africa", "ZA"), ("ZR", "South Africa", "ZA"), ("ZT", "South Africa", "ZA"),
    ("SU", "Egypt", "EG"), ("CN", "Morocco", "MA"), ("7X", "Algeria", "DZ"), ("3V", "Tunisia", "TN"),
    ("5A", "Libya", "LY"), ("5H", "Tanzania", "TZ"), ("5Z", "Kenya", "KE"), ("5N", "Nigeria", "NG"),
    ("9G", "Ghana", "GH"), ("EL", "Liberia", "LR"), ("D2", "Angola", "AO"), ("C9", "Mozambique", "MZ"),
    ("Z2", "Zimbabwe", "ZW"), ("9J", "Zambia", "ZM"), ("5R", "Madagascar", "MG"), ("3B8", "Mauritius", "MU"),
    ("FR", "Reunion", "FR"), ("FT", "France (Africa)", "FR"),
    // ---- Asia ----
    ("JA", "Japan", "JP"), ("JE", "Japan", "JP"), ("JF", "Japan", "JP"), ("JG", "Japan", "JP"),
    ("JH", "Japan", "JP"), ("JI", "Japan", "JP"), ("JJ", "Japan", "JP"), ("JK", "Japan", "JP"),
    ("JR", "Japan", "JP"), ("JL", "Japan", "JP"), ("7K", "Japan", "JP"), ("7N", "Japan", "JP"),
    ("HL", "South Korea", "KR"), ("DS", "South Korea", "KR"), ("6K", "South Korea", "KR"),
    ("BY", "China", "CN"), ("BA", "China", "CN"), ("BD", "China", "CN"), ("BG", "China", "CN"),
    ("BH", "China", "CN"), ("BV", "Taiwan", "TW"), ("BU", "Taiwan", "TW"), ("VR", "Hong Kong", "HK"),
    ("XX9", "Macau", "MO"), ("VU", "India", "IN"), ("AT", "India", "IN"), ("AP", "Pakistan", "PK"),
    ("S2", "Bangladesh", "BD"), ("4S", "Sri Lanka", "LK"), ("9N", "Nepal", "NP"), ("XW", "Laos", "LA"),
    ("XZ", "Myanmar", "MM"), ("HS", "Thailand", "TH"), ("E2", "Thailand", "TH"), ("XV", "Vietnam", "VN"),
    ("9M", "Malaysia", "MY"), ("9V", "Singapore", "SG"), ("YB", "Indonesia", "ID"), ("YC", "Indonesia", "ID"),
    ("YD", "Indonesia", "ID"), ("DU", "Philippines", "PH"), ("DV", "Philippines", "PH"), ("DW", "Philippines", "PH"),
    // ---- Oceania ----
    ("VK", "Australia", "AU"), ("AX", "Australia", "AU"), ("VI", "Australia", "AU"),
    ("ZL", "New Zealand", "NZ"), ("ZM", "New Zealand", "NZ"), ("ZK", "New Zealand", "NZ"),
    ("FK", "New Caledonia", "FR"), ("FO", "Fr. Polynesia", "FR"), ("E5", "Cook Is.", "CK"),
    ("3D2", "Fiji", "FJ"), ("KH6", "Hawaii", "US"), ("KH2", "Guam", "GU"), ("KH8", "Amer. Samoa", "AS"),
];

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
        assert_eq!(lookup("DL/N0JDC/M").map(|c| c.name), Some("Germany"));
    }

    #[test]
    fn unknown_returns_none() {
        assert_eq!(lookup(""), None);
        assert_eq!(lookup("///"), None);
    }
}
