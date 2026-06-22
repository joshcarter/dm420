# CLAUDE.md — orientation for future sessions

**DM420** ("Dingus Mangler 420", a nod to DM780) is a Rust desktop app for operating
**digital-mode amateur radio** — FT8/FT4 first, at QRP (~5 W). Authors: Josh Carter
(N0JDC) and Joel Odom (W4LL). It is **not** a WSJT-X clone; the differentiators are a
rotated-waterfall **"waterslide"** display (decoded text in each signal's frequency
lane), first-class **multi-band monitoring** (a band scanner that time-slices one
receiver), and **LAN sharing** of contacts/heard-stations between operators with no
central database. There is a **Field Day** target driving the roadmap.

## Where the truth lives (read order)

1. **`OVERVIEW.md`** — design objectives and the 7 principles. The orientation layer.
2. **`docs/`** — per-component specs; **authoritative for their panel/module**. When a
   behavior is specified there, that file wins over `OVERVIEW.md`.
3. **This file** — how the code is actually wired *today* (which can differ from the
   aspirational docs; see the next section).

Key spec files: `docs/bus-handoff.md` (the message bus — the real spine),
`docs/message-catalog.md` (all message payload types & topics),
`docs/waterslide_panel.md`, `docs/radio_control.md`, `docs/qso_flow.md`,
`docs/wsjtx_qso_sequencing.md`, `docs/map_panel.md`, `docs/band_scanner.md`,
`docs/log_book.md`, `docs/keyboard_control.md`, `docs/FEASIBILITY.md` (the egui theme
spike — verdict GO), `docs/live_pipeline_notes.md` (**read this**: prototype shortcuts
+ known issues in the live FT8 path), `docs/fft_migration_proposal.md`.

> **Doc vs. reality:** `OVERVIEW.md §3` sketches a "single `App` struct + channels"
> model. That was superseded by the **message-bus architecture** in `docs/bus-handoff.md`,
> which is what's built. Treat `OVERVIEW.md` as design intent, the bus docs as the
> implemented contract.

## Architecture (as built)

Cargo **workspace**, Rust **edition 2024**, toolchain **stable** (rustfmt + clippy).
Everything talks **only over the message bus** using the shared `types` vocabulary;
crates don't depend on each other directly, so they stay independently buildable.

```
┌─ GUI (crates/gui, binary `dm420`) ── egui_tiles panels, Martian Hybrid theme ─┐
│      reads/writes the bus via BusView (the sync↔async seam)                    │
├─ core ── bus-adapter: wires Joel's vendored rig/audio/modes onto the bus ──────┤
│      rig_adapter (serves RigCommand, publishes RigState) · decode (slot→FFT→   │
│      modes::decode→Decode) · health (supervised reconnect) · map · parse       │
├─ bus ── the spine: BusHandle over tokio channels, 4 delivery classes ──────────┤
│      State(watch) · StreamLossy(broadcast) · StreamLossless(mpsc+ring) · Command│
├─ types ── shared serde message vocabulary (no async, no I/O) ──────────────────┤
└─ back-ends: rig (Kenwood CAT + mock) · audio (cpal) · dsp (FFT) · modes (FT8/4) ┘
```

Dependency direction: `types` ← `bus` ← every component. Producers `publish`,
consumers `subscribe`, commands use `request`/`serve`. Topics are scoped per radio
(`radio/{id}/...`); the default radio id is **`"rig0"`** (`core::radio_id()`, matches
`mocks::radio_id()`). The invariant that shapes the whole bus: **a slow/absent
subscriber must never stall a publisher.**

### Crate status (don't assume a crate is implemented just because it exists)

| Crate | State | Notes |
|---|---|---|
| `types` | ✅ implemented | message vocabulary |
| `bus` | ✅ implemented + tested | full 4-class bus, recorder/replay, wildcard subs |
| `core` | ✅ implemented | bus-adapter seam between bus and Joel's prototype |
| `rig` | ✅ implemented | Kenwood CAT, mock rig, port/baud/profile autodetect (vendored, W4LL) |
| `audio` | ✅ implemented | cpal capture/playback/device list (vendored) |
| `dsp` | ✅ implemented | hand-rolled radix-2 FFT + spectrum rows |
| `modes` | ✅ implemented | FT8+FT4 **decode and encode/synth** (FT4 synth verified sample-identical to the `ft8_lib` reference offline; on-air TX not yet confirmed). MIT `ft8_lib` port (vendored, W4LL; see `crates/modes/ATTRIBUTION.md`) |
| `mocks` | ✅ implemented | fake producers for every topic (`mocks::spawn`) — the `DM420_MOCK=1` no-hardware path |
| `gui` | ✅ active dev | the app |
| `qso` | ✅ implemented | contact auto-sequencer (CQ→report→RR73→73, incl. Field Day); tracks mode from decodes and drives the real TX path (`engine.rs`/`shell.rs`/`message.rs`, ~1.8k lines) |
| `logbook` | ✅ implemented | JSON-persistent log store: logs on RR73, replays history on startup; ADIF + peer-merge still pending |
| `archive` | ✅ implemented | raw decode/transmit archive: append-only JSONL of every heard + sent message (off by default; opt in via `[archive] decodes`). Diagnostics/analysis; not QSO-grouped |
| `callbook` | ✅ implemented | offline call-sign → country/ISO prefix resolver (Tier-1 of the Call Sign panel); pure, no I/O. Online name enrichment (Tier-2) not built |
| `scanner` | ✅ implemented | pure band-scan sweep engine; the `core::scan` shell drives it — time-slices RX across 40/20/15/10 in FT8+FT4, 2-slot dwell, loops until cancel, blocks TX |

In **real mode everything is real now** — the **qso** auto-sequencer, **logbook**, decode,
interlock granter, **audio-TX**, the opt-in **decode archive** (`archive` crate), and the
**band scanner** (`scanner` engine + `core::scan` shell) are all spawned by `core::spawn`,
and the clock is real wall-clock. So real mode runs live
**end-to-end QSOs — RX and TX — on FT8** (FT4 TX is implemented and offline-verified,
not yet keyed on a real radio), and the Log Book and Contacts/map panels show
**real** QSOs. Mock mode still uses `mocks::spawn` for everything (seeded **fake** QSOs,
no keying).

### GUI internals (`crates/gui/src`)

- `main.rs` — `eframe` entry, fonts, the egui_tiles `Tactical` behavior (linear splits,
  **not tabs**), the top bar (call/grid edit fields, UTC LCD clock, lock/unlock + focus
  marker), and `App::ui` (palette, brushed-metal/relief textures, panel-focus key
  routing `Cmd/Ctrl+1..5`).
- `app.rs` — the `App` struct (single source of UI state) + `BusView`.
- `bus_view.rs` — **the sync↔async seam**. Owns a tokio runtime holding the `BusHandle`,
  runs one *pump* task per subscribed topic into shared `Cell`/`Ring`s; panels read
  those each frame with **no `.await`**. The one piece the handoff docs don't cover.
- `panels/` — the active instruments behind the `Panel` trait + `PanelCtx`:
  `waterfall` (the live FT8/FT4 **waterslide** + real auto-sequenced Send row + mode toggle + unlocked config form),
  `log_book`, `band_scan`, `call_sign` (selected-station country/flag/distance/
  bearing, offline via the `callbook` crate + `flag.rs`), `contacts` (the map).
- `waterslide_panel.rs` / `waterslide_sim.rs` — **still live**: rendering helpers,
  `Target`, `martian_cmap`, and the decode-placement sim used *by* `panels/waterfall.rs`
  (not a dead older panel).
- `theme.rs` / `chrome.rs` — "Martian Hybrid" palette + brushed-metal/engraved chrome,
  runtime dark/light switch. `settings.rs` — env → `Settings`/`CoreConfig`. `send.rs` —
  outgoing-message construction. `geo_data.rs`/`panel_data.rs` — map basemap + (mock) data.

## Build & run

```sh
cargo build --workspace          # build everything
cargo run -p gui                 # run the app (binary: dm420), real producers by default
cargo test --workspace
cargo clippy --all-targets -- -D warnings
```

By default the GUI runs the **real** rig/decode producers; pass `DM420_MOCK=1` to run
on mocks with no radio/audio hardware needed. It needs the **system clock within ~1 s
of UTC (NTP)** for FT8/FT4 slot timing.

Hardware bindings are set via env vars (interim; a settings UI will replace them) and
persisted to `$HOME/.dm420/config.toml`. A missing/disconnected device **degrades to an
on-screen fault and reconnects on its own**:

| Variable | Purpose | Default |
|---|---|---|
| `DM420_MOCK` | mock producers instead of the real rig/decode path | real |
| `DM420_AUDIO_INPUT` | capture device (case-insensitive substring, e.g. `USB PnP`) | system default |
| `DM420_SERIAL_PORT` | rig CAT device, e.g. `/dev/cu.usbserial-120` | autodetect |
| `DM420_SERIAL_BAUD` | rig baud | `19200` |
| `DM420_SERIAL_PROFILE` | serial line profile: `none` \| `dtr-rts` \| `rtscts` | `none` |
| `DM420_MODE` | on-air mode: `ft8` \| `ft4` | `ft8` |
| `DM420_WAV` | replay a WAV instead of live capture (bring-up) | live capture |

At startup the dark/light palette is **seeded from the host OS appearance** (egui
`system_theme()`, read on the first frame since it isn't populated in `App::new`);
the top-bar DARK/LIGHT toggle then owns it for the session (no live OS following).
`MARTIAN_LIGHT` pins the light palette and opts out of the OS seed (used by the
screenshot path); `MARTIAN_SHOT=<path>` saves a screenshot.

```sh
cargo run -p gui                                               # real, autodetect rig
DM420_AUDIO_INPUT="USB PnP" DM420_SERIAL_PORT=/dev/cu.usbserial-120 \
  DM420_MODE=ft4 cargo run -p gui                               # explicit
DM420_MOCK=1 cargo run -p gui                                   # no hardware (mocks)
```

## Conventions, guardrails & gotchas

- **The `core` crate-name footgun:** this workspace has a member literally named `core`,
  which shadows Rust's `core` prelude path *inside that crate*. Use `::core::…` if you
  need the std `core` from within `crates/core`.
- **Prefer `ct` over shell tools for code intelligence.** Use the `ct` daemon
  (`mcp__ct__*`: search, read, outline, references, callers/callees, edits) over `bash`
  `grep`/`find`/`cat`/`sed` for navigating and reading code — it serves from an in-memory
  index with structural context. `bash` stays the tool for builds, git, and filesystem ops.
- **Commit directly to `main`** for this project — no feature branch (per repo convention).
- **Pin the egui stack** to the versions in the root `Cargo.toml` (egui/eframe 0.34.3,
  egui_tiles 0.15.0, egui_extras 0.34.3). APIs shift across egui minors.
- **No radio controls in the header.** Per-radio control (freq/band/mode/send) lives in
  the **waterslide panel**, not the top bar. (`radio_control.md` overrides the spike's
  placeholder header.)
- **Two postures: locked (operate) / unlocked (configure).** Global; every panel reads
  `PanelCtx.unlocked` to reveal/hide edit affordances. Radio settings apply on **re-lock**.
- **Keyboard focus is global:** one panel holds focus at a time (`Cmd/Ctrl+1..5` or
  click); only the active panel acts on typed input. The waterslide also routes slash
  commands (`/f 14.074`, `/b 20m`) — keep parsing a shared utility.
- **Keep dial/center frequency distinct from outgoing audio (TX) offset** everywhere —
  clicking the waterslide sets the *outgoing* freq without retuning the radio.
- **`origin: Mine | Peer(id)`** must survive through log/map — *heard ≠ worked*, and the
  UI must visually distinguish my data from peers'.
- **macOS exit-crash workaround** (winit/AppKit teardown) — see `FEASIBILITY.md` reader
  notes; carry it forward. Both quit paths hard-exit via `std::process::exit`: the close
  button (`close_requested` in `App::ui`) and ⌘Q (`on_exit`, which macOS delivers *instead*).
- **Unkey on exit (safety):** both quit paths first call `BusView::unkey_for_shutdown` to drop
  the rig's PTT before the hard `process::exit`, so a mid-over quit can't leave the transmitter
  keyed (the exit bypasses Drop; the rig's PTT watchdog is only a ~15 s backstop). Real mode
  only, 1 s bound. Keep this in any new exit path you add.
- **`unsafe` is forbidden** in `bus`/`core` (`#![forbid(unsafe_code)]`).
- `eframe` is immediate-mode: never block the UI thread; all I/O lives behind the bus +
  `BusView` pumps. CPU-heavy decode runs off-thread in `core::decode`.

## Current state & near-term directions

The **full FT8 QSO path works end-to-end on air** — real Kenwood CAT, cpal capture, FT8
decode + scrolling waterslide on RX, and a real auto-sequenced TX path (interlock granter
→ PTT → audio-TX → `TxReport`) driven by the `qso` engine; `allow_transmit` is **on**.
**Read `docs/live_pipeline_notes.md` before touching the pipeline** — it catalogs the
remaining prototype shortcuts and severity-tagged known issues.

Notable open items (see `TODO.md`, `docs/live_pipeline_notes.md`, `OVERVIEW.md §7`):

- 🟡 **FT4 transmit: implemented, on-air-unverified.** The FT4 tone/GFSK synth
  (`encode.rs`), mode-aware `synth_message`, slot-relative TX cap (`core/tx.rs::max_tx_for`),
  and the clock-sourced CQ-first mode (`qso/shell.rs`) are all in and tested — the synth is
  **sample-identical to the `ft8_lib` reference** offline (`ft4_cq_1200.wav`, Pearson r = 1.0).
  Remaining: a first on-air FT4 QSO, and `rig::actor::PTT_WATCHDOG` is still FT8-sized (15 s).
  See `docs/live_pipeline_notes.md`.
- ✅ **Band scanner built** — `scanner` (pure sweep engine) + `core::scan` shell time-slice
  the RX across 40/20/15/10 in FT8+FT4 (2-slot dwell, loops until cancel, blocks TX), wired
  into `core::spawn`. The last real-mode mock is gone. Enhancements (per-offset sweep,
  Field-Day-exchange filter, SNR floor) are in `JOELS_ROADMAP.md` Now-#1.
- 🔴 **Spectrogram ↔ decode-text drift:** text is placed by wall-clock age, the
  spectrogram scrolls by accumulated frame `dt`. Fix = place spectrogram columns by
  their `SpectrumRow.t`.
- **Waterslide decode-text layout:** nudge overlapping decodes up/down for legibility
  **without** rearranging everything; clamp min/max font size; click-to-select must hit
  the true audio center (ignore any text-shift offset).
- **Field Day:** quick log-book reset that marks everyone unworked again; "unworked" is
  **per band** (same call on another band = a new contact).
- Open design decisions still live in `OVERVIEW.md §7` (decoder strategy — already
  resolved toward the `ft8_lib` port; network protocol/discovery; ADIF; map base data;
  the public product name).
```
