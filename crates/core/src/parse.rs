//! **[Joel owns]** Raw decode text → dm420 `ParsedMessage`.
//!
//! Joel's decoder (`modes::decode`) emits a raw `message: String` (e.g.
//! `"CQ K1ABC FN42"`, `"W9XYZ K1ABC RR73"`); the catalog wants a structured
//! [`ParsedMessage`] so the map/QSO/log never re-parse text. This is a first cut
//! over the FT8/FT4 message grammar — the **final variant taxonomy is Joel's
//! call** (it must track `ft8_lib` output + the ARRL Field Day set; see
//! `crates/modes/ATTRIBUTION.md` and the catalog §3 `[Joel owns]` note). Keep the
//! `Raw` fallback: never drop text the grammar doesn't cover.

use bus::types::{
    Callsign, ContestTag, ExchangePayload, GridSquare, ParsedMessage, Section, Signoff,
};

/// Parse one decoded FT8/FT4 message line into a [`ParsedMessage`].
pub fn parse_message(text: &str) -> ParsedMessage {
    let toks: Vec<&str> = text.split_whitespace().collect();
    match toks.as_slice() {
        [] => ParsedMessage::Raw(text.to_string()),
        ["CQ"] => ParsedMessage::Free(text.to_string()),
        ["CQ", rest @ ..] => parse_cq(rest, text),
        [to, from, rest @ ..] => parse_directed(to, from, rest, text),
        _ => ParsedMessage::Free(text.to_string()),
    }
}

/// `CQ [modifier] CALL [GRID]` — e.g. `CQ K1ABC FN42`, `CQ DX VK3ABC QF22`,
/// `CQ TEST K1ABC FN42`.
fn parse_cq(rest: &[&str], text: &str) -> ParsedMessage {
    let mut rest = rest;

    // Optional 4-char grid as the trailing token.
    let mut grid = None;
    if let Some((last, head)) = rest.split_last() {
        if is_grid(last) {
            grid = Some(GridSquare((*last).to_string()));
            rest = head;
        }
    }

    // The caller is the last remaining token; anything before it is a modifier.
    let Some((caller, mods)) = rest.split_last() else {
        return ParsedMessage::Free(text.to_string());
    };
    let contest = mods.first().and_then(|m| contest_tag(m));

    ParsedMessage::Cq {
        caller: callsign(caller),
        contest,
        grid,
    }
}

/// Build a [`Callsign`] from a decoded token, stripping the angle brackets the
/// decoder puts around a *hashed* callsign once it has resolved the 22-bit hash
/// to a full call (`<W1AW/0>` → `W1AW/0`). Stripping is what lets a compound
/// station's hashed reply compare equal to the partner the engine is working
/// (and log/plot under the real call). The unresolved sentinel `<...>` is left
/// verbatim — it names no station, so it must never match a real partner.
fn callsign(tok: &str) -> Callsign {
    let resolved = tok
        .strip_prefix('<')
        .and_then(|s| s.strip_suffix('>'))
        .filter(|inner| *inner != "...");
    Callsign(resolved.unwrap_or(tok).to_string())
}

/// `TO FROM [exchange]` — signoff, report, grid, R-report, or Field Day exchange.
fn parse_directed(to: &str, from: &str, rest: &[&str], text: &str) -> ParsedMessage {
    let to = callsign(to);
    let from = callsign(from);

    match rest {
        // RRR / RR73 / 73
        [tok] if signoff(tok).is_some() => ParsedMessage::Signoff {
            to,
            from,
            kind: signoff(tok).unwrap(),
        },
        [tok] if is_grid(tok) => ParsedMessage::Exchange {
            to,
            from,
            payload: ExchangePayload::Grid(GridSquare((*tok).to_string())),
        },
        [tok] if roger_report(tok).is_some() => ParsedMessage::Exchange {
            to,
            from,
            payload: ExchangePayload::RogerReport(roger_report(tok).unwrap()),
        },
        [tok] if report(tok).is_some() => ParsedMessage::Exchange {
            to,
            from,
            payload: ExchangePayload::Report(report(tok).unwrap()),
        },
        // Roger + grid ("R FN42") — a rover/VHF idiom (`R1` bit + grid). We don't
        // sequence on the roger, so fold it into a plain grid instead of dropping it
        // to `Free`. (P4 — see docs/qso_engine_improvements.md.)
        ["R", g] if is_grid(g) => ParsedMessage::Exchange {
            to,
            from,
            payload: ExchangePayload::Grid(GridSquare((*g).to_string())),
        },
        // ARRL Field Day exchange, rogered form: "R 3A CO" — both rogers the
        // partner's exchange and sends ours (the FD analogue of `R-09`).
        ["R", class, section] if is_fd_class(class) => ParsedMessage::Exchange {
            to,
            from,
            payload: ExchangePayload::FieldDay {
                class: (*class).to_string(),
                section: Section((*section).to_string()),
                rogered: true,
            },
        },
        // ARRL Field Day exchange: class + section, e.g. "3A CO".
        [class, section] if is_fd_class(class) => ParsedMessage::Exchange {
            to,
            from,
            payload: ExchangePayload::FieldDay {
                class: (*class).to_string(),
                section: Section((*section).to_string()),
                rogered: false,
            },
        },
        _ => ParsedMessage::Free(text.to_string()),
    }
}

/// Maidenhead 4-char locator, e.g. `FN31` (two letters, two digits).
fn is_grid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 4
        && b[0].is_ascii_uppercase()
        && b[1].is_ascii_uppercase()
        && b[2].is_ascii_digit()
        && b[3].is_ascii_digit()
}

/// A signal report like `-12` or `+05` (FT8 range roughly −30..+30 dB).
fn report(s: &str) -> Option<i8> {
    if s.starts_with('+') || s.starts_with('-') {
        s.parse::<i8>().ok()
    } else {
        None
    }
}

/// An R-prefixed report (`R-12`, `R+05`) acknowledging receipt.
fn roger_report(s: &str) -> Option<i8> {
    s.strip_prefix('R').and_then(report)
}

fn signoff(s: &str) -> Option<Signoff> {
    match s {
        "RRR" => Some(Signoff::Rrr),
        "RR73" => Some(Signoff::Rr73),
        "73" => Some(Signoff::Seven3),
        _ => None,
    }
}

fn contest_tag(s: &str) -> Option<ContestTag> {
    match s {
        "TEST" => Some(ContestTag::Test),
        "FD" => Some(ContestTag::FieldDay),
        "DX" => None, // directional hint, not a contest
        other => Some(ContestTag::Other(other.to_string())),
    }
}

/// Field Day class is a digit run followed by a transmitter-class letter, e.g.
/// `3A`, `1D`, `20H`.
fn is_fd_class(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 2
        && b[..b.len() - 1].iter().all(u8::is_ascii_digit)
        && b[b.len() - 1].is_ascii_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cq_with_grid() {
        match parse_message("CQ K1ABC FN42") {
            ParsedMessage::Cq {
                caller,
                contest,
                grid,
            } => {
                assert_eq!(caller.0, "K1ABC");
                assert!(contest.is_none());
                assert_eq!(grid.unwrap().0, "FN42");
            }
            other => panic!("expected Cq, got {other:?}"),
        }
    }

    #[test]
    fn cq_dx_modifier() {
        match parse_message("CQ DX VK3ABC QF22") {
            ParsedMessage::Cq { caller, grid, .. } => {
                assert_eq!(caller.0, "VK3ABC");
                assert_eq!(grid.unwrap().0, "QF22");
            }
            other => panic!("expected Cq, got {other:?}"),
        }
    }

    #[test]
    fn exchange_report_and_signoff() {
        assert!(matches!(
            parse_message("K1ABC W9XYZ -15"),
            ParsedMessage::Exchange {
                payload: ExchangePayload::Report(-15),
                ..
            }
        ));
        assert!(matches!(
            parse_message("W9XYZ K1ABC RR73"),
            ParsedMessage::Signoff {
                kind: Signoff::Rr73,
                ..
            }
        ));
        assert!(matches!(
            parse_message("K1ABC W9XYZ R-09"),
            ParsedMessage::Exchange {
                payload: ExchangePayload::RogerReport(-9),
                ..
            }
        ));
    }

    #[test]
    fn field_day_exchange() {
        assert!(matches!(
            parse_message("K1ABC W9XYZ 3A CO"),
            ParsedMessage::Exchange {
                payload: ExchangePayload::FieldDay { rogered: false, .. },
                ..
            }
        ));
        // Rogered form: "R 3A CO" both rogers and sends the exchange.
        assert!(matches!(
            parse_message("K1ABC W9XYZ R 3A CO"),
            ParsedMessage::Exchange {
                payload: ExchangePayload::FieldDay { rogered: true, .. },
                ..
            }
        ));
    }

    #[test]
    fn roger_grid_folds_into_grid() {
        // "R <grid>" (roger + grid, a rover/VHF idiom) parses as a grid exchange
        // rather than vanishing into Free; the roger bit isn't sequenced on.
        assert!(matches!(
            parse_message("K1ABC W9XYZ R FN42"),
            ParsedMessage::Exchange {
                payload: ExchangePayload::Grid(_),
                ..
            }
        ));
    }

    #[test]
    fn unparseable_keeps_text() {
        assert!(matches!(parse_message(""), ParsedMessage::Raw(_)));
    }

    #[test]
    fn resolves_bracketed_hashed_call() {
        // A compound station replies with its own call as a resolved hash; the
        // brackets are stripped so it compares equal to the partner being worked.
        match parse_message("W4LL <W1AW/0> -10") {
            ParsedMessage::Exchange { to, from, payload } => {
                assert_eq!(to.0, "W4LL");
                assert_eq!(from.0, "W1AW/0");
                assert!(matches!(payload, ExchangePayload::Report(-10)));
            }
            other => panic!("expected Exchange, got {other:?}"),
        }
        // Our call hashed in the `to` slot resolves the same way.
        match parse_message("<W4LL> W1AW/0 RR73") {
            ParsedMessage::Signoff { to, from, .. } => {
                assert_eq!(to.0, "W4LL");
                assert_eq!(from.0, "W1AW/0");
            }
            other => panic!("expected Signoff, got {other:?}"),
        }
    }

    #[test]
    fn unresolved_hash_sentinel_is_preserved() {
        // Until the decoder learns the full call, `<...>` names no station — keep
        // it verbatim so it can never be mistaken for a real partner.
        match parse_message("W4LL <...> -10") {
            ParsedMessage::Exchange { from, .. } => assert_eq!(from.0, "<...>"),
            other => panic!("expected Exchange, got {other:?}"),
        }
    }
}
