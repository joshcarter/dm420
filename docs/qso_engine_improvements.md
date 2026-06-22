# Proposal: a more robust QSO auto-sequencer (grammar + no-stall)

**Status:** proposal · **Owner:** (Joel/Josh) · **Crate:** `qso` (`crates/qso/src/engine.rs`)
**Driving goal:** survive Field Day — never sit on a dead frequency repeating an over,
never ignore a station that is trying to work us.

---

## TL;DR

The auto-sequencer is correct on the *happy path* (the `standard_answering_full_flow`
test passes on air — see the W4LL↔KD5CT QSO in Appendix B). It fails in two ways, both
of which we can see in our own decode archive:

1. **It doesn't reply when it should.** Not because the *text parser* fails — that
   handles 99 % of on-air traffic (Appendix B). It's because the **engine's state
   machine ignores valid messages it parsed perfectly well.** The biggest one: a
   station that answers our CQ with a *signal report* instead of a grid is dropped on
   the floor, and we keep calling CQ. A close second: a partner who signs off with a
   bare **`73`** (the single most common ending on the band — 27.9 % of QSOs) is not
   recognized as "we're done," so we never finish.

2. **It repeats the same over forever.** There is **no give-up anywhere** in a committed
   contact. If the partner stops answering (or we stop decoding them), the engine
   re-transmits the current over every single TX slot, indefinitely. We have two real
   examples in the archive: 5× `KN4WRX W4LL EM74` and a 13-slot churn with KF0IBB. The
   `QsoPhase::TimedOut` state that was designed for exactly this is **never emitted**.

The fixes are small and local — they live almost entirely in three functions of
`engine.rs` (`commit_from_cq`, `advance_active`, `tick_active`) plus one new field on
`Active`. This proposal specifies them, in priority order, each with a regression test.
Lower-value ideas (compound/hashed-call support, a table-driven refactor, multi-caller
pick) are collected in **§6 Suggestions** so they don't distract from the two things that
matter: **understand the grammar, and don't stall.**

---

## 1. How the engine works today (the 30-second version)

The engine (`crates/qso/src/engine.rs`) is a content-driven state machine. State:

```
Idle → Armed{target} → Active{role, …}        (answering a clicked CQ)
Idle → Calling{offset} → Active{role, …}      (we call CQ, someone answers)
```

`Active` carries a `role` (`CallingCq` | `Answering`), the `partner`, the queued
`next: Option<OutgoingMessage>`, and a few captured facts (`rcvd_report`, `rcvd_fd`,
`partner_grid`). Two driving methods:

- **`on_decode` → `commit_from_cq` / `commit_from_armed` / `advance_active`** — inbound
  decodes advance the contact by *replacing* `next`.
- **`on_tick` → `tick_active`** — at each slot boundary, if it's our T/R parity, we
  transmit `next`. **`tick_active` never clears `next`** — it re-sends the same over
  every slot until a decode changes it. That repetition is intentional (resend until
  acked); the bug is that **nothing ever caps it.**

The published phase is `QsoPhase { Idle, Armed, Calling, InExchange{step}, Complete,
TimedOut }` (`crates/types/src/lib.rs:437`). The engine only ever publishes the first
four; **`Complete` and `TimedOut` are dead.**

The decode *text* → typed `ParsedMessage` step happens earlier, in
`core::parse::parse_message` (`crates/core/src/parse.rs`). It is a separate concern and
mostly fine (§3.3).

---

## 2. Failure mode A — "doesn't reply" (sequencing-grammar gaps)

These are cases where `parse_message` produced a perfectly good `ParsedMessage`, but the
engine's `match` falls through to `_ => None` and the contact never advances.

### A1. A report-opener (skip-grid answer) to our CQ is ignored — *highest impact*

When we call CQ, `commit_from_cq` (`engine.rs:320`) only commits on two openers:

| Mode | Opener it accepts | Reply |
|---|---|---|
| Standard | `Exchange{ to=me, Grid }` | report (Tx2) |
| Field Day | `Exchange{ to=me, FieldDay{rogered:false} }` | R+exchange (Tx3) |

Everything else hits `_ => None`. So a station that answers with a **signal report**
(`W4LL K1ABC -09`) — the standard "skip Tx1" opening, very common in pile-ups, POTA and
contests — is dropped, and we just keep calling CQ. WSJT-X jumps straight to Tx3 here
(receive report → roger it + send ours). This is roadmap **Now-#11**.

### A2. A bare `73` is not accepted as completion — *highest impact*

On the **answering side**, the completion arm (`engine.rs:565`) only fires for
`is_roger` = {`RR73`, `RRR`}:

```rust
(Role::Answering, false, ParsedMessage::Signoff { kind, .. }) if is_roger(*kind) => { … }
```

A partner who closes with a plain **`73`** (`is_final` but not `is_roger`) falls through,
so we never log and **keep re-sending our R-report.** Appendix B shows **bare 73 is the
single most common explicit ending we hear (27.9 %, vs 17.8 % RR73).** This is not an
edge case — it is the modal case. The CQ side has the mirror gap: a partner who closes
with `RRR` instead of `73` is not recognized as final (`is_final` excludes `RRR`).

### A3. (revised) Opener-mode tolerance — *intentionally narrow*

The opener arms are guarded by `self.me.is_field_day()`. **In Field Day mode that guard is
correct and must stay:** a station answering our `CQ FD` with a plain grid or bare report
gives us no class+section, so the QSO scores **zero** ARRL FD points — and replying to a
non-FD station with our FD exchange just manufactures an exchange mismatch that never
completes (a *new* B1 stall). Ignoring non-FD callers in FD mode is the rate-protecting
filter we want; it matches roadmap Now-#1 ("filter to Field Day participants, class A–F
only"). So this is a feature, not the bug an earlier draft called it.

The only real gap is the **Standard-mode** mirror: when we are *not* running FD, an FD
station that answers our ordinary `CQ` with its exchange is a perfectly good (ordinary)
QSO we currently drop. That's minor and carries its own mild mismatch risk, so it lives in
§6 as a suggestion, not a core fix.

### A4. (minor) Parser-level gaps

Genuinely unparsed traffic is only **1.03 %** of decodes (Appendix B), and most of it is
un-actionable: telemetry hex, garbled decodes, hashed-call noise. Two patterns recur and
are worth a cheap parser arm:

- **`TO FROM R GRID`** (roger + grid, e.g. `0N2JZB 5B6EZM/R R CN00`) — 20 instances. The
  parser handles `R <class> <section>` (FD) but not `R <grid>`. One arm in
  `parse_directed`.
- **`<HASH> CALL`** (a nonstandard/compound station answering, e.g. `<...> W5C/D`) — 103
  instances. This needs real compound-call support and is deferred to §6.

---

## 3. Failure mode B — "repeats forever" (no give-up)

### B1. No retry cap anywhere — *highest impact*

`Active` has no notion of "how long have I been waiting." `tick_active` (`engine.rs:739`)
transmits `next` every TX slot and never gives up. If the partner goes quiet mid-exchange,
we transmit the same over until the operator intervenes. Two real archive examples:

```
W4LL ↔ KN4WRX   Grid×5, never completed     (we re-sent "KN4WRX W4LL EM74" 5 times)
W4LL ↔ KF0IBB   Grid/Report churn, 13 slots (we re-sent "-17"×3 then "-08"×4)
```

The fix is the one the roadmap already scoped (Now / "retry-timeout"): **after N
unanswered overs, give up and fall back.** The `QsoPhase::TimedOut` variant already
exists for it.

### B2. The CQ side never releases the terminal `RR73`

Standard CQ-side end-game (`engine.rs:585`): on the partner's R-report we queue `RR73`
and log on send (`log_on_tx`), but we set **no `finish_after_tx`**. So after the QSO is
already in the log, `next` stays `RR73` and we transmit it every slot, **waiting forever
for a courtesy `73` that the data says usually never comes** (52 % of QSOs we reconstruct
have no final sign-off at all). WSJT-X disables Tx after sending RR73; we should resume CQ.

This is a special case of B1 and the same give-up machinery fixes it — but because the
QSO is *already logged*, its cap should be tighter (1–2 sends, then resume CQ).

---

## 4. Proposal

Four changes, priority-ordered. P1–P3 are the load-bearing ones; all are small and
local. Each names its site and its regression test (test style mirrors the existing
`mod tests` in `engine.rs`).

### P1 — Give up after N unanswered overs (wire `TimedOut`)  ⟶ fixes B1, B2

Add one field to `Active` (`engine.rs:115`):

```rust
/// TX slots we've transmitted the current `next` without the contact advancing.
overs_since_progress: u8,
```

- **Reset to 0** wherever a decode advances the contact (every arm in `commit_from_*`,
  `resume_from`, and `advance_active` that assigns `a.next = Some(…)`).
- **Increment** in `tick_active`, just before transmitting `next`.
- **Trip** when `overs_since_progress >= cap`: instead of transmitting, fire the
  give-up — publish `QsoPhase::TimedOut` for one snapshot, then
  `Finish::ResumeCq` if `role == CallingCq` (or if we were running CQ), else `Idle`
  (re-arm to `target` if we have one). This reuses the existing `Finish` plumbing.

```rust
// cap is mode-aware: FT8 slot = 15 s, FT4 = 7.5 s.
const TX_CAP_DEFAULT: u8 = 4;   // ~60 s FT8 / ~30 s FT4 before falling back
const TX_CAP_AFTER_LOG: u8 = 2; // once logged (terminal RR73), bail sooner
```

For **B2**, the same counter applies, but once `a.logged` we use `TX_CAP_AFTER_LOG`; the
CQ side stops re-sending `RR73` after a slot or two and resumes CQ. (Equivalently: give
the post-R-report `RR73` a `finish_after_tx = Some(Finish::ResumeCq)` with a tiny grace
count — either is fine; the counter is more uniform.)

**Tests:**
- `times_out_after_n_unanswered_overs`: answer our CQ with a grid, we send report; feed
  *no* further decodes; tick N+1 times; assert phase passes through `TimedOut` and lands
  in `Calling` (resumed CQ).
- `cq_side_releases_rr73_after_log`: drive a standard CQ-side QSO to the RR73 (logged);
  feed no `73`; assert we resume CQ within `TX_CAP_AFTER_LOG` slots instead of looping.

**Open knobs (call them, then close them):** N default (4 proposed), whether `Answering`
times out to `Idle` vs re-`Armed`, and whether to surface "timed out → resumed CQ" in the
waterslide tag. Recommend: N=4, `Answering`→`Idle`, show a brief `TIMEOUT ▸ {call}` tag.

### P2 — Accept any directed sign-off as completion  ⟶ fixes A2

Generalize the rule to: **a directed `Signoff` from our partner (`RRR` | `RR73` | `73`)
ends the QSO**, on whichever side we're on. Concretely, in `advance_active`:

- **Answering side:** broaden the completion guard from `is_roger(*kind)` to *any*
  `Signoff` addressed to us by `partner` → log (if not yet), send a single courtesy `73`,
  go `Idle`. (Receiving a bare `73` means they consider us worked.)
- **CQ side:** broaden the final guard from `is_final(*kind)` to *any* `Signoff` → log (if
  not yet), `resume_cq`.

This collapses A2 and the `RRR`-as-final gap into one consistent rule and matches WSJT-X
(`message_is_73` treats `RR73` as a `73`; receiving any roger/73 at/after the report
exchange completes the QSO). The `is_roger`/`is_final` helpers stay for the *log-trigger
vs courtesy-73* distinction, but they no longer gate *whether we recognize completion*.

**Tests:**
- `answering_completes_on_bare_73`: standard answering flow, but partner closes with
  `Signoff::Seven3` (not `Rr73`); assert we log and go `Idle`.
- `cq_side_completes_on_rrr`: standard CQ flow, partner closes with `Signoff::Rrr`; assert
  we log and resume CQ.

### P3 — Accept report/R-report openers when calling CQ  ⟶ fixes A1 (roadmap Now-#11)

In `commit_from_cq`, add an arm after the Grid arm (Standard mode):

```rust
// A caller skipped the grid and answered with a signal report (Tx2-style).
// Roger it and send our report (Tx3) — WSJT-X's skip-ahead.
ParsedMessage::Exchange { to, from, payload: ExchangePayload::Report(r) }
    if to == &self.me.call && !self.me.is_field_day() =>
{
    let reply = message::roger_report(&self.me, from, snr);
    self.state = State::Active(Box::new(Active {
        role: Role::CallingCq, partner: from.clone(), target: None, offset,
        tx_parity: parity, next: Some(reply), finish_after_tx: None,
        log_on_tx: false, logged: false, step: 2,
        partner_grid: None, partner_snr: snr,
        rcvd_report: Some(*r), rcvd_fd: None,
        overs_since_progress: 0,
    }));
    None
}
```

`advance_active`'s existing CQ-side arms then carry it home (their `RR73` → log + resume,
now via P2). This is the exact behavior the roadmap pre-specified, including the test:

**Test** (`calling_cq_caller_answers_with_report_not_grid`, per `JOELS_ROADMAP.md:266`):
call CQ → feed `exch(ME, HIM, Report(0))` at snr −8 → assert `InExchange` and next TX is
`K1ABC W9XYZ R-08` → feed `Signoff::Rr73` → assert we log (`exchange_sent == "-08"`,
`exchange_rcvd == "+00"`) and resume CQ.

*(Optional, same shape: accept an `R-report` opener too — rare as a true opener, but the
manual `resume_from` path already handles it, so the live path is inconsistent without
it.)*

### P4 — One cheap parser arm: roger + grid  ⟶ fixes A4 (the `R GRID` case)

In `parse_directed` (`crates/core/src/parse.rs:76`), add before the FD arms:

```rust
["R", g] if is_grid(g) => ParsedMessage::Exchange {
    to, from,
    payload: ExchangePayload::RogerGrid(GridSquare((*g).to_string())),
},
```

This needs a new `ExchangePayload::RogerGrid` (or reuse `Grid` with a `rogered` flag).
Low urgency (20 instances, VHF/rover idiom), but it's a two-line parser change and stops a
real template from vanishing into `Free`. The engine can treat `RogerGrid` like a grid for
sequencing.

---

## 5. Why this is the right altitude

- **It's a grammar/idiom fix, not a rewrite.** P1–P3 touch three functions of `engine.rs`
  and add one field; P4 is a two-line parser arm. They make the engine *recognize messages
  it already parses* and *stop when the other guy stops* — exactly the two complaints.
- **It's data-driven.** Every change maps to something we actually heard on the air
  (Appendix B), not a hypothetical.
- **It moves the `Complete`/`TimedOut` states from dead code to load-bearing**, which the
  networking layer also wants (`TODO_NETWORK.md:86` maps `TimedOut → None` for working-
  intent gossip).

---

## 6. Suggestions (lower priority — nice, not necessary)

These improve the engine but are **not** required to fix the stalls. Listed so they're
captured, not so they bloat the core work.

- **Field Day is deliberately exclusive (do not relax).** In FD mode we *intentionally*
  ignore openers that aren't a valid class+section exchange (see A3) — auto-working non-FD
  callers dilutes points and risks mismatch stalls. Optional nicety: surface an ignored
  non-FD caller as a muted "heard · non-FD" tag so the operator can choose to work one by
  hand during a lull, but never automatically.
- **Standard-mode cross-mode answers.** When *not* running FD, accept an FD-exchange answer
  to our ordinary `CQ` as a normal QSO instead of dropping it (the only surviving half of
  the original "mode-tolerant openers" idea). Low value, and needs care so the cross-mode
  exchange still terminates.
- **Answer-immediately toggle.** Today `Armed` only commits on the target's *next* CQ
  (`commit_from_armed`); if they answer someone else or never re-CQ, we never fire. Offer a
  mode that answers the clicked CQ in the next slot (WSJT-X "Call 1st"). Already roadmapped.
- **Multi-caller auto-pick.** When several stations answer our CQ in one slot, pick the
  best (highest-SNR non-dupe, later: not being worked by a peer) instead of whichever
  decode commits first.
- **Compound/nonstandard & hashed calls.** `<HASH> CALL` answers (103 in the archive) and
  `/P`, `/R`, `PJ4/K1ABC`-style calls need real Type-4 support: dehash from recently-heard
  full calls, and never let our own opener go out hashed. Meaningful for DX/Field-Day
  specials; sizable change (touches `modes` + `parse` + `qso`).
- **Preserve, don't drop, exotic types.** Telemetry (Type 0.5) and the EU-VHF/RTTY/contest
  types currently vanish; keep them as `Free`/`Raw` for the archive even when the
  sequencer ignores them (mostly already true).
- **Table-driven transitions.** The `(role, mode, msg)` arms in `advance_active`/
  `commit_*` are hand-enumerated and easy to leave a hole in (that's how A1/A2 happened).
  A small transition table keyed on `(role, contest, received-kind)` → `(reply, next-step,
  log-trigger)` would make gaps structurally visible and shrink the code. Worth doing
  *after* P1–P4 land and the tests pin behavior.
- **SNR-tracked reports.** We recompute the report each over (the `-17`→`-08` drift in the
  KF0IBB churn). Latch the first report sent so a repeated over is byte-identical (also
  helps any future WSJT-X-style "Tx text changed" watchdog reset).
- **A time-based watchdog** in addition to the over-count cap (WSJT-X uses ~6 min), as a
  belt-and-suspenders backstop on truly stuck states.

---

## 7. Testing plan (new regressions, all in `crates/qso/src/engine.rs` `mod tests`)

| Test | Pins |
|---|---|
| `calling_cq_caller_answers_with_report_not_grid` | P3 / A1 |
| `answering_completes_on_bare_73` | P2 / A2 |
| `cq_side_completes_on_rrr` | P2 |
| `times_out_after_n_unanswered_overs` | P1 / B1 |
| `cq_side_releases_rr73_after_log` | P1 / B2 |
| `parse_roger_grid` (in `core::parse` tests) | P4 / A4 |

The existing `standard_answering_full_flow` / `standard_calling_cq_full_flow` must keep
passing unchanged — they pin the happy path these changes must not regress.

---

## Appendix A — The QSO decode analyzer

**`crates/archive/tools/qso_grammar.py`** (added with this proposal; pure stdlib,
read-only). The `archive` crate appends every heard `Decode` and every sent `TxLogEntry`
to `decodes.jsonl`; this tool is the offline lens on that archive that produced all the
evidence above. It does three things:

1. **Learns the grammar.** Every raw message is abstracted to a *token template* —
   `K1ABC W9XYZ R-09` → `CALL CALL R-RPT`, `CQ POTA W5DOC EL09` → `CQ MOD CALL GRID`. Each
   template is tallied and tagged **PARSED / UNPARSED** by whether dm420's own parser
   classified it (the archived `structured` field) or fell back to `Free`/`Raw`. The
   UNPARSED templates *are* the parser's blind spots.
2. **Reconstructs QSOs.** Directed messages are grouped by unordered callsign pair and
   segmented by slot gaps; each QSO's message sequence and *ending* (`RR73`/`RRR`/bare-73/
   none) is recorded. The spread of endings is what the sequencer must tolerate.
3. **Detects stalls.** For our station (`--me`, default `W4LL`): runs of an identical
   non-CQ over re-sent with no progress (the "repeats forever" failure), and openers
   addressed to us that we never answered (the "doesn't reply" failure).

```sh
# default archive (~/.dm420/decodes.jsonl), or pass a path:
python3 crates/archive/tools/qso_grammar.py /Users/Shared/decodes.jsonl --me W4LL
# sections: --grammar / --qsos / --stalls   (default: all)
```

It is deliberately decoupled from the live path — it reads only the archive, so it can
grow (SQLite export, per-band breakdowns) without touching the radio code. This is the
"data analysis" half of roadmap Now-#2.

---

## Appendix B — Learned grammars (run over 19,423 messages)

Source: `decodes.jsonl`, 19,375 heard + 48 sent, mostly 20 m FT8.

### B.1 What the sequencer actually sees (`ParsedMessage` variants)

```
   6909   35.6%  Exchange/Grid
   4880   25.1%  Cq
   2979   15.3%  Exchange/Report
   2062   10.6%  Exchange/RogerReport
   1137    5.9%  Signoff/Rr73
   1021    5.3%  Signoff/Seven3      ← bare 73
    235    1.2%  Signoff/Rrr
    151    0.8%  Free                ← unparsed
     49    0.3%  Raw                 ← empty/garble
  → 200 (1.03%) fell through to Free/Raw — invisible to the auto-sequencer.
```

### B.2 The grammar, as templates (the learned grammar)

Top templates, all **PARSED** unless noted — this is the on-air FT8/FT4 grammar the app
encounters, ranked by frequency:

```
   6703  CALL CALL GRID        e.g. 'YB4SJA KF9UG EN71'     (answer / Tx1)
   3828  CQ CALL GRID          e.g. 'CQ K0DTM EN34'         (CQ)
   2841  CALL CALL RPT         e.g. 'YE5TA K6AGA -04'       (report / Tx2)
   1957  CALL CALL R-RPT       e.g. 'VE7DSS N9EP R-04'      (roger+report / Tx3)
   1062  CALL CALL RR73        e.g. 'WV4O XE2SSB RR73'      (roger / Tx4)
    961  CALL CALL 73          e.g. 'KG5OWB KE8WAV 73'      (sign-off / Tx5)
    815  CQ MOD CALL GRID      e.g. 'CQ POTA W5DOC EL09'    (directed/contest CQ)
    231  CALL CALL RRR         e.g. 'N4MIC W2C RRR'         (roger)
    205  <HASH> CALL GRID      e.g. '<...> W8UCO EN80'      (nonstd call, parses)
    185  CQ CALL               e.g. 'CQ W1AW/0'             (CQ, no grid)
    815± CALL <HASH> …         compound/hashed exchanges (mostly parse)
```

### B.3 The grammar gaps (UNPARSED templates — the parser's blind spots)

```
    103  <HASH> CALL           e.g. '<...> W5C/D'       nonstandard station answering  → §6
     49  <empty>               e.g. ''                  empty/garble decode            → ignore
     20  CALL CALL R GRID      e.g. '0N2JZB 5B6EZM/R R CN00'  roger+grid (rover/VHF)   → P4
      ~25 (long tail)          telemetry hex, split rovers, corrupt decodes           → ignore
```

**Takeaway:** the parser is not the problem — 1.03 % unparsed, and most of that is
un-actionable garble/telemetry. The recognizable misses are `<HASH> CALL` (deferred, §6)
and `CALL CALL R GRID` (cheap, P4). The real wins are in the **sequencer** (§4 P1–P3).

### B.4 How real QSOs end (3,073 reconstructed QSOs with ≥2 messages)

```
   1611   52.4%  no sign-off seen   (incomplete capture, or genuinely no final 73)
    857   27.9%  bare 73            ← most common explicit ending
    548   17.8%  RR73
     57    1.9%  RRR
```

This is the empirical case for **P2**: a robust sequencer must treat a bare `73` (and
`RRR`) as completion on *both* sides — it's the modal ending, and the engine ignores it on
the answering side today.

### B.5 Our own QSOs and stalls (`--me W4LL`)

```
QSOs involving W4LL: 4
  W4LL↔KD5CT    end=73  Grid→Grid→Grid→Grid→Report→RogerReport→RogerReport→Rr73→Seven3→Seven3
  W4LL↔KN4WRX   end=—   Grid→Grid→Grid→Grid→Grid                         ← STALL (B1)
  W4LL↔KF0IBB   end=—   Grid→Report→Report→Grid→Report→…(19 msgs, 13 slots)← STALL (B1)
  W4LL↔K0DTM    end=—   Grid→Grid                                         (lost/incomplete)

Stall detector (repeated overs, no progress):
   3×  'KN4WRX W4LL EM74'  [Exchange/Grid]   over ~4 slots
   3×  'KF0IBB W4LL -17'   [Exchange/Report] over ~4 slots
   4×  'KF0IBB W4LL -08'   [Exchange/Report] over ~6 slots
```

- **KD5CT** is the happy path working end-to-end — and it completed precisely because the
  partner sent **`RR73`** (which we handle).
- **KN4WRX / KF0IBB** are failure mode B exactly: we re-sent the same over with no
  give-up. KF0IBB also shows the report drift (`-17`→`-08`) that motivates the
  latch-the-report suggestion in §6. Neither would have stalled with **P1** in place.
```
