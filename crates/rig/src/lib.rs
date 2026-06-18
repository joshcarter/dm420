//! Kenwood CAT rig control.
//!
//! Layers, bottom to top:
//! - [`codec`] — pure CAT encode/decode (no I/O, exhaustively tested).
//! - [`channel`] — the [`channel::CatChannel`] seam; [`channel::SerialChannel`]
//!   does framing over a byte stream.
//! - [`mock`] — an in-memory radio simulator implementing `CatChannel`.
//! - [`CatRig`] — implements [`Rig`] for any channel (`KenwoodRig` / `MockRig`).
//! - [`actor`] — a thread that solely owns the rig and serializes access,
//!   exposing the cloneable [`actor::RigHandle`].
//!
//! Everything crossing the actor boundary is `serde`-serializable so this lifts
//! cleanly into the message-bus architecture of the full application later.

pub mod actor;
pub mod catrig;
pub mod channel;
pub mod codec;
pub mod mock;
pub mod ports;
pub mod probe;

pub use actor::{RigEvent, RigHandle, RigRequest, RigResponse, spawn};
pub use catrig::{CatRig, KenwoodRig, MockRig, mock_rig, open_serial};
pub use codec::{CodecError, Mode, RigState, Vfo};
pub use ports::{PortInfo, list_ports};
pub use probe::LineProfile;

use std::time::Duration;

/// Errors from rig operations.
#[derive(Debug, thiserror::Error)]
pub enum RigError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serial port error: {0}")]
    Serial(#[from] serialport::Error),
    #[error("radio rejected command '{0}' (responded '?') — likely unsupported on this model")]
    Rejected(String),
    #[error("radio reported a communication error (responded 'E')")]
    CommErr,
    #[error("radio reported a buffer overflow (responded 'O')")]
    Overflow,
    #[error("timed out waiting for reply to '{0}'")]
    Timeout(String),
    #[error("transmit is blocked — restart kenctl with --allow-transmit to enable TX")]
    TransmitBlocked,
    #[error("no response from radio")]
    NoResponse,
    #[error("unexpected response from actor: {0}")]
    Unexpected(String),
    #[error(transparent)]
    Codec(#[from] CodecError),
    #[error("rig actor is not running")]
    ActorGone,
}

/// The radio control interface. Deliberately small (per ft8-app-design.md §4) so
/// a hamlib-backed implementation could slot in later; the broad "full control"
/// command set is reached through [`Rig::raw`]. Implemented by [`CatRig`].
pub trait Rig: Send {
    fn set_freq(&mut self, vfo: Vfo, hz: u64) -> Result<(), RigError>;
    fn get_freq(&mut self, vfo: Vfo) -> Result<u64, RigError>;
    fn set_mode(&mut self, mode: Mode) -> Result<(), RigError>;
    fn get_mode(&mut self) -> Result<Mode, RigError>;
    fn set_ptt(&mut self, tx: bool) -> Result<(), RigError>;
    /// One `IF;` snapshot of radio state.
    fn get_state(&mut self) -> Result<RigState, RigError>;
    /// Send a verbatim CAT command (escape hatch = full control). Returns the
    /// response, or `None` for a set command that produced none.
    fn raw(&mut self, cmd: &str) -> Result<Option<String>, RigError>;
    /// Set the Auto-Information level (`AI0`/`AI2`).
    fn set_auto_info(&mut self, level: u8) -> Result<(), RigError>;
    /// Collect unsolicited messages received, waiting up to `timeout`.
    fn pump_events(&mut self, timeout: Duration) -> Result<Vec<String>, RigError>;
}
