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

use crate::waterslide_panel::Target;

/// Where the send row is in its transmit lifecycle.
///
/// `Idle` → the auto-filled message is shown, nothing queued. `Armed` → queued,
/// will transmit on the next slot. `Transmitting` → on the air this slot. The
/// button reads Send/Send/Cancel across these (color shifts idle→accent2).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ArmState {
    #[default]
    Idle,
    Armed,
    Transmitting,
}

/// A parsed slash/colon command. Only frequency-set exists for now.
#[derive(Clone, PartialEq, Debug)]
pub enum Command {
    /// Set the dial frequency, in MHz (e.g. `/f 14.074`).
    SetFrequency(f64),
}

/// True if `input` looks like a command attempt (a leading `/` or `:`), so the
/// caller can route Enter to the parser rather than the arm/disarm toggle.
pub fn is_command(input: &str) -> bool {
    let s = input.trim_start();
    s.starts_with('/') || s.starts_with(':')
}

/// Parse a slash/colon command. Returns `None` if it isn't a command or the
/// verb/argument isn't recognized. Verb matching is case-insensitive; the
/// frequency verb accepts `f`, `freq`, or `frequency`.
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
        _ => None,
    }
}

/// The next auto-generated message for the current selection: a CQ call when the
/// operator picked bare spectrum, or a Tx1 grid answer when they picked a station.
pub fn next_message(target: &Target, mycall: &str, grid: &str) -> String {
    match target.station() {
        Some(call) => format!("{call} {mycall} {grid}"),
        None => format!("CQ {mycall} {grid}"),
    }
}

/// State of the send row: the edit buffer plus where we are in the lifecycle.
#[derive(Default)]
pub struct SendState {
    pub armed: ArmState,
    /// What's shown in / typed into the box. Auto-refreshed from the selection
    /// while idle and unfocused; left alone while the operator is typing.
    pub buf: String,
}

impl SendState {
    /// While idle and not being typed into, keep the box mirroring the message
    /// the engine would send next. Skipped when focused so typing isn't clobbered.
    pub fn sync_auto(&mut self, focused: bool, target: &Target, mycall: &str, grid: &str) {
        if !focused && self.armed == ArmState::Idle {
            self.buf = next_message(target, mycall, grid);
        }
    }

    /// Handle Enter or a button press. If the buffer holds a command, parse it
    /// and return it for the caller to apply (the buffer is cleared so it
    /// re-syncs to the auto message next frame). Otherwise toggle arm/disarm and
    /// return `None`.
    pub fn activate(&mut self) -> Option<Command> {
        if is_command(&self.buf) {
            let cmd = parse_command(&self.buf);
            self.buf.clear();
            return cmd;
        }
        self.armed = match self.armed {
            ArmState::Idle => ArmState::Armed,
            // Both Armed and Transmitting disarm (the single Stop control).
            _ => ArmState::Idle,
        };
        None
    }

    /// Advance the mock lifecycle on a slot boundary. FT8 transmits in alternate
    /// slots, so once armed we ping-pong Armed↔Transmitting: queue, transmit,
    /// listen, transmit again — matching the real even/odd-slot cadence.
    pub fn slot_tick(&mut self) {
        self.armed = match self.armed {
            ArmState::Armed => ArmState::Transmitting,
            ArmState::Transmitting => ArmState::Armed,
            ArmState::Idle => ArmState::Idle,
        };
    }

    /// Button label for the current state.
    pub fn button_label(&self) -> &'static str {
        match self.armed {
            ArmState::Transmitting => "Cancel",
            _ => "Send",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frequency_verbs_and_aliases() {
        for s in ["/f 14.074", ":f 14.074", "/freq 14.074", "/frequency 14.074", "  /F  14.074 "] {
            assert_eq!(parse_command(s), Some(Command::SetFrequency(14.074)), "input {s:?}");
        }
    }

    #[test]
    fn rejects_non_commands_and_bad_args() {
        // Not a command (no leading / or :).
        assert_eq!(parse_command("f 14.074"), None);
        assert_eq!(parse_command("CQ N0JDC DN70"), None);
        // Unknown verb.
        assert_eq!(parse_command("/b 20"), None);
        // Missing or non-numeric argument.
        assert_eq!(parse_command("/f"), None);
        assert_eq!(parse_command("/f abc"), None);
        // Nonsensical frequency.
        assert_eq!(parse_command("/f 0"), None);
        assert_eq!(parse_command("/f -1"), None);
    }

    #[test]
    fn is_command_detects_prefixes() {
        assert!(is_command("/f 14.074"));
        assert!(is_command("  :freq 7.074"));
        assert!(!is_command("CQ N0JDC DN70"));
        assert!(!is_command(""));
    }

    #[test]
    fn auto_message_reflects_selection() {
        let cq = next_message(&Target::Offset(300), "N0JDC", "DN70");
        assert_eq!(cq, "CQ N0JDC DN70");
        let answer = next_message(
            &Target::Station { call: "K1ABC".into(), off: 1180 },
            "N0JDC",
            "DN70",
        );
        assert_eq!(answer, "K1ABC N0JDC DN70");
    }

    #[test]
    fn enter_toggles_arm_and_command_does_not() {
        let mut s = SendState::default();
        s.buf = "CQ N0JDC DN70".into();
        assert_eq!(s.activate(), None);
        assert_eq!(s.armed, ArmState::Armed);
        // Enter again disarms.
        assert_eq!(s.activate(), None);
        assert_eq!(s.armed, ArmState::Idle);
        // A command applies and clears the buffer without arming.
        s.buf = "/f 14.074".into();
        assert_eq!(s.activate(), Some(Command::SetFrequency(14.074)));
        assert_eq!(s.armed, ArmState::Idle);
        assert!(s.buf.is_empty());
    }

    #[test]
    fn slot_tick_pingpongs_only_when_armed() {
        let mut s = SendState::default();
        s.slot_tick();
        assert_eq!(s.armed, ArmState::Idle); // idle stays idle
        s.armed = ArmState::Armed;
        s.slot_tick();
        assert_eq!(s.armed, ArmState::Transmitting);
        s.slot_tick();
        assert_eq!(s.armed, ArmState::Armed);
    }
}
