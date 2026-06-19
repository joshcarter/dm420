//! Best-effort serial auto-detection and low-level diagnostics.
//!
//! When the radio isn't talking, the question is always "are bytes coming back
//! at all?" The normal channel only surfaces `;`-framed messages, so this module
//! works at the raw byte level: it opens a port at a baud, sends `ID;`, captures
//! every byte that returns (logged as hex), reads the modem control lines, and
//! classifies the result. [`autodetect`] sweeps candidate ports × baud rates and,
//! on failure, emits a hypothesis from the aggregate outcomes.
//!
//! This deliberately bypasses [`crate::CatRig`] and the actor: it needs raw,
//! unframed bytes and direct control of one short-lived port open per attempt.

use crate::RigError;
use crate::ports::{self, PortInfo};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// Standard Kenwood USB/COM baud rates, fastest first (most installs use 115200).
pub const KENWOOD_BAUDS: &[u32] = &[115_200, 57_600, 38_400, 19_200, 9_600, 4_800];

/// Serial control-line / flow-control profile used when opening a port. A radio
/// that receives commands but never replies is usually gating its transmit on a
/// handshake line the host isn't driving — these cover the fixes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LineProfile {
    /// No flow control; control lines left as the OS set them (original behavior).
    Default,
    /// No flow control, but DTR and RTS driven high. Fixes radios that hold their
    /// transmit until the host asserts RTS/DTR (the usual one-way-comms cause).
    AssertDtrRts,
    /// Hardware (RTS/CTS) flow control, managed by the OS.
    HardwareFlow,
}

impl LineProfile {
    pub fn label(self) -> &'static str {
        match self {
            LineProfile::Default => "none",
            LineProfile::AssertDtrRts => "dtr-rts",
            LineProfile::HardwareFlow => "rtscts",
        }
    }

    /// Parse a `--flow` value.
    pub fn parse(s: &str) -> Option<LineProfile> {
        Some(match s.trim().to_lowercase().as_str() {
            "none" | "off" => LineProfile::Default,
            "dtr-rts" | "dtrrts" | "auto" => LineProfile::AssertDtrRts,
            "rtscts" | "hardware" | "hw" => LineProfile::HardwareFlow,
            _ => return None,
        })
    }

    /// True if opening with this profile drives RTS/DTR high — which can key PTT
    /// on a radio configured for PTT/SEND-by-RTS/DTR.
    pub fn asserts_control_lines(self) -> bool {
        matches!(self, LineProfile::AssertDtrRts)
    }
}

/// Profiles auto-detect tries per baud, in order (baseline first for a clean A/B).
pub const PROBE_PROFILES: &[LineProfile] = &[
    LineProfile::Default,
    LineProfile::AssertDtrRts,
    LineProfile::HardwareFlow,
];

/// Open a serial port (8N1) with the given control-line/flow profile. Shared by
/// the probe and the normal rig open path so they behave identically.
pub fn open_port(
    port: &str,
    baud: u32,
    profile: LineProfile,
    timeout: Duration,
) -> Result<Box<dyn serialport::SerialPort>, serialport::Error> {
    let flow = match profile {
        LineProfile::HardwareFlow => serialport::FlowControl::Hardware,
        _ => serialport::FlowControl::None,
    };
    let mut sp = serialport::new(port, baud)
        .data_bits(serialport::DataBits::Eight)
        .parity(serialport::Parity::None)
        .stop_bits(serialport::StopBits::One)
        .flow_control(flow)
        .timeout(timeout)
        .open()?;
    if profile == LineProfile::AssertDtrRts {
        // Best effort; some platforms/devices don't support setting these.
        let _ = sp.write_data_terminal_ready(true);
        let _ = sp.write_request_to_send(true);
    }
    Ok(sp)
}

/// How one (port, baud) probe turned out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProbeOutcome {
    /// A valid `IDxxx` came back — this port+baud works.
    Identified(String),
    /// `;`-framed message(s) returned, but none was a valid ID.
    Framed,
    /// Bytes returned but never framed into a `;`-terminated message — almost
    /// always a baud mismatch (wrong bit timing → no recognizable terminator).
    Garbage,
    /// The port opened and we sent, but zero bytes came back — one-way comms.
    Silent,
    /// The port could not be opened at all.
    OpenFailed(String),
}

/// Result of probing one (port, baud), including the raw bytes for diagnosis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeResult {
    pub port: String,
    pub baud: u32,
    pub profile: LineProfile,
    pub outcome: ProbeOutcome,
    pub rx_bytes: Vec<u8>,
    pub framed: Vec<String>,
    /// Modem control lines read just after open (CD/DSR/CTS), if the OS exposed
    /// them. Carrier-detect low on a macOS `/dev/tty.*` device is a classic
    /// "opens but never receives" cause.
    pub carrier_detect: Option<bool>,
    pub data_set_ready: Option<bool>,
    pub clear_to_send: Option<bool>,
    pub elapsed_ms: u128,
}

impl ProbeResult {
    pub fn is_success(&self) -> bool {
        matches!(self.outcome, ProbeOutcome::Identified(_))
    }

    /// The identified ID string, or `""` if this attempt didn't identify.
    pub fn id_str(&self) -> &str {
        match &self.outcome {
            ProbeOutcome::Identified(id) => id,
            _ => "",
        }
    }

    /// One-line human summary of the outcome.
    pub fn describe(&self) -> String {
        match &self.outcome {
            ProbeOutcome::Identified(id) => format!("OK — radio identified ({id})"),
            ProbeOutcome::Framed => format!("framed reply but no ID: {:?}", self.framed),
            ProbeOutcome::Garbage => format!(
                "{} bytes returned but unframed (baud mismatch?): {}",
                self.rx_bytes.len(),
                hex_ascii(&self.rx_bytes)
            ),
            ProbeOutcome::Silent => "silent — port opened, sent, zero bytes back".to_string(),
            ProbeOutcome::OpenFailed(e) => format!("open failed: {e}"),
        }
    }
}

/// Aggregate result of an auto-detect sweep.
#[derive(Debug, Clone)]
pub struct AutodetectReport {
    pub attempts: Vec<ProbeResult>,
    pub winner: Option<ProbeResult>,
}

/// Render bytes as hex plus a printable-ASCII gutter, for logs/diagnostics.
pub fn hex_ascii(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "<none>".to_string();
    }
    let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02X}")).collect();
    let ascii: String = bytes
        .iter()
        .map(|&b| {
            if (0x20..0x7f).contains(&b) {
                b as char
            } else {
                '.'
            }
        })
        .collect();
    format!("{} |{ascii}|", hex.join(" "))
}

/// Validate a framed message as a Kenwood ID response (`ID` + digits).
fn parse_id(msg: &str) -> Option<String> {
    let m = msg.trim();
    let rest = m.strip_prefix("ID")?;
    if !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()) {
        Some(m.to_string())
    } else {
        None
    }
}

/// Split raw bytes into `;`-terminated messages (trailing partial dropped).
fn frame(buf: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, &b) in buf.iter().enumerate() {
        if b == b';' {
            let s = String::from_utf8_lossy(&buf[start..i]).trim().to_string();
            if !s.is_empty() {
                out.push(s);
            }
            start = i + 1;
        }
    }
    out
}

/// Classify raw bytes + framed messages into an outcome.
fn classify(rx: &[u8], framed: &[String]) -> ProbeOutcome {
    if let Some(id) = framed.iter().find_map(|m| parse_id(m)) {
        ProbeOutcome::Identified(id)
    } else if !framed.is_empty() {
        ProbeOutcome::Framed
    } else if !rx.is_empty() {
        ProbeOutcome::Garbage
    } else {
        ProbeOutcome::Silent
    }
}

/// Probe one port at one baud with a given line profile: open, read modem lines,
/// settle, send `ID;`, and capture whatever returns within `window`. Never
/// panics; logs everything.
pub fn probe_once(port: &str, baud: u32, profile: LineProfile, window: Duration) -> ProbeResult {
    let start = Instant::now();
    let mk = |outcome, rx: Vec<u8>, framed, cd, dsr, cts| ProbeResult {
        port: port.to_string(),
        baud,
        profile,
        outcome,
        rx_bytes: rx,
        framed,
        carrier_detect: cd,
        data_set_ready: dsr,
        clear_to_send: cts,
        elapsed_ms: start.elapsed().as_millis(),
    };

    debug!(%port, baud, profile = profile.label(), "probe: opening (8N1)");
    let mut sp = match open_port(port, baud, profile, Duration::from_millis(100)) {
        Ok(sp) => sp,
        Err(e) => {
            warn!(%port, baud, profile = profile.label(), error = %e, "probe: open failed");
            return mk(
                ProbeOutcome::OpenFailed(e.to_string()),
                Vec::new(),
                Vec::new(),
                None,
                None,
                None,
            );
        }
    };

    // Modem control lines (best effort). CD low on /dev/tty.* explains a port
    // that opens and sends but never receives.
    let cd = sp.read_carrier_detect().ok();
    let dsr = sp.read_data_set_ready().ok();
    let cts = sp.read_clear_to_send().ok();
    debug!(%port, baud, ?cd, ?dsr, ?cts, "probe: modem lines (CD/DSR/CTS)");

    // Settle, then drain any bytes the adapter emitted on open.
    std::thread::sleep(Duration::from_millis(120));
    let mut tmp = [0u8; 256];
    let mut pre = Vec::new();
    if let Ok(n) = sp.read(&mut tmp) {
        if n > 0 {
            pre.extend_from_slice(&tmp[..n]);
        }
    }
    if !pre.is_empty() {
        debug!(%port, baud, pre = %hex_ascii(&pre), "probe: pre-send bytes (discarded)");
    }

    // Send the identity query and capture the reply window.
    if let Err(e) = sp.write_all(b"ID;").and_then(|_| sp.flush()) {
        warn!(%port, baud, error = %e, "probe: write failed");
        return mk(
            ProbeOutcome::OpenFailed(e.to_string()),
            Vec::new(),
            Vec::new(),
            cd,
            dsr,
            cts,
        );
    }
    debug!(%port, baud, profile = profile.label(), "probe: sent 'ID;'");

    let mut rx = Vec::new();
    let deadline = Instant::now() + window;
    while Instant::now() < deadline {
        match sp.read(&mut tmp) {
            Ok(0) => {}
            Ok(n) => rx.extend_from_slice(&tmp[..n]),
            Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => {
                warn!(%port, baud, error = %e, "probe: read error");
                break;
            }
        }
    }

    let framed = frame(&rx);
    let outcome = classify(&rx, &framed);
    info!(
        %port,
        baud,
        profile = profile.label(),
        ?outcome,
        rx = %hex_ascii(&rx),
        framed = ?framed,
        "probe result"
    );
    mk(outcome, rx, framed, cd, dsr, cts)
}

/// Candidate ports for auto-detect, ordered best-first.
pub fn candidate_ports(all: bool) -> Result<Vec<String>, RigError> {
    Ok(order_ports(ports::list_ports()?, all))
}

/// Order/filter discovered ports: likely radios first, `cu.*` before `tty.*`
/// (macOS call-out vs dial-in), then by name. Without `all`, keep only the
/// likely-radio ports when any exist.
fn order_ports(mut ports: Vec<PortInfo>, all: bool) -> Vec<String> {
    if !all && ports.iter().any(|p| p.likely_radio) {
        ports.retain(|p| p.likely_radio);
    }
    ports.sort_by_key(|p| (!p.likely_radio, p.name.contains("/tty."), p.name.clone()));
    ports.into_iter().map(|p| p.name).collect()
}

/// Resolve a remembered USB device identity to the port path it currently holds.
///
/// The device path (`/dev/cu.usbserial-{location}`) is the USB *location id* and
/// changes whenever the cable moves to a different port/hub. The USB serial
/// number is stable, so we persist that and re-resolve it to the live path on
/// every connect. Returns `None` if nothing matches — the caller falls back to
/// the saved path, then autodetect.
pub fn resolve_port_by_identity(
    serial: Option<&str>,
    vid: Option<u16>,
    pid: Option<u16>,
) -> Option<String> {
    let ports = ports::list_ports().ok()?;
    resolve_in(&ports, serial, vid, pid)
}

/// The pure matching core (testable without hardware): an exact serial-number
/// match is the strong key; with no serial we accept a *unique* vid/pid match and
/// otherwise refuse to guess.
fn resolve_in(
    ports: &[PortInfo],
    serial: Option<&str>,
    vid: Option<u16>,
    pid: Option<u16>,
) -> Option<String> {
    if let Some(want) = serial.filter(|s| !s.is_empty()) {
        return ports
            .iter()
            .find(|p| p.serial_number.as_deref() == Some(want))
            .map(|p| p.name.clone());
    }
    // No serial recorded: fall back to vid/pid, but only when it identifies a
    // single port — two same-model adapters can't be told apart this way.
    if let (Some(vid), Some(pid)) = (vid, pid) {
        let mut hits = ports.iter().filter(|p| p.vid == Some(vid) && p.pid == Some(pid));
        let first = hits.next()?;
        if hits.next().is_none() {
            return Some(first.name.clone());
        }
    }
    None
}

/// Form a plain-language hypothesis from a failed sweep's aggregate outcomes.
fn hypothesis(attempts: &[ProbeResult]) -> String {
    use ProbeOutcome::*;
    let total = attempts.len();
    let count = |f: fn(&ProbeOutcome) -> bool| attempts.iter().filter(|a| f(&a.outcome)).count();
    let silent = count(|o| matches!(o, Silent));
    let garbage = count(|o| matches!(o, Garbage));
    let framed = count(|o| matches!(o, Framed));
    let openfail = count(|o| matches!(o, OpenFailed(_)));
    // Did any tty.* device read carrier-detect low? Strong signal for the macOS
    // tty-vs-cu trap.
    let tty_cd_low = attempts
        .iter()
        .any(|a| a.port.contains("/tty.") && a.carrier_detect == Some(false));
    // Did we try asserting control lines / hardware flow (and still get nothing)?
    let tried_lines = attempts.iter().any(|a| a.profile != LineProfile::Default);

    let mut h = String::from("Hypothesis: ");
    if total == 0 {
        h.push_str("no ports were probed — is the radio connected and powered?");
    } else if openfail == total {
        h.push_str("no port could be opened — check the device path and permissions.");
    } else if silent + openfail == total && silent > 0 && tried_lines {
        h.push_str(
            "the radio returns ZERO bytes even with DTR/RTS asserted AND with hardware (RTS/CTS) \
             flow control, at every baud. So it is not a handshake/flow-control problem. The radio \
             receives commands (the dial moves) but transmits nothing back. Check the radio side: \
             is the USB port enabled for CAT / 'PC control' turned on, and is CAT routed to USB \
             rather than the DB-9 COM port? Then RX wiring. The CD/DSR/CTS lines are logged per \
             attempt.",
        );
    } else if silent + openfail == total && silent > 0 {
        h.push_str(
            "the port opens and we send, but the radio returns ZERO bytes at every baud. \
             This is one-way comms: TX (computer->radio) is reaching it (you see the dial move), \
             while RX (radio->computer) is dead. Likely causes, in order: (1) wrong device node — \
             on macOS use /dev/cu.* not /dev/tty.* (tty blocks on carrier-detect)",
        );
        if tty_cd_low {
            h.push_str(" — and indeed a tty.* device reported carrier-detect LOW here, which fits");
        }
        h.push_str(
            "; (2) the radio needs DTR/RTS asserted (try --flow dtr-rts or --flow rtscts); \
             (3) the radio's CAT/USB output is off or routed to the DB-9 COM port; (4) RX wiring. \
             The CD/DSR/CTS modem lines are in the log per attempt.",
        );
    } else if garbage > 0 && framed == 0 {
        h.push_str(
            "bytes come back but never frame into ';'-terminated messages — a baud-rate mismatch, \
             and none of the standard Kenwood rates lined up. Check the radio's USB baud menu \
             against the bytes logged per attempt.",
        );
    } else {
        h.push_str("mixed results — see the per-attempt outcomes and raw bytes in the log.");
    }
    h
}

/// Sweep `ports` × `bauds` × `profiles`, stopping at the first combination that
/// identifies. `report` receives each progress line (the CLI prints it to stdout
/// and logs it). Detailed per-attempt bytes are logged by [`probe_once`].
pub fn autodetect(
    ports: &[String],
    bauds: &[u32],
    profiles: &[LineProfile],
    window: Duration,
    mut report: impl FnMut(&str),
) -> AutodetectReport {
    let mut attempts = Vec::new();
    let mut winner = None;

    report(&format!(
        "Auto-detect: {} port(s) x {} baud rate(s) x {} flow setting(s), {}ms window each",
        ports.len(),
        bauds.len(),
        profiles.len(),
        window.as_millis()
    ));

    'outer: for port in ports {
        report(&format!("Port {port}"));
        for &baud in bauds {
            for &profile in profiles {
                let r = probe_once(port, baud, profile, window);
                report(&format!(
                    "  @ {baud:>6} [{:<7}]: {}",
                    profile.label(),
                    r.describe()
                ));
                let success = r.is_success();
                attempts.push(r.clone());
                if success {
                    winner = Some(r);
                    break 'outer;
                }
            }
        }
    }

    match &winner {
        Some(w) => report(&format!(
            "FOUND radio on {} @ {} baud, flow={} ({}). Re-run with: --port {} --baud {} --flow {}",
            w.port,
            w.baud,
            w.profile.label(),
            w.id_str(),
            w.port,
            w.baud,
            w.profile.label()
        )),
        None => {
            report("No responding radio found on any port / baud / flow setting.");
            report(&hypothesis(&attempts));
        }
    }

    AutodetectReport { attempts, winner }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_ascii_format() {
        assert_eq!(hex_ascii(b"ID021;"), "49 44 30 32 31 3B |ID021;|");
        assert_eq!(hex_ascii(&[]), "<none>");
        assert_eq!(hex_ascii(&[0x00, 0xFF]), "00 FF |..|");
    }

    #[test]
    fn parse_id_validation() {
        assert_eq!(parse_id("ID021"), Some("ID021".to_string()));
        assert_eq!(parse_id("ID023"), Some("ID023".to_string()));
        assert_eq!(parse_id("IF00014074000"), None);
        assert_eq!(parse_id("IDxy"), None);
        assert_eq!(parse_id("ID"), None);
    }

    #[test]
    fn frame_splits_on_semicolon() {
        assert_eq!(
            frame(b"ID021;FA00014074000;"),
            vec!["ID021".to_string(), "FA00014074000".to_string()]
        );
        assert_eq!(frame(b"ID0"), Vec::<String>::new()); // partial, no terminator
        assert_eq!(frame(b""), Vec::<String>::new());
    }

    #[test]
    fn classify_outcomes() {
        assert_eq!(
            classify(b"ID021;", &["ID021".to_string()]),
            ProbeOutcome::Identified("ID021".to_string())
        );
        assert_eq!(
            classify(b"FA00014074000;", &["FA00014074000".to_string()]),
            ProbeOutcome::Framed
        );
        // Bytes but no terminator -> baud mismatch.
        assert_eq!(classify(&[0xAA, 0x55, 0x13], &[]), ProbeOutcome::Garbage);
        // Nothing at all -> silent.
        assert_eq!(classify(&[], &[]), ProbeOutcome::Silent);
    }

    fn port(name: &str, likely: bool) -> PortInfo {
        PortInfo {
            name: name.to_string(),
            description: None,
            vid: None,
            pid: None,
            product: None,
            serial_number: None,
            likely_radio: likely,
        }
    }

    fn usb_port(name: &str, vid: u16, pid: u16, serial: Option<&str>) -> PortInfo {
        PortInfo {
            name: name.to_string(),
            description: None,
            vid: Some(vid),
            pid: Some(pid),
            product: None,
            serial_number: serial.map(str::to_string),
            likely_radio: vid == 0x10C4,
        }
    }

    #[test]
    fn resolve_matches_by_usb_serial_regardless_of_path() {
        // Same radio, different path than last time (replugged).
        let ports = vec![
            usb_port("/dev/cu.usbserial-120", 0x10C4, 0xEA60, Some("ABC123")),
            usb_port("/dev/cu.Bluetooth", 0x0000, 0x0000, None),
        ];
        assert_eq!(
            resolve_in(&ports, Some("ABC123"), Some(0x10C4), Some(0xEA60)),
            Some("/dev/cu.usbserial-120".to_string())
        );
        // Serial not present -> no match (don't fall through to a wrong device).
        assert_eq!(resolve_in(&ports, Some("NOPE"), None, None), None);
    }

    #[test]
    fn resolve_falls_back_to_unique_vid_pid_without_serial() {
        let one = vec![usb_port("/dev/cu.usbserial-120", 0x10C4, 0xEA60, None)];
        assert_eq!(
            resolve_in(&one, None, Some(0x10C4), Some(0xEA60)),
            Some("/dev/cu.usbserial-120".to_string())
        );
        // Two identical-model adapters are ambiguous -> refuse to guess.
        let two = vec![
            usb_port("/dev/cu.usbserial-120", 0x10C4, 0xEA60, None),
            usb_port("/dev/cu.usbserial-130", 0x10C4, 0xEA60, None),
        ];
        assert_eq!(resolve_in(&two, None, Some(0x10C4), Some(0xEA60)), None);
        // No identity at all -> None.
        assert_eq!(resolve_in(&two, None, None, None), None);
    }

    #[test]
    fn order_prefers_radio_and_cu_over_tty() {
        let ports = vec![
            port("/dev/tty.usbserial-X", true),
            port("/dev/cu.usbserial-X", true),
            port("/dev/cu.Bluetooth", false),
        ];
        // Default: only likely-radio ports, cu before tty.
        assert_eq!(
            order_ports(ports.clone(), false),
            vec!["/dev/cu.usbserial-X", "/dev/tty.usbserial-X"]
        );
        // all=true keeps the non-radio port too, still ordered radio-first/cu-first.
        assert_eq!(
            order_ports(ports, true),
            vec![
                "/dev/cu.usbserial-X",
                "/dev/tty.usbserial-X",
                "/dev/cu.Bluetooth"
            ]
        );
    }

    fn silent(port: &str, cd: Option<bool>, profile: LineProfile) -> ProbeResult {
        ProbeResult {
            port: port.to_string(),
            baud: 115_200,
            profile,
            outcome: ProbeOutcome::Silent,
            rx_bytes: vec![],
            framed: vec![],
            carrier_detect: cd,
            data_set_ready: None,
            clear_to_send: None,
            elapsed_ms: 1,
        }
    }

    #[test]
    fn hypothesis_for_all_silent_baseline() {
        // Only the baseline profile tried -> classic one-way-comms guidance.
        let h = hypothesis(&[silent(
            "/dev/tty.usbserial-X",
            Some(false),
            LineProfile::Default,
        )]);
        assert!(h.contains("one-way comms"), "{h}");
        assert!(h.contains("carrier-detect LOW"), "{h}");
    }

    #[test]
    fn hypothesis_when_lines_tried_points_at_radio() {
        // Silent even after asserting DTR/RTS -> not a flow-control problem.
        let h = hypothesis(&[
            silent("/dev/cu.usbserial-X", Some(false), LineProfile::Default),
            silent(
                "/dev/cu.usbserial-X",
                Some(false),
                LineProfile::AssertDtrRts,
            ),
            silent(
                "/dev/cu.usbserial-X",
                Some(false),
                LineProfile::HardwareFlow,
            ),
        ]);
        assert!(h.contains("not a handshake/flow-control problem"), "{h}");
        assert!(h.contains("PC control"), "{h}");
    }

    #[test]
    fn hypothesis_for_garbage_is_baud() {
        let garbage = ProbeResult {
            port: "/dev/cu.x".into(),
            baud: 9600,
            profile: LineProfile::Default,
            outcome: ProbeOutcome::Garbage,
            rx_bytes: vec![0xAA],
            framed: vec![],
            carrier_detect: None,
            data_set_ready: None,
            clear_to_send: None,
            elapsed_ms: 1,
        };
        assert!(hypothesis(&[garbage]).contains("baud-rate mismatch"));
    }
}
