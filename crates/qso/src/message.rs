//! Building the outgoing FT8/FT4 messages for both contest profiles.
//!
//! The QSO engine ([`crate::engine`]) decides *which* message to send; this
//! module turns (my station, partner, payload) into a concrete
//! [`OutgoingMessage`] — the on-air text plus its structured [`ParsedMessage`]
//! twin, so the rest of the app never re-parses the string.
//!
//! The strings must match WSJT-X exactly (word order, `R` placement, `RR73`/
//! `73`) so a WSJT-X partner's auto-sequencer accepts our decodes as "for us".
//! See `docs/wsjtx_qso_sequencing.md` §2 (the six slots) and §5 (Field Day).

use types::{
    Callsign, ContestProfile, ContestTag, ExchangePayload, GridSquare, OutgoingMessage,
    ParsedMessage, Section, Signoff,
};

/// The local operator's identity and exchange — everything needed to *build*
/// outgoing messages. The engine owns one of these; the GUI updates it live from
/// the call/grid header and the contest control (mirroring the rig/audio
/// live-reconfig handles, since nobody publishes `OperatingState` yet).
#[derive(Clone, Debug, PartialEq)]
pub struct StationConfig {
    pub call: Callsign,
    pub grid: GridSquare,
    /// Field Day transmitter count + class letter, e.g. `"3A"`.
    pub fd_class: String,
    /// Field Day ARRL/RAC section, e.g. `"CO"`.
    pub fd_section: Section,
    pub contest: ContestProfile,
}

impl StationConfig {
    /// True when operating the ARRL Field Day exchange.
    pub fn is_field_day(&self) -> bool {
        matches!(self.contest, ContestProfile::ArrlFieldDay)
    }
}

/// Format a signal report the WSJT-X way (`%+2.2d`): sign + two digits, ASCII
/// (not the Unicode minus the console uses for display). `-7 -> "-07"`,
/// `5 -> "+05"`, `-15 -> "-15"`.
fn fmt_report(snr: i8) -> String {
    format!("{snr:+03}")
}

/// `Tx6` — call CQ. Standard: `CQ <mine> <grid>`; Field Day: `CQ FD <mine>
/// <grid>` (the grid is retained in the CQ even though it is dropped from the
/// answering exchange).
pub fn cq(me: &StationConfig) -> OutgoingMessage {
    let (text, contest) = if me.is_field_day() {
        (
            format!("CQ FD {} {}", me.call.0, me.grid.0),
            Some(ContestTag::FieldDay),
        )
    } else {
        (format!("CQ {} {}", me.call.0, me.grid.0), None)
    };
    OutgoingMessage {
        text,
        structured: ParsedMessage::Cq {
            caller: me.call.clone(),
            contest,
            grid: Some(me.grid.clone()),
        },
    }
}

/// `Tx1` — answer a CQ with our grid: `<his> <mine> <grid>` (Standard only; the
/// grid step is skipped in Field Day, where the opener is the bare exchange).
pub fn answer_grid(me: &StationConfig, his: &Callsign) -> OutgoingMessage {
    OutgoingMessage {
        text: format!("{} {} {}", his.0, me.call.0, me.grid.0),
        structured: exchange(his, me, ExchangePayload::Grid(me.grid.clone())),
    }
}

/// `Tx2` — send a signal report: `<his> <mine> <report>` (Standard).
pub fn report(me: &StationConfig, his: &Callsign, snr: i8) -> OutgoingMessage {
    OutgoingMessage {
        text: format!("{} {} {}", his.0, me.call.0, fmt_report(snr)),
        structured: exchange(his, me, ExchangePayload::Report(snr)),
    }
}

/// `Tx3` — roger + report: `<his> <mine> R<report>` (Standard).
pub fn roger_report(me: &StationConfig, his: &Callsign, snr: i8) -> OutgoingMessage {
    OutgoingMessage {
        text: format!("{} {} R{}", his.0, me.call.0, fmt_report(snr)),
        structured: exchange(his, me, ExchangePayload::RogerReport(snr)),
    }
}

/// `Tx2` — Field Day bare exchange: `<his> <mine> <class> <section>`. This is the
/// answering station's opener (no grid).
pub fn fd_exchange(me: &StationConfig, his: &Callsign) -> OutgoingMessage {
    OutgoingMessage {
        text: format!(
            "{} {} {} {}",
            his.0, me.call.0, me.fd_class, me.fd_section.0
        ),
        structured: exchange(his, me, fd_payload(me, false)),
    }
}

/// `Tx3` — Field Day rogered exchange: `<his> <mine> R <class> <section>`. One
/// message that both rogers the partner's exchange and sends ours (CQ side).
pub fn fd_roger_exchange(me: &StationConfig, his: &Callsign) -> OutgoingMessage {
    OutgoingMessage {
        text: format!(
            "{} {} R {} {}",
            his.0, me.call.0, me.fd_class, me.fd_section.0
        ),
        structured: exchange(his, me, fd_payload(me, true)),
    }
}

/// `Tx4` — roger: `<his> <mine> RR73`. (We always send `RR73`, never `RRR`; we
/// accept both inbound — `docs/qso_flow.md` §7.)
pub fn rr73(me: &StationConfig, his: &Callsign) -> OutgoingMessage {
    signoff(me, his, Signoff::Rr73, "RR73")
}

/// `Tx5` — sign off: `<his> <mine> 73`.
pub fn seven3(me: &StationConfig, his: &Callsign) -> OutgoingMessage {
    signoff(me, his, Signoff::Seven3, "73")
}

fn signoff(me: &StationConfig, his: &Callsign, kind: Signoff, word: &str) -> OutgoingMessage {
    OutgoingMessage {
        text: format!("{} {} {}", his.0, me.call.0, word),
        structured: ParsedMessage::Signoff {
            to: his.clone(),
            from: me.call.clone(),
            kind,
        },
    }
}

fn exchange(to: &Callsign, from: &StationConfig, payload: ExchangePayload) -> ParsedMessage {
    ParsedMessage::Exchange {
        to: to.clone(),
        from: from.call.clone(),
        payload,
    }
}

fn fd_payload(me: &StationConfig, rogered: bool) -> ExchangePayload {
    ExchangePayload::FieldDay {
        class: me.fd_class.clone(),
        section: me.fd_section.clone(),
        rogered,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn me_standard() -> StationConfig {
        StationConfig {
            call: Callsign("W9XYZ".into()),
            grid: GridSquare("EM48".into()),
            fd_class: "3A".into(),
            fd_section: Section("WI".into()),
            contest: ContestProfile::Standard,
        }
    }

    fn me_fd() -> StationConfig {
        StationConfig {
            contest: ContestProfile::ArrlFieldDay,
            ..me_standard()
        }
    }

    fn k1abc() -> Callsign {
        Callsign("K1ABC".into())
    }

    #[test]
    fn report_formatting_matches_wsjtx() {
        assert_eq!(fmt_report(-7), "-07");
        assert_eq!(fmt_report(5), "+05");
        assert_eq!(fmt_report(-15), "-15");
        assert_eq!(fmt_report(0), "+00");
    }

    #[test]
    fn standard_strings() {
        assert_eq!(cq(&me_standard()).text, "CQ W9XYZ EM48");
        assert_eq!(
            answer_grid(&me_standard(), &k1abc()).text,
            "K1ABC W9XYZ EM48"
        );
        assert_eq!(report(&me_standard(), &k1abc(), -9).text, "K1ABC W9XYZ -09");
        assert_eq!(
            roger_report(&me_standard(), &k1abc(), -9).text,
            "K1ABC W9XYZ R-09"
        );
        assert_eq!(rr73(&me_standard(), &k1abc()).text, "K1ABC W9XYZ RR73");
        assert_eq!(seven3(&me_standard(), &k1abc()).text, "K1ABC W9XYZ 73");
    }

    #[test]
    fn field_day_strings() {
        assert_eq!(cq(&me_fd()).text, "CQ FD W9XYZ EM48");
        // No grid opener in FD — the answerer opens with the bare exchange.
        assert_eq!(fd_exchange(&me_fd(), &k1abc()).text, "K1ABC W9XYZ 3A WI");
        assert_eq!(
            fd_roger_exchange(&me_fd(), &k1abc()).text,
            "K1ABC W9XYZ R 3A WI"
        );
    }

    #[test]
    fn structured_twin_round_trips_through_parse_shape() {
        // The structured twin must agree with the text's meaning.
        match cq(&me_fd()).structured {
            ParsedMessage::Cq { contest, .. } => assert_eq!(contest, Some(ContestTag::FieldDay)),
            other => panic!("expected Cq, got {other:?}"),
        }
        match fd_roger_exchange(&me_fd(), &k1abc()).structured {
            ParsedMessage::Exchange {
                payload: ExchangePayload::FieldDay { rogered, .. },
                ..
            } => assert!(rogered),
            other => panic!("expected rogered FieldDay, got {other:?}"),
        }
    }
}
