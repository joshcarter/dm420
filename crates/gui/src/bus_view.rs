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

/// A latest-value cell for a `State` topic. The pump overwrites; the GUI reads.
type Cell<T> = Arc<Mutex<Option<T>>>;

fn cell<T>() -> Cell<T> {
    Arc::new(Mutex::new(None))
}

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

/// A station to plot on the Contacts map: a callsign, the grid we placed it from,
/// and the most-recent time we logged (worked) or heard (unworked) it, ms since
/// epoch. The panel uses `last_ms` for the recent/all filter and for dimming
/// unworked markers by age.
pub struct MapSpot {
    pub call: String,
    pub grid: String,
    pub last_ms: i64,
    /// `true` if worked (in the log) → filled marker; `false` if only heard →
    /// hollow marker that dims with age.
    pub worked: bool,
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
    // `scanner`/`clock` are pumped and exposed now; their panel consumers (idle
    // scan status, slot clock in the top bar) land in the next wiring pass.
    #[allow(dead_code)]
    scanner: Cell<ScannerState>,
    #[allow(dead_code)]
    clock: Cell<ClockStatus>,
    bands: Arc<Mutex<HashMap<Band, BandActivity>>>,
    logs: Ring<LogEntry>,
    decodes: Ring<Decode>,
    /// Stations heard with a grid, keyed by call → (grid, last-heard ms).
    /// Accumulated from the decode stream and pruned to the last hour on read; the
    /// Contacts map plots these as dimming "unworked" markers. A dedicated map
    /// (not the bounded `decodes` ring) so an hour of spots survives a busy band.
    heard: Arc<Mutex<HashMap<String, (GridSquare, i64)>>>,
    /// Latest QSO-engine state (phase, partner, queued next message). Drives the
    /// FT8 send row.
    qso: Cell<QsoState>,
    /// Latest health per hardware subsystem (real mode only). Drives the panels'
    /// fault display when a device is missing or disconnected.
    health: Arc<Mutex<HashMap<SubsystemId, SubsystemHealth>>>,

    /// Whether real producers are driving the bus (the default; `DM420_MOCK=1`
    /// opts into mocks). Panels use this to avoid presenting mock-only visuals
    /// (e.g. the simulated FFT) as if they were live radio data.
    real: bool,

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
        let bands: Arc<Mutex<HashMap<Band, BandActivity>>> = Arc::new(Mutex::new(HashMap::new()));
        let logs = Ring::new(512);
        let decodes = Ring::new(64);
        let heard: Arc<Mutex<HashMap<String, (GridSquare, i64)>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let health: Arc<Mutex<HashMap<SubsystemId, SubsystemHealth>>> =
            Arc::new(Mutex::new(HashMap::new()));

        // `tokio::spawn` (used by the producers and the pump helpers) needs an
        // active runtime context; hold the guard while we wire everything up.
        let settings = crate::settings::Settings::from_env();
        let real = settings.is_real();
        tracing::info!(
            mode = if real { "real" } else { "mock" },
            audio_input = ?settings.audio_input,
            audio_output = ?settings.audio_output,
            protocol = ?settings.protocol,
            "bus_view: starting producers",
        );
        let applied = Arc::new(Mutex::new(settings.hardware()));
        let _guard = rt.enter();
        let control = if real {
            // Real rig + decode producers; mocks still drive the topics `core`
            // doesn't cover yet (clock, logbook, scanner). Note: decodes are NOT
            // among them — `spawn_support` deliberately omits `run_decodes`, so the
            // decode stream is the real decoder's alone.
            let control = app_core::spawn(&bus, settings.core_config());
            mocks::spawn_support(&bus);
            control
        } else {
            mocks::spawn(&bus);
            app_core::CoreControl::default()
        };

        // The QSO engine is logic, not hardware — it runs in both modes, driven
        // by whichever decode/clock producers are live, serving `qso/{id}/command`
        // and publishing `QsoState`. It auto-sends in real mode (a rig + the PTT
        // interlock are present); in mock mode it sequences but never keys.
        let qso_control = qso::spawn(&bus, mocks::radio_id(), station, real);

        pump_state(
            &bus,
            Topic::QsoState(mocks::radio_id()),
            qso.clone(),
            egui_ctx.clone(),
        );
        pump_state(
            &bus,
            Topic::RigState(mocks::radio_id()),
            rig.clone(),
            egui_ctx.clone(),
        );
        pump_spectrum(
            &bus,
            Topic::Spectrum(mocks::radio_id()),
            spectrum.clone(),
            spectrum_tx.clone(),
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
            TopicSelector::Exact(Topic::Decodes(mocks::radio_id())),
            decodes.clone(),
            egui_ctx.clone(),
        );
        // Heard stations for the map: a second decode subscriber that keeps a
        // longer-lived (call → grid) map than the bounded `decodes` ring. Runs in
        // both modes — heard spots come from whatever decoder is live.
        pump_heard(&bus, heard.clone(), egui_ctx.clone());
        // Health is only produced in real mode (by `core`); in mock mode the map
        // stays empty and panels treat everything as healthy.
        if real {
            for id in [SubsystemId::Rig, SubsystemId::Audio] {
                pump_health(&bus, id, health.clone(), egui_ctx.clone());
            }
        }
        drop(_guard);

        Self {
            rig,
            spectrum,
            spectrum_tx,
            scanner,
            clock,
            bands,
            logs,
            decodes,
            heard,
            qso,
            health,
            real,
            control,
            qso_control,
            applied,
            bus,
            _rt: rt,
        }
    }

    /// True when real producers are driving the bus (the default; `DM420_MOCK=1`
    /// opts into mocks).
    pub fn is_real(&self) -> bool {
        self.real
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
        let mut v: Vec<BandActivity> = self.bands.lock().unwrap().values().cloned().collect();
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

    /// Distinct worked stations that carry a grid, most-recent contact per call.
    /// Feeds the Contacts map's "worked" (filled) layer.
    pub fn worked_spots(&self) -> Vec<MapSpot> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for e in self.logs.snapshot().into_iter().rev() {
            if let Some(grid) = &e.grid
                && seen.insert(e.call.0.clone())
            {
                out.push(MapSpot {
                    call: e.call.0.clone(),
                    grid: grid.0.clone(),
                    last_ms: e.time.0,
                    worked: true,
                });
            }
        }
        out
    }

    /// Stations heard with a grid in the last hour, most-recent per call. Feeds the
    /// Contacts map's "unworked" (hollow, dimming) layer. Older spots are dropped
    /// per `docs/map_panel.md` (transient points last at most an hour). Callers
    /// that also show worked spots should exclude calls already in the log.
    pub fn heard_spots(&self) -> Vec<MapSpot> {
        let cutoff = now_ms() - 3_600_000; // one hour
        self.heard
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, (_, t))| *t >= cutoff)
            .map(|(call, (grid, t))| MapSpot {
                call: call.clone(),
                grid: grid.0.clone(),
                last_ms: *t,
                worked: false,
            })
            .collect()
    }

    /// Current wall-clock time, ms since the Unix epoch — the reference the map
    /// uses for recent/all filtering and age-dimming.
    pub fn now_ms(&self) -> i64 {
        now_ms()
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

    /// The `n` most recent decodes, newest first.
    pub fn recent_decodes(&self, n: usize) -> Vec<Decode> {
        self.decodes.snapshot().into_iter().rev().take(n).collect()
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
            radio: mocks::radio_id(),
            slot,
            call: Some(Callsign(call)),
        };
        self.publish_selection(offset_hz, Some(target.clone()));
        self.send_qso_command(QsoCommand::Start { target });
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
                    &Topic::RigCommand(mocks::radio_id()),
                    RigCommand::SetFreq(AbsHz(hz)),
                    Duration::from_secs(1),
                )
                .await;
        });
    }

    /// Publish the current selection (outgoing offset + optional target) onto the
    /// `selection/{id}/active` State topic.
    fn publish_selection(&self, offset_hz: f32, target: Option<DecodeRef>) {
        let _ = self.bus.publish(
            &Topic::Selection(mocks::radio_id()),
            Selection {
                radio: mocks::radio_id(),
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
                    &Topic::QsoCommand(mocks::radio_id()),
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

/// Wall-clock time, ms since the Unix epoch.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Extract a placeable `(call, grid)` from a decode, if it advertises a locator —
/// a CQ with a grid, or a standard grid exchange.
///
/// NOTE (Field Day): a Field Day exchange carries an ARRL `Section`, not a grid
/// (`ExchangePayload::FieldDay`), so those stations yield `None` here and won't be
/// placed yet. Plotting them needs section → coordinate inference — see TODO.md.
fn station_grid(d: &Decode) -> Option<(String, GridSquare)> {
    let DecodeContent::Slotted { message, .. } = &d.content else {
        return None;
    };
    match message {
        ParsedMessage::Cq {
            caller,
            grid: Some(g),
            ..
        } => Some((caller.0.clone(), g.clone())),
        ParsedMessage::Exchange {
            from,
            payload: ExchangePayload::Grid(g),
            ..
        } => Some((from.0.clone(), g.clone())),
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
fn pump_spectrum(
    bus: &BusHandle,
    topic: Topic,
    rx: Cell<SpectrumRow>,
    tx: Cell<SpectrumRow>,
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
                    let cell = match v.source {
                        SignalSource::OwnTx => &tx,
                        SignalSource::Received => &rx,
                    };
                    *cell.lock().unwrap() = Some(v);
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
/// (call → newest (grid, time)). Lets the Contacts map plot stations heard but
/// not worked, retained far longer than the bounded `decodes` ring.
fn pump_heard(
    bus: &BusHandle,
    heard: Arc<Mutex<HashMap<String, (GridSquare, i64)>>>,
    ctx: egui::Context,
) {
    let mut sub =
        match bus.subscribe::<Decode>(TopicSelector::Exact(Topic::Decodes(mocks::radio_id()))) {
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
                    if let Some((call, grid)) = station_grid(&d) {
                        let t = d.t.0;
                        let mut m = heard.lock().unwrap();
                        // Keep the newest sighting per call.
                        let newer = m.get(&call).is_none_or(|(_, prev)| t >= *prev);
                        if newer {
                            m.insert(call, (grid, t));
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
fn pump_bands(bus: &BusHandle, bands: Arc<Mutex<HashMap<Band, BandActivity>>>, ctx: egui::Context) {
    let mut sub =
        match bus.subscribe::<BandActivity>(TopicSelector::Exact(Topic::ScannerCandidates)) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = ?e, "bus_view: candidates subscribe failed");
                return;
            }
        };
    tokio::spawn(async move {
        loop {
            match sub.recv().await {
                Ok(b) => {
                    bands.lock().unwrap().insert(b.band, b);
                    ctx.request_repaint();
                }
                Err(BusError::Lagged { .. }) => continue,
                Err(_) => break,
            }
        }
    });
}
