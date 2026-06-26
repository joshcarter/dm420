//! The FT8 panel's "Send" row: the outgoing-message box, its arm/transmit
//! lifecycle, and the slash-command parser.
//!
//! Mock-only for now — there is no QSO engine or rig wiring behind this yet.
//! The text box auto-fills with the next message implied by the current
//! selection (CQ, or a Tx1 answer to a clicked station); the operator arms it
//! with Enter or the button. Naming mirrors the future bus types (`QsoState`,
//! `OutgoingMessage`) so the later swap to a real engine is mechanical.
//!
//! See `docs/qso_flow.md` for the operator model this implements.

use bus::types::{Band, OverAirMode};

use crate::waterslide_panel::Target;

/// What pressing Enter / the Send button means right now. The transmit lifecycle
/// itself lives in the QSO engine (`QsoState.phase`); the send row only renders
/// that and issues commands.
#[derive(Clone, PartialEq, Debug)]
pub enum Activation {
    /// A completed slash command to apply (e.g. set frequency).
    Command(Command),
    /// Not composing a command — toggle the QSO engine. The panel decides
    /// arm-vs-abort from the live `QsoState` phase.
    Toggle,
    /// Nothing actionable (an empty or unrecognized command was abandoned).
    None,
}

/// A parsed slash/colon command.
#[derive(Clone, PartialEq, Debug)]
pub enum Command {
    /// Set the dial frequency, in MHz (e.g. `/f 14.074`).
    SetFrequency(f64),
    /// Switch to a band (e.g. `/b 20m`). The dial moves to the calling frequency
    /// for that band in the *current* mode — resolved at apply time via
    /// [`calling_freq_hz`], since the parser doesn't know the mode.
    SetBand(Band),
    /// Jump the TX audio offset to the clearest CQ lane (`/clear`).
    ClearQsy,
    /// Toggle the band scanner (`/scan`): start a survey of the configured stops, or
    /// cancel the one in progress. The Digital panel resolves which from `ScannerState`.
    Scan,
}

/// Parse a slash/colon command. Returns `None` if it isn't a command or the
/// verb/argument isn't recognized. Verb matching is case-insensitive; the
/// frequency verb accepts `f`, `freq`, or `frequency`; the band verb accepts
/// `b` or `band`.
pub fn parse_command(input: &str) -> Option<Command> {
    let s = input.trim();
    let rest = s.strip_prefix('/').or_else(|| s.strip_prefix(':'))?;
    let mut tokens = rest.split_whitespace();
    let verb = tokens.next()?.to_ascii_lowercase();
    match verb.as_str() {
        "f" | "freq" | "frequency" => {
            let mhz: f64 = tokens.next()?.parse().ok()?;
            (mhz.is_finite() && mhz > 0.0).then_some(Command::SetFrequency(mhz))
        }
        "b" | "band" => parse_band(tokens.next()?).map(Command::SetBand),
        "clear" => Some(Command::ClearQsy),
        "scan" => Some(Command::Scan),
        _ => None,
    }
}

/// Parse a band argument — `20`, `20m`, or `20M` — into a [`Band`]. The meter
/// count must name a real amateur HF/6 m band.
pub(crate) fn parse_band(arg: &str) -> Option<Band> {
    let meters: u16 = arg.trim_end_matches(['m', 'M']).parse().ok()?;
    Some(match meters {
        160 => Band::B160m,
        80 => Band::B80m,
        40 => Band::B40m,
        30 => Band::B30m,
        20 => Band::B20m,
        17 => Band::B17m,
        15 => Band::B15m,
        12 => Band::B12m,
        10 => Band::B10m,
        6 => Band::B6m,
        _ => return None,
    })
}

/// The dial (calling) frequency in Hz for a band in a given mode. Thin GUI-side
/// wrapper over [`bus::types::calling_freq`] — the shared source of truth — that
/// unwraps the `AbsHz` to the `u64` the send/parse path uses.
pub fn calling_freq_hz(band: Band, mode: OverAirMode) -> Option<u64> {
    bus::types::calling_freq(band, mode).map(|a| a.0)
}

/// The next auto-generated message for the current selection, built with the same
/// `qso::message` formatters the engine transmits with — so the send-box preview
/// can never drift from the real on-air text. A CQ when the operator picked bare
/// spectrum, or the Tx1 opener when they picked a station; the active
/// [`qso::StationConfig`] (its `ContestProfile`) selects the wording: Standard
/// gives `CQ <mine> <grid>` / `<his> <mine> <grid>`, Field Day gives
/// `CQ FD <mine> <grid>` / the bare `<his> <mine> <class> <section>` opener.
pub fn next_message(target: &Target, me: &qso::StationConfig) -> String {
    match target.station() {
        Some(call) => {
            let his = types::Callsign(call.to_string());
            if me.is_field_day() {
                qso::message::fd_exchange(me, &his).text
            } else {
                qso::message::answer_grid(me, &his).text
            }
        }
        None => qso::message::cq(me).text,
    }
}

/// State of the send row: the displayed/edit buffer plus where we are in the
/// lifecycle.
///
/// The box is **not** a free text field. Normally `buf` mirrors the engine's
/// next outgoing message (read-only). The operator can only do two things:
/// press `/` or `:` to start typing a slash command (which replaces the buffer),
/// or press Enter to activate the Send/Cancel button. `entering` distinguishes
/// "showing the auto message" from "the operator is composing a command".
#[derive(Default)]
pub struct SendState {
    pub buf: String,
    /// True while composing a slash command (`buf` is the operator's input, not
    /// the auto message). Set by `/`/`:`, cleared on Enter/Escape/empty backspace.
    pub entering: bool,
}

impl SendState {
    /// Keep the box mirroring the engine's next message, unless the operator is
    /// mid-command. Independent of arm state — the box always shows what we'd send.
    pub fn refresh_auto(&mut self, target: &Target, me: &qso::StationConfig) {
        if !self.entering {
            self.buf = next_message(target, me);
        }
    }

    /// Feed typed text (an egui `Event::Text` payload). Outside command entry,
    /// only a leading `/` or `:` does anything — it starts a command and clears
    /// the box. While composing, characters append. Everything else is ignored,
    /// so the box can never hold arbitrary free text.
    pub fn type_text(&mut self, text: &str) {
        for ch in text.chars() {
            if self.entering {
                self.buf.push(ch);
            } else if ch == '/' || ch == ':' {
                self.entering = true;
                self.buf.clear();
                self.buf.push(ch);
            }
        }
    }

    /// Backspace while composing a command. Backing out the last character exits
    /// command entry (the box returns to showing the auto message next frame).
    pub fn backspace(&mut self) {
        if self.entering {
            self.buf.pop();
            if self.buf.is_empty() {
                self.entering = false;
            }
        }
    }

    /// Abandon a command in progress.
    pub fn escape(&mut self) {
        if self.entering {
            self.entering = false;
            self.buf.clear();
        }
    }

    /// Handle Enter or a button press. While composing a command, parse and return
    /// it (then leave command entry). Otherwise it's a [`Activation::Toggle`] —
    /// the panel arms or aborts the engine depending on the live `QsoState`.
    pub fn activate(&mut self) -> Activation {
        if self.entering {
            let cmd = parse_command(&self.buf);
            self.entering = false;
            self.buf.clear();
            return cmd.map_or(Activation::None, Activation::Command);
        }
        Activation::Toggle
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frequency_verbs_and_aliases() {
        for s in [
            "/f 14.074",
            ":f 14.074",
            "/freq 14.074",
            "/frequency 14.074",
            "  /F  14.074 ",
        ] {
            assert_eq!(
                parse_command(s),
                Some(Command::SetFrequency(14.074)),
                "input {s:?}"
            );
        }
    }

    #[test]
    fn rejects_non_commands_and_bad_args() {
        // Not a command (no leading / or :).
        assert_eq!(parse_command("f 14.074"), None);
        assert_eq!(parse_command("CQ N0JDC DN70"), None);
        // Unknown verb.
        assert_eq!(parse_command("/x 20"), None);
        // Missing or non-numeric argument.
        assert_eq!(parse_command("/f"), None);
        assert_eq!(parse_command("/f abc"), None);
        // Nonsensical frequency.
        assert_eq!(parse_command("/f 0"), None);
        assert_eq!(parse_command("/f -1"), None);
    }

    #[test]
    fn parses_band_verbs_and_flexible_args() {
        // `/b` and `/band`, with or without a trailing `m`/`M`, all name 20 m.
        for s in ["/b 20", "/b 20m", "/band 20M", ":band 20m", "  /B  20m "] {
            assert_eq!(
                parse_command(s),
                Some(Command::SetBand(Band::B20m)),
                "input {s:?}"
            );
        }
        // Other bands resolve too.
        assert_eq!(parse_command("/b 40"), Some(Command::SetBand(Band::B40m)));
        assert_eq!(parse_command("/b 6"), Some(Command::SetBand(Band::B6m)));
        // A meter count that isn't a real band, and a missing argument, are rejected.
        assert_eq!(parse_command("/b 21"), None);
        assert_eq!(parse_command("/b"), None);
        assert_eq!(parse_command("/b xyz"), None);
    }

    #[test]
    fn parses_scan_and_clear_verbs() {
        for s in ["/scan", ":scan", "  /SCAN ", "/Scan"] {
            assert_eq!(parse_command(s), Some(Command::Scan), "input {s:?}");
        }
        assert_eq!(parse_command("/clear"), Some(Command::ClearQsy));
    }

    #[test]
    fn calling_freq_is_mode_dependent() {
        // The canonical 20 m split: FT8 at 14.074, FT4 at 14.080.
        assert_eq!(
            calling_freq_hz(Band::B20m, OverAirMode::Ft8),
            Some(14_074_000)
        );
        assert_eq!(
            calling_freq_hz(Band::B20m, OverAirMode::Ft4),
            Some(14_080_000)
        );
        // FT4 has no established 160 m calling frequency.
        assert_eq!(calling_freq_hz(Band::B160m, OverAirMode::Ft4), None);
        assert_eq!(
            calling_freq_hz(Band::B160m, OverAirMode::Ft8),
            Some(1_840_000)
        );
    }

    #[test]
    fn auto_message_reflects_selection() {
        let me = qso::StationConfig {
            call: types::Callsign("N0JDC".into()),
            grid: types::GridSquare("DN70".into()),
            fd_class: "3A".into(),
            fd_section: types::Section("CO".into()),
            contest: types::ContestProfile::Standard,
        };
        let station = Target::Station {
            call: "K1ABC".into(),
            off: 1180,
        };
        // Standard: plain CQ + grid answer.
        assert_eq!(next_message(&Target::Offset(300), &me), "CQ N0JDC DN70");
        assert_eq!(next_message(&station, &me), "K1ABC N0JDC DN70");

        // Field Day: CQ FD + the bare <class> <section> opener (no grid) — the same
        // strings the engine builds, so the preview matches what actually airs.
        let fd = qso::StationConfig {
            contest: types::ContestProfile::ArrlFieldDay,
            ..me.clone()
        };
        assert_eq!(next_message(&Target::Offset(300), &fd), "CQ FD N0JDC DN70");
        assert_eq!(next_message(&station, &fd), "K1ABC N0JDC 3A CO");
    }

    #[test]
    fn typing_only_accepts_slash_commands() {
        // standing auto message in the box
        let mut s = SendState {
            buf: "CQ N0JDC DN70".into(),
            ..Default::default()
        };
        // Arbitrary text is ignored — the box can't hold free text.
        s.type_text("hello");
        assert!(!s.entering);
        assert_eq!(s.buf, "CQ N0JDC DN70");
        // A leading `/` starts a command and replaces the box.
        s.type_text("/");
        assert!(s.entering);
        assert_eq!(s.buf, "/");
        s.type_text("f 14.074");
        assert_eq!(s.buf, "/f 14.074");
        // Enter parses, applies, and leaves command entry.
        assert_eq!(
            s.activate(),
            Activation::Command(Command::SetFrequency(14.074))
        );
        assert!(!s.entering);
        assert!(s.buf.is_empty());
        // `:` is an equivalent command prefix.
        s.type_text(":freq 7.074");
        assert_eq!(
            s.activate(),
            Activation::Command(Command::SetFrequency(7.074))
        );
    }

    #[test]
    fn enter_is_a_toggle_when_not_composing() {
        let mut s = SendState {
            buf: "CQ N0JDC DN70".into(),
            ..Default::default()
        };
        // Outside command entry, Enter toggles the engine — the panel resolves
        // arm-vs-abort from the live QsoState phase.
        assert_eq!(s.activate(), Activation::Toggle);
        assert_eq!(s.activate(), Activation::Toggle);
    }

    #[test]
    fn empty_command_is_a_noop_not_a_toggle() {
        let mut s = SendState::default();
        s.type_text("/"); // started a command, then…
        assert_eq!(s.activate(), Activation::None); // …submitted nothing parseable
        assert!(!s.entering);
    }

    #[test]
    fn backspace_and_escape_exit_command_entry() {
        let mut s = SendState::default();
        s.type_text("/");
        s.backspace(); // backing out the last char exits entry
        assert!(!s.entering);
        s.type_text("/f");
        s.escape();
        assert!(!s.entering);
        assert!(s.buf.is_empty());
    }
}
