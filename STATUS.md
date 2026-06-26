# DM420 — Status & Open Work

> Single source of truth for **open** work. Done work is **not** tracked here — `git log` is its record. The *why* behind architecture tasks lives in `ARCHITECTURE_REVIEW.md`; component specs in `docs/`; the multi-op protocol in `docs/networking.md`. Owners: **J** = Josh/N0JDC, **W** = Joel/W4LL, **—** = either. _Updated 2026-06-25._

## 🔴 Field Day blockers (June 27–28)

> **The Field Day QSO machinery is built but dormant.** The engine already sequences the full FD flow — `CQ FD …` → bare `<class> <section>` exchange → `R`-exchange → `RR73`/`73`, with the grid step skipped and the roger/`73` roles reversed (log-on-send vs. log-on-received) — plus the P1–P3 idiom fixes (give-up cap, any-sign-off completion, report-opener) and the class/section log columns. All of it is in `main` with characterization tests. **None of it is reachable at runtime:** `to_qso_config()` (`crates/gui/src/settings.rs:124`) hard-codes `ContestProfile::Standard`, and `Station` holds only call/grid, so the engine never enters FD mode. The first blocker is the switch that lights up the rest.
>
> _Cleared since last update — now in `main`, evaluated and **no longer blockers** (need **on-air** validation, not code): per-`(call,band)` dupe tracking via the single-owner `WorkedStatus` producer, with mode **deliberately collapsed** (the ARRL-correct digital rule — 20 m FT8 ⇒ dupe on 20 m FT4; supersedes the old "per-band/per-**mode**" framing); the "CQ answered with a report, not grid" drop (P3, `engine.rs:353`, correctly gated **off** in FD); the class/section log columns (`engine.rs:1191`; generic `SNT`/`RCV` columns already render the exchange string)._

- [ ] **Contest-mode setup UI — the gate** — J — the unlocked Digital panel needs a contest selector (`None` | `ARRL Field Day`) plus the operator's exchange (class e.g. `3A`, ARRL section e.g. `CO`). Core is fully wired (`ContestProfile::ArrlFieldDay`, `fd_class`/`fd_section` on `StationConfig`); add the three fields to `Station`, persist them to `[station]` in `config.toml` (mirror d404560's FT8/FT4 persistence), add a ComboBox + class/section fields to `ConfigForm` (`waterfall.rs`), and drop the `Standard` hard-code in `to_qso_config()`. **This one switch unblocks CQ FD, the FD exchange sequencing, and the class/section log columns — all already built.**
- [ ] **On-air validation of the FD QSO flow** — W/J — the FSM and the P1–P3 fixes are landed and unit-tested but have never run against real radios; reproduce/diagnose the earlier on-air symptoms against the now-landed engine. Subsumes the old "report not grid is ignored" item (the P3 fix is in — verify it on air) and gates on completing a real **FT4** contact (`docs/live_pipeline_notes.md`).
- [ ] **Log entries carry no FD-vs-normal tag** — — — `LogEntry` has no contest/exchange-kind field; `3A WI` vs. `-07` is only inferable by parsing the exchange string (the stored `Section` is a weak proxy). Add an explicit tag (serde-default for back-compat), set from `is_field_day()` at construction. Cheap; matters for clean export/scoring.
- [ ] **Shared-logbook group dupes + origin UI (multi-op headline)** — J — MVP transport (push/merge/catch-up) is landed and peer entries already flow onto `logbook/entries`, but the worked producer still hard-codes `WorkedByMe` (`crates/core/src/worked.rs:94`) — peers' QSOs aren't classified `WorkedByNetwork`, so a teammate's contact is not yet a group dupe or rendered origin-distinct. Classify by `entry.origin` + add origin-distinct UI (`docs/networking.md`).

## Field Day Desired

- [ ] **Multi-caller auto-pick (pileups)** — J — the FD norm is several stations answering one CQ slot; auto-select the highest-SNR non-dupe, exclude calls a peer is working, highlight all answerers, and allow number-key override (`docs/qso_flow.md` §6). Not started.

- [ ] Tri-state control for clear QSY/follow station/lock offset. In "Clear" mode it would always attempt to find a clear part of the audio passband. In "Follow" it would behave as it does today--following the offset of whatever station we are answering, or using the current offset for CQ. In "Lock" it would lock to the current offset.

- [ ] Selection during scanning needs to be disabled--map and/or decode panel

- [ ] Band scan needs to publish results to network

## Weird QSO State Thing

- [ ] Picking up a QSO mid-stream by clicking on somebody's traffic (addressed to my station) appeared to have the following odd behaviors: 1) My QSO state machine switched to calling CQ after finishing that QSO, even though I did not start by calling CQ. 2) My own CQ traffic is listed with a number shortcut for answering.

## After Field Day

**Architecture rework (open)** — IDs reference `ARCHITECTURE_REVIEW.md`:
- [ ] **1a — close the TX-outcome loop** ⚠ *the deepest fault*: the FSM advances open-loop (`TxAck` only has `Accepted`); a denied/failed over advances like a sent one → logged contacts that never aired. Safety-adjacent.
- [ ] 1c — clock unification (wire the dead `BusView.clock`; remaining direct `Utc::now()` reads) — spectrogram drift only partly fixed
- [ ] 2c — publish `OperatingState` (the mode+band owner) + a `SessionCommand` bus path — retires the interim band-from-`RigState` workaround in the beacon
- [ ] 2b follow-through — `core::enrich` now stamps `EnrichedDecode{band, worked}` onto `decodes_enriched` (consumed by `core::band_status`). Remaining: migrate the other band-from-VFO derivations (`bus_view::pump_heard`, the `net` beacon, `core::scan`'s private `slot_band`) onto it, and have the enricher emit `origin` / `WorkedByNetwork`
- [ ] 3b — per-radio control lease (Operate | Scanning | Configuring) + operate⊥configure invariant; config off the lock edge
- [ ] 0a — derive watchdog / `max_tx` / `grant_ttl` from one `slot_period` + `debug_assert`; explicit `ForceUnkey` (the stale comments are already fixed); tighten the FT4 TX watchdog (currently runs too long, ~2 slots — non-blocking)
- [ ] 0b — wire `Granter::revoke` into the QSO-Stop / scan-cancel abort path (the method exists but is unused)
- [ ] A2 — carve the dead prototype tables (`panel_data.rs`) + the mock-only `waterslide_panel.rs`
- [ ] `draw_waterslide`'s 22 positional args → a `WaterslideView` struct (deferred from the `waterfall/` decomposition; the fn now lives in `panels/waterfall/render.rs`)
- [ ] Phase 4 — reconcile `docs/message-catalog.md` with reality (mark each topic built / delete the dead ones)

**Multi-op feature track** — see `docs/networking.md`:
- [ ] Shared logbook, full (Step 2): outbound push, inbound merge, G-set, anti-entropy digest/request/reply, origin-distinct UI
- [ ] Origin prerequisites: `origin: Mine|Peer` on the GUI `HeardEntry` / `MapSpot`; the worked producer emits `WorkedByNetwork`
- [ ] Working-intent (Step 3): the deconfliction overlay shipped; remaining = auto-pick exclusion of peers' offsets
- [ ] Heard/band aggregation (Step 4): peers' heard-stations + band-activity into the local views — `core::band_status` already merges peer `StationSnapshot.heard` (now carrying `mode`) and `HeardStation` has the field; remaining is the LAN beacon *populating* `heard` (from the producer's `Mine`-origin set) so peer rows fill
- [ ] Shared band-scan: beacon `band_activity`; show peers' scan results

**Decoder** — see `docs/decoder_*.md` (W's lane):
- [ ] Sensitivity Phase 3.1 fit + profiling

**Reliability / live pipeline** — see `docs/live_pipeline_notes.md`:
- [ ] Spectrogram ↔ decode-text drift: rebuild columns by `SpectrumRow.t` (🔴 — same fix as 1c)
- [ ] Bound the per-slot decode threads (backpressure when decode > slot duration)
- [ ] Clean capture shutdown (enables device/source switching)
- [ ] Spectrum stream sampled lossily (a `Cell`, not a ring) — drain a ring per frame
- [ ] NTP-drift detection / warning (slot timing silently depends on the system clock)
- [ ] Brightness scale hardcoded (`COL_DB_FLOOR`/`CEIL`) — add a reference-level control / AGC

## Backlog / under consideration
- [ ] **Field Day log reset** — J — no clear/truncate path exists (no `SessionCommand::ClearLog`; logbook is append-only; `ARCHITECTURE_REVIEW.md:271` flags `scanner.worked` growing unbounded). Needed so practice/prior QSOs don't count as dupes at contest start. Hook: a reset command → logbook archives-then-zeros + republishes an empty `logbook/entries` → the `WorkedStatus` producer and every consumer fall to empty automatically (single-owner pays off here).
- Map: grid squares drawn in the wrong places
- Map: turn off crosshairs after a QSO clears; highlight a station that answers my CQ
- After a QSO finishes: unhighlight traffic + reset the Send box to CQ
- RX clipping indicator (audio level)
- Clear-lane finder: jump to an optimum CQ calling frequency (occupancy map + lane scoring) — `lane-finder` branch
- Band-scanner enhancements: per-offset sweep, FD-only filter, SNR floor, configurable dwell
- Band Status panel polish: tune the six-band grid + header SCAN-button placement once eyeballed; populate it in pure-mock mode (the producers run in real/WAV `core::spawn` only, so mock mode shows empty); rename the now-misnamed `BANDSCAN_H` constant
- Decode-archive analytics: querying, logbook recovery, whole-QSO view, SQLite, origin stamping
- Waterfall render gap on refocus (the App-Nap *unkey* is already fixed; spectrogram-freeze-on-refocus remains)
- _Design calls to settle:_ wait-for-CQ vs answer-immediately (`docs/joel/joels-notes.md`); jump on a station after their RR73; behavior when clicking another station (decode or map) while armed / mid-QSO; drop SNR from own transmissions
