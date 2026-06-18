# Joel's Notes

Running notes, gotchas, and reminders. Newest at the top.

## 2026-06-18

- **Decode/TX timing — optimizations done + planned (the "answered a repeated CQ but
  didn't" thread).** Root cause: decodes land ~0.7 s *after* the slot boundary (the
  decoder waits for the full slot, then ~0.7 s of compute), so the engine learns of a CQ
  too late to answer in the next slot.
  - **DONE — stream decodes as found.** `modes::decode_streaming` (callback per decode,
    strongest-first) + `core::decode` now publishes each onto the bus the instant it's
    found instead of batching the whole slot. The UI ring and QSO engine already consume
    a stream, so messages appear incrementally and the engine sees the strongest signal
    (a CQ being worked) first. No downstream changes.
  - **DONE — trim TX leading silence.** `core::tx` trims the synth's centered ~1.18 s
    lead down to ~0.5 s so our tones start near the top of the slot. We were effectively
    transmitting at DT ≈ +1.4 s (synth centering + ~0.76 s playback-open latency) — near
    the edge of decodability; this pulls it back to a safe ~+0.7 s.
  - **DONE — two-pass decode (early at signal-end + full at boundary).** Confirmed on-air
    that it helps. `core::decode` runs a speculative **early** pass when the signal is done
    (~13.5 s into an FT8 slot, `DECODE_TAIL` knob) so on-time stations — incl. a CQ being
    worked — reach the UI + engine *before* the boundary, plus the existing **full** pass at
    the boundary that catches late-DT / weak signals (deduped by message text per slot).
    Degrades to today's behavior if the early pass finds nothing; the dedup + trigger
    generalize to N passes for free (just add trigger points).
  - **IDEA (validated, not built) — "continuous" decoding.** Drop slot alignment for RX:
    a thread re-decodes the rolling last ~15 s every N ms (TOML knob, `decode_interval_ms`),
    sleeps, repeats — each station pops up ~one cycle (~0.8 s) after *it* finishes, uniform
    for early and late starters. **Feasible:** `find_candidates` already searches the whole
    slot (`time_offset −10..~+25` blocks ≈ −1.6..+4 s; comment: "spans the whole slot"), so
    a signal anywhere in the buffer is found. Cost ≈ one core (a single decode ~0.7–1.2 s
    keeps up with a ~100 ms-sleep loop) — acceptable. **The real work isn't CPU, it's two
    bits of bookkeeping:** (1) **time-bounded dedup** — each signal is caught ~3× as the
    window slides, so dedup by message text but *windowed* (~1 slot), else a station's
    repeat CQ next cycle is wrongly suppressed; (2) **slot recovery** — the decoder returns
    DT relative to the rolling buffer, but the engine still needs the real slot+parity to
    time TX, so recompute `slot = floor((buf_start + dt)/period)`. Buffer stays ~one slot
    (the waterfall is sized for one). Build behind the TOML flag to A/B vs two-pass.
  - **NOT WORTH IT — N full decodes every 200 ms (~16/slot).** ~16× redundant (each pass
    rebuilds the whole FFT waterfall + re-decodes already-found signals, ~90 % thrown away
    by dedup) for ~10–15 s of CPU/slot, just to show *already-late* stations a fraction
    sooner. A *few* passes (3–4) is the cheap middle ground; the efficient version is an
    **incremental decoder** (reuse the waterfall, decode only newly-completed candidates) —
    the real "every 200 ms" without the waste, and it pairs with multi-pass subtraction.
  - **HELD — forgiving late start (fire the answer mid-slot on commit).** The direct fix
    for a CQ whose decode lands just after the boundary, but it means emitting TX off a
    `Decode` event, not just the slot tick — intricate surgery on the 12-test QSO state
    machine (`next`/`tx_parity`/`step`/`finish`), and it overlaps the **wait-for-CQ-vs-
    immediate answer-model decision flagged for Josh**. Do it *with* that decision.
  - **HELD — pre-open the TX output stream.** The log shows ~0.5 s from key-up to
    `playback started` — cpal reopens the output device every over. Keeping it open across
    overs cuts that ~0.5 s (and loosens the lead-trim budget). Separate audio-lifecycle
    change.
  - **FUTURE — multi-pass subtraction decode.** Ours is single-pass; WSJT-X decodes
    strong → subtracts → re-decodes to dig out weak/colliding signals (and that, not
    "signals finishing sooner," is why its messages appear spread over ~1 s). Bigger
    feature; finds *more* signals, not just sooner — worth it for weak-signal/crowded
    bands.

- **🟠 OPEN DESIGN DECISION (Josh N0JDC + Joel W4LL need to talk) — the answering model:
  "wait-for-CQ" vs answer-immediately.** Diagnosed from `dm420.log`: arming to answer a
  clicked CQ **never fires** if that station doesn't call CQ *again*. DM420's engine
  (`qso::engine` `commit_from_armed`) only replies on the target's **next** CQ — so when
  you click a CQ and that station then gets worked by someone else, the engine sits
  `Armed` and never transmits ("armed but nothing fired"). WSJT-X instead **answers the
  CQ you clicked immediately** (next slot). The keying/TX path itself is fine — this is
  purely the sequencing model.
  - **Decide together:** this is a `docs/qso_flow.md` design call (Josh co-owns it).
    **Maybe make it a toggle** — "answer immediately" (reply to the selected CQ next slot,
    using its slot for TX parity) vs the current "wait-for-CQ" (wait for them to call
    again). Likely default = answer-immediately, since that's what "answer this CQ" means
    to most operators; keep wait-for-CQ as the option for arming a not-currently-calling
    station.
  - **Also wanted: user feedback about what the engine is *thinking*.** An armed-but-
    waiting engine is silent on screen, so it just looks broken. Surface the engine's
    state/intent in the UI — e.g. extend the waterslide's `ARMED ▸ {call}` tag to say
    *what it's waiting on* ("waiting for W1ABC's CQ"), and cover idle / calling /
    answering / nothing-heard. The engine now **logs** these transitions (`qso engine:
    armed — will answer when the target next calls CQ`, `… target called CQ → answering`,
    `calling CQ`, `abort → idle`) — the UI should mirror that thinking.
  - **Status:** no behavior change yet (per Joel) — talk to Josh first. The diagnosis and
    the engine-state logging (`qso/src/engine.rs`) are in place to inform the conversation.

- **🔴 IMPORTANT — EMI/RFI crashed the rig's USB on transmit (FIXED, station-side):**
  this was the real TX blocker. With keying + data-route audio all correct, the **entire
  TS-590 USB connection dropped out the instant RF actually flowed** — the audio codec
  (TX *and* RX) **and** the CAT serial port all died together — then re-enumerated a few
  seconds after TX stopped. **Root cause: RF feedback (common-mode RFI) crashing the USB
  device.** It is *not* a DM420 bug: the CAT **serial** dying (`Broken pipe`) proves the
  whole physical USB device dropped, not any audio/cpal contention. **Resolved with
  station-side RFI mitigation** (common-mode/ferrite choking of the RF path).
  - **Recognize it instantly in `dm420.log`:** three near-simultaneous (~0.1 s) lines —
    `core::rig_adapter: rig poll failed … Broken pipe` + `audio::player: … output stream
    error (device dropout?)` + `audio::recorder: audio input stream error` (all *"device
    no longer available"*) — landing **~0.2–0.5 s after the FT8 tones start**, i.e. the
    moment RF power flows. (The synth's ~1.18 s of leading silence makes no RF, so it
    survives keying and dies on modulation.) The device re-enumerates seconds later and
    DM420 auto-reconnects (rig autodetect + capture re-open).
  - **If it ever recurs:** more/better common-mode chokes (type 31 mix, rig end) on the
    **USB cable and the coax/feedline**; check SWR + feedline common-mode current; add a
    USB galvanic isolator or powered hub; drop power to confirm the dose-response; the
    decisive split-test is a **dummy load** (no drop ⇒ feedline RFI, choke the coax; still
    drops ⇒ RF getting in at the rig/USB).

- **Real logging to `dm420.log` (done; standard tracing levels):** the app installs a
  `tracing` subscriber (`gui/src/logging.rs`) that writes **`dm420.log`** in the launch
  dir, **appended across runs** (each run opens with a timestamped `DM420 starting`
  line, so sessions are easy to tell apart). Levels are the
  standard TRACE/DEBUG/INFO/WARN/ERROR. **Default is INFO; your `dm420.toml` is set to
  `[logging] level = "debug"`.** Third-party crates (egui/winit/wgpu/tokio/cpal) are
  pinned at `warn` so the log stays readable; only DM420's crates follow the level.
  `RUST_LOG` overrides everything when set (e.g. `RUST_LOG=core::tx=debug,info`).
  - **TX path is DEBUG-instrumented** end to end — qso (starting over / token acquired) →
    interlock (grant/release/deny) → core::tx (begin/synth/key-up/PTT refresh/key-down/
    abort, plus an INFO/WARN per over) → rig (`set_ptt` TX1/RX, key-up deny) → audio
    (output device opened, playback started, stream-error on a dropout). ~12 lines/over.
    Routine rig CAT byte chatter (the 0.5 s `IF` poll — send/rx/reply) sits at **`trace`**
    so `debug` stays readable; `RUST_LOG=rig::channel=trace` brings it back when needed.
  - **INFO seams:** app start + version, producer mode (real/mock) + audio devices,
    `core: launching producers`, `qso: engine spawned`. Every old `eprintln!` diagnostic
    is gone, replaced by a real log record at the right level.
  - **To use:** run as usual; `dm420.log` appears next to `dm420.toml`. Hand it to me and
    I can read the whole session — at `debug` (which yours is) the TX dropout will show as
    an `audio-tx: output stream error (device dropout?)` line with the device name.

- **Idea: a "radio configuration check" easter egg.** The mic-vs-USB saga below would
  have been a 5-second diagnosis if DM420 could *audit the rig's setup* and tell the
  operator exactly what's wrong. Proposal: a hidden self-test (an easter egg — e.g. a
  key combo or a `/checkrig` slash command) that reads the rig over CAT and prints a
  pass/fail checklist with the precise fix for each miss, e.g. *"Menu 63 = ACC2 → set to
  USB"*, *"not in DATA mode"*, *"USB input level (Menu 64) = 0"*.
  - **What's already readable:** mode + TX state from `IF`, mode from `MD`, the data-mode
    flag from `DA` (the driver issues these today). Menu items (63/64, and the SG's
    69/70/71) would need the **`EX` menu-read** CAT command — exact byte format TBD from
    the TS-590S PC Command Reference; it's a *read*, so the check stays non-destructive.
  - **Why "easter egg":** keep it playful and out of the main flow — a self-test the
    operator can invoke when TX looks wrong, not a nag. Model-specific menu numbers (S vs
    SG) mean the check needs a small per-model table, or it can just report the raw
    `EX` values and the expected one.

- **Rig transmitted from the MIC, not the USB audio — root cause + fix (done; needs on-air test):**
  the rig kept up but modulated the front mic even in USB-DATA mode, and DM420's own
  diagnostic confirmed it *was* playing FT8 to the "USB Audio CODEC" output — so the
  audio path was fine and the fault was the **keying command**. DM420's CAT driver keyed
  with bare `TX` (= `TX0`), which is the **mic SEND** route. Per the TS-590S USB Audio
  manual §4.2: *"the [SEND] keys... are the method for transmitting audio input to the
  microphone, so even if these operations are implemented, audio entered as audio
  signals from USB cannot be transmitted."* USB audio transmits only via **DATA SEND** —
  whose CAT equivalent is **`TX1`**. Fix: `catrig::set_ptt` now keys with `TX1` (rear/data
  route); key-down stays `RX`. The mock keys off the first two chars so it's unaffected.
  - **Rig menus still required (one-time, TS-590S):** **Menu 63 = USB** ("audio input
    line selection for data"; default is **ACC2**, so USB modulation is silent until you
    change it) and **Menu 64** ("audio level of USB input for data", 0–9) non-zero. RX
    decode over USB works regardless of Menu 63, so it can still be on ACC2 — set it.
    (On the **TS-590SG** the same two are **Menu 69** and **Menu 71**; the SG also has a
    Menu 70 FRONT/REAR, but `TX1`/DATA SEND bypasses it.)
  - **Maybe next:** have DM420 set the rig to USB-DATA itself (it currently never sends
    `MD`/`DA` and drops the data-mode flag in `rig_adapter::apply`), so you don't set the
    mode by hand. Menu 63/64 are rig-config, not CAT-settable in the normal command set.

- **Transmit Steps 1 + 2 — auto-sequenced TX behind an opt-in flag (done; needs on-air test):**
  the PTT interlock granter (single-holder token + TTL) and the full audio-TX path
  (synthesize FT8 → key → play → re-key inside the 10 s watchdog → key down →
  `tx_report`) are wired, and the QSO engine now auto-sends each over. **TX is OFF
  by default** — the binary stays RX-only until you opt in.
  - **To test (safety first — into a dummy load, QRP ~5 W, set call/grid first):**
    `DM420_REAL=1 DM420_ALLOW_TX=1 DM420_AUDIO_OUTPUT="<rig data-in>" ./target/debug/dm420`.
    Click a CQ + Enter to answer, or empty spectrum + Enter to call CQ; the rig
    should key, send FT8, and a second decoder (WSJT-X) should copy it. Watch the
    TX offset and slot timing (the synth sits ~1.18 s into the slot — within decode
    tolerance, but verify).
  - **Safety is layered:** rig-actor TX gate (`allow_transmit`) + 10 s PTT watchdog +
    single-holder interlock token. Leave `DM420_ALLOW_TX` unset for normal RX use.
  - **Still ahead:** Step 3 (feed `tx_report` back so the engine reacts to TX
    denials/failures), Step 4 (real TX offset window + `/f` retune in real mode),
    and FT4 TX synthesis (FT8 only today).

- **Station call + grid: no default; TOML config, set via file or UI, persisted — done:**
  no built-in default — a silent one risks transmitting as the wrong station (the old
  `N0JDC` / `DN70KA` fallbacks are gone). Implemented: identity resolves
  `DM420_CALLSIGN` / `DM420_GRID` env → `dm420.toml` (`[station]` table) → unset; with
  nothing set the app **boots unlocked to prompt**; operating (CQ/answer) is blocked
  until a call is set; editing call/grid in the unlocked top bar **writes `dm420.toml`
  on re-lock, preserving comments**. (`dm420.example.toml` is the committed template;
  `dm420.toml` is gitignored; env still overrides the file.) **Still TBD (UX owner):**
  the config format/location may change, a real `toml_edit` swap is the clean upgrade
  once config grows past call/grid, and the broader settings UX (everything beyond
  station identity) is open.

- **Signal strength calibrated to the noise floor — done (522fa46):** replaced the
  `score / 2` POC placeholder with a real noise-relative SNR — per-slot noise floor
  (median of the waterfall magnitudes, since signals are sparse) vs. signal power at
  the Costas sync tones, corrected from the per-bin bandwidth to a 2500 Hz reference.
  It is gain-independent (a power ratio), so a signal at the decode limit reads near
  −21 dB regardless of input level. The waterslide also stopped dimming weak decodes
  (2d31eb0). Target scale (standard FT8 reports), kept for reference:

  | Report | What it means |
  |---|---|
  | −24 or below | Extremely weak; at or near FT8's decoding limit |
  | −15 to −20 | Weak but solid copy; impressive propagation |
  | −10 to −14 | Moderate signal |
  | −5 to 0 | Good signal |
  | +1 and above | Strong signal; well above noise |

- **Wrong audio source while decoding:** apparently I was decoding off the
  **MacBook's built-in microphone**, not the rig's **audio input device**. Set
  `DM420_AUDIO_INPUT` (case-insensitive substring, e.g. `USB PnP`) so capture
  binds to the right device — or pick it in the unlocked FT8 panel's Radio Setup.
  - **Fixed:** selected the correct audio input device and the decode looks a
    lot stronger now — way more decodes coming through.
