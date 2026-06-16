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

/// The GUI-facing view of live bus state. Cheap to construct once at startup and
/// held by `App`; every accessor returns an owned snapshot so panels never hold a
/// lock across drawing. Dropping it drops the runtime, which stops the producers
/// and pumps.
pub struct BusView {
    rig: Cell<RigState>,
    /// Latest waterfall column (real decoder only; published once per slot).
    spectrum: Cell<SpectrumRow>,
    // `scanner`/`clock` are pumped and exposed now; their panel consumers (idle
    // scan status, slot clock in the top bar) land in the next wiring pass.
    #[allow(dead_code)]
    scanner: Cell<ScannerState>,
    #[allow(dead_code)]
    clock: Cell<ClockStatus>,
    bands: Arc<Mutex<HashMap<Band, BandActivity>>>,
    logs: Ring<LogEntry>,
    decodes: Ring<Decode>,
    /// Latest health per hardware subsystem (real mode only). Drives the panels'
    /// fault display when a device is missing or disconnected.
    health: Arc<Mutex<HashMap<SubsystemId, SubsystemHealth>>>,

    /// Whether real producers (`DM420_REAL=1`) are driving the bus. Panels use
    /// this to avoid presenting mock-only visuals (e.g. the simulated FFT) as if
    /// they were live radio data.
    real: bool,

    /// Handle for live reconfiguration of the running producers (real mode only;
    /// empty otherwise).
    control: app_core::CoreControl,
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
    pub fn start(egui_ctx: egui::Context) -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("build bus runtime");
        let bus = BusHandle::new();

        let rig = cell();
        let spectrum = cell();
        let scanner = cell();
        let clock = cell();
        let bands: Arc<Mutex<HashMap<Band, BandActivity>>> = Arc::new(Mutex::new(HashMap::new()));
        let logs = Ring::new(512);
        let decodes = Ring::new(64);
        let health: Arc<Mutex<HashMap<SubsystemId, SubsystemHealth>>> =
            Arc::new(Mutex::new(HashMap::new()));

        // `tokio::spawn` (used by the producers and the pump helpers) needs an
        // active runtime context; hold the guard while we wire everything up.
        let settings = crate::settings::Settings::from_env();
        let real = settings.is_real();
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

        pump_state(
            &bus,
            Topic::RigState(mocks::radio_id()),
            rig.clone(),
            egui_ctx.clone(),
        );
        pump_state(
            &bus,
            Topic::Spectrum(mocks::radio_id()),
            spectrum.clone(),
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
            scanner,
            clock,
            bands,
            logs,
            decodes,
            health,
            real,
            control,
            applied,
            bus,
            _rt: rt,
        }
    }

    /// True when real producers (`DM420_REAL=1`) are driving the bus.
    pub fn is_real(&self) -> bool {
        self.real
    }

    // ----------------------------------------------------------------- reads

    /// The current rig state (frequency, mode, PTT, meters), if seen yet.
    pub fn rig_state(&self) -> Option<RigState> {
        self.rig.lock().unwrap().clone()
    }

    /// The latest waterfall spectrum column, if one has arrived (real mode only).
    pub fn spectrum(&self) -> Option<SpectrumRow> {
        self.spectrum.lock().unwrap().clone()
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

    /// Distinct worked stations that carry a grid, as `(call, grid)`, most-recent
    /// contact per call. Feeds the Contacts map.
    pub fn worked_spots(&self) -> Vec<(String, String)> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for e in self.logs.snapshot().into_iter().rev() {
            if let Some(grid) = &e.grid
                && seen.insert(e.call.0.clone())
            {
                out.push((e.call.0.clone(), grid.0.clone()));
            }
        }
        out
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
        *self.applied.lock().unwrap() = cfg;
    }

    /// Input-capable audio device names, for the settings picker.
    pub fn audio_inputs(&self) -> Vec<String> {
        app_core::list_audio_inputs()
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

// ------------------------------------------------------------------- pumps

/// Spawn a pump that mirrors a `State` topic's latest value into `cell`.
fn pump_state<T: BusMessage>(bus: &BusHandle, topic: Topic, cell: Cell<T>, ctx: egui::Context) {
    let mut sub = match bus.subscribe::<T>(TopicSelector::Exact(topic)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("bus_view: state subscribe failed: {e:?}");
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
            eprintln!("bus_view: stream subscribe failed: {e:?}");
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
            eprintln!("bus_view: health subscribe failed: {e:?}");
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
    let mut sub = match bus.subscribe::<BandActivity>(TopicSelector::Exact(Topic::ScannerCandidates))
    {
        Ok(s) => s,
        Err(e) => {
            eprintln!("bus_view: candidates subscribe failed: {e:?}");
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
