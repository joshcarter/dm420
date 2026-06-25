# CLAUDE.md — orientation for future sessions

**DM420** ("Dingus Mangler 420", a nod to DM780) is a Rust desktop app for operating
**digital-mode amateur radio** — FT8/FT4 first, at QRP (~5 W). Authors: Josh Carter
(N0JDC) and Joel Odom (W4LL). It is **not** a WSJT-X clone; the differentiators are a
rotated-waterfall **"waterslide"** display (decoded text in each signal's frequency
lane), first-class **multi-band monitoring** (a band scanner that time-slices one
receiver), and **LAN sharing** of contacts/heard-stations between operators with no
central database. There is a **Field Day** target driving the roadmap.

## Where the truth lives (read order)

1. **`README.md`** — what DM420 is, the differentiators, and the design principles
   (the front-door / design-intent layer).
2. **`docs/`** — per-component specs; **authoritative for their panel/module**. When a
   behavior is specified there, that file wins over `README.md`.
3. **`ARCHITECTURE_REVIEW.md`** — the authoritative rework plan (single-owner state
   producers, phased sequence, decided design points). Read it before any structural
   change to the bus, producers, or state flow.
4. **This file** — how the code is actually wired *today* (which can differ from the
   aspirational docs; see the next section).

Key spec files: `docs/bus-handoff.md` (the message bus — the real spine),
`docs/message-catalog.md` (message payload types & topics — note some advertised topics
are *designed-but-unbuilt*; `ARCHITECTURE_REVIEW.md` flags which),
`docs/waterslide_panel.md`, `docs/radio_control.md`, `docs/qso_flow.md`,
`docs/wsjtx_qso_sequencing.md`, `docs/map_panel.md`, `docs/band_scanner.md`,
`docs/log_book.md`, `docs/keyboard_control.md`, `docs/networking.md` (the LAN multi-op
spec), `docs/live_pipeline_notes.md` (**read this**: prototype shortcuts + known issues
in the live FT8/FT4 path), `docs/FEASIBILITY.md` (the egui theme spike — verdict GO).

## Architecture (as built)

Cargo **workspace**, Rust **edition 2024**, toolchain **stable** (rustfmt + clippy).
Everything talks **only over the message bus** using the shared `types` vocabulary;
crates don't depend on each other directly, so they stay independently buildable.

```
┌─ GUI (crates/gui, binary `dm420`) ── egui_tiles panels, Martian Hybrid theme ─┐
│      reads/writes the bus via BusView (the sync↔async seam)                    │
├─ core ── bus-adapter: wires the rig/audio/modes back-ends onto the bus ────────┤
│      rig_adapter (serves RigCommand, publishes RigState) · decode (slot→FFT→   │
│      modes::decode→Decode) · health (supervised reconnect) · scan · map · parse│
├─ bus ── the spine: BusHandle over tokio channels, 4 delivery classes ──────────┤
│      State(watch) · StreamLossy(broadcast) · StreamLossless(mpsc+ring) · Command│
├─ types ── shared serde message vocabulary (no async, no I/O) ──────────────────┤
└─ back-ends: rig (Kenwood CAT + mock) · audio (cpal) · dsp (FFT) · modes (FT8/4) ┘
```

Dependency direction: `types` ← `bus` ← every component. Producers `publish`,
consumers `subscribe`, commands use `request`/`serve`. Topics are scoped per radio
(`radio/{id}/...`); the default radio id is **`"rig0"`** (`core::radio_id()`). The
invariant that shapes the whole bus: **a slow/absent subscriber must never stall a
publisher.**

**Crate roles** (all depend on `types`/`bus`, not on each other): `types` the message
vocabulary · `bus` the 4-class bus (+ recorder/replay, wildcard subs) · `core` the
bus-adapter seam that `spawn`s the producers · `rig` Kenwood CAT + an in-memory `MockRig`
+ port/baud/profile autodetect (vendored, W4LL) · `audio` cpal capture/playback/device
list (vendored) · `dsp` hand-rolled radix-2 FFT + spectrum rows · `modes` FT8/FT4 decode
+ encode/synth (`ft8_lib` port, vendored; see `crates/modes/ATTRIBUTION.md`) · `qso` the
contact auto-sequencer (CQ→report→RR73→73, incl. Field Day) · `logbook` JSON-persistent
log store · `archive` opt-in append-only JSONL of every heard/sent message · `callbook`
offline call-sign → country/ISO-prefix resolver (pure, no I/O) · `scanner` pure band-scan
sweep engine (driven by the `core::scan` shell) · `net` LAN multi-op gossip (mDNS+UDP;
see `docs/networking.md`).

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
  `waterfall` (the live FT8/FT4 **waterslide** + auto-sequenced Send row + mode toggle +
  unlocked config form), `log_book`, `band_scan`, `call_sign` (selected-station country/
  flag/distance/bearing, offline via the `callbook` crate + `flag.rs`), `contacts` (the map).
- `waterslide_panel.rs` / `waterslide_sim.rs` — **live** rendering helpers (`Target`,
  `martian_cmap`, decode-placement) used *by* `panels/waterfall.rs` (not a dead panel).
- `theme.rs` / `chrome.rs` — "Martian Hybrid" palette + brushed-metal/engraved chrome,
  runtime dark/light switch. `settings.rs` — env → `Settings`/`CoreConfig`. `send.rs` —
  outgoing-message construction. `geo_data.rs`/`panel_data.rs` — map basemap + data.

## Build & run

```sh
cargo build --workspace          # build everything
cargo run -p gui                 # run the app (binary: dm420)
cargo test --workspace
cargo clippy --all-targets -- -D warnings
```

The GUI drives the rig/decode producers live; it needs the **system clock within ~1 s
of UTC (NTP)** for FT8/FT4 slot timing.

Hardware bindings are set via env vars (interim; a settings UI will replace them) and
persisted to `$HOME/.dm420/config.toml`. A missing/disconnected device **degrades to an
on-screen fault and reconnects on its own**:

| Variable | Purpose | Default |
|---|---|---|
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
cargo run -p gui                                               # autodetect rig
DM420_AUDIO_INPUT="USB PnP" DM420_SERIAL_PORT=/dev/cu.usbserial-120 \
  DM420_MODE=ft4 cargo run -p gui                               # explicit
```

## State ownership (read before adding any shared state)

This is the most important rule in the codebase.

- **The rule.** Every piece of shared/observable state has exactly **ONE producer** that
  owns it and publishes it on the bus as a **State topic**. Every other component
  **subscribes and reads** it — nothing keeps a hand-reconciled local copy and nothing
  independently re-derives the same fact.
- **Why.** Independent re-derivation is the project's root fragility: copies drift, and
  changing the rule in one place silently breaks the others. The canonical example is
  **worked-status**, which was computed in three-to-five places with *divergent keys*
  (the scanner keyed `(call, band, mode)`, the GUI keyed `(call, band)` in one spot and
  bare `call` in another, `core::scan` kept its own tally). Result: a station worked on
  20m FT8 reads "worked" in the waterslide but "unworked" in the scanner on 20m FT4.
- **For contributors.** When you need some shared state, **find its producer and
  subscribe** — never add another local computation. If no producer exists yet, that is
  the gap the current rework is closing (`ARCHITECTURE_REVIEW.md` — *build the
  single-owner State producers the message catalog already specifies*). Add one with the
  **producer template**: a task in `core` that **subscribes its inputs and publishes a
  State topic**, with consumers reading it via a `BusView` pump (GUI) or a `subscribe`
  (other crates). The **worked-status / enrichment producer** (`WorkedStatus` /
  `EnrichedDecode`, keyed once through `types::worked_key()` and carrying
  `origin: Mine | Peer`) is the first / reference example.

> **Current reality (mid-migration).** The codebase is partway to this. Several facts —
> worked-status, on-air mode/band, TX offset, the slot clock — are still hand-reconciled
> across consumers and are being converted to single owners. **Do not add to the
> duplication;** extend (or build) the owner instead. Treat this like the "doc vs.
> reality" note above: the rule is the target, and some code hasn't landed there yet.

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
  keyed (the exit bypasses Drop; the rig's PTT watchdog is only a ~15 s backstop). 1 s bound.
  Keep this in any new exit path you add.
- **`unsafe` is forbidden** in `bus`/`core` (`#![forbid(unsafe_code)]`).
- `eframe` is immediate-mode: never block the UI thread; all I/O lives behind the bus +
  `BusView` pumps. CPU-heavy decode runs off-thread in `core::decode`.

Open work is tracked in **`STATUS.md`** (the single owner). Architecture rationale
lives in `ARCHITECTURE_REVIEW.md`; component specs in `docs/`; live-pipeline known
issues in `docs/live_pipeline_notes.md` — not here.
