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
| `modes` | ✅ implemented | FT8/FT4 encode+decode — MIT `ft8_lib` port (vendored, W4LL; see `crates/modes/ATTRIBUTION.md`) |
| `mocks` | ✅ implemented | fake producers for every topic; `mocks::spawn_support` |
| `gui` | ✅ active dev | the app |
| `qso` | 🪧 **stub** (~8 lines) | contact state machine — not built; currently mocked |
| `logbook` | ✅ implemented | JSON-persistent log store: logs on RR73, replays history on startup; ADIF + peer-merge still pending |
| `scanner` | 🪧 **stub** | band-scanner strategy — not built; currently mocked |

In **real mode** the **scanner** is still mock (`mocks::spawn_support`); the **logbook**
is now the real persistent crate (spawned by `core::spawn`, JSON at `~/.dm420/logbook.json`
or `DM420_LOGBOOK`), and the clock is real wall-clock. So the Log Book and Contacts/map
panels show **real** QSOs in real mode — and seeded **fake** QSOs in mock mode, which still
uses `mocks::spawn` for everything.

### GUI internals (`crates/gui/src`)

- `main.rs` — `eframe` entry, fonts, the egui_tiles `Tactical` behavior (linear splits,
  **not tabs**), the top bar (call/grid edit fields, UTC LCD clock, lock/unlock + focus
  marker), and `App::ui` (palette, brushed-metal/relief textures, panel-focus key
  routing `Cmd/Ctrl+1..4`).
- `app.rs` — the `App` struct (single source of UI state) + `BusView`.
- `bus_view.rs` — **the sync↔async seam**. Owns a tokio runtime holding the `BusHandle`,
  runs one *pump* task per subscribed topic into shared `Cell`/`Ring`s; panels read
  those each frame with **no `.await`**. The one piece the handoff docs don't cover.
- `panels/` — the active instruments behind the `Panel` trait + `PanelCtx`:
  `waterfall` (the live FT8 **waterslide** + Send row + unlocked config form),
  `log_book`, `contacts` (the map), `band_scan`.
- `waterslide_panel.rs` / `waterslide_sim.rs` — **still live**: rendering helpers,
  `Target`, `martian_cmap`, and the decode-placement sim used *by* `panels/waterfall.rs`
  (not a dead older panel).
- `theme.rs` / `chrome.rs` — "Martian Hybrid" palette + brushed-metal/engraved chrome,
  runtime dark/light switch. `settings.rs` — env → `Settings`/`CoreConfig`. `send.rs` —
  outgoing-message construction. `geo_data.rs`/`panel_data.rs` — map basemap + (mock) data.

## Build & run

```sh
cargo build --workspace          # build everything
cargo run -p gui                 # run the app (binary: dm420), mocks by default
cargo test --workspace
cargo clippy --all-targets -- -D warnings
```

By default the GUI runs on **mock** producers — no radio/audio hardware needed. It needs
the **system clock within ~1 s of UTC (NTP)** for FT8/FT4 slot timing.

Real hardware is opt-in via env vars (interim; a settings UI will replace them). A
missing/disconnected device **degrades to an on-screen fault and reconnects on its own**:

| Variable | Purpose | Default |
|---|---|---|
| `DM420_REAL` | real rig/decode producers instead of mocks | mocks |
| `DM420_AUDIO_INPUT` | capture device (case-insensitive substring, e.g. `USB PnP`) | system default |
| `DM420_SERIAL_PORT` | rig CAT device, e.g. `/dev/cu.usbserial-120` | autodetect |
| `DM420_SERIAL_BAUD` | rig baud | `19200` |
| `DM420_SERIAL_PROFILE` | serial line profile: `none` \| `dtr-rts` \| `rtscts` | `none` |
| `DM420_MODE` | on-air mode: `ft8` \| `ft4` | `ft8` |
| `DM420_WAV` | replay a WAV instead of live capture (bring-up) | live capture |

`MARTIAN_LIGHT` forces the light palette; `MARTIAN_SHOT=<path>` saves a screenshot.

```sh
DM420_REAL=1 cargo run -p gui                                   # real, autodetect rig
DM420_REAL=1 DM420_AUDIO_INPUT="USB PnP" DM420_SERIAL_PORT=/dev/cu.usbserial-120 \
  DM420_MODE=ft4 cargo run -p gui                               # explicit
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
- **Keyboard focus is global:** one panel holds focus at a time (`Cmd/Ctrl+1..4` or
  click); only the active panel acts on typed input. The waterslide also routes slash
  commands (`/f 14.074`, `/b 20m`) — keep parsing a shared utility.
- **Keep dial/center frequency distinct from outgoing audio (TX) offset** everywhere —
  clicking the waterslide sets the *outgoing* freq without retuning the radio.
- **`origin: Mine | Peer(id)`** must survive through log/map — *heard ≠ worked*, and the
  UI must visually distinguish my data from peers'.
- **macOS exit-crash workaround** (winit/AppKit teardown) — see `FEASIBILITY.md` reader
  notes; carry it forward (`App::ui` calls `std::process::exit` on close request).
- **`unsafe` is forbidden** in `bus`/`core` (`#![forbid(unsafe_code)]`).
- `eframe` is immediate-mode: never block the UI thread; all I/O lives behind the bus +
  `BusView` pumps. CPU-heavy decode runs off-thread in `core::decode`.

## Current state & near-term directions

The **live FT8 receive path works end-to-end** (real Kenwood CAT, cpal capture, FT8
decode, scrolling waterslide). Recent work (`git log`) has been GUI: FT8 Send row with a
**mock** arm/transmit lifecycle, active-panel keyboard routing, focus markers, and
configurable station call/grid. **Read `docs/live_pipeline_notes.md` before touching the
pipeline** — it catalogs the prototype shortcuts and severity-tagged known issues.

Notable open items (see `TODO.md`, `docs/live_pipeline_notes.md`, `OVERVIEW.md §7`):

- 🔴 **TX is hard-blocked** (`allow_transmit: false`). No PTT/audio-TX path; the
  interlock-token granter (a `core` service) is stubbed. Real TX needs the granter, PTT
  sequencing, and the `AudioTx`/`TxReport` topics. The Send row's transmit is mock.
- 🔴 **Build the real `qso`, `logbook`, `scanner` crates** to displace the mocks (one
  topic at a time, mirroring the `mocks::spawn` / `core::spawn` pattern).
- 🔴 **Spectrogram ↔ decode-text drift:** text is placed by wall-clock age, the
  spectrogram scrolls by accumulated frame `dt`. Fix = place spectrogram columns by
  their `SpectrumRow.t`.
- **FT4/FT8 mode switch + per-band calling-frequency tables** in the UI (`TODO.md`).
- **Waterslide decode-text layout:** nudge overlapping decodes up/down for legibility
  **without** rearranging everything; clamp min/max font size; click-to-select must hit
  the true audio center (ignore any text-shift offset).
- **Field Day:** quick log-book reset that marks everyone unworked again; "unworked" is
  **per band** (same call on another band = a new contact).
- Open design decisions still live in `OVERVIEW.md §7` (decoder strategy — already
  resolved toward the `ft8_lib` port; network protocol/discovery; ADIF; map base data;
  the public product name).
```
