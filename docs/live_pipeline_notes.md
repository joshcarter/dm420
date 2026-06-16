# Live FT8 pipeline — prototype notes & "real implementation" TODO

This documents the shortcuts taken while bringing up the live path (real Kenwood
CAT control, live audio capture, FT8 decode, and the scrolling waterslide/
spectrogram panel). It works end-to-end, but several pieces are prototype-grade.
Severity: 🔴 correctness/blocker · 🟡 should-fix · 🟢 polish.

## Configuration & hardcoding

- 🔴 **Rig port/baud hardcoded.** `crates/core/src/rig_adapter.rs` has
  `PORT = "/dev/cu.usbserial-120"` and `BAUD = 19_200` as in-function constants.
  Should come from `CoreConfig` (a `serial: Option<{port, baud, profile}>` field)
  driven by config file and/or `DM420_*` env vars. Wire `rig::autodetect` as a
  fallback so the user isn't guessing baud.
- 🔴 **Audio device hardcoded.** `crates/gui/src/bus_view.rs` `real_core_config()`
  pins `input: Some("USB PnP Sound Device")`. Should be config/env-driven, with a
  device picker in the UI (`audio::list_devices()` already exists).
  Note `open_cpal_device` matches device name **exactly** — no substring/fuzzy.
- 🟡 **Mode is hardcoded to FT8.** No FT4 selection; `Protocol::Ft8` is baked into
  `real_core_config()`. Real impl needs a band/mode selector.
- 🟡 **Real path gated by `DM420_REAL` / `DM420_WAV` env vars.** Fine for bring-up;
  a real build needs proper source/device/radio selection (settings + UI).
- 🟢 **Radio id hardcoded** to `mocks::radio_id()` ("rig0") throughout; single-radio
  assumption baked in.

## Rig control

- 🔴 **Panics on serial open failure.** `open_serial(...).unwrap_or_else(|e| panic!())`.
  Should surface an error to the UI and keep the app running (degrade to no-rig).
- 🔴 **TX is hard-blocked.** `allow_transmit: false` everywhere; no PTT/audio-TX
  path is wired. The interlock-token validation is stubbed — see the comment in
  `rig_adapter::apply` ("future core granter"). A real TX path needs the granter,
  PTT sequencing, and the `AudioTx`/`TxReport` topics.

## Still-mock subsystems (in real mode)

- 🔴 **Logbook, scanner, and clock are still mock** even with `DM420_REAL=1`
  (`mocks::spawn_support`). The Contacts map and Log Book panels show **fake QSOs**.
  Real producers needed (logbook persistence + ADIF, scanner strategy, UTC clock).

## Audio capture

- 🔴 **No device disconnect/recovery.** `audio::capture_stream` opens once; if the
  device disappears or errors, the stream silently ends and decoding stops with no
  retry/reconnect. Needs supervision + reopen.
- 🟡 **Slot alignment depends on the system clock (NTP).** `spawn_live` keys slot
  boundaries off wall-clock `current_slot_start`. Already documented as a
  requirement, but there's no drift detection/warning if the clock is off.
- 🟡 **First slot is partial** (device warm-up between open and first samples);
  harmless but produces a short first decode.

## Decode pipeline (`crates/core/src/decode.rs`)

- 🟡 **Unbounded per-slot decode threads.** Each slot boundary spawns a fresh
  `std::thread` to decode. If a decode ever runs longer than a slot, threads pile
  up with no backpressure. Use a bounded worker / single decode thread with a
  queue.
- 🟡 **No clean shutdown.** `spawn_live` loops forever on a detached thread; the
  `CaptureStream` is only dropped at process exit. Fine today, but there's no way
  to stop/restart capture (needed for device/source switching).
- 🟢 **WAV-replay path is inconsistent with live.** `spawn_wav` paces on
  `REPLAY_INTERVAL` (not real slot timing), stamps decodes with `now_ms()` (so
  horizontal placement differs from live), and produces **no spectrogram**.
- 🟢 **`eprintln!` proof-of-life logging** should be `tracing` once the GUI installs
  a subscriber (it currently has none, which is why `eprintln!` was used).

## Spectrogram / FFT (`crates/dsp`, `crates/gui/.../waterfall.rs`)

- 🔴 **Spectrogram and decode text can drift apart.** The decode text is positioned
  by **wall-clock age** (real `Decode.t`), but the spectrogram scrolls by
  **accumulated frame `dt`** and ignores `SpectrumRow.t` entirely. Over time, or
  with dropped frames, the two can desync. Real fix: position spectrogram columns
  by their timestamp (resync to wall clock), not by frame-time integration.
- 🟡 **Spectrum stream is sampled lossily.** `BusView` keeps only the latest
  `SpectrumRow` in a `Cell`; the panel writes that one column into every pixel it
  scrolled since the last frame. So spectrogram time-resolution is bounded by frame
  rate, not the ~20 columns/s the decoder emits. A ring drained per frame (placing
  each column by its `t`) would be faithful and fixes the drift above.
- 🟡 **Brightness scale is empirical.** `COL_DB_FLOOR`/`COL_DB_CEIL` in
  `crates/dsp/src/lib.rs` are fixed guesses. Real impl wants a user "reference
  level" control (à la WSJT-X) or an adaptive AGC.
- 🟢 **Colormap rebuilt every frame.** `martian_cmap()` (256-entry LUT) is rebuilt
  per paint in `waterfall.rs`. Cache it (rebuild only on palette flip).
- 🟢 **Hand-rolled FFT.** `dsp::fft` is a tested radix-2 Cooley–Tukey, power-of-two
  only. Fine, but `rustfft`/`realfft` would be faster and handle arbitrary sizes if
  performance matters. (The "no dependencies" comments in `dsp`/`modes` are
  vendored-code ethos, not a project mandate — pulling a crate is fine.)

## Alignment refinements

- 🟢 **Decode horizontal position uses slot-start time only.** It ignores FT8's
  in-slot start (~0.5 s) and the decoder's per-signal `dt`. Folding `dt` into the
  timestamp would tighten per-signal alignment with the spectrogram.
- 🟢 **Left-aligned decode text crosses the NOW line.** Fresh decodes (near centre)
  render rightward into the spectrogram half. Clip the text lane at NOW, or offset
  by text width, if that overlap is undesirable.

## UI tunables (knobs that currently live as constants)

- `WS_HISTORY_SECS` (waterfall.rs) — seconds per half / scroll rate.
- `SPECTRUM_HOP_S` (decode.rs) — spectrogram column cadence.
- `COL_DB_FLOOR` / `COL_DB_CEIL` (dsp) — waterfall brightness range.
- `FFT_SIZE` / `SPECTRUM_MAX_HZ` (decode.rs) — bin resolution & frequency span.

These should become settings (or UI controls) rather than recompile-to-change
constants.
