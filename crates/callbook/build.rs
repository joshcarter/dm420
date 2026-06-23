use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=data/cty.dat");

    let cty = fs::read_to_string("data/cty.dat").expect("data/cty.dat not found");
    let iso_map: HashMap<&str, &str> = ISO.iter().copied().collect();

    let entities = parse_cty(&cty);

    let out_dir = env::var("OUT_DIR").unwrap();
    let dest = Path::new(&out_dir).join("table.rs");

    let mut entries: Vec<(String, String, String)> = Vec::new();
    for (name, main_prefix, prefixes) in &entities {
        let iso = iso_map.get(main_prefix.as_str()).copied().unwrap_or("XX");
        for prefix in prefixes {
            entries.push((prefix.clone(), name.clone(), iso.to_string()));
        }
    }

    let mut code = String::from("static TABLE: &[(&str, &str, &str)] = &[\n");
    for (prefix, name, iso) in &entries {
        code.push_str(&format!("    ({:?}, {:?}, {:?}),\n", prefix, name, iso));
    }
    code.push_str("];\n");

    fs::write(&dest, code).unwrap();
}

fn strip_modifiers(s: &str) -> &str {
    let end = s
        .find(|c| matches!(c, '(' | '[' | '{' | '~'))
        .unwrap_or(s.len());
    s[..end].trim()
}

fn parse_cty(text: &str) -> Vec<(String, String, Vec<String>)> {
    let mut result = Vec::new();
    let mut lines = text.lines().peekable();

    while let Some(line) = lines.next() {
        if line.starts_with(|c: char| !c.is_whitespace()) && line.contains(':') {
            let parts: Vec<&str> = line.splitn(8, ':').collect();
            if parts.len() < 8 {
                continue;
            }
            let name = parts[0].trim().to_string();
            let main_prefix_raw = parts[7]
                .trim()
                .trim_start_matches('*')
                .trim_end_matches(':')
                .trim()
                .to_string();

            let mut prefix_tokens = String::new();
            for cont_line in lines.by_ref() {
                prefix_tokens.push_str(cont_line.trim());
                prefix_tokens.push(' ');
                if cont_line.contains(';') {
                    break;
                }
            }

            let mut prefixes = Vec::new();
            for token in prefix_tokens.split(',') {
                let token = token.trim().trim_end_matches(';').trim();
                if token.is_empty() {
                    continue;
                }
                if token.starts_with('=') {
                    continue;
                }
                let p = strip_modifiers(token).to_string();
                if !p.is_empty() {
                    prefixes.push(p);
                }
            }

            result.push((name, main_prefix_raw, prefixes));
        }
    }
    result
}

static ISO: &[(&str, &str)] = &[
    // Special / international
    ("1A", "XX"),
    ("1S", "XX"),
    ("4U1I", "XX"),
    ("4U1U", "XX"),
    ("4U1V", "XX"),
    ("BS7", "XX"),
    ("BV9P", "XX"),
    ("CE9", "AQ"),
    ("3Y/b", "NO"),
    ("3Y/p", "XX"),
    ("CY0", "CA"),
    ("CY9", "CA"),
    ("FO/c", "XX"),
    ("FK/c", "XX"),
    ("FT/g", "XX"),
    ("FT/j", "XX"),
    ("FT/t", "XX"),
    ("FT/w", "XX"),
    ("FT/x", "TF"),
    ("FT/z", "XX"),
    ("KG4", "CU"),
    ("KH1", "UM"),
    ("KH3", "UM"),
    ("KH4", "UM"),
    ("KH5", "UM"),
    ("KH7K", "UM"),
    ("KH9", "UM"),
    ("KP1", "UM"),
    ("KP5", "UM"),
    ("R1FJ", "RU"),
    ("S0", "EH"),
    ("VP8/g", "GS"),
    ("VP8/h", "AQ"),
    ("VP8/o", "AQ"),
    ("VP8/s", "AQ"),
    ("VQ9", "IO"),
    ("YV0", "XX"),
    ("ZC4", "GB"),
    ("ZS8", "ZA"),
    // Africa
    ("3B6", "MU"),
    ("3B8", "MU"),
    ("3B9", "MU"),
    ("3C", "GQ"),
    ("3C0", "GQ"),
    ("3DA", "SZ"),
    ("3V", "TN"),
    ("3X", "GN"),
    ("5A", "LY"),
    ("5H", "TZ"),
    ("5N", "NG"),
    ("5R", "MG"),
    ("5T", "MR"),
    ("5U", "NE"),
    ("5V", "TG"),
    ("5X", "UG"),
    ("5Z", "KE"),
    ("6W", "SN"),
    ("7O", "YE"),
    ("7P", "LS"),
    ("7Q", "MW"),
    ("7X", "DZ"),
    ("9G", "GH"),
    ("9L", "SL"),
    ("9Q", "CD"),
    ("9U", "BI"),
    ("9X", "RW"),
    ("A2", "BW"),
    ("CN", "MA"),
    ("D2", "AO"),
    ("D4", "CV"),
    ("D6", "KM"),
    ("E3", "ER"),
    ("EL", "LR"),
    ("ET", "ET"),
    ("FR", "RE"),
    ("FT5W", "XX"),
    ("FY", "GF"),
    ("J2", "DJ"),
    ("J5", "GW"),
    ("S9", "ST"),
    ("ST", "SD"),
    ("SU", "EG"),
    ("T5", "SO"),
    ("TJ", "CM"),
    ("TL", "CF"),
    ("TN", "CG"),
    ("TR", "GA"),
    ("TT", "TD"),
    ("TU", "CI"),
    ("TY", "BJ"),
    ("TZ", "ML"),
    ("XT", "BF"),
    ("Z2", "ZW"),
    ("Z8", "SS"),
    ("ZD7", "SH"),
    ("ZD8", "SH"),
    ("ZD9", "SH"),
    ("ZS", "ZA"),
    ("9J", "ZM"),
    ("C9", "MZ"),
    ("FH", "YT"),
    // Americas - North / Caribbean
    ("6Y", "JM"),
    ("8P", "BB"),
    ("8R", "GY"),
    ("C6", "BS"),
    ("CM", "CU"),
    ("FG", "GP"),
    ("FJ", "BL"),
    ("FM", "MQ"),
    ("FP", "PM"),
    ("FS", "MF"),
    ("G", "GB"),
    ("GD", "IM"),
    ("GI", "GB"),
    ("GJ", "JE"),
    ("GM", "GB"),
    ("GM/s", "GB"),
    ("GU", "GG"),
    ("GW", "GB"),
    ("HH", "HT"),
    ("HI", "DO"),
    ("HK", "CO"),
    ("HK0/a", "CO"),
    ("HK0/m", "CO"),
    ("HP", "PA"),
    ("HR", "HN"),
    ("J3", "GD"),
    ("J6", "LC"),
    ("J7", "DM"),
    ("J8", "VC"),
    ("K", "US"),
    ("KH0", "MP"),
    ("KH2", "GU"),
    ("KH6", "US"),
    ("KH8", "AS"),
    ("KH8/s", "AS"),
    ("KL", "US"),
    ("KP2", "VI"),
    ("KP4", "PR"),
    ("OX", "GL"),
    ("PJ2", "CW"),
    ("PJ4", "BQ"),
    ("PJ5", "BQ"),
    ("PJ7", "SX"),
    ("TG", "GT"),
    ("TI", "CR"),
    ("TI9", "CR"),
    ("V2", "AG"),
    ("V3", "BZ"),
    ("V4", "KN"),
    ("VE", "CA"),
    ("VP2E", "AI"),
    ("VP2M", "MS"),
    ("VP2V", "VG"),
    ("VP5", "TC"),
    ("VP9", "BM"),
    ("VR", "HK"),
    ("XE", "MX"),
    ("XF4", "MX"),
    ("YN", "NI"),
    ("YS", "SV"),
    ("ZF", "KY"),
    // Americas - South
    ("CE", "CL"),
    ("CE0X", "CL"),
    ("CE0Y", "CL"),
    ("CE0Z", "CL"),
    ("CP", "BO"),
    ("CX", "UY"),
    ("HC", "EC"),
    ("HC8", "EC"),
    ("LU", "AR"),
    ("OA", "PE"),
    ("PY", "BR"),
    ("PY0F", "BR"),
    ("PY0S", "BR"),
    ("PY0T", "BR"),
    ("PZ", "SR"),
    ("VP6", "PN"),
    ("VP6/d", "PN"),
    ("VP8", "FK"),
    ("XR", "CL"),
    ("YV", "VE"),
    ("ZP", "PY"),
    // Europe
    ("3A", "MC"),
    ("C3", "AD"),
    ("CT", "PT"),
    ("CT3", "PT"),
    ("CU", "PT"),
    ("DL", "DE"),
    ("E7", "BA"),
    ("EA", "ES"),
    ("EA6", "ES"),
    ("EA8", "ES"),
    ("EA9", "ES"),
    ("EI", "IE"),
    ("EK", "AM"),
    ("ER", "MD"),
    ("ES", "EE"),
    ("EU", "BY"),
    ("EW", "BY"),
    ("EX", "KG"),
    ("EY", "TJ"),
    ("EZ", "TM"),
    ("F", "FR"),
    ("HA", "HU"),
    ("HB", "CH"),
    ("HB0", "LI"),
    ("HV", "VA"),
    ("I", "IT"),
    ("IG9", "IT"),
    ("IS", "IT"),
    ("IT9", "IT"),
    ("JW", "SJ"),
    ("JW/b", "SJ"),
    ("JX", "SJ"),
    ("LA", "NO"),
    ("LX", "LU"),
    ("LY", "LT"),
    ("LZ", "BG"),
    ("OE", "AT"),
    ("OH", "FI"),
    ("OH0", "AX"),
    ("OJ0", "FI"),
    ("OK", "CZ"),
    ("OM", "SK"),
    ("ON", "BE"),
    ("OY", "FO"),
    ("OZ", "DK"),
    ("PA", "NL"),
    ("S5", "SI"),
    ("SM", "SE"),
    ("SP", "PL"),
    ("SV", "GR"),
    ("SV/a", "GR"),
    ("SV5", "GR"),
    ("SV9", "GR"),
    ("T7", "SM"),
    ("TA", "TR"),
    ("TA1", "TR"),
    ("TF", "IS"),
    ("TK", "FR"),
    ("UA", "RU"),
    ("UA2", "RU"),
    ("UA9", "RU"),
    ("UK", "UZ"),
    ("UN", "KZ"),
    ("UR", "UA"),
    ("YL", "LV"),
    ("YO", "RO"),
    ("YU", "RS"),
    ("Z3", "MK"),
    ("Z6", "XK"),
    ("ZA", "AL"),
    ("ZB", "GI"),
    ("4J", "AZ"),
    ("4K", "AZ"),
    ("4L", "GE"),
    ("4O", "ME"),
    // Middle East / Med
    ("4S", "LK"),
    ("4W", "TL"),
    ("4X", "IL"),
    ("5B", "CY"),
    ("9H", "MT"),
    ("9K", "KW"),
    ("A4", "OM"),
    ("A5", "BT"),
    ("A6", "AE"),
    ("A7", "QA"),
    ("A9", "BH"),
    ("AP", "PK"),
    ("E4", "PS"),
    ("EP", "IR"),
    ("HZ", "SA"),
    ("JY", "JO"),
    ("OD", "LB"),
    ("S2", "BD"),
    ("YA", "AF"),
    ("YI", "IQ"),
    ("YK", "SY"),
    // Asia / Pacific
    ("9M2", "MY"),
    ("9M6", "MY"),
    ("9N", "NP"),
    ("9V", "SG"),
    ("9Y", "TT"),
    ("A3", "TO"),
    ("BV", "TW"),
    ("BY", "CN"),
    ("C2", "NR"),
    ("DU", "PH"),
    ("E5/n", "CK"),
    ("E5/s", "CK"),
    ("E6", "NU"),
    ("H4", "SB"),
    ("H40", "SB"),
    ("HL", "KR"),
    ("HS", "TH"),
    ("JA", "JP"),
    ("JD/m", "JP"),
    ("JD/o", "JP"),
    ("JT", "MN"),
    ("P2", "PG"),
    ("P4", "AW"),
    ("P5", "KP"),
    ("T2", "TV"),
    ("T30", "KI"),
    ("T31", "KI"),
    ("T32", "KI"),
    ("T33", "KI"),
    ("T8", "PW"),
    ("V6", "FM"),
    ("V7", "MH"),
    ("V8", "BN"),
    ("VK", "AU"),
    ("VK0H", "HM"),
    ("VK0M", "AU"),
    ("VK9C", "CC"),
    ("VK9L", "AU"),
    ("VK9M", "XX"),
    ("VK9N", "NF"),
    ("VK9W", "AU"),
    ("VK9X", "CX"),
    ("VU", "IN"),
    ("VU4", "IN"),
    ("VU7", "IN"),
    ("XU", "KH"),
    ("XW", "LA"),
    ("XX9", "MO"),
    ("XZ", "MM"),
    ("YB", "ID"),
    ("YJ", "VU"),
    ("3W", "VN"),
    // Oceania
    ("3D2", "FJ"),
    ("3D2/c", "XX"),
    ("3D2/r", "FJ"),
    ("5W", "WS"),
    ("FK", "NC"),
    ("FO", "PF"),
    ("FO/a", "PF"),
    ("FO/m", "PF"),
    ("FW", "WF"),
    ("ZK3", "TK"),
    ("ZL", "NZ"),
    ("ZL7", "NZ"),
    ("ZL8", "NZ"),
    ("ZL9", "NZ"),
];
