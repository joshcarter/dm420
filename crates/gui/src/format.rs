//! GUI display formatters: turning bus/`types` values into the strings the
//! console shows. Display-only — Unicode glyphs and WSJT-X-style rendering for the
//! screen. These are **not** used to build on-air message payloads; that ASCII
//! construction lives in the `qso` crate and must stay separate.

use types::{
    Band, Callsign, ContestTag, Decode, DecodeContent, ExchangePayload, OverAirMode, ParsedMessage,
    Signoff,
};

/// SNR like the rest of the console: Unicode minus, two digits.
pub(crate) fn fmt_snr(snr: i8) -> String {
    let sign = if snr < 0 { '−' } else { '+' };
    format!("{sign}{:02}", snr.unsigned_abs())
}

/// The human-readable body of a decode (`CQ EA7KW IM67`, an exchange, etc.).
pub(crate) fn decode_text(d: &Decode) -> String {
    match &d.content {
        DecodeContent::Slotted { message, raw, .. } => match message {
            ParsedMessage::Cq { caller, contest, grid } => {
                // Re-emit the CQ modifier the parser captured (e.g. Field Day's `FD`),
                // built as an owned String so the grid/no-grid arms stay uniform —
                // otherwise it silently drops, showing `CQ FD …` as a plain `CQ …`.
                let m: String = match contest {
                    Some(ContestTag::FieldDay) => "FD ".into(),
                    Some(ContestTag::Test) => "TEST ".into(),
                    Some(ContestTag::Other(s)) => format!("{s} "),
                    None => String::new(),
                };
                match grid {
                    Some(g) => format!("CQ {m}{} {}", display_call(caller, raw), g.0),
                    None => format!("CQ {m}{}", display_call(caller, raw)),
                }
            }
            ParsedMessage::Exchange { to, from, payload } => {
                format!(
                    "{} {} {}",
                    display_call(to, raw),
                    display_call(from, raw),
                    fmt_payload(payload)
                )
            }
            ParsedMessage::Signoff { to, from, kind } => {
                format!(
                    "{} {} {}",
                    display_call(to, raw),
                    display_call(from, raw),
                    fmt_signoff(*kind)
                )
            }
            ParsedMessage::Free(s) | ParsedMessage::Raw(s) => s.clone(),
        },
        DecodeContent::Streaming { text } => text.clone(),
    }
}

/// Re-add the decoder's `<…>` hashed-call cue for display. Parsing strips the
/// brackets so a resolved hash matches/logs as the real station (`<W1AW/0>` →
/// `W1AW/0`); but when the decoder's verbatim `raw` line shows the call
/// bracketed, it arrived as a 22-bit hash we resolved from the session table, not
/// a directly-decoded call. Surfacing the brackets here keeps that lower-
/// confidence cue visible (a hash *could* collide). An unresolved hash already
/// reads `<...>` as the call itself, so it falls through the `else` unchanged.
pub(crate) fn display_call(call: &Callsign, raw: &str) -> String {
    let bracketed = format!("<{}>", call.0);
    if raw.contains(&bracketed) {
        bracketed
    } else {
        call.0.clone()
    }
}

/// The exchange body as WSJT-X renders it: grid verbatim, reports as `%+2.2d`
/// (`-07`, `+05`), the roger form prefixed `R`, Field Day as `[R ]<class> <section>`.
pub(crate) fn fmt_payload(p: &ExchangePayload) -> String {
    match p {
        ExchangePayload::Grid(g) => g.0.clone(),
        ExchangePayload::Report(r) => format!("{r:+03}"),
        ExchangePayload::RogerReport(r) => format!("R{r:+03}"),
        ExchangePayload::FieldDay {
            class,
            section,
            rogered,
        } => format!("{}{class} {}", if *rogered { "R " } else { "" }, section.0),
    }
}

/// A sign-off rendered as its on-air token (not always `73`).
pub(crate) fn fmt_signoff(kind: Signoff) -> &'static str {
    match kind {
        Signoff::Rrr => "RRR",
        Signoff::Rr73 => "RR73",
        Signoff::Seven3 => "73",
    }
}

/// Full display label for a band, e.g. `"40m"`. The single home for band labels,
/// shared by the Band Status panel, the active-bands config grid, and elsewhere.
pub(crate) fn band_label(b: Band) -> &'static str {
    match b {
        Band::B160m => "160m",
        Band::B80m => "80m",
        Band::B40m => "40m",
        Band::B30m => "30m",
        Band::B20m => "20m",
        Band::B17m => "17m",
        Band::B15m => "15m",
        Band::B12m => "12m",
        Band::B10m => "10m",
        Band::B6m => "6m",
    }
}

/// Compact (meters-only) band label, e.g. `"40"` — for tight clusters like the
/// Contacts map's band switcher.
pub(crate) fn band_short(b: Band) -> &'static str {
    band_label(b).trim_end_matches('m')
}

/// Short display label for an over-the-air mode, e.g. `"FT8"`.
pub(crate) fn mode_label(m: OverAirMode) -> &'static str {
    match m {
        OverAirMode::Ft8 => "FT8",
        OverAirMode::Ft4 => "FT4",
        OverAirMode::Psk31 => "PSK",
        OverAirMode::Rtty => "RTTY",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_call_marks_resolved_hashes() {
        let raw = "W4LL <W1AW/0> -10";
        // The hashed call gets its resolved-hash brackets back for display…
        assert_eq!(display_call(&Callsign("W1AW/0".into()), raw), "<W1AW/0>");
        // …but a directly-decoded call in the same line stays bare.
        assert_eq!(display_call(&Callsign("W4LL".into()), raw), "W4LL");
        // An unresolved hash is already `<...>` as the call itself; leave it be.
        assert_eq!(display_call(&Callsign("<...>".into()), "W4LL <...> -10"), "<...>");
    }
}
