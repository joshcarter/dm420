# Joel's Notes

Running notes, gotchas, and reminders. Newest at the top.

## 2026-06-18

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
