//! The CAT I/O boundary.
//!
//! [`CatChannel`] is the seam every higher layer talks through: send a command,
//! optionally wait for its reply, and drain unsolicited (Auto-Information)
//! traffic. [`SerialChannel`] implements it over a real (or fake) byte stream,
//! owning all the framing/timeout/interleave logic. The in-memory simulator
//! lives in `mock.rs`.

use crate::RigError;
use std::collections::VecDeque;
use std::io;
use std::time::{Duration, Instant};
use tracing::{debug, trace, warn};

/// What a caller expects back from a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expect {
    /// A *get*: wait up to the response timeout for a reply matching the mnemonic.
    Reply,
    /// A *set*: no reply expected; briefly watch for a `?` rejection, then return.
    NoReply,
    /// Best-effort (used by `raw`): return a matching reply if one arrives within
    /// the response timeout, else `None`.
    Any,
}

/// The seam between rig logic and the wire. Implemented by [`SerialChannel`]
/// (real radio) and `MockChannel` (in-memory simulator).
pub trait CatChannel: Send {
    /// Send `cmd` (without trailing `;`) and handle the reply per `expect`.
    /// Returns the reply string (sans `;`) for gets, or `None` for sets / no reply.
    fn exchange(&mut self, cmd: &str, expect: Expect) -> Result<Option<String>, RigError>;

    /// Collect unsolicited messages already buffered, plus any that arrive within
    /// `timeout`. A zero timeout returns immediately with whatever is buffered
    /// (no blocking read).
    fn drain_unsolicited(&mut self, timeout: Duration) -> Result<Vec<String>, RigError>;
}

/// A minimal byte stream: the part of a serial port we depend on. Abstracted so
/// the framing logic can be exercised against a scripted [`FakeByteIo`].
pub trait ByteIo: Send {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()>;
    /// Read available bytes. Returns 0 on timeout / no data (never an error for a
    /// plain timeout).
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
}

/// CAT channel over a byte stream. Owns an accumulation buffer and frames
/// `;`-terminated messages, separating solicited replies from unsolicited events.
pub struct SerialChannel<B: ByteIo> {
    io: B,
    buf: Vec<u8>,
    /// Messages seen while awaiting a reply that weren't the reply — surfaced
    /// later via [`CatChannel::drain_unsolicited`].
    pending: VecDeque<String>,
    response_timeout: Duration,
    error_window: Duration,
}

impl<B: ByteIo> SerialChannel<B> {
    pub fn new(io: B, response_timeout: Duration, error_window: Duration) -> Self {
        SerialChannel {
            io,
            buf: Vec::with_capacity(256),
            pending: VecDeque::new(),
            response_timeout,
            error_window,
        }
    }

    /// Pull all complete `;`-terminated messages out of the buffer, leaving any
    /// trailing partial message in place.
    fn frame(&mut self) -> Vec<String> {
        let mut msgs = Vec::new();
        while let Some(pos) = self.buf.iter().position(|&b| b == b';') {
            let chunk: Vec<u8> = self.buf.drain(..=pos).collect();
            let text = String::from_utf8_lossy(&chunk[..chunk.len() - 1])
                .trim()
                .to_string();
            if !text.is_empty() {
                trace!(msg = %text, "framed CAT message");
                msgs.push(text);
            }
        }
        msgs
    }

    fn read_into_buf(&mut self) -> Result<usize, RigError> {
        let mut tmp = [0u8; 256];
        let n = self.io.read(&mut tmp)?;
        if n > 0 {
            self.buf.extend_from_slice(&tmp[..n]);
            // Log the actual bytes so a silent/garbled radio is diagnosable from
            // the log alone (this is what tells "no bytes" from "wrong baud").
            debug!(bytes = n, rx = %crate::probe::hex_ascii(&tmp[..n]), "serial rx");
        }
        Ok(n)
    }

    /// Classify a framed message. Returns `Err` for radio-level error tokens.
    fn check_error(msg: &str, cmd: &str) -> Result<(), RigError> {
        match msg {
            "?" => {
                warn!(%cmd, "radio rejected command (responded '?')");
                Err(RigError::Rejected(cmd.to_string()))
            }
            "E" => Err(RigError::CommErr),
            "O" => Err(RigError::Overflow),
            _ => Ok(()),
        }
    }
}

impl<B: ByteIo> CatChannel for SerialChannel<B> {
    fn exchange(&mut self, cmd: &str, expect: Expect) -> Result<Option<String>, RigError> {
        // Drain stale traffic before issuing a new command: keep any complete
        // messages (they may be real events) but discard a partial remainder that
        // would otherwise corrupt the next reply match.
        for m in self.frame() {
            self.pending.push_back(m);
        }
        if !self.buf.is_empty() {
            trace!(stale = self.buf.len(), "discarding partial stale bytes");
            self.buf.clear();
        }

        let prefix: String = cmd.chars().take(2).collect();
        debug!(tx = %cmd, ?expect, "CAT send");
        self.io.write_all(format!("{cmd};").as_bytes())?;

        let timeout = match expect {
            Expect::Reply | Expect::Any => self.response_timeout,
            Expect::NoReply => self.error_window,
        };
        let deadline = Instant::now() + timeout;

        loop {
            for msg in self.frame() {
                Self::check_error(&msg, cmd)?;
                let matches = msg.len() >= 2 && msg.starts_with(&prefix);
                match expect {
                    Expect::NoReply => self.pending.push_back(msg),
                    Expect::Reply | Expect::Any if matches => {
                        debug!(rx = %msg, "CAT reply");
                        return Ok(Some(msg));
                    }
                    _ => self.pending.push_back(msg),
                }
            }
            if Instant::now() >= deadline {
                return match expect {
                    Expect::Reply => {
                        warn!(%cmd, ?timeout, "no reply within timeout");
                        Err(RigError::Timeout(cmd.to_string()))
                    }
                    Expect::NoReply | Expect::Any => Ok(None),
                };
            }
            self.read_into_buf()?;
        }
    }

    fn drain_unsolicited(&mut self, timeout: Duration) -> Result<Vec<String>, RigError> {
        let mut out: Vec<String> = self.pending.drain(..).collect();
        if timeout.is_zero() {
            out.extend(self.frame());
            return Ok(out);
        }
        let deadline = Instant::now() + timeout;
        loop {
            self.read_into_buf()?;
            out.extend(self.frame());
            if Instant::now() >= deadline {
                break;
            }
        }
        Ok(out)
    }
}

/// A real serial port behind [`ByteIo`]. A read timeout becomes `Ok(0)`.
pub struct SerialByteIo {
    port: Box<dyn serialport::SerialPort>,
}

impl SerialByteIo {
    pub fn new(port: Box<dyn serialport::SerialPort>) -> Self {
        SerialByteIo { port }
    }
}

impl ByteIo for SerialByteIo {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        self.port.write_all(data)?;
        self.port.flush()
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.port.read(buf) {
            Ok(n) => Ok(n),
            Err(e) if e.kind() == io::ErrorKind::TimedOut => Ok(0),
            Err(e) => Err(e),
        }
    }
}

/// Scripted byte stream for tests. `to_read` is a queue of chunks; each `read`
/// pops one (modelling partial reads), and an exhausted queue reads as a timeout.
#[derive(Default)]
pub struct FakeByteIo {
    pub written: Vec<u8>,
    pub to_read: VecDeque<Vec<u8>>,
}

impl FakeByteIo {
    /// Queue a complete radio response string (the `;` is appended for you).
    pub fn queue_response(&mut self, msg: &str) {
        self.to_read.push_back(format!("{msg};").into_bytes());
    }

    /// Queue an arbitrary raw byte chunk (to model split / partial reads).
    pub fn queue_bytes(&mut self, bytes: &[u8]) {
        self.to_read.push_back(bytes.to_vec());
    }

    /// The commands written so far, split on `;`.
    pub fn written_commands(&self) -> Vec<String> {
        String::from_utf8_lossy(&self.written)
            .split(';')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect()
    }
}

impl ByteIo for FakeByteIo {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        self.written.extend_from_slice(data);
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.to_read.pop_front() {
            Some(chunk) => {
                let n = chunk.len().min(buf.len());
                buf[..n].copy_from_slice(&chunk[..n]);
                if n < chunk.len() {
                    // Buffer was smaller than the chunk; push the remainder back.
                    self.to_read.push_front(chunk[n..].to_vec());
                }
                Ok(n)
            }
            None => Ok(0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(io: FakeByteIo) -> SerialChannel<FakeByteIo> {
        // Tiny timeouts keep tests fast; FakeByteIo never blocks.
        SerialChannel::new(io, Duration::from_millis(20), Duration::from_millis(5))
    }

    #[test]
    fn exchange_reply_matches_prefix() {
        let mut io = FakeByteIo::default();
        io.queue_response("FA00014074000");
        let mut ch = channel(io);
        let resp = ch.exchange("FA", Expect::Reply).unwrap();
        assert_eq!(resp.as_deref(), Some("FA00014074000"));
        assert_eq!(ch.io.written_commands(), vec!["FA"]);
    }

    #[test]
    fn exchange_set_returns_none() {
        let mut ch = channel(FakeByteIo::default());
        let resp = ch.exchange("FA00014074000", Expect::NoReply).unwrap();
        assert_eq!(resp, None);
        assert_eq!(ch.io.written_commands(), vec!["FA00014074000"]);
    }

    #[test]
    fn exchange_stashes_interleaved_unsolicited() {
        // An unsolicited frequency change arrives before the IF reply we asked for.
        let mut io = FakeByteIo::default();
        io.queue_response("FA00014074000"); // unsolicited
        io.queue_response("IF00014074000aaaaaaaaaaaaaaaaaaaaaaa"); // the reply
        let mut ch = channel(io);
        let resp = ch.exchange("IF", Expect::Reply).unwrap().unwrap();
        assert!(resp.starts_with("IF"));
        let events = ch.drain_unsolicited(Duration::ZERO).unwrap();
        assert_eq!(events, vec!["FA00014074000"]);
    }

    #[test]
    fn exchange_rejected_is_error() {
        let mut io = FakeByteIo::default();
        io.queue_response("?");
        let mut ch = channel(io);
        let err = ch.exchange("DA", Expect::Reply).unwrap_err();
        assert!(matches!(err, RigError::Rejected(_)));
    }

    #[test]
    fn framing_across_partial_reads() {
        // The reply is split across three reads, including mid-token boundaries.
        let mut io = FakeByteIo::default();
        io.queue_bytes(b"FA000");
        io.queue_bytes(b"14074");
        io.queue_bytes(b"000;");
        let mut ch = channel(io);
        let resp = ch.exchange("FA", Expect::Reply).unwrap();
        assert_eq!(resp.as_deref(), Some("FA00014074000"));
    }

    #[test]
    fn reply_timeout_errors() {
        // Nothing queued: a get times out.
        let mut ch = channel(FakeByteIo::default());
        let err = ch.exchange("FA", Expect::Reply).unwrap_err();
        assert!(matches!(err, RigError::Timeout(_)));
    }

    #[test]
    fn drain_unsolicited_reads_within_timeout() {
        let mut io = FakeByteIo::default();
        io.queue_response("MD2");
        let mut ch = channel(io);
        let events = ch.drain_unsolicited(Duration::from_millis(20)).unwrap();
        assert_eq!(events, vec!["MD2"]);
    }
}
