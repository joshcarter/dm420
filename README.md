# Dingus Mangler 420

A desktop application for operating **digital-mode amateur radio** (FT8/FT4 first),
with a rotated-waterfall "waterslide" display, first-class multi-band monitoring, and
local-network collaboration between operators. See **[`OVERVIEW.md`](OVERVIEW.md)** for
the design objectives and **[`docs/`](docs/)** for the per-component specs.

> `DM420` is an internal codename (a nod to DM780). A public product name is an open
> decision (`OVERVIEW.md` §7.6).

## Layout

A Cargo workspace. Every component communicates **only** over the message bus, using
the shared `types` vocabulary — so the crates don't depend on each other directly and
stay independently buildable.

```
crates/
  types/    shared serde message vocabulary (no async, no I/O)   docs/message-catalog.md
  bus/      the message-bus spine (BusHandle, topics, replay)     docs/bus-handoff.md   [Josh]
  core/     clock/scheduler, interlock granter, enrichment        OVERVIEW.md §3
  rig/      RadioBackend trait + Kenwood CAT                       docs/radio_control.md [Joel]
  audio/    cross-platform audio I/O (cpal)
  dsp/      FFT / spectrum rows                                    docs/waterslide_panel.md
  modes/    FT8/FT4 encode+decode, calling-freq tables            docs/message-catalog.md §3 [Joel]
  qso/      contact state machine / QSO engine                    docs/radio_control.md
  logbook/  log store + ADIF + peer-merge                         docs/log_book.md
  scanner/  band-scanner strategy                                 docs/band_scanner.md  [Josh]
  gui/      egui front-end (the binary `dm420`)                   docs/FEASIBILITY.md
```

Dependency direction: `types` ← `bus` ← every component. Components wire to each other
only through the bus.

## Build & run

```sh
cargo build --workspace      # build everything
cargo run -p gui             # run the app (binary: dm420)
```

By default the GUI runs on **mock** producers, so it launches with no radio or
audio hardware present. The GUI requires the system clock within ~1 s of UTC
(NTP) for FT8/FT4 slot timing.

### Configuration (environment variables)

Real hardware is opt-in via `DM420_REAL`. Everything else has a sensible default;
nothing here is required, and a missing/disconnected device degrades to an
on-screen fault (the app keeps running and reconnects on its own). These are
interim env vars — a per-panel settings UI will replace them.

| Variable | Purpose | Default |
|---|---|---|
| `DM420_REAL` | Use real rig/decode producers instead of mocks | mocks |
| `DM420_AUDIO_INPUT` | Capture device name (case-insensitive substring, e.g. `USB PnP`) | system default input |
| `DM420_SERIAL_PORT` | Rig CAT device, e.g. `/dev/cu.usbserial-120` | autodetect |
| `DM420_SERIAL_BAUD` | Rig baud (standard Kenwood rate) | `19200` |
| `DM420_SERIAL_PROFILE` | Serial line profile: `none` \| `dtr-rts` \| `rtscts` | `none` |
| `DM420_MODE` | On-air mode: `ft8` \| `ft4` | `ft8` |
| `DM420_WAV` | Replay a WAV instead of live capture (bring-up/testing) | live capture |

```sh
# Real radio + audio, explicit serial port, FT4:
DM420_REAL=1 DM420_AUDIO_INPUT="USB PnP" DM420_SERIAL_PORT=/dev/cu.usbserial-120 \
  DM420_SERIAL_BAUD=19200 DM420_MODE=ft4 cargo run -p gui

# Real mode, let the rig autodetect its port/baud:
DM420_REAL=1 cargo run -p gui
```
