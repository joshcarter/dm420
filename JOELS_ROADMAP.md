# Joel's Roadmap — now / next / later

**Joel's working roadmap** (kept separate from Josh's workflow). This consolidates the
explicit roadmap items scattered across `TODO.md`, `TODO_NETWORK.md`, `OVERVIEW.md §7`,
`docs/live_pipeline_notes.md`, and `docs/decoder_sensitivity_plan.md`. Those files stay
authoritative for their own detail; this file is the prioritized overview.

**Driving goal:** *work as many Field Day stations as possible in a short amount of
time.* Everything in **Now** serves that. **ARRL Field Day 2026 is June 27–28** — so
"Now" is this-weekend-critical; "Next" is the weeks after; "Later" is the longer arc
(SDR, new modes, sensitivity parity, product polish).

**Severity tags** (text, not colored dots — legible regardless of color vision):
**[BLOCKER]** correctness/blocker · **[SHOULD-FIX]** worth fixing soon · **[POLISH]**
nice-to-have · **[DECISION]** open design decision.

---

## Now — Field Day readiness (the short list that wins contacts)

The mission is throughput: find workable stations across bands/modes fast, work them,
don't double-work them, capture everything, and share the log across operating positions.

### 1. [BLOCKER] Build the real `scanner` crate — *the headline Field Day feature*
The band scanner is the last remaining mock (`mocks::spawn_support`); `qso` and
`logbook` are now real. It is the single biggest lever on "work many stations fast,"
because it's how an operator discovers where the workable, **unworked** Field Day
stations are. Mirror the `mocks::spawn` / `core::spawn` pattern, one topic at a time
(spec: `docs/band_scanner.md`).

**Extend the scan to sweep modes *and* bands (Josh, 2026-06-22).** The base spec only
time-slices bands; for Field Day, scan the mode×band×offset space:

> Switch to mode [FT4, FT8]; for each:
> &nbsp;&nbsp;Switch to band at calling freq, −1000 Hz, and +1000 Hz (maybe ±2000 Hz as well); for each:
> &nbsp;&nbsp;&nbsp;&nbsp;Receive for 5–10 intervals
> &nbsp;&nbsp;&nbsp;&nbsp;Report on all heard stations and all unworked stations, filtered by those doing the Field Day exchange.
> &nbsp;&nbsp;&nbsp;&nbsp;May also need to filter by SNR so we're not chasing −20 dB stations we'll never hit.

Implications to design in:
- **Mode dimension** on top of the band/offset sweep (the scanner today is band-only).
- **Filter to Field Day participants** — stations sending the FD `<class> <section>`
  exchange (the parser already understands this form; `gui/src/settings.rs:145` notes
  the `ContestProfile` UI is still TODO).
- **SNR floor filter** so the report surfaces only workable signals.
- **Unworked-aware**, per band *and* per mode (ties into item 3).

### 2. Decode archive — persist *every* decoded message to a database
Capture every decoded message (not just completed QSOs) into a queryable database. Uses:
- **Data analysis** — band / mode / time / SNR / station trends; propagation; what's
  actually workable right now.
- **Logbook recovery** — rebuild or repair the logbook from raw decode history if a log
  is lost or corrupted (complements the JSON logbook and the planned peer-merge).
- **Whole-QSO view** — reconstruct and view an *entire* QSO (every over from both
  stations), not just the single logged contact row.

Implementation notes:
- Decodes already flow on the bus (`Decode` / `radio/{id}/decodes_enriched`). Add a
  persistence service that subscribes and appends — mirror the `logbook` crate's
  spawn pattern (`core::spawn`).
- Store enough to reconstruct a QSO: UTC, dial + audio frequency, SNR, mode, raw text,
  parsed message, and `origin: Mine | Peer(id)`.
- **SQLite** is the natural store for queryable analysis + recovery (vs. the current
  append-only JSON logbook); it becomes the substrate for the whole-QSO view UI.

### 3. [SHOULD-FIX] Field Day log reset + per-band/per-mode "unworked" tracking
When Field Day starts, one click resets the logbook so everyone is "unworked" again.
"Unworked" is **per band** (and, with item 1, effectively per mode) — the same call on
another band is a new contact. Suggested UX (from `TODO.md`): an unlocked-mode "reset"
button on the Log Book that moves the old log to a new filename. This is what makes the
scanner's "unworked stations" count meaningful during the event.

### 4. [SHOULD-FIX] First on-air FT4 QSO (verify the implemented TX path)
FT4 transmit is built and **sample-identical to the `ft8_lib` reference offline**
(`ft4_cq_1200.wav`, Pearson r = 1.0) but **never keyed on a real radio**. Field Day
wants both modes. Remaining (from `live_pipeline_notes.md`):
- Confirm a first on-air FT4 QSO; watch `core::tx`'s `into_slot_ms` — FT4's DT window is
  tighter than FT8, so verify tones land in tolerance after key + audio-buffer latency.
- [SHOULD-FIX] `rig::actor::PTT_WATCHDOG` is still FT8-sized (15 s = two FT4 slots); make
  it slot-relative (derive from mode).
- [SHOULD-FIX] Promote the FT4 encode-reference check to a committed test so an encode
  regression can't pass silently.

### 5. [SHOULD-FIX] Shared logbook across operating positions (Network Step 2)
Multi-op Field Day means several positions converging one log. Transport/discovery
(Step 1) is **done**; Step 2 (gossip completed contacts → every op's logbook converges)
is "the headline win" of the network plan. See `TODO_NETWORK.md` Step 2 for the full
task list (outbound push, inbound merge, anti-entropy loop, G-set range math, origin
distinction in UI). **Prereqs:** the two-host smoke test (Josh, laptop + Pi) and the
"decisions to settle before Step 2" (wire encoding, `seq` persistence, LAN-trust
security posture).

### 6. [BLOCKER] Spectrogram ↔ decode-text drift
Decode text is placed by wall-clock age; the spectrogram scrolls by accumulated frame
`dt` and ignores `SpectrumRow.t`. They desync over time / on dropped frames. Fix: place
spectrogram columns by their timestamp (resync to wall clock). A blocker because it
undermines the core operating display you stare at all day.

### 7. [SHOULD-FIX] Real-mode band-scan panel + armed-state operating affordances
Quick operating-surface fixes from `TODO.md` that matter mid-contest:
- Make the band-scan panel blank in real mode (until item 1 lands) instead of showing
  mock data.
- Panel corner accents turn blue when armed; better behavior when you select another
  channel while armed; consider auto-arm when selecting a channel's traffic.
- Sent text should be accent2 and is not rendering at the correct vertical height.

---

## Next — operating polish & multi-op depth (weeks after)

### Operating surface (`TODO.md`)
- Map scroll / zoom / reset.
- Map: highlight (crosshairs) the station you're armed to work.
- Unlocked view should show each panel's keyboard shortcut.
- Evaluate color schemes, especially light mode.
- Review whether hashed callsigns are handled correctly.
- **Split audio offset:** double-click to lock the audio offset (click again to unlock);
  determine the real TX audio-offset limits (currently a hard 1000–2000 Hz window);
  figure out how to tell when two stations are working at different offsets.

### QSO sequencing
- **[DECISION] Answering model: "wait-for-CQ" vs "answer-immediately."** Arming to a
  clicked CQ never fires if that station doesn't call CQ again (`qso::engine`
  `commit_from_armed`). WSJT-X answers the clicked CQ immediately. Likely make it a
  toggle, default answer-immediately. A `docs/qso_flow.md` design decision (see
  `joels-notes.md` 2026-06-18). Pairs with the "forgiving late start" TX-on-commit change.
- **Surface what the engine is thinking** in the UI — extend the waterslide `ARMED ▸
  {call}` tag to say *what it's waiting on* (idle / calling / answering / waiting for
  W1ABC's CQ / nothing-heard). The engine already logs these transitions.
- **Multi-caller auto-pick** lands in `qso::engine` (`engine.rs:14`); when it does,
  exclude network-worked and peer-`working` stations (Network Step 3 hook).

### Network sharing (`TODO_NETWORK.md` Steps 3–4)
- **Step 3 — working-intent ("don't compete"):** publish what you're working so peers
  don't double up; consume theirs; flag a peer-worked station in the waterslide + map
  crosshair; auto-pick exclusion.
- **Step 4 — heard-station + band-activity aggregation:** surface what the *whole
  network* is hearing (not just this receiver), aged by local receive time, mine vs.
  peer distinguished. Feeds the map and band-scan panels.

### Decode pipeline robustness (`live_pipeline_notes.md`)
- [SHOULD-FIX] Bounded decode worker (today each slot spawns a fresh thread — no
  backpressure if a decode outlives its slot).
- [SHOULD-FIX] Clean shutdown / restart of capture (needed for device + source switching).
- [SHOULD-FIX] Clock-drift detection/warning (slot alignment leans on NTP; no warning if
  off).
- [SHOULD-FIX] Faithful spectrum stream — drain a per-frame ring placing each column by
  its `t` (also fixes the drift in Now-#6); reference-level / AGC brightness control
  instead of the fixed `COL_DB_FLOOR`/`COL_DB_CEIL` guesses.
- Stamp real band/freq/mode in `build_log` (`qso/shell.rs:286`, currently placeholder
  until `OperatingState` is published).

### Decode sensitivity — finish the gap-closing plan (`docs/decoder_sensitivity_plan.md`)
FT8 is at gap ~35% (matched 606) after the coherent front-end; FT4 coherent handoff is
**done** (gap 30%→10%). Remaining:
- **Phase 3.1** — frequency-refined, more-global subtraction fit (the next masking lever
  for close-spaced signals); re-measure the standalone subtraction delta on top of the
  coherent front-end.
- Make `ab_jt9` measure FT4 (currently FT8-only); capture more corpus across bands/times.
- Profiling pass (coherent FT8 decode is ~3 s/slot).
- Cheap knobs to sweep: `MAX_CANDIDATES` (140→300), `MIN_SCORE`, `TIME_OSR`/`FREQ_OSR`,
  `LDPC_ITERS`.

---

## Later — longer arc & lower-urgency

### Interop & data
- **ADIF import/export** (logbook crate; `OVERVIEW.md §7`, `docs/log_book.md`) — the
  amateur-radio interchange format; pairs with the merged peer log and the decode archive
  (Now-#2).
- **Network wire format** — switch JSON → bincode once the schema settles
  (`TODO_NETWORK.md`).
- UI tunables → real settings (the `ws_history_secs`, `SPECTRUM_HOP_S`, `FFT_SIZE`,
  brightness constants currently require a recompile); broader settings UX beyond station
  identity (config format/persistence is interim/TBD).

### Hardware & mode reach (`OVERVIEW.md §3.5, §6`)
- **Multi-radio-in-one-box** (Field-Day-timed in the network plan): spawn >1 radio id, a
  radio-selector/add-radio config UI, inter-radio PTT interlock. Bus topics already scope
  by `RadioId`; only `rig0` exists today.
- **SDR multi-receiver back-ends** (HPSDR/Hermes-Lite 2, RX-888 + ka9q-radio, FlexRadio):
  true simultaneous multi-band RX. The radio abstraction is meant to advertise
  `simultaneous_receivers` so the scanner/waterslide adapt — don't hard-code the
  one-receiver assumption.
- **New modes:** PSK31 (live-typing TX behavior) and RTTY, exercising the
  waterslide/decoder/mode abstractions.
- **A-priori (AP) decoding** (sensitivity Phase 4) — couples the decoder to `qso` contact
  state; deferred, narrow payoff.

### Polish & nice-to-haves (`TODO.md`, `joels-notes.md`)
- [POLISH] Cache `martian_cmap()` (rebuilt every paint); move `dsp::fft` to `realfft` (the
  decoder FFT already did, ~10× faster).
- [POLISH] WAV-replay path consistency with live (timing, timestamps, no spectrogram today).
- "Radio configuration check" easter egg (`/checkrig`) — read the rig over CAT and print
  a pass/fail setup checklist (`EX` menu reads).
- Shared multi-operator notebook panel (freeform text).
- Matrix-style "scrolling letter" decode-in-progress effect on the waterslide.
- **Pick the product name** — the app is still effectively unnamed beyond "DM420 /
  Dingus Mangler 420" (`OVERVIEW.md §7`).

---

## Resolved (for context — no longer roadmap)

These were `OVERVIEW.md §7` open decisions, now settled: decoder strategy (Rust
`ft8_lib` port, with shelling out to `jt9` as the documented fallback if full parity is
required) · audio/serial crates (`cpal` / `serialport`) · network protocol (mDNS + UDP
gossip, eventual consistency) · map base data (bundled coastline mesh + land-snapping) ·
FFT migration to `realfft` in `modes`.

---

### Source files this consolidates
- `TODO.md` — operating-surface & UI tasks (authoritative detail)
- `TODO_NETWORK.md` — the 4-step LAN sharing plan (authoritative detail)
- `docs/live_pipeline_notes.md` — severity-tagged live-path shortcuts & known issues
- `docs/decoder_sensitivity_plan.md` — the FT8/FT4 decode-gap plan
- `docs/band_scanner.md` — band-scanner spec (extended by Josh's mode-scan note above)
- `OVERVIEW.md §7` — original open design decisions
- `joels-notes.md` — running gotchas & the open QSO-sequencing design call
</content>
