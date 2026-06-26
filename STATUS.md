# DM420 — Status & Open Work

> Single source of truth for **open** work. Done work is **not** tracked here — `git log` is its record. The *why* behind architecture tasks lives in `ARCHITECTURE_REVIEW.md`; component specs in `docs/`; the multi-op protocol in `docs/networking.md`. Owners: **J** = Josh/N0JDC, **W** = Joel/W4LL, **—** = either. _Updated 2026-06-25._

## 🔴 Field Day blockers (June 27–28)
- [ ] **Per-band/per-mode "unworked" tracking + Field-Day log reset** — J — the core FD dupe rule; reset path + per-`(call,band)` worked display
- [ ] **QSO correctness investigation** — W/J — on-air symptoms observed, not yet pinned; reproduce + diagnose
- [ ] **CQ answered with a report (not grid) is ignored** — J — drops real FD contacts; fix drafted+reverted on `lane-finder` branch, needs landing + regression test
- [ ] **Logbook shows class/section, not signal report** — — — the FD exchange is class+section, not SNR
- [ ] **Send box doesn't update live during a QSO** — J — only refreshes after TX (`tx_hold` latch); operational blindness mid-over
- [ ] **Shared logbook MVP (multi-op headline)** — J — Network Step 2; full anti-entropy is risky in 2 days, but an outbound-push + inbound-merge MVP may land (see `docs/networking.md`)

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
