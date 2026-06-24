//! The synchronous egui ↔ asynchronous bus bridge.
//!
//! egui renders on the main thread and must never block; the bus speaks async
//! `recv().await`. [`BusView`] owns a background multi-threaded tokio runtime that
//! holds the [`BusHandle`], runs the mock producers, and runs one *pump* task per
//! topic the GUI cares about. Each pump `recv()`s from its subscription and writes
//! the result into a shared cell (latest-wins State) or a rolling ring (streams),
//! then asks egui to repaint. Panels read those shared structures each frame via
//! the accessor methods below — no `.await`, no touching the bus directly.
//!
//! This is the one piece the handoff docs don't cover: the sync/async seam. The
//! bus's own `publish`/`subscribe` are synchronous (they lock a `std::Mutex`), so
//! subscription happens on the main thread and only the `recv` loop runs on the
//! runtime.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bus::types::*;
use bus::{BusError, BusHandle, BusMessage, Topic, TopicSelector};

use crate::panel_data::Locator;

/// A latest-value cell for a `State` topic. The pump overwrites; the GUI reads.
type Cell<T> = Arc<Mutex<Option<T>>>;

fn cell<T>() -> Cell<T> {
    Arc::new(Mutex::new(None))
}

/// Capacity of the decode ring (see the comment at the `Ring::new` call site): big
/// enough that a wide monitor on a crowded band never evicts a still-visible decode.
const DECODE_RING_CAP: usize = 8192;

/// Capacity of the recent-RX-spectrum ring that backs the clear-lane finder
/// (`lane_finder`). The finder weights rows by recency (~8 s half-life), so this
/// only needs the last several seconds of frames; 512 is comfortably more than
/// that at any plausible spectrum cadence (~20 rows/s → ~25 s). Kept small on
/// purpose: the finder snapshots (clones) the whole ring every frame, so this must
/// not balloon. The spectrogram's deeper history lives in a separate ring (see
/// [`SPECTRUM_DISP_CAP`]).
const SPECTRUM_HIST_CAP: usize = 512;

/// Approximate spectrum-row cadence (rows/s). The decoder emits a column every
/// ~50 ms (`SPECTRUM_HOP_S` in `core::decode`), i.e. ~20/s. Used only to size the
/// display-history rings below in terms of seconds of coverage.
const SPECTRUM_ROWS_PER_SEC: usize = 20;

/// Seconds of spectrum history the waterslide spectrogram can reconstruct. It
/// rebuilds its whole texture each frame from the rows still in the ring, placing
/// every column by its `SpectrumRow.t`, so the ring must hold at least as many
/// seconds as the widest on-screen time window the panel can ask for
/// (`ws_history_secs()` grows with panel width and has no fixed cap). Anything
/// older than the ring renders as a black band at the old (right) edge — the
/// regression this guards against. 240 s (4 min) covers a 4K monitor at FT8's
/// 15 s slots with margin.
const SPECTRUM_DISP_SECS: usize = 240;

/// Capacity of the display-history rings that back the spectrogram rebuild (RX and
/// own-TX). Sized for [`SPECTRUM_DISP_SECS`] of coverage at the spectrum cadence:
/// ~4800 rows. Each `SpectrumRow.mags` is a few hundred bytes to ~2 KB, so the two
/// rings together stay well under ~10 MB — memory is not a constraint here.
const SPECTRUM_DISP_CAP: usize = SPECTRUM_DISP_SECS * SPECTRUM_ROWS_PER_SEC;

/// A rolling window for a stream topic: the pump pushes, dropping the oldest past
/// `cap`; the GUI snapshots the tail it wants each frame.
#[derive(Clone)]
struct Ring<T> {
    buf: Arc<Mutex<VecDeque<T>>>,
    cap: usize,
}

impl<T: Clone> Ring<T> {
    fn new(cap: usize) -> Self {
        Self {
            buf: Arc::new(Mutex::new(VecDeque::with_capacity(cap))),
            cap,
        }
    }

    fn push(&self, v: T) {
        let mut b = self.buf.lock().unwrap();
        if b.len() == self.cap {
            b.pop_front();
        }
        b.push_back(v);
    }

    /// Oldest-to-newest snapshot.
    fn snapshot(&self) -> Vec<T> {
        self.buf.lock().unwrap().iter().cloned().collect()
    }
}

/// A station to plot on the Contacts map: a callsign, the locator we place it from
/// (grid, or an ARRL/RAC section for Field Day stations that carry no grid), and
/// the most-recent time we logged (worked) or heard (unworked) it, ms since epoch.
/// The panel uses `last_ms` for the recent/all filter and for dimming unworked
/// markers by age.
pub struct MapSpot {
    pub call: String,
    pub loc: Locator,
    pub last_ms: i64,
    /// `true` if worked (in the log) → plus marker; `false` if only heard →
    /// circle (or triangle while calling CQ) that dims with age.
    pub worked: bool,
    /// The band this spot belongs to: a worked spot's logged band, or the band the
    /// rig was on when an unworked station was heard. `None` for heard stations
    /// caught while off any amateur band. The map filters spots to its selected
    /// band (the per-band "worked" rule, mirroring the waterslide).
    pub band: Option<Band>,
    /// `true` if the most recent sighting was a CQ call (heard spots only) — the
    /// map marks these with a triangle so the operator can spot answerable callers.
    pub cq: bool,
    /// Absolute frequency (dial + audio offset, Hz) we last saw this station at —
    /// captured at hearing time so the operator can click the map marker to tune to
    /// it, compensating for any dial change since. `None` for worked-only spots (the
    /// log stores no offset) or heard spots caught before the rig state was known.
    pub abs: Option<AbsHz>,
    /// The slot the last sighting landed in, for building a `DecodeRef` when the
    /// marker is clicked. `None` when unknown (worked-only spots).
    pub slot: Option<SlotId>,
}

/// A station heard with a placeable locator (grid, or an ARRL/RAC section for
/// Field Day): the locator we place it from, the last-heard time (ms since epoch),
/// the band the rig was on when heard, and whether its newest sighting was a CQ.
/// Accumulated by [`pump_heard`].
struct HeardEntry {
    loc: Locator,
    last_ms: i64,
    band: Option<Band>,
    cq: bool,
    /// Absolute frequency (dial + audio offset, Hz) at hearing time, if the rig
    /// state was known. Lets the Contacts map tune to this station on a click.
    abs: Option<AbsHz>,
    /// The slot this sighting landed in, for the `DecodeRef` built on a map click.
    slot: SlotId,
}

/// The GUI-facing view of live bus state. Cheap to construct once at startup and
/// held by `App`; every accessor returns an owned snapshot so panels never hold a
/// lock across drawing. Dropping it drops the runtime, which stops the producers
/// and pumps.
pub struct BusView {
    rig: Cell<RigState>,
    /// Latest RX waterfall column (real decoder only; published ~20×/s).
    spectrum: Cell<SpectrumRow>,
    /// Latest own-TX waterfall column, streamed by `core::tx` while keyed. Kept
    /// separate from `spectrum` so RX columns don't race it onto one cell; the
    /// panel shows it in place of the RX waterfall during an over.
    spectrum_tx: Cell<SpectrumRow>,
    /// Short rolling window of recent RX spectrum rows feeding the clear-lane finder
    /// (`lane_finder`). Capped small ([`SPECTRUM_HIST_CAP`]) because the finder
    /// snapshots the whole ring every frame and only weighs the last several seconds
    /// (~8 s half-life). The spectrogram's deeper history is `spectrum_disp_hist`.
    spectrum_hist: Ring<SpectrumRow>,
    /// Deep rolling window of recent RX spectrum rows backing the waterslide
    /// spectrogram, which rebuilds its texture from this window by timestamp each
    /// frame. Sized ([`SPECTRUM_DISP_CAP`]) to cover the widest on-screen time window
    /// so no history older than the ring shows as a black band. Short-term and
    /// in-memory by design — distinct from any long-term decode archive
    /// (`JOELS_ROADMAP.md` Now-#10).
    spectrum_disp_hist: Ring<SpectrumRow>,
    /// Rolling window of recent own-TX columns — the TX-side twin of
    /// `spectrum_disp_hist`. Lets the spectrogram place each over's columns by their
    /// `SpectrumRow.t` while keyed (they were latest-only before, so a TX over
    /// couldn't be reconstructed from timestamps). Read only by the spectrogram (not
    /// the lane finder), so it carries the same deep display sizing as the RX window.
    spectrum_tx_hist: Ring<SpectrumRow>,
    // `scanner`/`clock` are pumped and exposed now; their panel consumers (idle
    // scan status, slot clock in the top bar) land in the next wiring pass.
    #[allow(dead_code)]
    scanner: Cell<ScannerState>,
    #[allow(dead_code)]
    clock: Cell<ClockStatus>,
    bands: Arc<Mutex<Vec<BandActivity>>>,
    logs: Ring<LogEntry>,
    decodes: Ring<Decode>,
    /// Stations heard with a grid, keyed by call → newest [`HeardEntry`].
    /// Accumulated from the decode stream and pruned to the last hour on read; the
    /// Contacts map plots these as dimming "unworked" markers. A dedicated map
    /// (not the bounded `decodes` ring) so an hour of spots survives a busy band.
    heard: Arc<Mutex<HashMap<String, HeardEntry>>>,
    /// Latest QSO-engine state (phase, partner, queued next message). Drives the
    /// FT8 send row.
    qso: Cell<QsoState>,
    /// Latest health per hardware subsystem (real mode only). Drives the panels'
    /// fault display when a device is missing or disconnected.
    health: Arc<Mutex<HashMap<SubsystemId, SubsystemHealth>>>,

    /// Handle for live reconfiguration of the running producers (real mode only;
    /// empty otherwise).
    control: app_core::CoreControl,
    /// Handle to push station-identity / contest changes to the running QSO
    /// engine (e.g. when the operator edits the call/grid and re-locks).
    qso_control: qso::QsoControl,
    /// The currently-applied hardware config — the settings form's source of
    /// truth. Updated on apply alongside pushing to `control`.
    applied: Arc<Mutex<crate::settings::HardwareConfig>>,

    /// The bus, kept for issuing commands later (TX, tuning, scanner control).
    #[allow(dead_code)]
    bus: BusHandle,
    /// Owns the background worker threads; must outlive every pump. Kept last so
    /// it drops last.
    _rt: tokio::runtime::Runtime,
}

impl BusView {
    /// Stand up the runtime, launch the mock producers, and start a pump per
    /// topic. `egui_ctx` is cloned into each pump so new data wakes the UI even
    /// when it's otherwise idle.
    pub fn start(egui_ctx: egui::Context, station: qso::StationConfig) -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("build bus runtime");
        let bus = BusHandle::new();

        let rig = cell();
        let spectrum = cell();
        let spectrum_tx = cell();
        let scanner = cell();
        let clock = cell();
        let qso = cell();
        let bands: Arc<Mutex<Vec<BandActivity>>> = Arc::new(Mutex::new(Vec::new()));
        let logs = Ring::new(512);
        // Sized to hold every decode that can be on the waterslide at once: the panel
        // shows (panel_width / line_width) slot-columns of history (monitor-dependent —
        // an ultrawide shows many), each column stacking a busy slot's 30–50 decodes,
        // and the panel culls by age, not count. Keep this well past the worst case so
        // a wide monitor + crowded band never drops still-visible decodes. ~1 MB.
        let decodes = Ring::new(DECODE_RING_CAP);
        let spectrum_hist = Ring::new(SPECTRUM_HIST_CAP);
        let spectrum_disp_hist = Ring::new(SPECTRUM_DISP_CAP);
        let spectrum_tx_hist = Ring::new(SPECTRUM_DISP_CAP);
        let heard: Arc<Mutex<HashMap<String, HeardEntry>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let health: Arc<Mutex<HashMap<SubsystemId, SubsystemHealth>>> =
            Arc::new(Mutex::new(HashMap::new()));

        // `tokio::spawn` (used by the producers and the pump helpers) needs an
        // active runtime context; hold the guard while we wire everything up.
        let settings = crate::settings::Settings::from_env();
        tracing::info!(
            audio_input = ?settings.audio_input,
            audio_output = ?settings.audio_output,
            protocol = ?settings.protocol,
            "bus_view: starting producers",
        );
        let applied = Arc::new(Mutex::new(settings.hardware()));
        let _guard = rt.enter();
        // Real rig + decode + clock + logbook + band-scanner producers — `core::spawn`
        // covers them all (the scanner is real too). The decode stream is the real decoder's.
        let control = app_core::spawn(&bus, settings.core_config());

        // The QSO engine is logic, not hardware — it runs in both modes, driven
        // by whichever decode/clock producers are live, serving `qso/{id}/command`
        // and publishing `QsoState`. It auto-sends in real mode (a rig + the PTT
        // interlock are present); in mock mode it sequences but never keys.
        let qso_control = qso::spawn(&bus, app_core::radio_id(), station, true);

        pump_state(
            &bus,
            Topic::QsoState(app_core::radio_id()),
            qso.clone(),
            egui_ctx.clone(),
        );
        pump_state(
            &bus,
            Topic::RigState(app_core::radio_id()),
            rig.clone(),
            egui_ctx.clone(),
        );
        pump_spectrum(
            &bus,
            Topic::Spectrum(app_core::radio_id()),
            spectrum.clone(),
            spectrum_tx.clone(),
            spectrum_hist.clone(),
            spectrum_disp_hist.clone(),
            spectrum_tx_hist.clone(),
            egui_ctx.clone(),
        );
        pump_state(&bus, Topic::ScannerState, scanner.clone(), egui_ctx.clone());
        pump_state(&bus, Topic::ClockStatus, clock.clone(), egui_ctx.clone());
        pump_bands(&bus, bands.clone(), egui_ctx.clone());
        pump_stream(
            &bus,
            TopicSelector::Exact(Topic::LogbookEntries),
            logs.clone(),
            egui_ctx.clone(),
        );
        pump_stream(
            &bus,
            TopicSelector::Exact(Topic::Decodes(app_core::radio_id())),
            decodes.clone(),
            egui_ctx.clone(),
        );
        // Heard stations for the map: a second decode subscriber that keeps a
        // longer-lived (call → grid) map than the bounded `decodes` ring. Runs in
        // both modes — heard spots come from whatever decoder is live.
        pump_heard(&bus, heard.clone(), rig.clone(), egui_ctx.clone());
        // Health for the rig + audio subsystems — drives the panels' fault display.
        for id in [SubsystemId::Rig, SubsystemId::Audio] {
            pump_health(&bus, id, health.clone(), egui_ctx.clone());
        }
        drop(_guard);

        Self {
            rig,
            spectrum,
            spectrum_tx,
            spectrum_hist,
            spectrum_disp_hist,
            spectrum_tx_hist,
            scanner,
            clock,
            bands,
            logs,
            decodes,
            heard,
            qso,
            health,
            control,
            qso_control,
            applied,
            bus,
            _rt: rt,
        }
    }

    // ----------------------------------------------------------------- reads

    /// The current rig state (frequency, mode, PTT, meters), if seen yet.
    pub fn rig_state(&self) -> Option<RigState> {
        self.rig.lock().unwrap().clone()
    }

    /// The latest RX waterfall spectrum column, if one has arrived (real mode only).
    pub fn spectrum(&self) -> Option<SpectrumRow> {
        self.spectrum.lock().unwrap().clone()
    }

    /// The latest own-TX waterfall column, if one has arrived. Streamed by
    /// `core::tx` while keyed; the panel shows it in place of the RX waterfall
    /// during an over (the RX capture is meaningless while transmitting).
    pub fn tx_spectrum(&self) -> Option<SpectrumRow> {
        self.spectrum_tx.lock().unwrap().clone()
    }

    /// Recent RX spectrum rows (oldest→newest) for the clear-lane finder. A short,
    /// bounded window — see [`SPECTRUM_HIST_CAP`]. The spectrogram uses the deeper
    /// [`Self::recent_spectrum_disp`] instead so this stays cheap to clone per frame.
    pub fn recent_spectrum(&self) -> Vec<SpectrumRow> {
        self.spectrum_hist.snapshot()
    }

    /// Recent RX spectrum rows (oldest→newest) for the waterslide spectrogram, which
    /// rebuilds its texture from this window by timestamp each frame. A deeper window
    /// than [`Self::recent_spectrum`] — see [`SPECTRUM_DISP_CAP`] — so the spectrogram
    /// can fill the widest on-screen time window without a black band at the old edge.
    pub fn recent_spectrum_disp(&self) -> Vec<SpectrumRow> {
        self.spectrum_disp_hist.snapshot()
    }

    /// Recent own-TX columns (oldest→newest) — the spectrogram places each over's
    /// columns by their timestamp, mirroring [`Self::recent_spectrum_disp`]. Empty
    /// until the first over; holds the last [`SPECTRUM_DISP_CAP`] TX columns.
    pub fn recent_tx_spectrum(&self) -> Vec<SpectrumRow> {
        self.spectrum_tx_hist.snapshot()
    }

    /// Enable/disable auto-QSY (the AUTO QSY toggle): after 3 unanswered CQs the
    /// engine hops to the lane finder's best offset.
    pub fn set_auto_hop(&self, on: bool) {
        self.qso_control.set_auto_hop(on);
    }

    /// Feed the QSO engine the lane finder's current best CQ offset — the auto-QSY
    /// hop target.
    pub fn set_cq_hop_offset(&self, offset_hz: f32) {
        self.qso_control.set_cq_hop_offset(OffsetHz(offset_hz));
    }

    /// The current scanner status, if seen yet. (Consumer lands next pass.)
    #[allow(dead_code)]
    pub fn scanner(&self) -> Option<ScannerState> {
        self.scanner.lock().unwrap().clone()
    }

    /// The current clock/slot status, if seen yet. (Consumer lands next pass.)
    #[allow(dead_code)]
    pub fn clock(&self) -> Option<ClockStatus> {
        *self.clock.lock().unwrap()
    }

    /// Per-band activity, sorted low band → high. Accumulated from the (provisional)
    /// single-value `scanner/candidates` State topic — see [`pump_bands`].
    pub fn band_activity(&self) -> Vec<BandActivity> {
        let mut v = self.bands.lock().unwrap().clone();
        v.sort_by_key(|b| band_order(b.band));
        v
    }

    /// The `n` most recent log entries, newest first.
    pub fn recent_logs(&self, n: usize) -> Vec<LogEntry> {
        self.logs.snapshot().into_iter().rev().take(n).collect()
    }

    /// How many log entries are currently held (capped at the ring size).
    pub fn log_count(&self) -> usize {
        self.logs.buf.lock().unwrap().len()
    }

    /// Distinct worked stations that carry a placeable locator (a grid, or — for
    /// Field Day contacts that carried no grid — an ARRL/RAC section), most-recent
    /// contact per call. Feeds the Contacts map's "worked" (filled) layer.
    pub fn worked_spots(&self) -> Vec<MapSpot> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for e in self.logs.snapshot().into_iter().rev() {
            // Grid is preferred; fall back to the Field Day section.
            let loc = e
                .grid
                .as_ref()
                .map(|g| Locator::Grid(g.0.clone()))
                .or_else(|| e.section.as_ref().map(|s| Locator::Section(s.0.clone())));
            if let Some(loc) = loc
                && seen.insert(e.call.0.clone())
            {
                out.push(MapSpot {
                    call: e.call.0.clone(),
                    loc,
                    last_ms: e.time.0,
                    worked: true,
                    band: Some(e.band),
                    cq: false,
                    abs: None,
                    slot: None,
                });
            }
        }
        out
    }

    /// Distinct callsigns already logged on `band`, upper-cased for case-folded
    /// matching. "Worked" is **per band** (the Field Day rule): the same call on
    /// another band is still unworked there. Feeds the waterslide's worked-station
    /// dimming.
    pub fn worked_calls_on_band(&self, band: Band) -> HashSet<String> {
        self.logs
            .snapshot()
            .into_iter()
            .filter(|e| e.band == band)
            .map(|e| e.call.0.to_ascii_uppercase())
            .collect()
    }

    /// Stations heard with a grid in the last hour, most-recent per call. Feeds the
    /// Contacts map's "unworked" (hollow, dimming) layer. Older spots are dropped
    /// per `docs/map_panel.md` (transient points last at most an hour). Callers
    /// that also show worked spots should exclude calls already in the log.
    pub fn heard_spots(&self) -> Vec<MapSpot> {
        let cutoff = types::now_ms() - 3_600_000; // one hour
        self.heard
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, e)| e.last_ms >= cutoff)
            .map(|(call, e)| MapSpot {
                call: call.clone(),
                loc: e.loc.clone(),
                last_ms: e.last_ms,
                worked: false,
                band: e.band,
                cq: e.cq,
                abs: e.abs,
                slot: Some(e.slot),
            })
            .collect()
    }

    /// Current wall-clock time, ms since the Unix epoch — the reference the map
    /// uses for recent/all filtering and age-dimming.
    pub fn now_ms(&self) -> i64 {
        types::now_ms()
    }

    /// The currently-applied hardware config (the settings form's starting point).
    pub fn current_config(&self) -> crate::settings::HardwareConfig {
        self.applied.lock().unwrap().clone()
    }

    /// Apply edited hardware settings: push them to the running rig/audio
    /// producers (which reconnect with them) and record them as applied. A no-op
    /// on subsystems that aren't running (mock mode, WAV replay).
    pub fn apply_config(&self, cfg: crate::settings::HardwareConfig) {
        if let Some(rig) = &self.control.rig {
            rig.set(cfg.serial.clone());
        }
        if let Some(audio) = &self.control.audio {
            audio.set(cfg.audio_input.clone(), cfg.protocol);
        }
        if let Some(tx) = &self.control.tx {
            tx.set(cfg.audio_output.clone());
        }
        *self.applied.lock().unwrap() = cfg;
    }

    /// Switch the on-air mode (FT8 ⇄ FT4) live, without disturbing the rig link.
    /// Restarts only the capture/decode session with the new mode (a no-op under
    /// mocks/WAV replay, where there's no live capture) and records + persists it
    /// so the settings form and header readouts stay in sync across restarts.
    pub fn set_protocol(&self, proto: app_core::Protocol) {
        let cfg = {
            let mut applied = self.applied.lock().unwrap();
            if applied.protocol == proto {
                return;
            }
            applied.protocol = proto;
            if let Some(audio) = &self.control.audio {
                audio.set(applied.audio_input.clone(), proto);
            }
            applied.clone()
        };
        crate::settings::save_hardware_config(&cfg);
    }

    /// Whether a live audio capture producer is running and therefore
    /// reconfigurable. `false` for WAV replay or rig-only setups, where the audio
    /// device and decode mode are fixed at startup and the settings form should
    /// present them as read-only.
    pub fn has_live_audio(&self) -> bool {
        self.control.audio.is_some()
    }

    /// Input-capable audio device names, for the settings picker.
    pub fn audio_inputs(&self) -> Vec<String> {
        app_core::list_audio_inputs()
    }

    /// Output-capable audio device names, for the TX-output settings picker.
    pub fn audio_outputs(&self) -> Vec<String> {
        app_core::list_audio_outputs()
    }

    /// Available serial port names (likely-radio first), for the settings picker.
    pub fn serial_ports(&self) -> Vec<String> {
        app_core::list_serial_ports()
    }

    /// The latest health for a subsystem, if it has reported (real mode only).
    /// `None` ⇒ no report yet (mock mode, or before the first publish), which
    /// panels treat as "not faulted".
    pub fn health(&self, id: SubsystemId) -> Option<SubsystemHealth> {
        self.health.lock().unwrap().get(&id).cloned()
    }

    /// All retained decodes, newest first. The waterslide wants the full ring (it
    /// culls by on-screen age, not count), so the only bound is `DECODE_RING_CAP`.
    pub fn recent_decodes(&self) -> Vec<Decode> {
        self.decodes.snapshot().into_iter().rev().collect()
    }

    /// Push a station-identity / contest change to the running QSO engine (call on
    /// re-lock after the operator edits the call/grid).
    pub fn set_qso_station(&self, station: qso::StationConfig) {
        self.qso_control.set_station(station);
    }

    /// The current QSO-engine state (phase, partner, queued next message), if it
    /// has published yet.
    pub fn qso_state(&self) -> Option<QsoState> {
        self.qso.lock().unwrap().clone()
    }

    /// Call CQ at `offset_hz`: set the outgoing offset (no retune) and start the
    /// engine calling.
    pub fn call_cq(&self, offset_hz: f32) {
        self.publish_selection(offset_hz, None);
        self.send_qso_command(QsoCommand::CallCq);
    }

    /// Arm to answer `call` at `offset_hz` (the DM420 wait-for-CQ model — the
    /// engine replies when that station next calls CQ). `slot` is the slot the
    /// target's decode landed in, threaded from the click so the `DecodeRef` is the
    /// real one. (The engine still re-derives TX parity from the target's own CQ
    /// when it commits, but the ref now carries the true slot for selection/gossip.)
    pub fn answer_station(&self, offset_hz: f32, call: String, slot: SlotId) {
        let target = DecodeRef {
            radio: app_core::radio_id(),
            slot,
            call: Some(Callsign(call)),
        };
        self.publish_selection(offset_hz, Some(target.clone()));
        self.send_qso_command(QsoCommand::Start { target });
    }

    /// Pick up a contact mid-stream from a decode addressed to us: the operator
    /// clicked a `<my call> <their call> …` line answering a call we'd already
    /// disarmed from. Commits the engine at once (vs [`Self::answer_station`],
    /// which arms and waits for a CQ). `message`/`snr` come from the clicked
    /// decode; `slot` is the slot it landed in (for TX parity).
    pub fn resume_qso(
        &self,
        offset_hz: f32,
        call: String,
        slot: SlotId,
        message: ParsedMessage,
        snr: i8,
    ) {
        let target = DecodeRef {
            radio: app_core::radio_id(),
            slot,
            call: Some(Callsign(call)),
        };
        self.publish_selection(offset_hz, Some(target.clone()));
        self.send_qso_command(QsoCommand::Resume {
            target,
            message,
            snr,
            offset: OffsetHz(offset_hz),
        });
    }

    /// Disarm / stop the engine (the single Stop control).
    pub fn abort_qso(&self) {
        self.send_qso_command(QsoCommand::Abort);
    }

    /// Retune the rig's dial to `hz` (the `/f` and `/b` slash commands). Issues a
    /// `RigCommand::SetFreq` to the rig-command server; fire-and-forget, since the
    /// new frequency comes back on `RigState` and drives the header readout. The
    /// request `await`s, so it runs on the bus runtime; in mock mode (no rig
    /// server) it simply times out harmlessly.
    pub fn set_freq(&self, hz: u64) {
        let bus = self.bus.clone();
        self._rt.spawn(async move {
            let _ = bus
                .request::<RigCommand, app_core::CommandResult>(
                    &Topic::RigCommand(app_core::radio_id()),
                    RigCommand::SetFreq(AbsHz(hz)),
                    Duration::from_secs(1),
                )
                .await;
        });
    }

    /// Start a band-scan survey over `bands`, dwelling `dwell_slots` slots per
    /// band/mode (the scanner clamps to ≥2 for even/odd parity). Fire-and-forget on
    /// the bus runtime, like [`set_freq`]; progress comes back on `scanner/state`
    /// and `scanner/candidates`. In mock mode (no scanner server) it times out
    /// harmlessly.
    pub fn start_scan(&self, stops: Vec<(Band, OverAirMode)>, dwell_slots: u8) {
        self.send_scanner_command(ScannerCommand::StartSurvey { stops, dwell_slots });
    }

    /// Cancel the running survey; the scanner restores the operator's prior
    /// band + mode.
    pub fn cancel_scan(&self) {
        self.send_scanner_command(ScannerCommand::Cancel);
    }

    /// Replace the live sweep's stops (the panel's band/mode toggles) without
    /// resetting counts. Sent as the operator toggles during a scan.
    pub fn set_stops(&self, stops: Vec<(Band, OverAirMode)>) {
        self.send_scanner_command(ScannerCommand::SetStops { stops });
    }

    fn send_scanner_command(&self, cmd: ScannerCommand) {
        let bus = self.bus.clone();
        self._rt.spawn(async move {
            let _ = bus
                .request::<ScannerCommand, ScannerAck>(
                    &Topic::ScannerCommand,
                    cmd,
                    Duration::from_secs(1),
                )
                .await;
        });
    }

    /// Drop the rig's PTT immediately, **blocking** until the rig acknowledges (or a
    /// short timeout). Called from the app's close path so quitting mid-over can't
    /// leave the transmitter keyed — the rig's PTT watchdog is only a ~15 s backstop,
    /// and our exit (`std::process::exit`) bypasses normal teardown. Unlike
    /// [`set_freq`] this *blocks* rather than spawns: the caller exits the process
    /// right after, so the key-down has to land first. Releasing TX needs no live
    /// interlock token (the rig adapter always allows key-down), so a bare
    /// `PttRequest { on: false }` does it. Real mode only — in mock there's no rig
    /// server to answer, and the 1 s bound keeps a wedged/absent rig from hanging the
    /// quit.
    pub fn unkey_for_shutdown(&self) {
        let bus = self.bus.clone();
        let res = self._rt.block_on(async move {
            bus.request::<RigCommand, app_core::CommandResult>(
                &Topic::RigCommand(app_core::radio_id()),
                RigCommand::PttRequest {
                    on: false,
                    token: InterlockToken(0),
                },
                Duration::from_secs(1),
            )
            .await
        });
        match res {
            Ok(_) => tracing::info!("shutdown: dropped PTT before exit"),
            Err(e) => tracing::warn!(error = ?e, "shutdown: PTT key-down on exit failed"),
        }
    }

    /// Publish the current selection (outgoing offset + optional target) onto the
    /// `selection/{id}/active` State topic.
    fn publish_selection(&self, offset_hz: f32, target: Option<DecodeRef>) {
        let _ = self.bus.publish(
            &Topic::Selection(app_core::radio_id()),
            Selection {
                radio: app_core::radio_id(),
                outgoing: OffsetHz(offset_hz),
                target,
            },
        );
    }

    /// Fire a QSO command at the engine's command server. The engine reflects the
    /// result on `qso/{id}/state`, so the ack is ignored (fire-and-forget); the
    /// request runs on the bus runtime since it `await`s.
    fn send_qso_command(&self, cmd: QsoCommand) {
        let bus = self.bus.clone();
        self._rt.spawn(async move {
            let _ = bus
                .request::<QsoCommand, qso::QsoAck>(
                    &Topic::QsoCommand(app_core::radio_id()),
                    cmd,
                    Duration::from_secs(1),
                )
                .await;
        });
    }

    /// The underlying bus, for issuing commands (TX, tuning) from the UI later.
    #[allow(dead_code)]
    pub fn bus(&self) -> &BusHandle {
        &self.bus
    }
}

/// Sort key for a band, low frequency → high.
fn band_order(b: Band) -> u8 {
    match b {
        Band::B160m => 0,
        Band::B80m => 1,
        Band::B40m => 2,
        Band::B30m => 3,
        Band::B20m => 4,
        Band::B17m => 5,
        Band::B15m => 6,
        Band::B12m => 7,
        Band::B10m => 8,
        Band::B6m => 9,
    }
}

/// Extract a placeable `(call, locator, calling_cq)` from a decode, if it
/// advertises a location — a CQ with a grid, a standard grid exchange, or a Field
/// Day exchange (which carries an ARRL/RAC section, not a grid: responders send
/// only their section). The third element marks the CQ case so the map can flag
/// answerable callers.
fn station_locator(d: &Decode) -> Option<(String, Locator, bool)> {
    let DecodeContent::Slotted { message, .. } = &d.content else {
        return None;
    };
    match message {
        ParsedMessage::Cq {
            caller,
            grid: Some(g),
            ..
        } => Some((caller.0.clone(), Locator::Grid(g.0.clone()), true)),
        ParsedMessage::Exchange {
            from,
            payload: ExchangePayload::Grid(g),
            ..
        } => Some((from.0.clone(), Locator::Grid(g.0.clone()), false)),
        // Field Day: a responder's exchange carries a section, never a grid.
        ParsedMessage::Exchange {
            from,
            payload: ExchangePayload::FieldDay { section, .. },
            ..
        } => Some((from.0.clone(), Locator::Section(section.0.clone()), false)),
        _ => None,
    }
}

// ------------------------------------------------------------------- pumps

/// Spawn a pump that mirrors a `State` topic's latest value into `cell`.
fn pump_state<T: BusMessage>(bus: &BusHandle, topic: Topic, cell: Cell<T>, ctx: egui::Context) {
    let mut sub = match bus.subscribe::<T>(TopicSelector::Exact(topic)) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = ?e, "bus_view: state subscribe failed");
            return;
        }
    };
    tokio::spawn(async move {
        loop {
            match sub.recv().await {
                Ok(v) => {
                    *cell.lock().unwrap() = Some(v);
                    ctx.request_repaint();
                }
                // State never lags, but be exhaustive: a closed channel ends the pump.
                Err(BusError::Lagged { .. }) => continue,
                Err(_) => break,
            }
        }
    });
}

/// Spawn a pump for the spectrum topic, routing each column to the RX or own-TX
/// cell by its `source`. Splitting them keeps RX columns (still produced by the
/// decoder while we transmit) from racing the own-TX columns onto one cell, so the
/// panel can cleanly swap to the outgoing-signal waterfall while keyed.
#[allow(clippy::too_many_arguments)]
fn pump_spectrum(
    bus: &BusHandle,
    topic: Topic,
    rx: Cell<SpectrumRow>,
    tx: Cell<SpectrumRow>,
    hist: Ring<SpectrumRow>,
    disp_hist: Ring<SpectrumRow>,
    tx_hist: Ring<SpectrumRow>,
    ctx: egui::Context,
) {
    let mut sub = match bus.subscribe::<SpectrumRow>(TopicSelector::Exact(topic)) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = ?e, "bus_view: spectrum subscribe failed");
            return;
        }
    };
    tokio::spawn(async move {
        loop {
            match sub.recv().await {
                Ok(v) => {
                    match v.source {
                        // RX columns feed both the lane finder's short window (`hist`)
                        // and the spectrogram's deep one (`disp_hist`).
                        SignalSource::Received => {
                            hist.push(v.clone());
                            disp_hist.push(v.clone());
                            *rx.lock().unwrap() = Some(v);
                        }
                        // TX columns ring too, so the spectrogram can place an over's
                        // columns by their timestamp (not just the latest one).
                        SignalSource::OwnTx => {
                            tx_hist.push(v.clone());
                            *tx.lock().unwrap() = Some(v);
                        }
                    }
                    ctx.request_repaint();
                }
                // Lossy lag: keep reading. Closed/dropped: end the pump.
                Err(BusError::Lagged { .. }) => continue,
                Err(_) => break,
            }
        }
    });
}

/// Spawn a pump that appends a stream topic's messages into `ring`.
fn pump_stream<T: BusMessage>(
    bus: &BusHandle,
    sel: TopicSelector,
    ring: Ring<T>,
    ctx: egui::Context,
) {
    let mut sub = match bus.subscribe::<T>(sel) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = ?e, "bus_view: stream subscribe failed");
            return;
        }
    };
    tokio::spawn(async move {
        loop {
            match sub.recv().await {
                Ok(v) => {
                    ring.push(v);
                    ctx.request_repaint();
                }
                // Lossy lag: keep reading. Closed/dropped: end the pump.
                Err(BusError::Lagged { .. }) => continue,
                Err(_) => break,
            }
        }
    });
}

/// Spawn a pump that folds grid-bearing decodes into the heard-stations map
/// (call → newest [`HeardEntry`]). Lets the Contacts map plot stations heard but
/// not worked, retained far longer than the bounded `decodes` ring. The band is
/// taken from the rig's current dial frequency at decode time, so the map can apply
/// the same per-band "worked" rule as the waterslide.
fn pump_heard(
    bus: &BusHandle,
    heard: Arc<Mutex<HashMap<String, HeardEntry>>>,
    rig: Cell<RigState>,
    ctx: egui::Context,
) {
    let mut sub =
        match bus.subscribe::<Decode>(TopicSelector::Exact(Topic::Decodes(app_core::radio_id()))) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("bus_view: heard subscribe failed: {e:?}");
                return;
            }
        };
    tokio::spawn(async move {
        loop {
            match sub.recv().await {
                Ok(d) => {
                    if let Some((call, loc, cq)) = station_locator(&d) {
                        let t = d.t.0;
                        // The rig's current dial — gives both the band this station
                        // was heard on and the absolute frequency we saw it at
                        // (dial + audio offset). `None` if the rig state isn't known.
                        let vfo = rig.lock().unwrap().as_ref().map(|r| r.vfo.0);
                        let band = vfo.and_then(|hz| Band::from_hz(AbsHz(hz)));
                        let abs = vfo.map(|v| AbsHz(v + d.offset.0.round().max(0.0) as u64));
                        // The slot this decode landed in (for a click-built DecodeRef).
                        let slot = match &d.content {
                            DecodeContent::Slotted { slot, .. } => *slot,
                            _ => SlotId(0),
                        };
                        let mut m = heard.lock().unwrap();
                        // Keep the newest sighting per call.
                        let newer = m.get(&call).is_none_or(|e| t >= e.last_ms);
                        if newer {
                            m.insert(
                                call,
                                HeardEntry {
                                    loc,
                                    last_ms: t,
                                    band,
                                    cq,
                                    abs,
                                    slot,
                                },
                            );
                            drop(m);
                            ctx.request_repaint();
                        }
                    }
                }
                Err(BusError::Lagged { .. }) => continue,
                Err(_) => break,
            }
        }
    });
}

/// Spawn a pump that mirrors one subsystem's `health/{id}` State topic into the
/// shared health map.
fn pump_health(
    bus: &BusHandle,
    id: SubsystemId,
    health: Arc<Mutex<HashMap<SubsystemId, SubsystemHealth>>>,
    ctx: egui::Context,
) {
    let mut sub = match bus.subscribe::<SubsystemHealth>(TopicSelector::Exact(Topic::Health(id))) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = ?e, "bus_view: health subscribe failed");
            return;
        }
    };
    tokio::spawn(async move {
        loop {
            match sub.recv().await {
                Ok(h) => {
                    health.lock().unwrap().insert(h.id, h);
                    ctx.request_repaint();
                }
                Err(BusError::Lagged { .. }) => continue,
                Err(_) => break,
            }
        }
    });
}

/// Spawn a pump that accumulates per-band activity keyed by [`Band`].
///
/// `scanner/candidates` is a single-value `State` topic carrying one
/// `BandActivity` (the catalog marks the payload shape *provisional*). The mock
/// publishes the bands spaced apart; this pump folds each into a map so all bands
/// are visible at once. A `Vec<BandActivity>` snapshot payload would let us drop
/// this accumulation — a question for whoever finalizes the scanner seam.
fn pump_bands(bus: &BusHandle, bands: Arc<Mutex<Vec<BandActivity>>>, ctx: egui::Context) {
    let mut sub =
        match bus.subscribe::<Vec<BandActivity>>(TopicSelector::Exact(Topic::ScannerCandidates)) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = ?e, "bus_view: candidates subscribe failed");
                return;
            }
        };
    tokio::spawn(async move {
        loop {
            match sub.recv().await {
                Ok(snapshot) => {
                    *bands.lock().unwrap() = snapshot;
                    ctx.request_repaint();
                }
                Err(BusError::Lagged { .. }) => continue,
                Err(_) => break,
            }
        }
    });
}
