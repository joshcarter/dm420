//! [`CatRig`] implements the [`Rig`] trait for any [`CatChannel`]. The real and
//! mock radios differ *only* in their channel, so all CAT orchestration lives
//! here once. `KenwoodRig` = CAT over serial; `MockRig` = CAT over the in-memory
//! simulator.

use crate::channel::{CatChannel, Expect, SerialByteIo, SerialChannel};
use crate::codec::{self, Mode, RigState, Vfo};
use crate::mock::MockChannel;
use crate::{Rig, RigError};
use std::time::Duration;
use tracing::{info, warn};

/// A rig driven by CAT over some [`CatChannel`].
pub struct CatRig<C: CatChannel> {
    ch: C,
}

impl<C: CatChannel> CatRig<C> {
    pub fn new(ch: C) -> Self {
        CatRig { ch }
    }
}

impl<C: CatChannel> Rig for CatRig<C> {
    fn set_freq(&mut self, vfo: Vfo, hz: u64) -> Result<(), RigError> {
        self.ch
            .exchange(&codec::set_freq_cmd(vfo, hz), Expect::NoReply)?;
        Ok(())
    }

    fn get_freq(&mut self, vfo: Vfo) -> Result<u64, RigError> {
        let resp = self
            .ch
            .exchange(&codec::get_freq_cmd(vfo), Expect::Reply)?
            .ok_or(RigError::NoResponse)?;
        Ok(codec::parse_freq_response(&resp)?)
    }

    fn set_mode(&mut self, mode: Mode) -> Result<(), RigError> {
        self.ch
            .exchange(&codec::set_mode_cmd(mode), Expect::NoReply)?;
        Ok(())
    }

    fn get_mode(&mut self) -> Result<Mode, RigError> {
        let resp = self
            .ch
            .exchange("MD", Expect::Reply)?
            .ok_or(RigError::NoResponse)?;
        Ok(codec::parse_mode_response(&resp)?)
    }

    fn set_ptt(&mut self, tx: bool) -> Result<(), RigError> {
        // `TX1` keys the **rear/data** audio route (USB/ACC2); bare `TX` (= `TX0`)
        // keys the front **mic** route. DM420 is a digital-mode app, so the data
        // route is always what we want — keying it is what makes the rig modulate
        // the USB audio we play, independent of the rig's "source of SEND/PTT"
        // (FRONT/REAR) menu. Key-down (`RX`) is route-independent.
        let cmd = if tx { "TX1" } else { "RX" };
        self.ch.exchange(cmd, Expect::NoReply)?;
        Ok(())
    }

    fn get_state(&mut self) -> Result<RigState, RigError> {
        let resp = self
            .ch
            .exchange("IF", Expect::Reply)?
            .ok_or(RigError::NoResponse)?;
        let state = codec::parse_if_response(&resp)?;
        if !state.fields_parsed {
            // Frequency is still trustworthy; the rest needs offset correction.
            warn!(
                raw_if = %state.raw_if,
                len = state.raw_if.len(),
                "IF response shorter than expected — only frequency decoded; \
                 verify IF offsets against this capture (see poc-cli-plan.md Test 1.2)"
            );
        }
        Ok(state)
    }

    fn raw(&mut self, cmd: &str) -> Result<Option<String>, RigError> {
        let cmd = cmd.trim().trim_end_matches(';');
        if cmd.len() < 2 {
            return Err(RigError::Rejected(cmd.to_string()));
        }
        self.ch.exchange(cmd, Expect::Any)
    }

    fn set_auto_info(&mut self, level: u8) -> Result<(), RigError> {
        self.ch.exchange(&format!("AI{level}"), Expect::NoReply)?;
        Ok(())
    }

    fn pump_events(&mut self, timeout: Duration) -> Result<Vec<String>, RigError> {
        self.ch.drain_unsolicited(timeout)
    }
}

/// A real Kenwood radio: CAT over a serial port.
pub type KenwoodRig = CatRig<SerialChannel<SerialByteIo>>;

/// An in-memory simulated radio (offline demo / tests).
pub type MockRig = CatRig<MockChannel>;

/// Open a serial port (8N1) with the given control-line/flow [`LineProfile`] and
/// return a ready [`KenwoodRig`].
pub fn open_serial(
    port: &str,
    baud: u32,
    profile: crate::probe::LineProfile,
) -> Result<KenwoodRig, RigError> {
    info!(%port, baud, profile = profile.label(), "opening serial port (8N1)");
    let sp = crate::probe::open_port(port, baud, profile, Duration::from_millis(100))?;
    let ch = SerialChannel::new(
        SerialByteIo::new(sp),
        Duration::from_millis(600), // response timeout
        Duration::from_millis(150), // error window for set commands
    );
    Ok(CatRig::new(ch))
}

/// Build an in-memory mock radio.
pub fn mock_rig() -> MockRig {
    CatRig::new(MockChannel::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_rig_freq_roundtrip() {
        let mut rig = mock_rig();
        rig.set_freq(Vfo::A, 18_100_000).unwrap();
        assert_eq!(rig.get_freq(Vfo::A).unwrap(), 18_100_000);
    }

    #[test]
    fn mock_rig_mode_roundtrip() {
        let mut rig = mock_rig();
        rig.set_mode(Mode::Cw).unwrap();
        assert_eq!(rig.get_mode().unwrap(), Mode::Cw);
    }

    #[test]
    fn mock_rig_state_reflects_ptt_and_mode() {
        let mut rig = mock_rig();
        rig.set_freq(Vfo::A, 14_074_000).unwrap();
        rig.set_mode(Mode::Usb).unwrap();
        rig.set_ptt(true).unwrap();
        let st = rig.get_state().unwrap();
        assert_eq!(st.freq_hz, 14_074_000);
        assert_eq!(st.mode, Some(Mode::Usb));
        assert!(st.tx);
        assert!(st.fields_parsed);
        rig.set_ptt(false).unwrap();
        assert!(!rig.get_state().unwrap().tx);
    }

    #[test]
    fn mock_rig_raw_passthrough() {
        let mut rig = mock_rig();
        assert_eq!(rig.raw("ID").unwrap().as_deref(), Some("ID021"));
        // Raw set then raw get of a command we never wrapped (keyer speed).
        rig.raw("KS030").unwrap();
        assert_eq!(rig.raw("KS").unwrap().as_deref(), Some("KS030"));
        // A trailing ';' typed by the user is tolerated.
        assert_eq!(rig.raw("ID;").unwrap().as_deref(), Some("ID021"));
        assert!(rig.raw("ZZ").is_err());
    }
}
