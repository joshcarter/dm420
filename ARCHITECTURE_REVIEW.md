# DM420 Architecture & Fragility Review

> Generated 2026-06-23 by a 16-agent Opus review (9 module-by-module architects +
> 5 cross-cutting architects + a structural map + synthesis). Snapshot of `main`
> at the time of writing; file:line references are from that snapshot and should be
> re-confirmed before acting. This is an analysis artifact, not a spec — safe to edit/delete.

## DM420 Architecture Review — Structural Fragility Report

**Author:** Chief Architect synthesis of 14 focused reviews (9 module + 5 cross-cutting)
**Verdict:** The spine is sound. The fragility is not in the bus, the type vocabulary, or the interlock granter — those are genuinely well-built. The fragility comes from a small number of repeated design decisions that let *runtime facts* exist in many hand-reconciled copies, and that let *the TX/QSO lifecycle run without a feedback loop*. Almost every "weird state transition" and "duplicated state" bug the owner is feeling traces back to 6–8 root decisions. Fix those and whole classes of bugs disappear.

---

### Executive Summary

The bus architecture (4 delivery classes, per-topic `Cell`/`Ring` pumps, the `interlock::Granter` single-token machine, the `AudioControl` single-mutex generation design) is the right shape and is not the problem. Keep it.

The problem is **ownership**. The message catalog designed single-owner State producers for exactly the facts that are now causing pain — `decodes_enriched`/`WorkedStatus` (worked-status), `OperatingState` (mode/band/posture), `tx_report` (TX outcome) — and **those producers were never built**. So every consumer grew its own copy of the fact and reconciles it by hand, per-frame, gated on GUI-only flags. The catalog therefore *advertises* a clean contract that the code does not honor; new code wires to dead topics, and old code bypasses the bus to poke in-process control handles (`AudioControl`) directly.

This produces four recurring bug families the owner is feeling:

1. **The QSO sequencer is open-loop.** It never learns whether an over actually keyed. `tx_report` has zero subscribers and `TxAck` is replied `Accepted` even on `Failed`/`Denied`. A denied/failed/aborted over advances the FSM exactly like a successful one — the sequence silently desyncs from the far station, and contacts get logged that never went on air. This is the single deepest fault and it is safety-adjacent.
2. **The same fact is recomputed in 3–5 places and they disagree.** Worked-status, on-air mode, TX offset, current band, and "now" each have multiple owners. Concrete live bug: work a station on 20m FT8, switch to 20m FT4 — the scanner says unworked, the waterslide dims it worked. Concrete live bug: every logged QSO is stamped `Band::B20m` regardless of the real band, breaking the Field-Day per-band rule everywhere downstream.
3. **Config and identity apply only on a hidden edge** (the unlock→lock click), staged in GUI form copies, pushed imperatively through producer handles the GUI holds — and silently no-op in mock/WAV mode. A missed re-lock, or any TX before re-lock, runs on stale mode/device/callsign.
4. **The GUI has four 850–2,458-line god-files** that braid rendering + settings + formatting + command parsing + async pumps, so every edit to one concern forces a reader to load all of them and risks the others.

The fixes are tractable and mostly additive (build the owner producers; subscribe instead of re-deriving). The recommended sequence front-loads two safety/correctness items (close the TX outcome loop; right-size the FT4 watchdog) and a cheap shared-utility pass, then converges the duplicated facts onto single owners, then splits the god-files.

---

### The Core Fragility Drivers (ranked by impact)

#### 1. The QSO sequencer advances open-loop — TX outcome is structurally unobservable (severity: high, effort: M, safety-adjacent)

**Where it hurts now.** A QSO that should send a report/RR73 advances its state even when the over was *denied* (e.g. the scanner holds the PTT token), *failed* (rig offline, device gone, encode failed), or *aborted*. The engine then sends the *next* message next slot, permanently out of step with the partner, and `build_log` records a contact that never reached the air. The mid-over "Stop" only works because `core::tx` reverse-engineers it by watching the `QsoState` State topic for an `Idle` edge (`tx.rs:152-176`), which forces a fragile deferred-final-state-publish hack in the shell (`shell.rs:235-258`) to stop the abort watcher from false-firing on the legitimate final RR73/73.

**Root cause.** The catalog designed `radio/{id}/tx_report` (State) as the channel by which the engine learns the real outcome, and made `TxAck` receipt-only. But the engine never subscribes to `tx_report` (`core::tx` is the only party that touches it), and `transmit()` returns the true `TxOutcome` then **unconditionally replies `TxAck::Accepted`** (`tx.rs:142`) regardless. `TxAck` has exactly one variant. The FSM has no closed loop, so downstream code reconstructs transitions from whatever State edges happen to be observable.

**Fix.** Add `Event::TxOutcome { slot, outcome }` to the engine; have `qso::shell` consume the outcome (either subscribe to `tx_report`, or give `TxAck` `Sent`/`Denied`/`Failed` variants and branch on the reply). Advance the sequence **only** on a confirmed `Sent`; a `Denied`/`Failed` holds or re-arms the current over. Replace the `QsoState`-Idle-edge abort inference with an explicit abort (a `Granter::revoke(token)` — see driver #10) and delete the deferred-publish hack. This one change closes the loop the whole sequencer's correctness silently depends on.

---

#### 2. No canonical owner for cross-cutting runtime facts — the unbuilt enrichment/operating producers (severity: high, effort: L, the meta-driver)

**Where it hurts now.**
- **Worked-status** is re-derived in ≥3 places with *different keys*: `scanner::Scanner.worked` keys on `(Callsign, Band, OverAirMode)` (`scanner/lib.rs:80`); `gui::bus_view::worked_calls_on_band` keys on `(call, band)` only, dropping mode (`bus_view.rs:417`); `worked_spots` keys on `call` globally (`bus_view.rs:385`); `core::scan` independently subscribes the log stream for its own tally (`scan.rs:72`). Live bug: a station worked on 20m FT8 reads unworked in the scanner and worked in the waterslide on 20m FT4.
- **The fact they all read is itself wrong:** `qso::build_log` hardcodes `band: Band::B20m, freq: AbsHz(14_074_000)` into *every* logged contact (`shell.rs:368`) because the engine never subscribes to `RigState`. So the Field-Day "same call on another band = a new contact" rule is broken identically in the waterslide, the map, and the scanner.
- **On-air mode** lives in `AudioControl.cfg.Protocol` (canonical driver), `BusView.applied.protocol` (form/header, persisted to disk), `ClockStatus.mode` (slot length), and the `qso::shell` `mode` local (TX synth). During a switch these update asynchronously, so a boundary slot can be decoded/keyed at the wrong protocol. In mock/WAV mode `control.audio` is `None`, so `set_protocol` no-ops the real mode while the UI and config still flip — UI and reality diverge and the divergence is persisted.
- **Current band** has no owner; it is reclassified from `RigState.vfo` with a private hz→band table in the waterslide (`waterfall.rs:1437`), tracked separately in the scanner (`scan.rs:93`), and carried independently on `HeardEntry`/`LogEntry`/`BandActivity`. A band-edge frequency can classify differently per panel.

**Root cause.** `radio/{id}/decodes_enriched` + `WorkedStatus` (the single worked-status owner) and `radio/{id}/operating` (`OperatingState`, the single mode+band owner — its originally-designed `posture` field moves to the control lease; see the *Radio Ownership* addendum) are defined in `types`, routed in `bus/topic.rs`, listed in the catalog — and **never produced**. With no single owner, every consumer reconstructs the fact from raw streams with its own rule.

**Fix (stage it).**
1. Subscribe `qso::shell` to `RigState` and stamp real band/freq/time in `build_log` (small, unblocks everything below).
2. Add `Band::from_hz(AbsHz) -> Option<Band>` in `types` next to `calling_freq` and route the waterslide, scanner, and map through it (one band table).
3. Build the enrichment producer in `core`: subscribe `decodes` + `logbook/entries` (+ future peer snapshots), maintain the canonical worked set keyed *once* and carrying `origin: Mine|Peer`, publish `EnrichedDecode`/`WorkedStatus`. Convert the three GUI/scanner re-derivations into subscribers.
4. Publish `OperatingState` (mode+band) from one owner; the GUI form/header and the qso shell become *observers* of mode, not co-owners. This also fixes the mock/WAV mode-no-op honestly (the owner can report "not applied"). (Posture — Operate/Scanning/Configuring — is owned separately by the control lease, Phase 3b, not by `OperatingState`.)

---

#### 3. TX audio offset lives in ~5 copies, reconciled per-frame against a lock the engine can't see (severity: high, effort: M)

**Where it hurts now.** This is the "TX went out on the wrong frequency" bug. The outgoing offset exists as `Waterfall.real_sel.offset`, `Selection.outgoing`, `Engine.outgoing`, `State::Calling.offset`/`Active.offset`, and the published `QsoState.tx_offset`. The waterslide copies the engine's `tx_offset` *back* into `real_sel.offset` every frame — but **only when `!offset_locked`** (`waterfall.rs:894-904`). `offset_locked` (`waterfall.rs:160`) is a GUI-only boolean never published to the bus, so an engine auto-QSY hop, `/clear`, a map pick, or a scanner retune can move the lane the operator believes is padlocked. Which copy is authoritative flips with lock state and QSO phase; correctness depends entirely on the per-frame guard ordering in one panel (and the same panel enforces the lock in ~5 scattered conditional guards).

**Root cause.** The intended owner channel (`selection/{id}/active.outgoing`, a State topic) exists, but both the GUI and the engine keep private copies reconciled imperatively each frame instead of one side owning and the other subscribing. The lock is a UI concept that gates a write rather than a piece of state the offset's owner enforces.

**Fix.** Pick one owner and make data flow one-directional. Publish `offset_locked` as part of `Selection`/operating state so the engine's auto-QSY honors it server-side. Delete the per-frame back-copy (`waterfall.rs:898-904`) and the post-hoc overwrite (`waterfall.rs:1027-1030`); the GUI commands offset changes and renders the lane from the engine's echoed value, never producing an offset change while locked at the source.

---

#### 4. QSO exchange progress is implicit field-soup, not a typed state — the direct source of "weird transitions" (severity: high, effort: M)

**Where it hurts now.** This is the owner's "bugs from weird state transitions" complaint, literally. Inside `State::Active`, the real "where are we in the exchange" is implicit in the *combination* of `next` + `finish_after_tx` + `log_on_tx` + `logged` + `step` + `overs_since_progress` plus captured facts (`engine.rs:115-144`). Transitions are hand-enumerated `(role, contest, msg)` match arms that fall through to `_ => None` (`engine.rs:444, 697`). `qso_engine_improvements.md` documents that the report-opener-ignored and bare-73-ignored "never replies" bugs (A1/A2 in `qso_engine_improvements.md`) happened *precisely because* "the arms are hand-enumerated and easy to leave a hole in." The same content→action mapping is duplicated across four functions (`commit_from_cq`, `commit_from_armed`, `advance_active`, `resume_from`), so a hole in one is invisible — `resume_from` already handles a report-to-us differently than the live path because it only reaches that case via a `_ => advance_active` fallthrough that has no report-opener arm.

**Root cause.** `qso_flow.md` and `wsjtx_qso_sequencing.md` mandate mirroring WSJT-X's `m_QSOProgress` enum as the authoritative state with `step` display-only; `step` is display-only but no enum replaced it, so progress remained a derived combination of six fields advanced by parallel match arms.

**Fix.** Introduce a `Progress` enum on `Active` (CALLING/REPLYING/REPORT/ROGER_REPORT/ROGERS/SIGNOFF) and drive one transition table keyed on `(role, contest, received-kind) -> (reply, next-progress, log-trigger)`, replacing all four parallel match sites. Missing transitions become compile-visible holes instead of silent `_ => None`. Collapse `next`/`finish_after_tx`/`log_on_tx`/`step` into derived consequences. Also make `completed()` take `&Active` so the empty-callsign escape hatch (`engine.rs:909`) disappears.

**Status — ✅ DONE** (item 3a, landed on `fd-progress-fsm`): all four sites route through **two** exhaustive tables (`open` + `advance`, plus `signoff_outcome`/`answer_opener`) — two rather than one, because `advance` is progress-agnostic and a single progress-keyed table would change the give-up/timeout drop-set (see `docs/joel/qsos-and-the-progress-fsm.md`). Every `_ => None`/`_ => {}` content catch-all is gone (a missing content case is now a compile error); `completed()` takes `&Active` (escape hatch removed). The `next`/`finish_after_tx`/`log_on_tx`/`step` fields stay on `Active` but are now set in **one place** (the appliers `open_at`/`apply_advance`), not removed. `Progress` lives on `Active` but is **internal** — the published phase is still `step`; surfacing `progress` on the bus is a follow-up. Behavior is characterization-pinned (the suite stayed green unchanged across every routing commit). The diagnosis above is kept for the record; `engine.rs:444/697/909` line refs are pre-refactor.

---

#### 5. Config and station identity apply imperatively on the lock edge, through GUI-held producer handles (severity: high, effort: M)

**Where it hurts now.** While unlocked, the top bar edits `App.station.call/grid` live (`main.rs:699`), but the engine's identity (`engine.me`) and the rig/audio producers update **only** on the LOCK click (`main.rs:755-764` → `set_qso_station`, `apply_config`). For the entire unlocked window the engine's identity and the producers' device/mode are stale relative to what the operator sees. Any code path that keys or logs before re-lock uses the old callsign/device. A missed or partial re-lock leaves producers running stale config with no indication. `apply_config` is a multi-field imperative push with no transactionality — in WAV/mock mode one handle is `None`, so some fields silently no-op while the whole cfg is recorded as "applied."

**Root cause.** Configuration is GUI-resident state pushed through in-process control handles the GUI holds (`CoreControl`/`QsoControl` in `BusView`), committed on a single posture transition, rather than declarative desired-state that producers converge to. `edit_mode` is overloaded as first-run-setup flag, operate/configure posture, *and* the config-commit trigger (`app.rs:27,100`). The designed `session/{id}/command` (`SessionCommand`) bus path is dead; mode switching bypasses it via direct `AudioControl` mutation.

**Fix.** Decouple commit from the lock edge: apply config changes as they happen (debounced), or make "staged vs applied" an explicit typed state the UI renders so a forgotten re-lock is visible. Push identity to the engine when it changes, not only on re-lock; block TX while the engine's identity is provisional. Route mode/band/station as bus commands (revive `SessionCommand`/`OperatingState`) so the change is an observable event, not a side-channel poke. Record `applied` only for fields that actually reached a running producer.

---

#### 6. Fragmented clocks — the authoritative bus clock is pumped but dead in the GUI (severity: medium, effort: S–M)

**Where it hurts now.** This is the documented 🔴 spectrogram↔decode-text drift. `core::clock` publishes an authoritative mode-aware slot id on `clock/status`, consumed correctly by `qso::shell`. But the waterslide reads `chrono::Utc::now()` directly up to four times per frame (`waterfall.rs:239,547,606,822`) to place decode text and digit-shortcut slot boundaries, while the spectrogram scrolls by accumulated frame `dt` (`waterfall.rs:1297`) — two timebases that separate under load. `core::tx` keeps its own `now_ms`/`slot_period_ms` (`tx.rs:430-445`), so slot parity computed in the engine vs tx can diverge under skew. `BusView.clock` is pumped every frame and then `#[allow(dead_code)]` (`bus_view.rs:138`).

**Root cause.** The bus clock was made authoritative but never wired into the consumers that predate it; they kept reading the wall clock for convenience, and the place to feed the bus clock into the GUI (the clock cell) was wired but never read.

**Fix.** Make `clock/status` the only slot/now source on the operating path. Wire the dead `BusView.clock` cell into the waterslide for `now_ms` and slot-boundary math; place spectrogram columns by `SpectrumRow.t` (the producer already stamps it, `decode.rs:133`) rather than integrated `dt`. Have `core::tx` derive slot timing from the clock topic.

---

#### 7. No shared low-level vocabulary helpers — time, band, mode-conversion, formatting each reimplemented N times with silent divergence (severity: medium, effort: S)

**Where it hurts now.** `types` holds the message vocabulary but almost no pure helpers that operate on it, so every consumer grows its own copy and several have already diverged:
- `now_ms()` is byte-duplicated **9 times** (`tx.rs:430`, `health.rs:12`, `scan.rs:342`, `clock.rs:70`, `decode.rs:61`, `bus_view.rs:735`, `mocks/lib.rs:33`, `qso/shell.rs:382`, `bus/handle.rs:126`).
- `OverAirMode↔Protocol` conversion exists 4 times with **incompatible fallbacks**: `decode.rs:84` correctly returns `None` for PSK31/RTTY, but `scan.rs:335` silently coerces every non-FT4 mode (including PSK31/RTTY) to FT8 — a latent wrong-decode bug in any future PSK/RTTY scan.
- `band_for_hz` (domain logic) is trapped `pub(crate)` inside a **GUI panel** (`waterfall.rs:1437`) and reached cross-module via `crate::panels::waterfall::band_for_hz` from `bus_view.rs:897`.
- `fmt_snr` is identical in `waterfall.rs:25` and `mocks/lib.rs:42`, with a *third* incompatible variant in `engine.rs:929` (ASCII `+` vs the GUI's `−` glyph) — so the engine's logged report and the GUI's displayed report don't match.
- `haversine_km` exists twice with different precision (`panel_data.rs:529` f32 vs `call_sign.rs:167` f64) in a file that already imports the shared copy.

**Root cause.** There is no shared "units/time/format" home just above `types`, so the path of least resistance is always a private copy.

**Fix.** Hoist `now_ms`/`now_unix` and `Band::from_hz`/`OverAirMode↔Protocol` into `types` (or a tiny `util` crate); add a `format`/`display` module for `fmt_snr`/`decode_text`/`fmt_payload`/`display_call`/mode labels; one `geo::distance_km`. Delete the copies. Small, mechanical, and it removes the scanner's silent FT8 coercion and the engine/GUI SNR mismatch as a side effect.

---

#### 8. `selected_station` has two per-frame writers, a reverse channel, and a separate operational copy (severity: medium, effort: S–M)

**Where it hurts now.** `App.selected_station` is written every frame by **both** the Waterfall (`waterfall.rs:1107`) and the Contacts map (`contacts.rs:217`); last-writer-wins by egui_tiles draw order, so a map click and a waterslide selection in the same frame clobber each other. The map's operational intent rides a *separate* `map_pick` reverse channel consumed a frame later, and the QSO target is a *third* copy (`real_sel.target`). So the display string and the armed target can refer to different stations across the one-frame handoff.

**Root cause.** A single piece of shared UI state has no owning panel, and the display representation was split from the operational representation without a single source of truth.

**Fix.** Give selection one owner: route both panels' selection *intents* through the existing `selection/{id}/active` bus topic (or one `App` method), and have every panel read it back. Collapse the display string and `real_sel.target` into one typed `Selection { display, target, origin }` so they cannot disagree; drop `map_pick`.

---

#### 9. The GUI god-files are change-amplifiers (severity: medium, effort: M–L)

**Where it hurts now.** Five GUI files past 850 lines each braid 3–5 unrelated responsibilities, so editing the Send row, the config form, or a pump forces a reader to load the whole file and risks the others: `waterfall.rs` (2,458 — spectrogram render + decode-text placement + Send/TX row + slash/digit input + hardware ConfigForm + mode toggle + CQ-shortcut machine), `bus_view.rs` (988 — async pumps + sync accessor API + worked/heard derivation + config-apply state), `settings.rs` (957 — hand-rolled TOML codec + Settings domain model), `main.rs` (891 — app lifecycle + tiles layout + top-bar chrome), `contacts.rs` (852 — map projection/render engine + Panel). `draw_waterslide` alone takes 22 positional args because it has no cohesive home.

**Root cause.** The `Panel` trait gives one struct one file and no sub-seams were carved as features landed; `bus_view` accreted everything that crosses the sync↔async seam.

**Fix.** Targeted extractions (see Split list). Do these *as* you touch each file for the fixes above, so each big correctness fix lands in a smaller, safer file.

---

#### 10. Keying-safety: keyed state is inferred from weak proxies, and the safety-timeout ordering is implicit across three crates with stale comments (severity: medium but SAFETY-critical, effort: S–M — jump the queue)

**Where it hurts now.** "Are we transmitting" is recomputed in the GUI from three disagreeing signals — `tx_hold`, "own-TX spectrum within 500 ms" (`waterfall.rs:374`), and `QsoState.phase` — instead of from `RigState.ptt`. They disagree at over boundaries (the final 73 keeps showing after the engine goes idle). The stuck-key safety contract (`max_tx (~14 s FT8 / ~6.5 s FT4) < rig PTT_WATCHDOG (15 s) < interlock GRANT_TTL (20 s)`) is three magic constants in three crates with **no compile-time link**, and the comments are actively wrong (`interlock.rs:17` and `rig_adapter.rs:336` both say "10 s" — it is 15 s). For FT4 the 15 s watchdog is two full slots, so it only bites on a double failure. The scanner's TX-block also lapses if slot boundaries stall longer than the TTL, because it only refreshes the token on a boundary. The shutdown unkey relies on a sentinel `InterlockToken(0)` working *only because* `rig_adapter` happens to gate key-up but not key-down — a safety guarantee encoded in an implicit branch.

**Root cause.** No single published keyed flag, and the safety-timeout relationship is expressed nowhere; it lives as independent constants plus an abort path bolted on as a second keying-control channel.

**Fix.** Publish one keyed signal (`RigState.ptt` / the interlock holder) and render the GUI from it. Derive all three timeouts from one mode-aware `modes::slot_period` source with a `debug_assert` on the ordering; right-size the FT4 watchdog. Refresh the scanner's block token on a fixed `TTL/2` timer (or a long-lived "block" grant variant) decoupled from slot boundaries. Make the shutdown unkey an explicit, documented `ForceUnkey` instead of a sentinel-token trick. Add `Granter::revoke(token)` so abort is a property of the interlock, giving both the QSO Stop and the scanner a single, explicit abort path (also unblocks driver #1).

---

### State Ownership & Duplication

The state inventory is the heart of the fragility. Every fact below should have exactly one canonical producer publishing on a State topic, and every other site should subscribe — not cache-and-reconcile.

| Fact | Should-be owner | Reality (copies) | Symptom |
|---|---|---|---|
| **Worked/heard status** | `decodes_enriched`/`WorkedStatus` enrichment producer (UNBUILT) | scanner `(call,band,mode)`; gui `worked_calls_on_band (call,band)`; gui `worked_spots (call)`; core::scan tally | worked-on-map / unworked-in-scanner; per-band Field-Day rule disagrees |
| **On-air mode** | `OperatingState.mode` or `ClockStatus.mode` (one) | `AudioControl.Protocol` (real); `BusView.applied`; `ClockStatus.mode`; qso `mode` local; per-message `mode` fields | mode flips in UI but not on air (mock/WAV); wrong-protocol boundary slot |
| **TX offset** | `Selection.outgoing` (one direction) | `real_sel.offset`; `Engine.outgoing`; `Calling/Active.offset`; `QsoState.tx_offset` | TX on wrong frequency; lock invisible to engine |
| **Current band** | `Band::from_hz` resolver + `OperatingState.band` | waterslide local table; scanner `active_band`; per-message band; hardcoded `B20m` in LogEntry | band-edge misclassification across panels; broken dupe detection |
| **Now / slot phase** | `clock/status` | bus clock (dead in GUI); `chrono::Utc::now()` ×4 in waterslide; tx's own `now_ms` | spectrogram↔text drift; slot-parity divergence |
| **Selected station** | one `Selection` owner | two GUI writers + `map_pick` + `real_sel.target` | cross-panel clobber; display ≠ operational target |
| **TX outcome** | `tx_report` (published, ZERO consumers) | engine infers success from `TxAck::Accepted` (always Accepted) | sequencer desync; logged contacts that never aired |
| **Station identity** | engine `StationConfig`, pushed on change | `App.station` (live-edited); engine.me (re-lock only) | TX/log with stale callsign during unlocked window |

The unifying remedy: **build the owner producers the catalog already specified, then convert every cache into a subscriber.** `origin: Mine|Peer` must be threaded through the worked/heard owner now (it has nowhere to live today — `MapSpot`/`HeardEntry` carry no `origin` field), or peer gossip from the `net` crate (which currently publishes `StationSnapshot` with **zero subscribers**) will silently render as "I worked them."

---

### State-Transition / Lifecycle Issues

1. **Open-loop TX (driver #1).** The FSM advances on `Tick`, not on confirmed `Sent`. Closing this loop is the highest-value lifecycle fix.
2. **QSO progress is field-soup (driver #4).** No typed `Progress` enum; four parallel `(role, contest, kind)` match sites with `_ => None` holes. This is the documented cause of the A1/A2 "never replies" bugs.
3. **Deferred final-state publish race.** Because abort is inferred from the `QsoState` Idle edge, the shell defers publishing the engine's real Idle until the over finishes (`shell.rs:235-258`); a `CallCq` or new-contact mid-over can be clobbered back to Idle by the late publish. Disappears once abort is explicit (driver #10) and the loop is closed (driver #1).
4. **Config/identity apply on a hidden edge (driver #5).** Lock/unlock posture is overloaded as the commit trigger; a missed re-lock leaves producers stale.
5. **Scanner ↔ TX mutual exclusion is best-effort and one-directional.** The scanner acquires the PTT token "best-effort" and sweeps (issuing `SetFreq` retunes) even when denied; the engine is never told a scan owns the rig, so it keeps sequencing. Model rig ownership as an explicit posture (Operate | Scanning | Configuring) the engine subscribes to and refuses to arm against.
6. **Scanner VFO save/restore has no crash recovery.** `saved` is captured at StartSurvey and only restored on graceful Cancel; an abnormal exit mid-sweep parks the rig off the operator's frequency. Pair with a rig-adapter "safe park" on token loss.

---

### Module Boundaries & Cross-Cutting Features

- **The bus contract has drifted from reality.** Defined-but-dead topics: `radio/{id}/operating` (never published), `session/{id}/command` (no server; mode switch bypasses via `AudioControl`), `logbook/query` (no server), `decodes_enriched`/`WorkedStatus` (no producer), `tx_report` (producer, no consumer). `types` is the coupling hub every crate compiles against, so a new contributor "doing it the right way" wires to a dead seam. **Decide one model per topic: build the owner or delete the topic, and make `message-catalog.md` match the code.**
- **The GUI reaches past the bus into producer internals.** `BusView` holds `CoreControl`/`QsoControl` and pokes them imperatively (mode, station, device). This is the coupling that gives mode and identity multiple owners. Prefer bus commands; keep `BusView` a pure pump + command facade.
- **Domain logic trapped in GUI panels.** `band_for_hz` (a pure domain fact) lives in `waterfall.rs` and is cross-imported by `bus_view`; `Projection`/`draw_map` is a reusable map renderer embedded in `contacts.rs`. Move domain primitives to `types`/`core`; keep panels thin.
- **Parser sits away from its producer.** `core::parse` depends intimately on `modes`' raw-string conventions but lives in `core`; a `modes` output change needs a coordinated `core` edit with no compile-time link. Pin it with a fixture test in `modes`, or move the grammar next to its producer.
- **Healthy seams — leave them alone.** The `scanner` (pure FSM) vs `core::scan` (IO shell) split is textbook pure-core/imperative-shell, not duplication. `interlock::Granter`, `control::AudioControl`'s single-mutex generation design, and the bus delivery-class machinery are the models to imitate, not refactor.

A couple of bus-internal sharp edges to fix opportunistically: wildcard `State` subscriptions silently degrade to lossy broadcast with no late-join snapshot (`handle.rs:391-406`); `ScannerCandidates` has two payload types registered for one topic, pinned by first-writer at runtime (`message.rs:48-49`); the topic taxonomy is an 8-site hand-edited change-amplifier (drive it from one declarative table). None are causing live bugs but each is a latent divergence source as multi-radio grows.

---

### Consolidate vs. Split

**CONSOLIDATE** (duplicated logic / parallel implementations to merge into one owner):

1. **`now_ms()` — 9 copies** (`tx.rs:430`, `health.rs:12`, `scan.rs:342`, `clock.rs:70`, `decode.rs:61`, `bus_view.rs:735`, `mocks/lib.rs:33`, `qso/shell.rs:382`, `bus/handle.rs:126`) + four `chrono::Utc::now()` reads in `waterfall.rs` → one `now_ms`/`now_unix` in `types`; operating-path slot/now reads from `clock/status`.
2. **Worked-status derivation** (`scanner/lib.rs:80`, `bus_view.rs:417`, `bus_view.rs:385`, `core/scan.rs:72`) → one `core` enrichment producer publishing `WorkedStatus`/`EnrichedDecode`; all consumers subscribe.
3. **`OverAirMode↔Protocol` conversion** (`decode.rs:84`, `scan.rs:335`, `settings.rs:769`, `core/lib.rs:264`) → one `TryFrom`/inverse; fixes the scanner's silent FT8 coercion of PSK31/RTTY.
4. **hz→`Band` classification** (`waterfall.rs:1437` + cross-import `bus_view.rs:897`, `core/scan.rs`, map) → `Band::from_hz` in `types` next to `calling_freq`.
5. **Decode/SNR/mode formatting** (`fmt_snr` at `waterfall.rs:25` == `mocks/lib.rs:42`, third variant `engine.rs:929`; `decode_text`/`fmt_payload`/`fmt_signoff`/`display_call` at `waterfall.rs:31-102`; mode labels at `waterfall.rs:2366`, `band_scan.rs:45`) → a `format`/`display` module.
6. **`haversine_km`** (`panel_data.rs:529` f32 vs `call_sign.rs:167` f64) → one `geo::distance_km` (f64).
7. **JSONL append plumbing** (`logbook/lib.rs:105-125` vs `archive/lib.rs:136-156`) → one shared `jsonl`-append helper so the on-disk durability discipline is single-sourced.
8. **TX offset copies** (`real_sel.offset`, `Selection.outgoing`, `Engine.outgoing`, `Calling/Active.offset`, `QsoState.tx_offset`) → one owner, one-directional flow.
9. **qso content→action mapping** (`commit_from_cq`, `commit_from_armed`, `advance_active`, `resume_from`) → one transition table.

**SPLIT** (god-files / multi-responsibility modules to break up):

1. **`crates/gui/src/panels/waterfall.rs` (2,458) — the *Digital* panel; rename `waterfall.rs` → `digital.rs` (or a `digital/` module) and `struct Waterfall` → `Digital`** → `waterslide_render.rs` (the waterslide *view*: `Spectrogram` + `draw_waterslide` + hatch/history), `send_row.rs` (`draw_send_row` + `apply_command` + slash parsing), `config_form.rs` (`ConfigForm`, pairs with settings), `cq_shortcuts.rs` (assignment logic), `scan_mode.rs` (the active-scan mode of the Digital panel — see the *Radio Ownership* addendum); move format helpers + `band_for_hz` to their shared homes. the renamed `Digital` struct becomes a thin orchestrator; `draw_waterslide`'s 22 args become a `WaterslideView` struct.
2. **`crates/gui/src/bus_view.rs` (988)** → `pumps.rs` (the `pump_*` tasks + `Ring`/`Cell`/`HeardEntry`), `derive.rs` (worked/heard derivation — ultimately migrates into the enrichment producer); `bus_view.rs` keeps the struct + accessor/command API. Extract the config-apply/`applied` state into a `Config` holder.
3. **`crates/gui/src/settings.rs` (957)** → `config_toml.rs` (the hand-rolled TOML codec + its tests; better, adopt `toml_edit`), leaving `settings.rs` with the `Settings`/`HardwareConfig`/`Station` model + env reading.
4. **`crates/gui/src/main.rs` (891)** → `layout.rs` (`Tactical` behavior + `build_tree`/`enforce_min_width`/`pin_band_height`/`TreeIds`), `top_bar.rs` (`top_bar`/`segmented`/`lcd_clock`); `main.rs` keeps entry + the `eframe::App` lifecycle.
5. **`crates/gui/src/panels/contacts.rs` (852) — the *Map* panel (renamed from *Contacts*); rename `contacts.rs` → `map.rs` and `struct Contacts` → `Map`** → `map_render.rs` (`Projection` + `draw_map` + `Marker`/polyline/ellipse primitives), leaving `map.rs` as the Panel.
6. **`crates/core/src/decode.rs::run_stream` (372-508 god-function)** → a `SpectrogramColumnizer` (window+hop+publish) and a `SlotAccumulator` (buffer+boundary+two-pass scheduling) the loop drives; `run_stream` becomes supervision (recv/health/reconnect) only.

**Do NOT split:** `scanner`/`core::scan` (healthy pure-core/shell split); `types`/`qso::engine` live code (they read large but are ~30–45% tests — relocate the `#[cfg(test)]` blocks into `tests/` files for navigability only, do not restructure).

---

> **Reading the task IDs.** Phases run **0 → A → 1 → 2 → 3 → 4 → 5** — Phase **A** (dead-weight removal) was deliberately pulled in *between* Phase 0 and Phase 1. A task ID is `<phase><letter>` (e.g. `2b`); **drivers** are `#1–#10` (the root-cause diagnoses tasks reference); **steps** (`step 1–4`) are `networking.md`'s multi-op build order — a separate axis. The original sequence's `1b` (shared-utility pass) is now **`A3`**. (`A1`/`A2` in driver #4 are *qso bug IDs* from `qso_engine_improvements.md`, unrelated to the Phase-A tasks.) **Live open-work status lives in [`STATUS.md`](STATUS.md), not here** — this section is the task taxonomy and sequence.

### Recommended Refactor Sequence

> **⚠ Superseded** by the **Revised refactor sequence** in the *Mock Removal & Multi-Operator Sequencing* addendum below (re-ordered + renumbered — e.g. old `1b` → `A3`). This original is kept for its rationale; **follow the Revised sequence for phase numbers**.

The ordering maximizes stability per unit effort: safety first, then a cheap shared-foundation pass that unblocks the big wins, then the single-owner conversions, then structural splits done opportunistically alongside.

**Phase 0 — Safety (jump the queue, S each):**
- **0a.** Right-size the FT4 PTT watchdog; derive `max_tx < watchdog < grant_ttl` from one `modes::slot_period` source with a `debug_assert`; fix the stale "10 s" comments (`interlock.rs:17`, `rig_adapter.rs:336`). Make the shutdown unkey an explicit `ForceUnkey`. *(driver #10)*
- **0b.** Add `Granter::revoke(token)` and make abort a property of the interlock, not the `QsoState` Idle edge. *(unblocks #1 and removes the scanner's un-abortable token)*

**Phase 1 — Close the TX loop + cheap shared foundation (M + S):**
- **1a.** Close the TX outcome loop: `Event::TxOutcome`, branch the FSM on `Sent` vs `Denied`/`Failed`, delete the deferred-final-state hack. *(driver #1 — highest correctness/safety value)*
- **1b.** Shared-utility pass: `now_ms` + `Band::from_hz` + `OverAirMode↔Protocol` + format module + `geo::distance_km`. Mechanical, low-risk, and it kills the scanner's silent FT8 coercion and the SNR-glyph mismatch. *(driver #7 / consolidate 1,3,4,5,6)*
- **1c.** Wire the dead `BusView.clock` cell into the waterslide; place spectrogram columns by `SpectrumRow.t`. *(driver #6 — fixes the documented drift)*

**Phase 2 — Single owners (kills duplication classes) (S → L):**
- **2a.** Subscribe `qso::shell` to `RigState`; stamp real band/freq/time in `build_log`. Small, unblocks all per-band correctness. *(prerequisite for #2)*
- **2b.** Build the enrichment producer (`WorkedStatus`/`EnrichedDecode`) carrying `origin`; convert the three GUI/scanner derivations to subscribers; make `net`'s peer snapshots flow into it. *(driver #2 — the meta-fix)*
- **2c.** Publish `OperatingState` (mode+band); GUI form/header and qso shell become observers; honest mock/WAV no-op reporting. *(driver #2 / #5; posture is owned by the control lease — Phase 3b — not OperatingState)*
- **2d.** One TX-offset owner + publish `offset_locked`; delete the per-frame back-copy. *(driver #3)*
- **2e.** One `Selection` owner; merge display string with `real_sel.target`; drop `map_pick`. *(driver #8)*

**Phase 3 — Lifecycle hardening (M):**
- **3a.** ✅ **DONE** (landed on `fd-progress-fsm`) — `Progress` enum + transition table in `qso::engine`; fold the four parallel match sites and `resume_from` into it. *(driver #4 — see the driver-#4 status note for what landed vs. deferred)*
- **3b.** Decouple config commit from the lock edge; explicit staged-vs-applied; push identity on change; block TX while identity is provisional; model rig ownership posture so a scan blocks the engine explicitly. *(driver #5 / lifecycle #5)*

**Phase 4 — Structure (M–L, interleave, don't big-bang):**
- Split the five GUI god-files and `decode::run_stream` *as you touch them* in Phases 1–3, so each correctness fix lands in a smaller file. Reconcile `message-catalog.md` with reality (build or delete each dead topic). Relocate the large test blocks in `types`/`engine` for navigability.

**Unblocking summary:** 0b unblocks 1a; A3's `Band::from_hz` unblocks 2b's per-band rule; 2a unblocks all of 2b/2c; closing the loop (1a) removes the abort hack that the deferred-publish race depends on. The two items that should not wait for anything are the safety pass (0a/0b) and the shared-utility pass (A3) — both are cheap and both remove latent bugs immediately.

---

### Appendix — Findings by module and cross-cutting lens (evidence preserved)

**types** — Healthy as a vocabulary, but encodes an aspirational architecture. `EnrichedDecode` (`lib.rs:267`), `OperatingState` (`lib.rs:301`), `SessionCommand` (`lib.rs:324`) are bus-registered but never produced; nothing distinguishes built from planned. No canonical mode-owner type (ClockStatus.mode vs dead OperatingState.mode + per-message `mode` on SpectrumRow/Decode/TxRequest/TxLogEntry). Band→freq exists (`calling_freq`, `lib.rs:340`) but not the inverse. `DecodeRef` identity is an unresolved Option-keyed tuple (`lib.rs:386`) — two decodes can collide as one selection target. `LogEntry` (the only persisted type) has ad-hoc forward-compat (only `section` has `#[serde(default)]`, `lib.rs:564`) — add a frozen-fixture test. SNR modeled three ways (`Decode.snr_db: Option<i8>` `lib.rs:171`, `HeardStation.snr: i8` `lib.rs:675`, drifting from catalog's `Option<i8>`).

**bus** — Soundest crate; the publisher-never-stalls invariant genuinely holds. Edges: wildcard State subs silently violate latest-wins + late-join semantics (`handle.rs:391-406`); topic taxonomy is an 8-site change-amplifier (`topic.rs:31-219` + `message.rs:32-62` + `recorder.rs:89-114`), already drifted past its spec (19→22 variants); two payload types on `ScannerCandidates` pinned by first writer (`message.rs:48-49`); `RecorderHandle::stop()` clears the shared slot unconditionally (`handle.rs:613-616`); Command traffic invisible to the recorder (correlation id minted then dropped, `handle.rs:463`; `record()` hardcodes `correlation: None` `:200`).

**core-ingest** — rig_adapter/map/health/control/parse are well-factored. Fragility: slot identity re-derived in three producers with "identical formula" comments (`decode.rs:168`, `clock.rs:52`, `tx.rs`), and the scanner cross-references two of these (`scan.rs:198` ClockStatus.slot vs `:210` Decode.slot) — desyncs during a mode switch. `run_stream` god-function (`decode.rs:372-508`). Scan shell duplicates `engine.current()` as `active_band/active_mode` with hand-synced resync at ~5 sites (`scan.rs:93-98,142-148,185-199`). Per-slot decode spawns unbounded detached threads sharing two mutexes (`decode.rs:206-246`). `AudioControl` populated mid-`spawn`, read by scan+clock with implicit ordering; absent for WAV silently disables mode-switching (`lib.rs:262-290`). `now_ms` reimplemented in 5 modules.

**core-tx** — `interlock.rs` is the healthiest module in the codebase (keep as the model). `control.rs` solid. Fragility in tx.rs and boundaries: engine blind to TX outcome (`tx.rs:133-142` always replies Accepted; `TxAck` one variant `lib.rs:513`); two parallel keying-abort mechanisms (`tx.rs:152-176` infers Stop from QsoState Idle edge); slot-period + mode duplicated cross-crate (`tx.rs:440` private table vs `clock.rs:47` from modes); three safety timeouts as independent magic constants with stale comments (`interlock.rs:16`, `tx.rs:28`, `actor.rs:30`); scanner TX-block lapses if boundaries stall >TTL (`scan.rs:178-184`); shutdown unkey relies on sentinel `InterlockToken(0)` + implicit ungated key-down (`rig_adapter.rs:337`); `now_ms` duplicated (`tx.rs:430` vs `clock.rs:70`).

**qso** — Top-level FSM well-tested; exchange progress one level down is field-soup (`engine.rs:115-144`) with hand-enumerated arms falling to `_ => None` (`:444,:697`), duplicated across four functions. Logbook entries stamped hardcoded `Band::B20m`/`14.074 MHz` (`shell.rs:368`). Deferred final-state publish races newer state (`shell.rs:235-268`). TX offset in three engine locations, Select mutates only one (`engine.rs:168,103-107,116,222`). Engine mode-blind despite docs (`on_decode` never reads `d.mode`; mode from ClockStatus `shell.rs:153`). `resume_from` a fourth copy of content→action (`engine.rs:460-579`). `completed()` empty-callsign escape hatch (`engine.rs:909`). CQ-side parity `unwrap_or(0)` default (`engine.rs:354`).

**gui-core** — Seam itself clean. Cross-panel `selected_station`/`map_pick` threaded through App, two writers (`waterfall.rs:1107`, `contacts.rs:217`), races by draw order. `set_protocol`/`apply_config` silently diverge in mock/WAV (`bus_view.rs:464-494`). `settings.rs` god-config with hand-rolled TOML parser (`311-331,403-461`) + ~15 parallel read/save fns. Layout persistence coupled to window-geometry saves, hard-codes tree shape (`main.rs:320-377,254-280`). Authoritative bus clock pumped but dead (`bus_view.rs:136-139`); top bar reads `chrono::Utc::now()` (`main.rs:793`). `edit_mode` overloaded (first-run + posture + commit trigger, `app.rs:27,100`; `main.rs:755-767`). `BusView` mixes pump seam + command facade + config-apply state. Window-geometry extraction duplicated (`main.rs:296-318` vs `528-539`).

**gui-waterfall** — Most fragile GUI subsystem. `waterfall.rs` 2,458-line god-file (`1-2458`). TX offset ~5 owners reconciled per-frame, "single source of truth" comment aspirational (`951,894-904,1027-1030,588-592`). `offset_locked` GUI-only, enforced by 5 scattered guards (`160,898,1027,527,590`). Spectrogram scrolls by frame dt while text placed by wall-clock age (`1297,1736`). Two competing mode controls; ConfigForm reverts a live header change (`629-695,2238-2245,791-799`). Five wall-clock reads + duplicate slot-boundary math (`239,547,606,822,960,1692`). Parallel mock-only waterslide (`waterslide_panel.rs:160-319`). Keying inferred from three weak signals + 500ms heuristic (`374-387,607,857`). `band_for_hz` local reimplementation (`1437-1451`). Bare-offset click reads bumped final_y not true lane (`1387,1852-1861,2058-2072`). `auto_hop` mirror never re-synced (`170,198,2357-2361`). `lane_finder.rs` is the one healthy, pure, tested file.

**gui-panels** — Panels individually cohesive. Worked/heard/band re-derived in 3+ panels with inconsistent case-folding/keys (`bus_view.rs:385,:417,:430`; `contacts.rs:241-262`; scanner BandTally). `selected_station` two writers + reverse channel + separate `real_sel.target` (`panels/mod.rs:70-74`; `contacts.rs:217,210-214`). `origin: Mine|Peer` has no representation in MapSpot/HeardEntry (`bus_view.rs:77-116`). Contacts map keeps its own band selector independent of the waterslide (`contacts.rs:51,287-302`). `panel_data.rs` mixes live geo with dead prototype tables behind blanket `#![allow(dead_code)]` (`:3,77-447,461-582`). Duplicated haversine (`panel_data.rs:529` vs `call_sign.rs:167`). Call Sign panel under-delivers vs spec (no bearing/last-heard/SNR/CQ, `call_sign.rs:121-162`). Multiple now/clock sources in panels (`band_scan.rs:55-66,108-115`). Stale marker-shape doc comments (`bus_view.rs:81-91` vs `contacts.rs:797`).

**domain (logbook/archive/callbook/scanner/mocks/net)** — Individually tight. Worked-status in 3+ places with inconsistent keys (`scanner/lib.rs:80`, `bus_view.rs:417,:385`; none read `origin`). Scanner per-(band,mode) rule contradicts per-band Field-Day rule and GUI (`scanner/lib.rs:80,237,249-253` vs `bus_view.rs:421`). `net` introduces a second station-identity definition disconnected from `LogEntry.origin` (`net/lib.rs:66-69,204`); per-process `seq` resets to 0 on restart, peers drop a restarted op as stale (`net/lib.rs:154`; `peers.rs:56-61`); peer `StationSnapshot` has zero subscribers. `scanner.worked` grows unbounded with no reset path (`scanner/lib.rs:113-114,237-239`) — breaks the planned Field-Day reset. JSONL append plumbing duplicated (`logbook/lib.rs:105-125` vs `archive/lib.rs:136-156`). Mock mode serves no `ScannerCommand` — band-scan controls are a dead surface under `DM420_MOCK=1` (`mocks/lib.rs:279-311`).

**Cross-cutting lenses** corroborate the above: state-duplication (worked-status/mode/offset/band/clock/selection/identity all multi-owner); state-transition (open-loop TX as the deepest fault, scanner↔TX best-effort exclusion, config apply-on-edge); module-boundaries (dead catalog topics, GUI poking producer handles, domain logic in panels, net publishing into the void); consolidate-vs-split (the 9 consolidations and 6 splits above); coupling (TX fail-unsafe, no canonical owners, config bypass, fragmented clocks as the five highest-leverage amplifiers).

---

## Addendum — Mock Removal & Multi-Operator Sequencing

Two owner-supplied factors fold into the plan: (1) the `DM420_MOCK=1` path is essentially unused and is dead weight; (2) multi-operator (shared logbook + currently-working station + heard stations across LAN instances) is the *next* feature, and the worry is that "cleaning up dead code" could un-plumb concepts multi-op is about to need.

### The reframing: there are two different kinds of "dead"

The trap is reading the main report's "build the owner or delete the topic" as license to delete the unbuilt producers. It isn't — because two unrelated things both read as "dead," and they need opposite treatment:

| Kind | What it is | Examples | Action |
|---|---|---|---|
| **Implemented-but-unused** | code that runs but nothing needs | `mocks` crate, the mock spawn branch, `panel_data` prototype tables, the mock-only waterslide block | **DELETE / carve** |
| **Designed-but-unimplemented** | a contract defined in `types`/catalog with no producer yet | `EnrichedDecode`/`WorkedStatus`, `OperatingState`, `SessionCommand`, `StationSnapshot`/`HeardStation`/`WorkingTarget`, `origin: Mine\|Peer` threading | **BUILD (mostly)** — these are the review's own fixes *and* the multi-op substrate |

Almost every "dead topic" the main report flagged is the second kind. `networking.md` (the authoritative multi-op spec, design-status) shows they are load-bearing for the feature you're about to add — so the cleanup and the feature are one continuous arc, not sequential strangers. **`net` already subscribes `radio/{id}/decodes_enriched`** to derive shared heard-stations and **consumes `WorkedStatus::WorkedByNetwork`** for auto-pick exclusion — i.e. driver #2 (the enrichment producer) is simultaneously the local single-source fix and a hard multi-op prerequisite. Build it once, multi-op-aware.

### Mock removal — precise scope (verified)

De-risking facts confirmed against the tree: `mocks` is a dependency of **`crates/gui` only**; **no test or example** references `mocks::`/`DM420_MOCK`; the mock-vs-real choice is a single `else` branch (`bus_view.rs:225-229`); and `DM420_WAV` replay is wired into the **real** path (`settings.rs:784` → `core::spawn`), entirely independent of mocks.

**DELETE:**
- `crates/mocks/` (whole crate, 311 lines) + the `mocks.workspace = true` line in `crates/gui/Cargo.toml`.
- The mock branch `bus_view.rs:225-229` → always `core::spawn`; drop the `real` flag (`settings.rs:189`) and its accessors (`bus_view.rs:155,309`) and the `CoreConfig.real` plumbing — once mock is gone, `real` is always true.
- The "available in real mode — relaunch without DM420_MOCK=1" affordance (`waterfall.rs:782`) and any "real mode only" UI gating.
- The mock copies of `now_ms` / `fmt_snr` (also covered by Consolidate #1/#5 — they delete for free).

**CARVE (split live from dead — do not nuke the whole file):**
- `waterslide_panel.rs:160-319` — the mock-only parallel waterslide → delete; **keep** `Target` / `WaterslideTheme` / `martian_cmap[_light]` (live: used by `send.rs`, `waterfall.rs`).
- `waterslide_sim.rs` (398) — `mod` is declared but no `waterslide_sim::` use-site appears; confirm unreferenced, then delete or fold what survives into `waterslide_render.rs` (Split #1).
- `panel_data.rs` (709) — delete the prototype data tables behind `#![allow(dead_code)]` (`:77-447,461-582`); **keep** `Locator`, the live layout constants, and the `CALLSIGN_H` placeholder (still read by `chrome.rs`/`contacts.rs`/`call_sign.rs`/`settings.rs`/`bus_view.rs`/`waterfall.rs`).

**KEEP (not mock — do not touch under this banner):**
- `DM420_WAV` replay — real-mode bring-up / decoder-dev input; orthogonal to mocks.
- `geo_data.rs` (LAND_VERTS/IDX) — live map basemap.

**Screenshot path — RESOLVED:** daylight-color screenshots (`MARTIAN_SHOT`/`MARTIAN_LIGHT`) are captured against a **real radio**, not mock-seeded data — and the code path is already decoupled from `DM420_MOCK`. So mock deletes outright; no fixture-replacement feed is needed.

**Bonus:** removing the mock branch shrinks the `Option<AudioControl> == None` "silently no-op" surface that drivers #2 and #5 named as a divergence source. WAV still yields `control.audio = None`, but Phase 2c's `OperatingState` owner turns that into an *honest* "not applied" instead of a silent UI-vs-reality divergence.

### Multi-op substrate — what to KEEP & BUILD (don't un-plumb)

Cross-referencing `networking.md`'s wiring against the main report's "dead/unbuilt" list:

| Designed-but-unbuilt | Defined | Multi-op role (`networking.md`) | Driver | Verdict |
|---|---|---|---|---|
| `EnrichedDecode` / `WorkedStatus` (incl. `WorkedByNetwork(StationId)`) | `types` lib.rs:268 / 578 | `net` derives shared `HeardStation` from `decodes_enriched`; engine excludes `WorkedByNetwork` from auto-pick | #2 | **BUILD** — produce the network variant + `origin` from day one |
| `origin: Mine\|Peer` on `MapSpot`/`HeardEntry` | missing (`LogEntry` already has it) | peer log entries injected on `logbook/entries`; UI must render mine ≠ peer | #2/#8 | **BUILD/EXTEND** (multi-op step 2 UI) |
| `OperatingState` (mode/band; **posture → lease**) | `types` lib.rs:303 | upstream of `WorkingTarget.band` + snapshot `band_activity` | #2/#5 | **BUILD** (local owner; posture lives in the control lease, Phase 3b) |
| `SessionCommand` (SetMode/SetContest/TuneBand) | `types` lib.rs:325 | local config-as-bus-command — the clean replacement for the GUI poking `AudioControl` | #5 | **BUILD** (this *is* the driver-#5 fix), not delete |
| `StationSnapshot` / `HeardStation` / `WorkingTarget` (§9) | `types` lib.rs:684 + spec | the gossip vocabulary on `station/{id}/snapshot` (State) | net steps 1–3 | **KEEP** — `net`'s "zero subscribers" is *expected at step 1*; consumers land in steps 2–3 |
| `tx_report` consumer / closed TX loop | `types` / `core::tx` | trustworthy `qso/{id}/state` → trustworthy shared `WorkingTarget` | #1 | **BUILD** — an open-loop FSM would gossip *wrong* working-intent to peers |
| typed `Progress` FSM | `qso::engine` | same: correct published state for intent derivation | #4 | **BUILT ✅** (item 3a, `fd-progress-fsm`) |
| wildcard `State` late-join snapshot | `bus/handle.rs:391-406` | panels subscribe `station/*/snapshot` (**wildcard State**); a late-joining instance needs the current snapshot, not just future ones | bus finding | **RECLASSIFY → multi-op blocker** (was "latent, no live bug") — fix before wildcard snapshot consumers ship |

**DELETE/DEFER (genuinely dead, no multi-op role):**

| `logbook/query` (`LogbookQuery`) | Exists only in `bus/topic.rs` + `recorder.rs` — no producer, server, or consumer; log sync uses `net`'s anti-entropy digests, not this Command topic. | **DELETE (confirmed by owner)** during the Phase-4 catalog reconciliation — remove the topic from the taxonomy + recorder. |

### Revised refactor sequence (cleanup → multi-op as one arc)

Re-ordered from the main report to (a) pull dead-weight removal forward so every later phase edits smaller files, and (b) make Phase 2's single-owners *be* the gossip inputs, so multi-op is built on clean substrate rather than racing it.

- **Phase 0 — Safety** (unchanged): 0a watchdog/timeout ordering + `ForceUnkey`; 0b `Granter::revoke`.
- **Phase A — Dead-weight removal (NEW, pull early):**
  - **A1.** Delete the `mocks` crate + spawn branch + `real` flag → one always-real `core::spawn` path. *(Screenshots run against a real radio, so no fixture replacement needed — clean delete.)*
  - **A2.** Carve dead prototype tables from `panel_data.rs`; delete the mock-only waterslide block; audit/remove `waterslide_sim.rs`.
  - **A3.** Shared-utility pass (was 1b): `now_ms` / `Band::from_hz` / `OverAirMode↔Protocol` / format / `geo::distance_km` — also deletes the mock copies as a side effect.
- **Phase 1 — Close the loops:** 1a TX outcome loop (#1); 1c clock unification (#6).
- **Phase 2 — Single owners, built multi-op-aware (the substrate):** 2a `RigState`→`build_log` band/freq **+ single-source `station_id`** (from `config.toml` → `CoreConfig` → `net` + logbook; see the *Multi-Op Identity* addendum); **2b enrichment producer = `WorkedStatus`/`EnrichedDecode` carrying `origin` and emitting `WorkedByNetwork` from day one** (#2 + the `net` heard/auto-pick prerequisite); 2c `OperatingState` (mode+band) owner + `SessionCommand` config-as-command (#2/#5; **posture is owned by the Phase-3b control lease, not OperatingState**); 2d offset owner + publish `offset_locked` (#3, also fixes `WorkingTarget.offset`); 2e `Selection` owner — **per-radio, written by the focused panel** (#8, the single source for "currently-working station" → `WorkingTarget.call`; resolves the map-click-with-N-panels question — see the *Radio Ownership* addendum).
- **Phase 3 — Lifecycle hardening:** 3a ✅ **(done — `fd-progress-fsm`)** `Progress` enum + transition table (#4); 3b config commit decoupled from the lock edge + **rig-ownership posture promoted to a per-radio control lease that gates *tuning* (not only TX), with safe-park-on-loss** (driver #5 / lifecycle #5 + #6; see the *Radio Ownership* addendum) + the **operate⊥configure invariant** (unlock gated on radio-idle; arm/TX gated on locked — the *Configuring* lease posture; see the *Lock Posture* addendum; the GUI-disable layer can ship earlier).
- **Phase 4 — Bus hardening for gossip + catalog truth:** fix wildcard `State` late-join snapshot (`handle.rs:391-406`, now a multi-op blocker); reconcile `message-catalog.md` with reality (mark each formerly-"dead" topic BUILT or tag the build-step that builds it; delete only `logbook/query`).
  - **Lossless overflow contract reversed (done):** `StreamLossless` now backs its live tail with `tokio::broadcast`, so an overflowing subscriber is signalled `Lagged` and stays subscribed instead of being evicted (→ `Closed`). Fixes the logbook startup-replay burst deleting its own writer loopback (new contacts stopped persisting) and a stalled GUI pump. Contract change co-owned with W4LL; see `docs/bus-handoff.md`.
- **Phase 5 — Multi-op feature** (`networking.md` build order, now standing on clean owners): step 2 shared logbook (origin rendering — threading already in place from 2b); step 3 working-intent (`WorkingTarget` from the clean FSM + selection + offset owners); step 4 heard/band aggregation (straight off the 2b enrichment producer). *(Step 1, the `net` skeleton + §9 types + snapshot exchange, is already built.)*

The throughline: **Phase 2's owners are the gossip inputs.** `decodes_enriched` → peers' heard map; `qso/{id}/state` → `WorkingTarget`; `logbook/entries` + `origin` → the shared G-set; `offset_locked`/`Selection` → correct working-intent. Do the cleanup multi-op-aware and Phase 5 becomes wiring, not re-plumbing.

### Open questions for the owner

1. ~~Screenshot population~~ — **RESOLVED:** captured against a real radio; mock deletes cleanly.
2. ~~`logbook/query`~~ — **RESOLVED:** delete it in the Phase-4 catalog reconciliation.
3. ~~`seq` persistence~~ (`networking.md` open Q) — **RESOLVED (as shipped):** the log (`QsoId`) `seq` is seeded from the wall clock at startup (`qso/shell.rs:183`); `now_ms()` at launch always exceeds any prior session's seqs, so a restart never reissues an id a peer already merged — no sidecar, no log-max scan. *(Distinct **snapshot/beacon** `seq`* (`net/lib.rs:240`) — **DECIDED:** seed it from the wall clock too, `seq = max(now_ms(), last + 1)` (the floor keeps it strictly-increasing across same-millisecond beacons — e.g. the immediate intent-change snapshot landing on a tick — and immune to in-session backward clock steps). It's only ever compared *within* one peer's own stream, so the cross-operator "never a wall-clock" rule doesn't apply. Removes the ~30 s post-restart deconfliction blackout; folded into the shared-logbook `net` work.)

---

## Addendum — Radio Ownership & the Scan/Operate Contention (decided)

**Owner decision:** a **per-radio control lease** (foundational), with active band-scan implemented as a **mode of the Digital panel** (modal operate ↔ scan), and band-*activity display* split out as a passive, non-contending panel. The lease is designed so a **standalone Scan panel could hold it later** — the "maybe eventually" dedicated scanning receiver — but that panel is not built now.

### Diagnosis — one missing abstraction, several symptoms

The only arbiter today is the interlock granter's single **PTT (keying)** token. Two radio uses escape it:
- **VFO / RX tuning is not arbitrated at all.** The scanner issues `SetFreq` retunes "even when [PTT] denied" (lifecycle #5), so it retunes out from under an in-progress QSO. Keying is arbitrated; *tuning* is not.
- **There is no operating *session*.** A QSO owns the radio for its whole RX→TX→RX lifecycle; a sweep owns it for its duration. Nothing represents "this radio is busy," so the engine "is never told a scan owns the rig, so it keeps sequencing."

Both reported symptoms are the same gap: *scan-mid-QSO / operate-during-scan* = two would-be owners with no lease; *which panel snaps to a clicked map station* = selection has no per-radio owner (driver #8: two panels write `selected_station` every frame, last-writer-wins — identical race with two Digital panels).

### The per-radio control lease (foundational — under either UX)

Generalize the interlock from "who may key" to **"who controls this radio."** At any moment exactly one *controller* holds `radio/{id}` for operating/RX-tuning — covering **tune + key + decode-context** — with the PTT interlock nested inside as the fine-grained keying sub-arbiter. Publish it as `radio/{id}` State so every panel **and** the engine can see it and refuse to act when they don't hold it; the bus is already per-radio scoped. This is the review's "rig-ownership posture (Operate | Scanning | Configuring)" (lifecycle #5), **promoted to gate RX tuning, not only TX**. Add safe-park-on-lease-loss to also close the VFO save/restore-on-crash gap (lifecycle #6). This alone kills the "scanner retunes mid-QSO" class: `SetFreq` is rejected unless the caller holds the lease, and a sweep cannot hold it while a QSO does.

> **Design constraint (from "maybe eventually"):** the lease holder is a *controller*, not specifically "the Digital panel." Do **not** bake panel identity or "only an operating panel can own a radio" into the lease — so a future standalone Scan panel (or a dedicated scan-only rig/SDR) can hold the same lease without rework.

### UX now: active scan = a mode of the Digital panel

The Digital panel holds the lease for its bound radio and switches internally between **operate** and **scan**; they are mutually exclusive *by physics* on one receiver, so scan is a *mode of the radio*, not a peer instrument. Entering scan while a QSO is active is **gated on idle, exactly as unlocking-to-configure is** — you Stop the QSO first (which uses `Granter::revoke`, Phase 0b); scan never silently auto-aborts a live QSO. One uniform rule: **operate ⊥ {scan, configure}**. The saved operating frequency is the panel's mode-state, so restore-on-exit and crash recovery are local (folds in lifecycle #6). Per-radio by construction: two transceivers = two Digital panels, each independently operate-or-scan.

### Band-activity display = a passive panel (no radio ownership)

The "where's the action" view — aggregated from your own decodes, and later peers' heard-data (multi-op) — owns **no** radio and never contends. This is the legitimate "separate instrument," and where the dynamic-panel intent is satisfied without arbitration.

### Multi-panel selection / map-click

Make selection **per-radio, written by the focused panel** (panel focus already exists: `Cmd/Ctrl+1..5`). A map click targets the focused operating panel's radio (optionally QSYing it); if that radio can't reach the clicked band, prompt or no-op. This is driver #8 / Phase 2e generalized from "one Selection owner" to "one Selection owner *per radio*."

### How it slots into the plan

- **Phase 0b** — `Granter::revoke` is the clean abort path for both QSO-Stop and scan-cancel (already planned).
- **Phase 2e** — selection becomes per-radio + focused-panel (extends the single-owner work; resolves the map-click question).
- **Phase 3b** — promote the rig-ownership posture to the per-radio **control lease that gates tuning**, published per-radio, with safe-park-on-loss (folds in lifecycle #6). The lease holder is a generic controller (see constraint above).
- **Split #1 (interleaved in Phases 1–3, per the main report's Phase-4 note, as `waterfall.rs` is touched)** — active-scan becomes a **`scan_mode.rs` module extracted from `waterfall.rs`**, not a radio-grabbing peer panel. The `scanner` crate stays the pure sweep engine; `core::scan` must **acquire the lease** (not best-effort PTT) before any `SetFreq`, and deny the sweep if it can't.

---

## Addendum — Multi-Op Identity (`StationId` vs callsign; merge-key vs dupe-key)

**Owner decision:** `station_id` lives in `config.toml` (the operator-identity file, alongside `call`/`grid`), single-sourced from there to every consumer.

### "Me" is the `StationId`, never the callsign

`StationId(String)` is *"one operator/core instance — the unit of multi-op gossip"* (`types` lib.rs:53-55); the G-set key is `QsoId { origin: StationId, seq }` (lib.rs:571-574). The on-air callsign is QSO **content** and is deliberately allowed to be **shared** — a club call across all positions in a multi-op. Keying authorship on callsign would collide the moment two positions both send `W1AW`. Four identities to keep distinct:

| Identity | Shared in multi-op? | Role |
|---|---|---|
| **On-air callsign** | yes (club call) | QSO content; the DX logs it; config (`engine.me`), applied at TX + export |
| **Worked call** | n/a | `LogEntry.call` — per-contact data |
| **`StationId`** | **no** (unique per writer) | authorship/gossip; G-set key `(origin, seq)`; `origin == me` push-guard |
| **Human operator** *(future)* | no | optional `operator` field; two humans can share one instance |

### Two different "same" — must not be conflated

- **Same record** (idempotent gossip merge) → `(origin, seq)` — the G-set.
- **Same contact for scoring/dupe** → `(worked_call, band)` (mode excluded — Field Day; see the *Worked-Status Key* addendum) evaluated **over the merged log, across all origins** — the `WorkedStatus`/enrichment layer (driver #2). Two ops working K1ABC on 20m before gossip syncs = two *real records*, one *dupe*: the merge keeps both, the dupe-layer flags it. Build the dupe-key in the driver-#2 enrichment producer, **separate** from the G-set merge key.

### `station_id` in `config.toml` — DECIDED

- **Precedence:** `DM420_STATION_ID` env > `config.toml` > generated default.
- **Generate-once, write-back:** if absent, generate a collision-resistant, human-labelable default (friendly prefix + random suffix — **not** bare hostname or `<pid>`: hostname collides on a Field-Day Raspberry-Pi stack, `<pid>` changes every restart) and write it back so it's stable next launch.
- **Single source, threaded down:** `config.toml → CoreConfig.station_id → core::spawn` hands the *same* value to both `net` (gossip `from` + `origin == me` push-guard) and the logbook write-path (stamps `origin`). Retire `net`'s `default_station_id()` = `dm420-<pid>` (`net/lib.rs:66-91`) — today it is both unstable *and* a second identity definition disconnected from `LogEntry.origin` (review domain finding). One canonical id removes both bugs.
- **Immutable once set:** changing it re-parents future contacts and orphans prior ones (they'd read as *peer* to the new id); the config UI treats a change as a deliberate, warned action.

### seq high-water — wall-clock seed (no sidecar, no log scan)

The log `seq` is seeded from the wall clock at startup (`qso/shell.rs:183`), then incremented per contact. `now_ms()` at launch always exceeds any prior session's seqs, so ids stay unique across restarts with no sidecar and without scanning the replayed log for a max. *(The earlier plan — resume minting from `1 + max(seq where origin == me)` over the replayed log — would also have been collision-free; the wall-clock seed shipped instead, simpler and drift-proof since it reads nothing.)*

### Smaller gap to pre-empt

`LogEntry` records the *worked* `call` but not the call *you* used (`my_call`). Fine while the club call is uniform; needed for mixed-call operating and clean Cabrillo/ADIF export. Add `my_call: Callsign` (+ optional `operator`) with `#[serde(default)]` while the logbook schema is open in Phase 2 — cheap now, a migration later.

### Plan placement

Phase 2a: persist + single-source `station_id` (config.toml → CoreConfig → net + logbook). Phase 2b (driver #2 enrichment): the `(call, band)` dupe-key carrying `origin` (mode excluded — Field Day; see the *Worked-Status Key* addendum). Optional `my_call`/`operator` on `LogEntry` when the schema is touched.

---

## Addendum — Lock Posture & Config-While-Operating (decided)

**Owner decision:** the unlock (configure) toggle is **gated on the radio being idle**, and the underlying guarantee is a bidirectional invariant enforced by the control lease, not just a greyed button. Folds into driver #5 + the Phase-3b lease.

### Framing: "unlocked / configuring" is a posture that wants the radio

Changing callsign / device / mode / serial port is a *Configuring* activity, so config-while-operating is the **same conflict class** as scan-while-operating — two would-be owners of one radio. It is therefore not a new mechanism: *Configuring* is the third posture in the control lease (**Operate | Scanning | Configuring**, mutually exclusive). The lock toggle is the gateway into Configuring; gate it on "radio idle" exactly as entering Scan is gated.

### The invariant (bidirectional)

> **You operate only when locked; you configure only when idle.**

- **Disable unlock while not-idle** (the owner's ask) — can't start fiddling mid-QSO.
- **Block arm/TX while unlocked** (the other half) — can't reach a non-idle state from Configuring.

Together the bad state is unreachable. **Bonus:** this *dissolves* the driver-#5 "applies immediately vs. on re-lock" hazard for radio config — there is no window in which a live-applying setting can change under an active QSO, because unlocked and operating are mutually exclusive. **"Armed" counts:** gate on *any* non-idle engine state (armed / calling / in-exchange) + PTT-held + scan-running — i.e. "the control lease is held by Operate or Scanning."

### Two layers (defense in depth)

1. **Quick GUI layer (independent, low-risk, can ship early):** disable the unlock toggle whenever `QsoState.phase != Idle` (the GUI already subscribes to `QsoState`), plus PTT-held / scan-running, with a tooltip pointing at Stop (*"Stop the QSO to configure"*). Pure GUI, reads already-published state.
2. **Lease-enforced layer (Phase 3b):** make *Configuring* a lease posture the **producers** honor — reject a config-apply unless the caller holds the Configuring lease, and reject arm/TX unless locked. The disabled button is the friendly front door; the lease rejection is the actual lock on the door.

### Refinements so this doesn't over-restrict

- **Split radio/station config from app/display prefs.** Only callsign / grid / device / mode / port are posture-gated. Theme, layout, and the like are not radio-affecting and should not sit behind the lock at all — so gating unlock never traps you from changing cosmetics mid-QSO.
- **Operate-time adjustments are not behind the lock anyway** (TX offset via a waterslide click, etc.), so gating unlock removes no legitimate mid-QSO control.

### Stranded-user path (correct, not a problem)

If you genuinely must reconfigure mid-QSO: Stop first (→ `Granter::revoke`, Phase 0b) → idle → unlock re-enables. Consciously stopping before reconfiguring is the intended behavior.

### Plan placement

The quick GUI layer can ship **early and independently** (reads `QsoState`, no new infrastructure). The lease-enforced invariant lands in **Phase 3b** alongside the control lease (the Configuring posture is the same lease, third state). Both under driver #5.

---

## Addendum — Worked-Status Key (Field Day dupe rule) (decided)

**Owner clarification (ARRL Field Day):** the dupe rule is *once per band per mode-**category***, where the categories are CW / Phone / **Digital** — and **all** digital modes (FT8, FT4, PSK31, RTTY…) are the single Digital category. dm420 is all-digital, so the category is constant and the worked/dupe key collapses to:

> **Canonical worked-key = `(callsign, band)`** — mode (FT8 vs FT4 …) is **not** part of the uniqueness constraint. Working K1ABC on 20m FT8 makes K1ABC on 20m FT4 a **dupe**, not a new contact.

**Reconciles the inconsistent keys the review flagged — one canonical, both current directions wrong:**
- `scanner::Scanner.worked` keys `(call, band, OverAirMode)` (`scanner/lib.rs:80,237`) — **drop `OverAirMode`**; today it wrongly treats 20m FT4 as workable after 20m FT8.
- `bus_view::worked_spots` keys `(call)` globally (`bus_view.rs:385`) — **add band**; today one QSO marks a call worked on *every* band.
- `bus_view::worked_calls_on_band` keys `(call, band)` (`bus_view.rs:417`) — already correct; becomes a subscriber to the canonical owner.

**Two parts, both required (both already in the plan):**
1. **Same key shape** — `(call, band)` everywhere, owned once by the driver-#2 enrichment producer (`WorkedStatus`: `New | WorkedByMe | WorkedByNetwork`).
2. **Same inputs** — band via `Band::from_hz` (Consolidate #4) so a band-edge frequency can't classify differently per panel, and the callsign upper-cased/normalized before keying (the review noted inconsistent case-folding). Without these, `(call, band)` can still disagree across panels.

**Keep the rule contest-defined, not hardcoded.** Put it in a single `worked_key(entry, contest)` in the enrichment producer: Field Day (and the all-digital app) returns `(call, band)`; a future per-mode award view (e.g. "WAS on FT8") becomes a *different query over the log*, not a literal scattered across consumers. `ContestProfile` selects the rule in one place.

---

## Addendum — Panel Naming / Renames (decided)

Retire the historical panel names as part of the restructuring. The key distinction: a panel named **Digital** *contains* a view named **waterslide** — they are not the same thing, and "waterfall" is dropped entirely.

| Concept | Canonical name | Historical / current code |
|---|---|---|
| The primary operating panel | **Digital** panel | "Waterfall" / "Waterslide panel"; `panels/waterfall.rs`, `struct Waterfall` |
| The sideways-spectrogram + decoded-traffic view *inside* it | **waterslide** (a *view*, not a panel) | kept — view helpers (`waterslide_panel.rs` / `waterslide_sim.rs` → `waterslide_render.rs`) |
| The map panel | **Map** panel | "Contacts"; `panels/contacts.rs`, `struct Contacts` |

- **"Waterfall" is dropped as a panel name.** The **Digital** panel *contains* the **waterslide** view (the sideways spectrogram + decoded lanes). Keep "waterslide" only for that view — `waterslide_render.rs` is correctly named; the misleadingly-named `waterslide_panel.rs` collapses into it.
- **"Contacts" → "Map."**
- Apply the file/struct renames *during the splits already planned*: **Split #1** renames `panels/waterfall.rs` → `panels/digital.rs` (`struct Waterfall` → `Digital`); **Split #5** renames `panels/contacts.rs` → `panels/map.rs` (`struct Contacts` → `Map`). Folding the rename into the split keeps the file move + decomposition + identifier churn in one commit per panel.
- The rename also touches **non-code surfaces**: tile/panel titles, the `Panel` impls and any panel-kind / `Cmd/Ctrl+N` focus-target identifiers, and the docs (`CLAUDE.md`, `docs/waterslide_panel.md` — really the Digital panel's waterslide *view*, `docs/map_panel.md`). Update those in the same pass.
- **Register note:** elsewhere in this document, `waterfall.rs` / `contacts.rs` are the **current (pre-rename)** filenames cited as evidence; the *target* names are **Digital** / **Map** per this table.
