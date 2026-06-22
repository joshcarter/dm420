//! Mock data providers for dm420.
//!
//! A handful of tokio tasks that publish *live-ish* FT8 traffic onto the bus, so
//! the GUI (and Joel's rig/decode work) has realistic data to render and
//! subscribe to before the real producers exist. The seed values are lifted from
//! the static tables the egui prototype used to read directly
//! (`gui::panel_data`), now animated over time onto their proper topics.
//!
//! [`spawn`] launches every producer; call it from inside a tokio runtime. The
//! tasks run until the runtime is dropped. This crate is the stand-in that the
//! real `rig` / `dsp` / `scanner` / `logbook` crates will displace, one topic at
//! a time — the GUI never knows the difference.

#![forbid(unsafe_code)]

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bus::types::*;
use bus::{BusHandle, Topic};

/// The single fake radio every producer publishes under.
pub fn radio_id() -> RadioId {
    RadioId("rig0".into())
}

/// The local station id stamped onto log entries.
pub fn station_id() -> StationId {
    StationId("n0jdc".into())
}

/// Milliseconds since the Unix epoch — real wall-clock, so timestamps line up
/// with what the GUI formats for display.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Format an SNR (dB) the way the console shows it: a Unicode minus and two
/// digits, e.g. `-8 -> "−08"`, `5 -> "+05"`.
fn fmt_snr(snr: i8) -> String {
    let sign = if snr < 0 { '−' } else { '+' };
    format!("{sign}{:02}", snr.unsigned_abs())
}

/// Launch every mock producer onto `bus`. Spawns detached tokio tasks, so this
/// must be called from within a tokio runtime context.
pub fn spawn(bus: &BusHandle) {
    // State: one publish is enough (late joiners get it via `send_replace`), but
    // we keep a task so the readout can breathe later if we want.
    publish_rig_state(bus);
    tokio::spawn(run_clock(bus.clone()));
    tokio::spawn(run_logbook(bus.clone()));
    tokio::spawn(run_decodes(bus.clone()));
    tokio::spawn(run_scanner(bus.clone()));
}

// `spawn_support` is gone: the band scanner is real now (`core::scan` in real
// mode), so `core::spawn` owns every real-mode topic and there are no remaining
// real-mode support mocks. The mock scanner (`run_scanner`) still backs the
// no-hardware `mocks::spawn` path below.

// --------------------------------------------------------------------- rig

fn publish_rig_state(bus: &BusHandle) {
    let _ = bus.publish(
        &Topic::RigState(radio_id()),
        RigState {
            radio: radio_id(),
            vfo: AbsHz(14_074_000),
            rig_mode: RigMode::UsbData,
            ptt: false,
            meters: Meters::default(),
        },
    );
}

// --------------------------------------------------------------------- clock

/// Mock-mode slot clock (no-hardware `DM420_MOCK=1` path; the real path uses
/// `core::clock`). Mock traffic is FT8, so the 15 s period here is correct — and
/// it matches `run_decodes`' own slot numbering, so the QSO engine's tick parity
/// lines up with the seeded decodes. Republished frequently so the clock UI has a
/// heartbeat and the QSO shell detects the slot boundary promptly (50 ms ⇒ ≤50 ms
/// boundary-detection lag).
async fn run_clock(bus: BusHandle) {
    const SLOT_MS: i64 = 15_000; // mock traffic is FT8
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    loop {
        tick.tick().await;
        let ms = now_ms();
        let slot_phase = (ms.rem_euclid(SLOT_MS) as f32) / SLOT_MS as f32;
        let slot = SlotId(ms.div_euclid(SLOT_MS) as u64);
        let _ = bus.publish(
            &Topic::ClockStatus,
            ClockStatus {
                offset_ms: 0.0,
                slot_phase,
                slot,
                mode: OverAirMode::Ft8,
            },
        );
    }
}

// --------------------------------------------------------------------- logbook

/// (call, grid, sent, rcvd-snr) — the worked stations that seed the log + map.
/// Grids spread across North America so the Contacts map has a realistic spread.
const LOG_SEED: &[(&str, &str, i8, i8)] = &[
    ("K7RA", "CN87", -12, -10),
    ("VE6AO", "DO21", -8, -14),
    ("W7PH", "DM33", -15, -11),
    ("K6XX", "CM97", -6, -9),
    ("K0DEN", "DM79", -3, -7),
    ("XE2OK", "DL95", -18, -16),
    ("K5ED", "EM12", -11, -8),
    ("N5JR", "EL29", -5, -12),
    ("XE1RC", "EK09", -20, -19),
    ("W9XYZ", "EN61", -9, -6),
    ("N4FL", "EL96", -14, -13),
    ("K1ABC", "FN31", -2, -4),
    ("W2NYC", "FN20", -7, -5),
    ("VE3EN", "FN25", -9, -2),
];

/// Fresh contacts to trickle in after the seed, so the log visibly advances.
const LOG_EXTRA: &[(&str, &str, i8, i8)] = &[
    ("W7GH", "CN94", -11, -9),
    ("JA1NUT", "PM95", -15, -13),
    ("G4ABC", "IO91", -13, -7),
    ("EA7KW", "IM67", -17, -6),
    ("PY2OG", "GG66", -23, -12),
    ("ZL2AB", "RE78", -24, -18),
];

/// The FT8 dial frequency for a band — just enough to make a mock log entry's
/// `freq` consistent with its `band`.
fn band_freq(band: Band) -> u64 {
    match band {
        Band::B40m => 7_074_000,
        Band::B20m => 14_074_000,
        Band::B15m => 21_074_000,
        Band::B10m => 28_074_000,
        _ => 14_074_000,
    }
}

/// Bands the seed log spreads across, so the Contacts map's band switcher has spots
/// on each — mirroring the real per-band partition.
const LOG_BANDS: [Band; 4] = [Band::B40m, Band::B20m, Band::B15m, Band::B10m];

fn log_entry(seq: u64, call: &str, grid: &str, snt: i8, rcv: i8, time_ms: i64, band: Band) -> LogEntry {
    LogEntry {
        id: QsoId {
            origin: station_id(),
            seq,
        },
        origin: station_id(),
        radio: Some(radio_id()),
        call: Callsign(call.into()),
        mode: OverAirMode::Ft8,
        band,
        freq: AbsHz(band_freq(band)),
        time: Timestamp(time_ms),
        exchange_sent: fmt_snr(snt),
        exchange_rcvd: fmt_snr(rcv),
        grid: Some(GridSquare(grid.into())),
        section: None,
    }
}

/// Publish the seed log (back-dated so it reads as history), then add a new QSO
/// every few seconds.
async fn run_logbook(bus: BusHandle) {
    let topic = Topic::LogbookEntries;
    let base = now_ms();
    let mut seq = 0u64;

    // History: oldest first, ~47 s apart, ending a minute before "now".
    let n = LOG_SEED.len() as i64;
    for (i, &(call, grid, snt, rcv)) in LOG_SEED.iter().enumerate() {
        seq += 1;
        let age = (n - i as i64) * 47_000 + 60_000;
        let band = LOG_BANDS[i % LOG_BANDS.len()];
        let _ = bus.publish(&topic, log_entry(seq, call, grid, snt, rcv, base - age, band));
    }

    // Live: a new contact every 9 s.
    let mut tick = tokio::time::interval(Duration::from_secs(9));
    tick.tick().await; // the first tick fires immediately — skip it
    let mut k = 0usize;
    loop {
        tick.tick().await;
        let (call, grid, snt, rcv) = LOG_EXTRA[k % LOG_EXTRA.len()];
        let band = LOG_BANDS[k % LOG_BANDS.len()];
        k += 1;
        seq += 1;
        let _ = bus.publish(&topic, log_entry(seq, call, grid, snt, rcv, now_ms(), band));
    }
}

// --------------------------------------------------------------------- decodes

/// (audio offset Hz, callsign, snr, grid) for the decode rail / ticker.
const DECODE_SEED: &[(f32, &str, i8, &str)] = &[
    (2680.0, "OH8X", -8, "KP24"),
    (2510.0, "JA1NUT", -15, "PM95"),
    (2360.0, "K1ABC", -2, "FN31"),
    (2200.0, "DL3XYZ", -19, "JO31"),
    (2050.0, "VK3WE", -21, "QF22"),
    (1880.0, "W7GH", -11, "CN94"),
    (1720.0, "EA7KW", -17, "IM67"),
    (1560.0, "N5JR", -5, "EL29"),
    (1400.0, "PY2OG", -23, "GG66"),
    (1240.0, "G4ABC", -13, "IO91"),
    (1080.0, "VE3EN", -9, "FN25"),
    (920.0, "ZL2AB", -24, "RE78"),
    (600.0, "UA9XYZ", -18, "MO06"),
];

/// Emit one decode roughly every 1.2 s, cycling the table, each tagged with the
/// current slot. A real decoder bursts a whole slot at once; this trickle is
/// enough to prove the lossless stream and keep the rail alive.
async fn run_decodes(bus: BusHandle) {
    let topic = Topic::Decodes(radio_id());
    let mut tick = tokio::time::interval(Duration::from_millis(1200));
    let mut i = 0usize;
    loop {
        tick.tick().await;
        let (hz, call, snr, grid) = DECODE_SEED[i % DECODE_SEED.len()];
        i += 1;
        let ms = now_ms();
        let _ = bus.publish(
            &topic,
            Decode {
                radio: radio_id(),
                mode: OverAirMode::Ft8,
                t: Timestamp(ms),
                offset: OffsetHz(hz),
                snr_db: Some(snr),
                source: SignalSource::Received,
                content: DecodeContent::Slotted {
                    slot: SlotId(ms.rem_euclid(i64::MAX).div_euclid(15_000) as u64),
                    dt: 0.2,
                    message: ParsedMessage::Cq {
                        caller: Callsign(call.into()),
                        contest: None,
                        grid: Some(GridSquare(grid.into())),
                    },
                    raw: format!("CQ {call} {grid}"),
                },
            },
        );
    }
}

// --------------------------------------------------------------------- scanner

/// (band, stations heard, unworked) — the band-scan panel's two columns.
const BAND_SEED: &[(Band, u32, u32)] = &[
    (Band::B40m, 23, 7),
    (Band::B20m, 41, 12),
    (Band::B15m, 18, 9),
    (Band::B10m, 6, 4),
];

/// Publish a steady `Idle` scanner state, plus one [`BandActivity`] per band on a
/// rotation. NOTE: `scanner/candidates` is a single-value `State` topic carrying
/// one `BandActivity` (the catalog marks its payload shape *provisional*). We
/// publish the bands spaced apart so a per-band accumulator on the GUI side
/// collects all four; a `Vec<BandActivity>` snapshot is the likely final shape.
async fn run_scanner(bus: BusHandle) {
    let _ = bus.publish(
        &Topic::ScannerState,
        ScannerState {
            status: ScanStatus::Idle,
            current: None,
            last_scan: Some(Timestamp(now_ms())),
        },
    );

    let mut tick = tokio::time::interval(Duration::from_millis(700));
    let mut i = 0usize;
    loop {
        tick.tick().await;
        let (band, seen, unworked) = BAND_SEED[i % BAND_SEED.len()];
        // A little jitter so the counts visibly tick — proof it's live, not baked.
        let wobble = (i / BAND_SEED.len()) as u32 % 3;
        i += 1;
        let _ = bus.publish(
            &Topic::ScannerCandidates,
            BandActivity {
                band,
                stations_seen: seen + wobble,
                unworked,
                t: Timestamp(now_ms()),
            },
        );
    }
}
