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

/// A parsed slash/colon command. Only frequency-set exists for now.
#[derive(Clone, PartialEq, Debug)]
pub enum Command {
    /// Set the dial frequency, in MHz (e.g. `/f 14.074`).
    SetFrequency(f64),
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
    pub fn refresh_auto(&mut self, target: &Target, mycall: &str, grid: &str) {
        if !self.entering {
            self.buf = next_message(target, mycall, grid);
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
    fn typing_only_accepts_slash_commands() {
        // standing auto message in the box
        let mut s = SendState { buf: "CQ N0JDC DN70".into(), ..Default::default() };
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
        assert_eq!(s.activate(), Activation::Command(Command::SetFrequency(14.074)));
        assert!(!s.entering);
        assert!(s.buf.is_empty());
        // `:` is an equivalent command prefix.
        s.type_text(":freq 7.074");
        assert_eq!(s.activate(), Activation::Command(Command::SetFrequency(7.074)));
    }

    #[test]
    fn enter_is_a_toggle_when_not_composing() {
        let mut s = SendState { buf: "CQ N0JDC DN70".into(), ..Default::default() };
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
