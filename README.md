# dm420

A desktop application for operating **digital-mode amateur radio** (FT8/FT4 first),
with a rotated-waterfall "waterslide" display, first-class multi-band monitoring, and
local-network collaboration between operators. See **[`OVERVIEW.md`](OVERVIEW.md)** for
the design objectives and **[`docs/`](docs/)** for the per-component specs.

> `dm420` is an internal codename (a nod to DM780). A public product name is an open
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

The GUI requires the system clock within ~1 s of UTC (NTP) for FT8/FT4 slot timing.

## Contributors

- Josh Carter — **N0JDC** — UI, message bus, scanner, cross-station gossip
- Joel Odom — **W4LL** — radio control, FT4/FT8 encode/decode

Dual-licensed under MIT OR Apache-2.0.
