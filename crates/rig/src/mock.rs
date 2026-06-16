//! An in-memory Kenwood radio. [`MockChannel`] interprets CAT commands against
//! mutable state and synthesizes spec-shaped responses, so the entire stack —
//! codec, [`crate::CatRig`], the actor, and the REPL — can run and be tested with
//! no hardware. It is also the seed of the design doc's "replay rig manager":
//! `kenctl --mock` is a usable offline demo.

use crate::channel::{CatChannel, Expect};
use crate::codec::{self, Mode, Vfo};
use crate::RigError;
use std::collections::VecDeque;
use std::time::Duration;
use tracing::trace;

/// Mutable state of the simulated radio.
#[derive(Debug, Clone)]
pub struct MockState {
    pub freq_a: u64,
    pub freq_b: u64,
    pub mode: Mode,
    pub tx: bool,
    pub rx_vfo: Vfo,
    pub tx_vfo: Vfo,
    pub rit_on: bool,
    pub power: u32,   // PC, watts
    pub af_gain: u32, // AG0, 0..255
    pub rf_gain: u32, // RG, 0..255
    pub smeter: u32,  // SM0, 0..30 (simulated)
    pub data_mode: bool,
    pub ant: u8, // 1 or 2
    pub preamp: bool,
    pub att: bool,
    pub nb: bool,
    pub nr: bool,
    pub agc: u32,       // GT, AGC time constant
    pub keyer_wpm: u32, // KS, used as the "unwrapped raw command" demo
    pub id: String,     // e.g. "021" = TS-590S
    pub ai_level: u8,
}

impl Default for MockState {
    fn default() -> Self {
        MockState {
            freq_a: 14_074_000,
            freq_b: 7_074_000,
            mode: Mode::Usb,
            tx: false,
            rx_vfo: Vfo::A,
            tx_vfo: Vfo::A,
            rit_on: false,
            power: 50,
            af_gain: 100,
            rf_gain: 255,
            smeter: 9,
            data_mode: false,
            ant: 1,
            preamp: false,
            att: false,
            nb: false,
            nr: false,
            agc: 2,
            keyer_wpm: 20,
            id: "021".to_string(),
            ai_level: 0,
        }
    }
}

impl MockState {
    fn split(&self) -> bool {
        self.rx_vfo != self.tx_vfo
    }

    fn rx_freq(&self) -> u64 {
        match self.rx_vfo {
            Vfo::A => self.freq_a,
            Vfo::B => self.freq_b,
        }
    }

    /// Build a spec-shaped `IF` response from current state, matching the offsets
    /// in [`codec::parse_if_response`] (37 chars, sans the framing `;`).
    fn if_response(&self) -> String {
        format!(
            "IF{freq:011}00000+0000{rit_on}00{mem}{tx}{mode}{fr}{scan}{split}000{p15}",
            freq = self.rx_freq(),
            rit_on = self.rit_on as u8,
            mem = "00",
            tx = self.tx as u8,
            mode = self.mode.to_digit(),
            fr = match self.rx_vfo {
                Vfo::A => 0,
                Vfo::B => 1,
            },
            scan = 0,
            split = self.split() as u8,
            p15 = 0,
        )
    }
}

/// In-memory CAT channel. Drives a [`MockState`] and can queue unsolicited
/// messages (e.g. to test Auto-Information / watch mode).
#[derive(Default)]
pub struct MockChannel {
    pub state: MockState,
    unsolicited: VecDeque<String>,
}

impl MockChannel {
    pub fn new(state: MockState) -> Self {
        MockChannel {
            state,
            unsolicited: VecDeque::new(),
        }
    }

    /// Push an unsolicited message that a later `drain_unsolicited` will return.
    pub fn push_event(&mut self, msg: &str) {
        self.unsolicited.push_back(msg.to_string());
    }

    /// Interpret one CAT command, mutating state and returning a response (sans
    /// `;`), or `None` if the command produces no reply. Unknown commands yield
    /// `Some("?")`, exactly as a radio rejects them.
    fn process(&mut self, cmd: &str) -> Option<String> {
        let s = &self.state;
        // Helper: does the command carry a parameter beyond the mnemonic?
        let body = cmd.get(2..).unwrap_or("");

        let resp = match &cmd[..cmd.len().min(2)] {
            "ID" => Some(format!("ID{}", s.id)),
            "PS" => Some("PS1".to_string()),
            "AI" => {
                if body.is_empty() {
                    Some(format!("AI{}", s.ai_level))
                } else {
                    self.state.ai_level = body.parse().unwrap_or(0);
                    None
                }
            }
            "FA" => self.freq_cmd(Vfo::A, body),
            "FB" => self.freq_cmd(Vfo::B, body),
            "MD" => {
                if body.is_empty() {
                    Some(format!("MD{}", s.mode.to_digit()))
                } else {
                    match Mode::from_digit(body.chars().next().unwrap()) {
                        Some(m) => {
                            self.state.mode = m;
                            None
                        }
                        None => Some("?".to_string()),
                    }
                }
            }
            "IF" => Some(s.if_response()),
            "TX" => {
                self.state.tx = true;
                None
            }
            "RX" => {
                self.state.tx = false;
                None
            }
            "FR" => self.vfo_cmd(false, body),
            "FT" => self.vfo_cmd(true, body),
            "RT" => self.bool_cmd(body, |st| &mut st.rit_on, "RT"),
            "RC" => {
                None // clear RIT offset — no-op in the mock
            }
            "PC" => self.int_cmd(body, |st| &mut st.power, 3, "PC"),
            "RG" => self.int_cmd(body, |st| &mut st.rf_gain, 3, "RG"),
            "AG" => {
                // AF gain has a channel digit: get is "AG0", set "AG0nnn".
                let arg = cmd.get(3..).unwrap_or("");
                if !cmd.starts_with("AG0") {
                    Some("?".to_string())
                } else if arg.is_empty() {
                    Some(format!("AG0{:03}", s.af_gain))
                } else {
                    self.state.af_gain = arg.parse().unwrap_or(s.af_gain);
                    None
                }
            }
            "SM" => {
                // S-meter read only, channel digit 0: "SM0" -> "SM0nnnn".
                if cmd.starts_with("SM0") {
                    Some(format!("SM0{:04}", s.smeter))
                } else {
                    Some("?".to_string())
                }
            }
            "DA" => self.bool_cmd(body, |st| &mut st.data_mode, "DA"),
            "PA" => self.bool_cmd(body, |st| &mut st.preamp, "PA"),
            "NB" => self.bool_cmd(body, |st| &mut st.nb, "NB"),
            "NR" => self.bool_cmd(body, |st| &mut st.nr, "NR"),
            "GT" => self.int_cmd(body, |st| &mut st.agc, 3, "GT"),
            "RA" => {
                // Attenuator, two digits: "RA00" off / "RA01" on.
                if body.is_empty() {
                    Some(format!("RA{:02}", s.att as u8))
                } else {
                    self.state.att = body.trim().parse::<u32>().map(|v| v != 0).unwrap_or(false);
                    None
                }
            }
            "AN" => {
                if body.is_empty() {
                    Some(format!("AN{}", s.ant))
                } else {
                    self.state.ant = body.parse().unwrap_or(s.ant);
                    None
                }
            }
            "KS" => self.int_cmd(body, |st| &mut st.keyer_wpm, 3, "KS"),
            _ => Some("?".to_string()),
        };
        trace!(%cmd, ?resp, "mock processed command");
        resp
    }

    fn freq_cmd(&mut self, vfo: Vfo, body: &str) -> Option<String> {
        let slot = match vfo {
            Vfo::A => &mut self.state.freq_a,
            Vfo::B => &mut self.state.freq_b,
        };
        if body.is_empty() {
            Some(codec::set_freq_cmd(vfo, *slot))
        } else {
            match body.trim().parse::<u64>() {
                Ok(hz) => {
                    *slot = hz;
                    None
                }
                Err(_) => Some("?".to_string()),
            }
        }
    }

    /// FR (rx vfo, `is_tx=false`) / FT (tx vfo, `is_tx=true`): 0 = A, 1 = B.
    fn vfo_cmd(&mut self, is_tx: bool, body: &str) -> Option<String> {
        let cur = if is_tx {
            self.state.tx_vfo
        } else {
            self.state.rx_vfo
        };
        if body.is_empty() {
            let digit = match cur {
                Vfo::A => 0,
                Vfo::B => 1,
            };
            Some(format!("{}{}", if is_tx { "FT" } else { "FR" }, digit))
        } else {
            let vfo = match body.chars().next() {
                Some('0') => Vfo::A,
                Some('1') => Vfo::B,
                _ => return Some("?".to_string()),
            };
            if is_tx {
                self.state.tx_vfo = vfo;
            } else {
                self.state.rx_vfo = vfo;
            }
            None
        }
    }

    fn bool_cmd(
        &mut self,
        body: &str,
        field: impl Fn(&mut MockState) -> &mut bool,
        mnemonic: &str,
    ) -> Option<String> {
        if body.is_empty() {
            Some(format!("{mnemonic}{}", *field(&mut self.state) as u8))
        } else {
            *field(&mut self.state) = body.starts_with('1');
            None
        }
    }

    fn int_cmd(
        &mut self,
        body: &str,
        field: impl Fn(&mut MockState) -> &mut u32,
        width: usize,
        mnemonic: &str,
    ) -> Option<String> {
        if body.is_empty() {
            Some(format!(
                "{mnemonic}{:0width$}",
                *field(&mut self.state),
                width = width
            ))
        } else {
            match body.trim().parse::<u32>() {
                Ok(v) => {
                    *field(&mut self.state) = v;
                    None
                }
                Err(_) => Some("?".to_string()),
            }
        }
    }
}

impl CatChannel for MockChannel {
    fn exchange(&mut self, cmd: &str, _expect: Expect) -> Result<Option<String>, RigError> {
        let cmd = cmd.trim().trim_end_matches(';');
        if cmd.len() < 2 {
            return Err(RigError::Rejected(cmd.to_string()));
        }
        match self.process(cmd) {
            Some(r) if r == "?" => Err(RigError::Rejected(cmd.to_string())),
            other => Ok(other),
        }
    }

    fn drain_unsolicited(&mut self, _timeout: Duration) -> Result<Vec<String>, RigError> {
        Ok(self.unsolicited.drain(..).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch() -> MockChannel {
        MockChannel::default()
    }

    #[test]
    fn freq_get_set() {
        let mut c = ch();
        assert_eq!(
            c.exchange("FA", Expect::Reply).unwrap().as_deref(),
            Some("FA00014074000")
        );
        c.exchange("FA00007074000", Expect::NoReply).unwrap();
        assert_eq!(
            c.exchange("FA", Expect::Reply).unwrap().as_deref(),
            Some("FA00007074000")
        );
    }

    #[test]
    fn mode_get_set_and_reject() {
        let mut c = ch();
        c.exchange("MD3", Expect::NoReply).unwrap();
        assert_eq!(
            c.exchange("MD", Expect::Reply).unwrap().as_deref(),
            Some("MD3")
        );
        assert!(c.exchange("MD8", Expect::NoReply).is_err());
    }

    #[test]
    fn if_reflects_state() {
        let mut c = ch();
        c.exchange("FA00021074000", Expect::NoReply).unwrap();
        c.exchange("MD1", Expect::NoReply).unwrap();
        c.exchange("TX", Expect::NoReply).unwrap();
        let resp = c.exchange("IF", Expect::Reply).unwrap().unwrap();
        let st = codec::parse_if_response(&resp).unwrap();
        assert_eq!(st.freq_hz, 21_074_000);
        assert_eq!(st.mode, Some(Mode::Lsb));
        assert!(st.tx);
    }

    #[test]
    fn split_via_ft() {
        let mut c = ch();
        c.exchange("FT1", Expect::NoReply).unwrap(); // TX on VFO B
        let st =
            codec::parse_if_response(&c.exchange("IF", Expect::Reply).unwrap().unwrap()).unwrap();
        assert!(st.split);
        c.exchange("FT0", Expect::NoReply).unwrap();
        let st =
            codec::parse_if_response(&c.exchange("IF", Expect::Reply).unwrap().unwrap()).unwrap();
        assert!(!st.split);
    }

    #[test]
    fn power_and_gains() {
        let mut c = ch();
        c.exchange("PC100", Expect::NoReply).unwrap();
        assert_eq!(
            c.exchange("PC", Expect::Reply).unwrap().as_deref(),
            Some("PC100")
        );
        c.exchange("AG0200", Expect::NoReply).unwrap();
        assert_eq!(
            c.exchange("AG0", Expect::Reply).unwrap().as_deref(),
            Some("AG0200")
        );
        assert_eq!(
            c.exchange("SM0", Expect::Reply).unwrap().as_deref(),
            Some("SM00009")
        );
    }

    #[test]
    fn keyer_speed_roundtrip() {
        // The "raw passthrough demonstrates full control" command from Phase 1.
        let mut c = ch();
        c.exchange("KS028", Expect::NoReply).unwrap();
        assert_eq!(
            c.exchange("KS", Expect::Reply).unwrap().as_deref(),
            Some("KS028")
        );
    }

    #[test]
    fn id_and_unknown() {
        let mut c = ch();
        assert_eq!(
            c.exchange("ID", Expect::Reply).unwrap().as_deref(),
            Some("ID021")
        );
        assert!(c.exchange("ZZ", Expect::Reply).is_err());
    }

    #[test]
    fn auto_info_and_events() {
        let mut c = ch();
        c.exchange("AI2", Expect::NoReply).unwrap();
        assert_eq!(c.state.ai_level, 2);
        c.push_event("FA00014074000");
        let events = c.drain_unsolicited(Duration::ZERO).unwrap();
        assert_eq!(events, vec!["FA00014074000"]);
    }
}
