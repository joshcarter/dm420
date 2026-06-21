# Live FT8/FT4 pipeline — prototype notes & "real implementation" TODO

This documents the shortcuts taken while bringing up the live path (real Kenwood
CAT control, live audio capture, FT8 decode, and the scrolling waterslide/
spectrogram panel). It works end-to-end, but several pieces are prototype-grade.
Severity: 🔴 correctness/blocker · 🟡 should-fix · 🟢 polish.

## Configuration & hardcoding

- ✅ **Rig port/baud now configurable.** `CoreConfig.serial: Option<SerialConfig
  {port, baud, profile, autodetect}>` drives `rig_adapter`; the GUI builds it from
  `DM420_SERIAL_PORT` / `DM420_SERIAL_BAUD` / `DM420_SERIAL_PROFILE` (see
  `crates/gui/src/settings.rs`). With no port (or on open failure) `rig::autodetect`
  sweeps ports × bauds × profiles as a fallback.
- ✅ **Audio device now configurable.** `DM420_AUDIO_INPUT` (case-insensitive
  **substring** match — `open_cpal_device` was upgraded from exact-match). Unset ⇒
  system default input. A panel-by-panel device picker (`audio::list_devices()`)
  is still the eventual UI.
- ✅ **Mode is configurable.** `DM420_MODE=ft8|ft4`, plus a live FT8/FT4 toggle in the
  waterslide header (`set_protocol`); switching retunes the dial to the mode's calling
  frequency. (A full band selector UI is still future work.)
- 🟡 **Real path is the default; `DM420_MOCK` / `DM420_WAV` env vars switch it.**
  Fine for bring-up; the env layer (`settings.rs`) is structured so a future
  per-panel settings UI edits the same `Settings`/`CoreConfig` instead of the
  environment.
- 🟢 **Radio id hardcoded** to `mocks::radio_id()` ("rig0") throughout; single-radio
  assumption baked in.

## Rig control

- ✅ **No longer panics on serial open failure.** `rig_adapter` is a supervised
  connection: it publishes `health/rig` (`SubsystemHealth`), keeps the app running,
  polls with a failure threshold, and drops + reopens with capped backoff on link
  loss. The `RigCommand` server replies `"rig offline"` while down. The Waterfall
  panel dims the VFO readout to `---.---` when the rig is faulted.
- ✅ **TX is live (FT8); FT4 TX implemented, on-air-unverified.** `allow_transmit` is on;
  `core::tx` serves `AudioTx`, keys PTT through the real interlock granter (`core::interlock`),
  plays the synth to the rig's data-in, and reports on `TxReport`; the `qso` engine drives the
  sequence. FT4 now synthesizes too (`encode.rs`) — **sample-identical to the `ft8_lib`
  reference** offline (`ft4_cq_1200.wav`, Pearson r = 1.0) — but has **not been keyed on a
  real radio yet**. Deferred follow-ups (won't block bring-up):
    - 🟡 **First on-air FT4 QSO** is the remaining acceptance test. Watch `core::tx`'s
      `into_slot_ms` log: FT4's DT window is tighter than FT8, so confirm the tones land in
      tolerance after key + audio-buffer latency.
    - 🟡 **`rig::actor::PTT_WATCHDOG` is still FT8-sized** (15 s = two FT4 slots). Normal and
      backstop key-down (`core::tx::max_tx_for`, ~6.5 s on FT4) both fire well before it, so it
      only bites on a double failure — but the FT4 design intended a slot-relative watchdog;
      derive it from the mode. (The "10 s" figure in some `interlock`/`rig_adapter` comments is
      stale — it's 15 s.)
    - 🟡 **No committed encode-reference test.** Only encode⇄our-decode round-trips are
      committed; the sample-identical match above was checked ad hoc. Promote it to a test
      (synth `CQ K1ABC FN42` FT4 @ 1200 Hz, cross-correlate vs `ft4_cq_1200.wav`, assert
      r ≈ 1) so an encode regression can't pass silently — the FT4 architecture doc's test
      strategy already called for a reference-vector check.

## Still-mock subsystems (in real mode)

- ✅ **Logbook is real in real mode.** `core::spawn` runs the `logbook` crate: it
  records the QSO the engine logs on RR73, persists the whole log as JSON
  (`~/.dm420/logbook.json` or `DM420_LOGBOOK`), and replays history on startup.
  `mocks::spawn_support` no longer publishes fake QSOs. Still pending: **ADIF**
  import/export and the **peer-merge** G-set; and `build_log` (in `qso/shell.rs`)
  still stamps placeholder **band/freq/mode** until `OperatingState` is published.
- 🟡 **Scanner is still mock** even in real mode (`mocks::spawn_support`);
  the clock is real wall-clock. Real scanner strategy still needed.

## Audio capture

- ✅ **Device disconnect/recovery handled.** `decode::spawn_live` wraps capture in a
  supervised reconnect loop: a stream that delivers no samples for
  `AUDIO_SILENCE_TIMEOUT` (≈ device lost) is rebuilt with backoff, the fault is
  published on `health/audio`, and each session restarts from clean
  spectrogram/slot state. The Waterfall panel shows `AUDIO OFFLINE/DEGRADED` with
  the reason where the spectrogram + decode rail would be.
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
- 🟢 **Hand-rolled FFT (`dsp` only).** `dsp::fft` (the GUI spectrum/waterfall
  display) is still a tested radix-2 Cooley–Tukey, power-of-two only — fine, but
  `realfft` would be faster if it matters. **The decoder FFT (`modes`) already
  moved to `realfft`** (~10× faster; see `docs/fft_migration_proposal.md`); `dsp`
  could follow the same way. (The old "no dependencies" comments were vendored-code
  ethos, not a mandate — pulling a crate is fine.)

## Alignment refinements

- 🟢 **Decode horizontal position uses slot-start time only.** It ignores FT8's
  in-slot start (~0.5 s) and the decoder's per-signal `dt`. Folding `dt` into the
  timestamp would tighten per-signal alignment with the spectrogram.
- 🟢 **Left-aligned decode text crosses the NOW line.** Fresh decodes (near centre)
  render rightward into the spectrogram half. Clip the text lane at NOW, or offset
  by text width, if that overlap is undesirable.

## UI tunables (knobs that currently live as constants)

- `ws_history_secs()` (waterfall.rs) — seconds per half / scroll rate, derived per
  frame from the rendered message width × slot period (FT8 15 s, FT4 7.5 s).
- `SPECTRUM_HOP_S` (decode.rs) — spectrogram column cadence.
- `COL_DB_FLOOR` / `COL_DB_CEIL` (dsp) — waterfall brightness range.
- `FFT_SIZE` / `SPECTRUM_MAX_HZ` (decode.rs) — bin resolution & frequency span.

These should become settings (or UI controls) rather than recompile-to-change
constants.
