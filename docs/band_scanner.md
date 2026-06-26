# Band Scanner panel

This document describes a new type of UI panel.

## Status — implemented (June 2026)

The scanner is **built and wired**: the `scanner` crate's pure sweep engine + the
`core::scan` I/O shell, spawned by `core::spawn` in real mode. It follows the spec
below, with three agreed tweaks:

- **Two intervals per stop** (not one): it dwells ≥2 slots per band/mode so both
  even/odd TX parities are covered — a one-slot dwell would miss every station whose
  transmit turn is the other slot.
- **Both FT8 and FT4** are scanned. The plan is *mode-major* (all bands in FT8, then
  all bands in FT4) because a mode change restarts audio capture while a band change is
  a cheap retune — so mode changes are minimized.
- **Loops until the user cancels** (rather than one pass then auto-return), so later
  passes pick up traffic an earlier one missed. Cancel restores the operator's prior
  band + mode (the spec's "return to normal operating state").
- **Active bands** (the "all active bands" the spec sweeps) are the operator's
  configured subset of the six HF bands (160/80/40/20/15/10 m), checked in the
  **BANDS** section of the unlocked Digital panel and persisted to `[bands] list` in
  `config.toml`. The selection commits on re-lock and narrows the scanner sweep, the
  Band Status panel, and the Contacts map's band switcher alike (an empty selection
  means all six). The band-status producer still tracks all six so a re-locked change
  applies live without a restart.

Per-band heard/unworked counts come from live decodes cross-referenced against the
logbook. Counts are cumulative over the scan (distinct callsigns) and split per
**band and mode** into **heard** (every station decoded transmitting), **cq** (those
calling CQ, a subset of heard), and **unworked** (heard but not yet logged on that
band + mode). The panel has a per-band **FT8/FT4 toggle pair** to skip bands/modes
(flippable live mid-scan, without resetting counts), shows the elapsed scan time
(mm:ss) while running, brightens the band/mode being dwelled (dimming its other
mode), and zeroes the counts when a scan starts. The original spec follows.

The band scanner will be selectively activated by the user, and when
it runs, it blocks radio transmissions and does the following:

- For each radio band that the user has selected in the panel, it
  switches to that band and listens for a FT4 / FT8 interval and
  decodes all traffic near the calling frequency for that band.
  
- Once that interval has completed, it switches to the next band
  automatically. It will go through all active bands.
  
- Once complete, the band scanner will automatically return the
  application to its normal operating state.
  
- The band scanner will have a "cancel" option for the user to
  immediately cancel the scan if necessary.
  
The panel will display:

Panel headery:

- "Band Scan" label.

- "Last scan: [x] minutes ago" OR "Currently scanning" text.

- "Scan" button (if not scanning) or "Cancel" if scanning.
  
Panel body:
  
- A text block for each band. The bands will be: 40m, 20m, 15m, 10m.
  They should be displayed in two columns, with 40 and 20 on the left
  and 15 and 10 on the right.
  
- Each text block should have the bandwidth as a large number on the
  left (e.g. "20m"). To the right of the bandwidth should be two lines
  which are half the height of the bandwidth. The first line shows the
  total number of stations seen on that band, the second line shows
  the number of unworked stations on that band.

As a future enhancement, data from the band scanner should also create
pips on the map display.
