# Dingus Mangler 420

A Rust desktop application for operating **digital-mode amateur radio** — **FT8/FT4**
first, at QRP power levels (~5 W). It is **not** a clone of WSJT-X. The emphasis is on
*multi-band situational awareness* and *small-network collaboration between operators*
(a Field Day or club setup), wrapped in a deliberate, instrument-panel visual identity.

> `DM420` ("Dingus Mangler 420", a nod to DM780) is an internal codename. A public
> product name is still an open item. Authors: Josh Carter (N0JDC) and Joel Odom (W4LL).

## What makes it different

- **The "waterslide" display.** A rotated waterfall that places decoded text directly
  beside each signal's frequency lane, so several simultaneous senders stay legible at a
  glance — the primary operating surface (`docs/waterslide_panel.md`).
- **First-class multi-band monitoring.** A band scanner time-slices a single receiver
  across bands (40/20/15/10 m), and the map and log are built to aggregate what's being
  *heard*, not just what's been *worked* (`docs/band_scanner.md`, `docs/map_panel.md`).
- **LAN sharing with no central database.** Operators on the local network gossip their
  logged contacts and heard stations to each other — eventually consistent, peer-to-peer,
  no server (`docs/networking.md`).
- **A Field Day target** drives the roadmap: work as many stations as possible, fast.

### Initial hardware, longer arc

The first supported radios are the **Kenwood TS-480SAT** and **TS-590S** — classic
superheterodyne rigs over a serial CAT link, presenting a *single* ~3 kHz audio passband
at a time. The band scanner exists precisely because one conventional receiver can't watch
several bands at once. The radio abstraction is shaped, though, for a longer-term goal of
**true simultaneous multi-band reception** via SDR back-ends (HPSDR/Hermes-Lite 2,
RX-888 + ka9q-radio, FlexRadio): a back-end can advertise how many simultaneous receivers
it has, and the scanner/waterslide adapt — so the one-receiver assumption is never
hard-coded into the domain model.

## Design principles

1. **Radio capability lives in panels, not app chrome.** DM420 is meant to drive more
   than one radio, in layouts the operator composes. Per-radio controls (frequency, band,
   mode, send) belong to a panel — the waterslide — *not* the top bar
   (`docs/radio_control.md`).
2. **Two operating postures: unlocked (configure) / locked (operate).** Unlocked exposes
   setup affordances; locked hides them to cut clutter and prevent fat-finger config
   changes mid-QSO. It's global; every panel reads it. Radio settings apply on re-lock.
3. **Keyboard-first operation.** `Cmd/Ctrl+1..N` assigns keyboard focus to a panel, which
   interprets keys by its role. The waterslide also routes free text to the outgoing
   message, or — when prefixed — to **slash-command** rig control (`/f 14.074`, `/b 20m`),
   with tolerant parsing (`docs/keyboard_control.md`).
4. **Heard ≠ worked, and both matter.** Much of the value is surfacing stations *heard but
   not yet worked* (map pips, per-band counts, waterslide text) so the operator can read
   band openings and pick targets. `origin: Mine | Peer(id)` rides through the log and map.
5. **Collaborative but eventually-consistent.** Peers broadcast their own contacts and
   (optionally) heard stations over the LAN. There is **no shared database and no
   guaranteed sync** — peers learn what they happen to receive, and the UI clearly
   distinguishes *my* data from *peer* data.
6. **Hardware-agnostic core.** The decode/contact/log/map domain knows nothing about a
   specific rig. Radios sit behind the rig back-end trait; every other component talks
   only over the **message bus** using the shared `types` vocabulary.
7. **Faithful, runtime-switchable theme.** The "Martian Hybrid" instrument look switches
   dark/light at runtime; all painters are palette-driven (`docs/FEASIBILITY.md`).

## Build & run

```sh
cargo build --workspace                  # build everything
cargo run -p gui                         # run the app (binary: dm420)
cargo test --workspace
cargo clippy --all-targets -- -D warnings
```

The GUI drives the real rig/decode producers; it needs the **system clock within ~1 s of
UTC (NTP)** for FT8/FT4 slot timing. A missing or disconnected device degrades to an
on-screen fault and reconnects on its own — the app keeps running.

### Configuration (environment variables)

Everything has a sensible default; nothing here is required. These are interim env vars —
a per-panel settings UI will replace them. Persistent config lives in
`$HOME/.dm420/config.toml` (`[station]`, `[audio]`, `[serial]` tables); edits made in the
unlocked UI are saved there, and the env vars override the saved values for a single launch.

| Variable | Purpose | Default |
|---|---|---|
| `DM420_AUDIO_INPUT` | Capture device name (case-insensitive substring, e.g. `USB PnP`) | system default input |
| `DM420_SERIAL_PORT` | Rig CAT device, e.g. `/dev/cu.usbserial-120` | autodetect |
| `DM420_SERIAL_BAUD` | Rig baud (standard Kenwood rate) | `19200` |
| `DM420_SERIAL_PROFILE` | Serial line profile: `none` \| `dtr-rts` \| `rtscts` | `none` |
| `DM420_MODE` | On-air mode: `ft8` \| `ft4` | `ft8` |
| `DM420_WAV` | Replay a WAV instead of live capture (bring-up/testing) | live capture |

```sh
# Autodetect rig + system audio (the default):
cargo run -p gui

# Explicit capture device + serial port, FT4:
DM420_AUDIO_INPUT="USB PnP" DM420_SERIAL_PORT=/dev/cu.usbserial-120 \
  DM420_SERIAL_BAUD=19200 DM420_MODE=ft4 cargo run -p gui
```

## Architecture at a glance

A Cargo workspace (Rust edition 2024, stable toolchain). Every component communicates
**only** over the message bus, using the shared `types` vocabulary — so the crates don't
depend on each other directly and stay independently buildable. Producers `publish`,
consumers `subscribe`, commands use `request`/`serve`; topics are scoped per radio
(`radio/{id}/...`). The invariant that shapes the whole bus: **a slow or absent subscriber
must never stall a publisher.**

```
crates/
  types/     shared serde message vocabulary (no async, no I/O)        docs/message-catalog.md
  bus/       the message-bus spine — 4 delivery classes, recorder/replay  docs/bus-handoff.md
  core/      bus-adapter seam: spawns the rig/decode/scan/map/net producers
  rig/       RadioBackend trait + Kenwood CAT                           docs/radio_control.md
  audio/     cross-platform audio capture/playback (cpal)
  dsp/       FFT / spectrum rows                                        docs/waterslide_panel.md
  modes/     FT8/FT4 decode + encode/synth (ft8_lib port)              docs/message-catalog.md
  qso/       contact auto-sequencer / QSO engine                       docs/qso_flow.md
  logbook/   JSON-persistent log store                                 docs/log_book.md
  archive/   opt-in raw decode/transmit JSONL archive
  callbook/  offline call-sign → country/ISO-prefix resolver           docs/call_sign_lookup_panel.md
  scanner/   pure band-scan sweep engine                               docs/band_scanner.md
  net/       LAN multi-op gossip (mDNS + UDP)                          docs/networking.md
  gui/       egui front-end (the binary `dm420`)                       docs/FEASIBILITY.md
```

Dependency direction: `types` ← `bus` ← every component. The GUI reads and writes the bus
through `BusView`, the sync↔async seam that lets immediate-mode egui panels read the latest
bus state each frame without blocking.

## Where to go next

- **`CLAUDE.md`** — how the code is actually wired *today*: the message-bus architecture as
  built, the crate roles, the GUI internals, and the conventions/guardrails. Start here to
  work in the code.
- **`docs/`** — per-component specs, **authoritative for their panel/module**. Notably
  `docs/bus-handoff.md` (the bus), `docs/message-catalog.md` (every message type & topic),
  and the per-panel specs linked in the crate table above.
- **`ARCHITECTURE_REVIEW.md`** — the authoritative rework plan (single-owner state
  producers, decided design points). Read before any structural change to the bus or state
  flow.
