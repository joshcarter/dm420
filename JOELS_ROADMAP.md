# Joel's Roadmap — now / next / later

**Joel's working roadmap** (kept separate from Josh's workflow). This consolidates the
explicit roadmap items scattered across `TODO.md`, `TODO_NETWORK.md`, `OVERVIEW.md §7`,
`docs/live_pipeline_notes.md`, and `docs/decoder_sensitivity_plan.md`. Those files stay
authoritative for their own detail; this file is the prioritized overview.

**Driving goal:** *work as many Field Day stations as possible in a short amount of
time.* Everything in **Now** serves that. **ARRL Field Day 2026 is June 27–28** — so
"Now" is this-weekend-critical; "Next" is the weeks after; "Later" is the longer arc
(SDR, new modes, sensitivity parity, product polish).

Items live in **Now / Next / Later** and nothing else — the bucket *is* the priority.
Where something is unusually critical, that's called out in prose, not with a tag.

---

## Now — Field Day readiness (the short list that wins contacts)

The mission is throughput: find workable stations across bands/modes fast, work them,
don't double-work them, capture everything, and share the log across operating positions.

### 1. Band scanner — *the headline Field Day feature* (base **shipped**; enhancements next)
**Shipped.** The `scanner` crate (pure sweep engine) + the `core::scan` shell now
time-slice the receiver across 40/20/15/10 in **both FT8 and FT4** (mode-major, to minimize
capture restarts), dwelling **2 slots per stop** (even/odd parity) and **looping until
cancelled** — blocking TX for the sweep (holds the interlock) and restoring the operator's
band+mode on cancel. Per-band heard/unworked counts come from live decodes cross-referenced
against the logbook. Wired into `core::spawn`; it was the last real-mode mock. Spec:
`docs/band_scanner.md`. It is the single biggest lever on "work many stations fast,"
because it's how an operator discovers where the workable, **unworked** Field Day stations
are.

**Remaining enhancements — sweep the mode×band×*offset* space (Josh, 2026-06-22).** The
shipped sweep covers mode×band at each band's calling frequency; extend it for Field Day:

> Switch to mode [FT4, FT8]; for each:
> &nbsp;&nbsp;Switch to band at calling freq, −1000 Hz, and +1000 Hz (maybe ±2000 Hz as well); for each:
> &nbsp;&nbsp;&nbsp;&nbsp;Receive for 5–10 intervals
> &nbsp;&nbsp;&nbsp;&nbsp;Report on all heard stations and all unworked stations, filtered by those doing the Field Day exchange.
> &nbsp;&nbsp;&nbsp;&nbsp;May also need to filter by SNR so we're not chasing −20 dB stations we'll never hit.

Still to design in (done so far: **mode×band sweep, 2-slot dwell, loop, per-band
FT8/FT4 toggles, and cumulative per-(band,mode) heard/cq/unworked counts**):
- **Per-offset sweep** — also dwell at the calling freq −1000/+1000 Hz (maybe ±2000) to
  cover more than one ~3 kHz passband per band. Each offset stop needs its own ≥2 slots.
- **Filter to Field Day participants** — stations sending the FD `<class> <section>`
  exchange (the parser already understands this form; `gui/src/settings.rs:145` notes
  the `ContestProfile` UI is still TODO).
- **SNR floor filter** so the report surfaces only workable signals.
- **Configurable interval count** — Josh's note wants 5–10 intervals; the shipped dwell
  is fixed at 2 (the parity minimum). Surface it in the panel.

### 2. Decode archive — persist *every* heard and sent message
**Raw capture is shipped** — the `archive` crate. When `[archive] decodes` names a file
(off by default), DM420 appends one JSONL row per heard **and** sent FT8/FT4 message: UTC
capture time, dial freq + audio offset, SNR, mode, the parsed message **and** raw decoder text, direction, and
(for sent overs) the TX outcome. Not grouped by QSO — raw data for offline analysis.
Heard rides `radio/{id}/decodes`; sent rides the new `radio/{id}/tx_log` (published by
`core::tx`, deliberately off the `decodes` topic the live QSO engine consumes).

Still to build *on top of* that raw firehose:
- **Data analysis** — band / mode / time / SNR / station trends; propagation; what's
  actually workable right now.
- **Logbook recovery** — rebuild or repair the logbook from raw decode history if a log
  is lost or corrupted (complements the JSON logbook and the planned peer-merge).
- **Whole-QSO view** — reconstruct and view an *entire* QSO (every over from both
  stations), not just the single logged contact row.
- **Queryable store** — **SQLite** is the natural step up from the append-only JSONL for
  queryable analysis + recovery, and the substrate for the whole-QSO view UI.
- **`origin: Mine | Peer(id)`** per row — not stamped yet (the archive is local-only
  today); add it once peer decodes are gossiped (Network Step 4).

### 3. Field Day log reset + per-band/per-mode "unworked" tracking
When Field Day starts, one click resets the logbook so everyone is "unworked" again.
"Unworked" is **per band** (and, with item 1, effectively per mode) — the same call on
another band is a new contact. Suggested UX (from `TODO.md`): an unlocked-mode "reset"
button on the Log Book that moves the old log to a new filename. This is what makes the
scanner's "unworked stations" count meaningful during the event.

### 4. First on-air FT4 QSO (verify the implemented TX path)
FT4 transmit is built and **sample-identical to the `ft8_lib` reference offline**
(`ft4_cq_1200.wav`, Pearson r = 1.0) but **never keyed on a real radio**. Field Day
wants both modes. Remaining (from `live_pipeline_notes.md`):
- Confirm a first on-air FT4 QSO; watch `core::tx`'s `into_slot_ms` — FT4's DT window is
  tighter than FT8, so verify tones land in tolerance after key + audio-buffer latency.
- `rig::actor::PTT_WATCHDOG` is still FT8-sized (15 s = two FT4 slots); make
  it slot-relative (derive from mode).
- Promote the FT4 encode-reference check to a committed test so an encode
  regression can't pass silently.

### 5. Shared logbook across operating positions (Network Step 2)
Multi-op Field Day means several positions converging one log. Transport/discovery
(Step 1) is **done**; Step 2 (gossip completed contacts → every op's logbook converges)
is "the headline win" of the network plan. See `TODO_NETWORK.md` Step 2 for the full
task list (outbound push, inbound merge, anti-entropy loop, G-set range math, origin
distinction in UI). **Prereqs:** the two-host smoke test (Josh, laptop + Pi) and the
"decisions to settle before Step 2" (wire encoding, `seq` persistence, LAN-trust
security posture).

### 6. Spectrogram ↔ decode-text drift
Decode text is placed by wall-clock age; the spectrogram scrolls by accumulated frame
`dt` and ignores `SpectrumRow.t`. They desync over time / on dropped frames. Fix: place
spectrogram columns by their timestamp (resync to wall clock). **Particularly critical:**
it undermines the core operating display you stare at all day.

### 7. Real-mode band-scan panel + armed-state operating affordances
Quick operating-surface fixes from `TODO.md` that matter mid-contest:
- Make the band-scan panel blank in real mode (until item 1 lands) instead of showing
  mock data.
- Panel corner accents turn blue when armed; better behavior when you select another
  channel while armed; consider auto-arm when selecting a channel's traffic.
- Sent text should be accent2 and is not rendering at the correct vertical height.

### 8. Waterfall pauses when DM420 is tabbed away (likely a bug)
When the app loses focus / is in the background, the waterslide appears to stop
advancing; it resumes on refocus. Almost certainly **not** intended — you want to keep
watching the band while another window is on top. Likely a render-throttle issue, not a
data-loss one: decode/RX runs off-thread on the bus, so the *pipeline* keeps going; it's
the egui repaint that stalls when the window is unfocused/occluded (eframe only redraws
on events or an explicit repaint request). The waterfall panel already asks for ~30 fps
via `request_repaint_after(33ms)` (`gui/src/panels/waterfall.rs:736`) and bus pumps call
`request_repaint()` on new data (`gui/src/bus_view.rs`), but winit/macOS may not honor
those for a backgrounded window. Investigate: confirm whether decodes/timestamps keep
flowing while tabbed away (data fine, display frozen) vs. the pipeline itself pausing;
then decide whether to drive continuous repaint while unfocused (and whether the
spectrogram correctly catches up on refocus — ties into the #6 drift fix).

### 9. Investigate QSO correctness
QSOs may be "a bit off" — needs closer observation on-air before we can pin it down.
This is an **investigation** item, not yet a defined fix. Watch the auto-sequencer
(`qso` engine/shell) against real exchanges and capture concrete symptoms (wrong/over-
repeated overs, mistimed TX, premature or missed RR73/73, bad exchange parsing, logging
the wrong call/report). Cross-check against `docs/qso_flow.md`,
`docs/wsjtx_qso_sequencing.md`, and the known shortcuts in `docs/live_pipeline_notes.md`
(e.g. `build_log` still stamps placeholder band/freq/mode — `qso/shell.rs:286`). Promote
each confirmed symptom into its own concrete fix item here as it's nailed down. Related:
the "wait-for-CQ vs answer-immediately" design call in **Next**.

### 10. Jump to the optimum CQ calling frequency (clear-lane finder)
When you start a CQ (especially when *running* a frequency for Field Day throughput),
move to the **audio offset** where you'll be heard cleanest and least likely to collide,
instead of landing on the default ~1500 Hz pileup. **Opinionated, not advisory:** it
doesn't present choices — one action just jumps your TX offset to the single best lane
(briefly flash where it moved you so it isn't jarring). It sets the **outgoing TX offset
only, never the dial** (CLAUDE.md guardrail — keep dial/center distinct from TX offset),
scoped to the *current* band/mode. The raw material already flows on the bus:
`SpectrumRow` energy per FFT bin and the placed `Decode` centers.

Score each candidate lane on:
- **Clearness over time** — integrate per-bin energy across the last N slots via a
  decaying histogram, not just the latest slot (a lane between someone's overs looks
  falsely empty for one slot). Lowest sustained energy wins.
- **Mode-width clear lane** — the empty lane must fit the mode plus a guard: ~50 Hz for
  FT8 (8 × 6.25 Hz), ~90 Hz for FT4 (4 × ~20.8 Hz). Require clearance on *both* sides so
  a neighbor's skirts don't clip you.
- **Margin from active decoders** — stay clear of the audio centers of currently-decoded
  signals (a station there may answer or come back); penalize proximity.
- **Bias toward passband center** — the rig's SSB filter/TX audio is flattest mid-band
  (~1300–1800 Hz) and rolls off at the skirts; below ~300 Hz / above ~2700 Hz part of the
  50–90 Hz signal falls outside a ~2.8–3.0 kHz filter and is attenuated on TX *and* RX.
  This one term does triple duty: it's the "FT8/FT4 performs best here" weighting, it
  keeps you in the window listeners actually watch (so the heard-vs-clear tension mostly
  resolves itself), and it naturally keeps you within the TX-offset limits. Still
  **hard-clamp** to the real limits as a safety — today a 1000–2000 Hz window (see the
  split-audio item in **Next**), which the picker must respect, so settle those limits as
  part of this.
- **Birdie / carrier rejection** — exclude bins with persistent narrowband energy that
  never produces a decode (rig spurs, power-line carriers, the DC region).

The shared piece — an **occupancy map**:
- A short-term, **in-memory** structure (the decaying per-bin energy histogram above),
  continuously updated as we keep receiving `SpectrumRow`s — a live picture of "what's
  busy right now," not a log. Natural to expose as a bus snapshot (a State topic) so the
  lane finder, the `scanner` (#1), and a future band-activity view all read the same map
  instead of each rebuilding it. Same `SpectrumRow.t` timestamping discipline as the
  drift fix (#6).
- **Kept separate from the decode archive (#2) on purpose** — the archive is long-running
  and persistent (append-only JSONL today, SQLite later for query/recovery); the occupancy map is short-term and ephemeral.
  Folding them together would mix concerns; they merely both read the decode/spectrum
  streams.

Other notes:
- **Auto-CQ hook** — let the `qso` auto-sequencer invoke the picker when it starts an
  unattended CQ run; exclude peer-`working` offsets later (Network Step 3).
- **Deliberately out of scope (for now):** no multi-option suggestion (be opinionated);
  no re-evaluate-while-calling / auto-hop if you get stepped on mid-run; no per-region
  noise-floor estimate (the floor is ~uniform across the passband, so it wouldn't change
  the pick).
- **Cross-band / cross-mode is *not* here** — switching bands, toggling FT4↔FT8, and
  moving to those calling frequencies belongs to the strategic "where to call CQ" advisor
  in **Later**, which folds in the logbook and band scanner. This item stays scoped to the
  audio offset on the current band/mode.

### 11. CQ answered with a *report* (not a grid) is ignored — *particularly critical*
**A station that answers our CQ with a signal report instead of a grid is dropped — we
just keep calling CQ.** Costs contacts directly; common on FT8 (and in contest/POTA
styles), so it's a real Field Day liability. **Not fixed on this branch on purpose**
(keeps `lane-finder` to the QSY work); a fix + regression test were drafted and reverted.
Do it on `main`.

**How we found it (on air, 2026-06-22).** W4LL was calling CQ on a fixed offset (2460 Hz,
auto-QSY off). KC8PFF answered, and the engine called CQ again instead of replying — from
`dm420.log`:

| time | slot | event |
|---|---|---|
| 13:37:00 | 028 | we TX `CQ W4LL EM74` |
| 13:37:28 | 029 | decode `W4LL KC8PFF +00` — KC8PFF answering us **with a report** |
| 13:37:30 | 030 | we TX `CQ W4LL EM74` again ❌ (should be `KC8PFF W4LL …`) |
| 13:37:58 | 031 | decode `W4LL KC8PFF +00` — KC8PFF repeats |
| 13:38:00 | 032 | we TX `CQ W4LL EM74` again ❌ |

It is **not** auto-QSY and **not** a decode/tick race: auto-QSY was off, the offset never
moved, and the reply was decoded ~28 s before the next CQ went out. The engine simply
didn't recognise the message as an answer.

**Root cause.** `crates/qso/src/engine.rs` → `commit_from_cq` (the "we're calling CQ, did
someone answer?" handler) only matches two openers: a **grid** (`ExchangePayload::Grid`,
Standard) and the **Field Day exchange** (`ExchangePayload::FieldDay`). A **report**
opener (`ExchangePayload::Report`, e.g. `+00`) hits the catch-all `_ => None`, so the
engine stays in `State::Calling` and `tick_calling` re-sends CQ. Answering a CQ with a
report (skipping the grid) is legal FT8 — WSJT-X jumps straight to Tx3 (the roger-report)
when it happens.

**How to fix.** Add a Report-opener arm to `commit_from_cq`, right after the Grid arm
(Standard mode — guard `to == &self.me.call && !self.me.is_field_day()`). Commit to
`State::Active` as `Role::CallingCq` and reply with the **roger-report (Tx3)** — roger
their report of us and send ours — exactly mirroring the existing Grid arm but skipping
the grid step:

```rust
// Standard: a caller skipped the grid and answered our CQ straight with a
// signal report (e.g. "W4LL KC8PFF +00"). Like WSJT-X, jump to the roger-report
// (Tx3). `r` is their report of us; `snr` is our report of them.
ParsedMessage::Exchange { to, from, payload: ExchangePayload::Report(r) }
    if to == &self.me.call && !self.me.is_field_day() =>
{
    let reply = message::roger_report(&self.me, from, snr); // "<from> <me> R<snr>"
    self.state = State::Active(Box::new(Active {
        role: Role::CallingCq,
        partner: from.clone(),
        target: None,
        offset,
        tx_parity: parity,
        next: Some(reply),
        finish_after_tx: None,
        log_on_tx: false,
        logged: false,
        step: 2,
        partner_grid: None,
        partner_snr: snr,        // our report of them → log's exchange_sent
        rcvd_report: Some(*r),   // their report of us → log's exchange_rcvd
        rcvd_fd: None,
    }));
    None
}
```

No other code needs to change: after we send the roger-report, the partner's `RR73`/`73`
is already handled by the existing `(Role::CallingCq, _, Signoff)` arm in `advance_active`,
which logs the contact (`completed()`) and calls `resume_cq()`.

**Regression test to add** (`engine.rs` tests, mirroring `standard_calling_cq_full_flow`):
call CQ → feed a decode `exch(ME, HIM, ExchangePayload::Report(0))` at snr −8 → assert we
go `InExchange` and the next TX is `K1ABC W9XYZ R-08` (Tx3) → feed their `Signoff::Rr73`
→ assert we log (`exchange_sent == "-08"`, `exchange_rcvd == "+00"`) and return to
`Calling`. Suggested name: `calling_cq_caller_answers_with_report_not_grid`.

**Edge cases to decide while fixing** (see `docs/qso_flow.md`, `docs/wsjtx_qso_sequencing.md`):
- **R-report opener.** A caller answering straight with `R+00`
  (`ExchangePayload::RogerReport`) still falls through the same way. Consider committing on
  it too (reply `RR73`, log on send) for maximum forgiveness.
- **Field Day mode.** This fix is Standard-only (matches the Grid arm). A plain-report
  opener while *we* are in Field Day is a separate, rarer gap — left out here.

### 12. Send box doesn't update live during a QSO — only after the over finishes
**Symptom (observed on air).** During a contact the bottom **Send box** doesn't refresh to
show the current/next message until *after* the transmission completes. While keyed you
can't see what's actually going out or what's queued to go next — you want eyes on both.

**Where.** `crates/gui/src/panels/waterfall.rs` → `draw_send_row`, the `tx_hold` / `next_tx`
/ `display` block (the part that latches `tx_hold` for the whole over, then picks the box
text with `self.tx_hold.clone().or(next_tx)`).

**Likely cause** (confirm with a live repro). On the first frame of an over the box latches
the on-air message into `self.tx_hold` and holds it for the entire ~13 s over (until
`tx_spectrum` columns stop arriving). The `display` precedence prefers `tx_hold` **over**
the live `next_tx`, so while keyed the box is pinned to the latched text and ignores any
newer `next_tx` the engine has already queued (e.g. after a decode advanced the contact).
It only catches up once the over ends and `tx_hold` clears — hence "updates only after the
transmission is sent." (Between overs, when not keyed, `tx_hold` is `None` and the box does
track `next_tx` live.)

**What we want.** Always surface *what's going out now* and *what's queued next*. Options:
- Show the live `next_tx` as the primary text, and mark "on air" with the existing state
  tint / a small indicator while keyed — rather than pinning the box text to `tx_hold`.
- Or split the box: `Sending: <on-air>` while keyed **plus** `Next: <next_tx>`, so both
  are visible at once.

**Watch out for.** `tx_hold` exists for a real reason — the engine can step to idle (the
final `73`/`RR73`) before the own-TX waterfall column reaches the GUI, so without the latch
the just-sent text would vanish mid-over. Any fix must keep the on-air message visible for
the whole over *while also* showing the queued next message (see the `tx_hold` comment in
`draw_send_row`). Not safety-critical, but a daily-driver clarity bug — you fly blind on TX
content mid-QSO.

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
- **Answering model: "wait-for-CQ" vs "answer-immediately" (open design decision).** Arming to a
  clicked CQ never fires if that station doesn't call CQ again (`qso::engine`
  `commit_from_armed`). WSJT-X answers the clicked CQ immediately. Likely make it a
  toggle, default answer-immediately. A `docs/qso_flow.md` design decision (see
  `joels-notes.md` 2026-06-18). Pairs with the "forgiving late start" TX-on-commit change.
- **Surface what the engine is thinking** in the UI — extend the waterslide `ARMED ▸
  {call}` tag to say *what it's waiting on* (idle / calling / answering / waiting for
  W1ABC's CQ / nothing-heard). The engine already logs these transitions.
- **Multi-caller auto-pick** lands in `qso::engine` (`engine.rs:14`); when it does,
  exclude network-worked and peer-`working` stations (Network Step 3 hook).
- **Retry/timeout limit for a stuck contact (Josh + Joel design call).** Once committed
  to a partner (`State::Active`), the engine repeats its over every TX slot *forever* if
  the partner stops responding — we just sit there retrying. Cap the retries: after N
  unanswered overs, give up and fall back (`Finish::ResumeCq` if we were running, else
  `Idle`) — the `QsoPhase::TimedOut` variant already exists for this but isn't wired.
  Open questions: N (likely 3–5, mode-aware), whether to resume CQ vs. idle, and whether
  to surface the timeout in the UI. Pairs with the auto-QSY CQ logic (same "no response"
  detection, different state — Active vs. Calling).

### Network sharing (`TODO_NETWORK.md` Steps 3–4)
- **Step 3 — working-intent ("don't compete"):** publish what you're working so peers
  don't double up; consume theirs; flag a peer-worked station in the waterslide + map
  crosshair; auto-pick exclusion.
- **Step 4 — heard-station + band-activity aggregation:** surface what the *whole
  network* is hearing (not just this receiver), aged by local receive time, mine vs.
  peer distinguished. Feeds the map and band-scan panels.

### Decode pipeline robustness (`live_pipeline_notes.md`)
- Bounded decode worker (today each slot spawns a fresh thread — no
  backpressure if a decode outlives its slot).
- Clean shutdown / restart of capture (needed for device + source switching).
- Clock-drift detection/warning (slot alignment leans on NTP; no warning if
  off).
- Faithful spectrum stream — drain a per-frame ring placing each column by
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

### Strategic "where to call CQ" advisor
A higher-level decision aid that extends the clear-lane finder (Now-#10) from "best audio
offset on the current band" to "**which band and mode** to call CQ on — and the main
calling frequency there — to maximize Field Day contacts." Folds in multiple signals:
the **logbook** (which bands/modes still hold many unworked stations vs. where you're
saturated), the **band scanner** (#1 — where workable, unworked FD stations are actually
being heard right now, with SNR), recent **occupancy** (Now-#10's map), and
propagation / time-of-day. The lane finder becomes the final step once the advisor has
chosen band+mode: switch band, toggle FT4/FT8, jump to the calling frequency, then pick
the clear audio lane. Strategic and data-hungry, so it lands after the scanner, decode
archive, and shared logbook exist.

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
- Cache `martian_cmap()` (rebuilt every paint); move `dsp::fft` to `realfft` (the
  decoder FFT already did, ~10× faster).
- WAV-replay path consistency with live (timing, timestamps, no spectrogram today).
- "Radio configuration check" easter egg (`/checkrig`) — read the rig over CAT and print
  a pass/fail setup checklist (`EX` menu reads).
- Shared multi-operator notebook panel (freeform text).
- Matrix-style "scrolling letter" decode-in-progress effect on the waterslide.
- **Pick the product name** — the app is still effectively unnamed beyond "DM420 /
  Dingus Mangler 420" (`OVERVIEW.md §7`).

---

## Resolved (for context — no longer roadmap)

**Shipped from the Now list:**
- **Unkey the transmitter on app close** (was Now-#11, *safety-critical*) — done in `d1abdf3`
  (`main`, cherry-picked onto `band-scanner`). Both quit paths now drop the rig's PTT before
  the hard `std::process::exit`: the red close button (`close_requested` in `App::ui`) and the
  ⌘Q / normal-termination path (`on_exit`, which macOS uses instead — it never delivers
  `close_requested`). `BusView::unkey_for_shutdown` blocks on a `RigCommand::PttRequest { on:
  false }` (key-down needs no interlock token); real mode only, 1 s bound so an absent rig
  can't hang the quit. Removes the dependence on the rig's ~15 s PTT watchdog as the only
  backstop against a mid-over quit leaving the transmitter keyed.

**Settled `OVERVIEW.md §7` design decisions:** decoder strategy (Rust `ft8_lib` port, with
shelling out to `jt9` as the documented fallback if full parity is required) · audio/serial
crates (`cpal` / `serialport`) · network protocol (mDNS + UDP gossip, eventual consistency) ·
map base data (bundled coastline mesh + land-snapping) · FFT migration to `realfft` in `modes`.

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
