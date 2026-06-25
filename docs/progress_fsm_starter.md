# Starter prompt — Progress FSM refactor (ARCHITECTURE_REVIEW.md item 3a)

This is a starter prompt for **Joel (W4LL)** to paste into a fresh Claude Code session
in the DM420 repo. It refactors the QSO sequencer into a typed `Progress` transition
table (behavior-preserving, test-first). Trim/adjust as you see fit — you know the
engine better than the prompt does. The load-bearing parts: **branch off
`fd-stabilization`**, **characterization tests before the refactor**, **exhaustive match /
no `_ => None`**, and **on-air-validate before merge**.

---

```
Task: refactor the DM420 QSO sequencer into a typed `Progress` transition table
(item 3a in ARCHITECTURE_REVIEW.md). Behavior-preserving, test-first.

ORIENT FIRST
- Repo: DM420 (digital-mode ham radio app). Work on a NEW branch off
  `fd-stabilization`, e.g. `fd-progress-fsm` (it's an active multi-dev branch;
  per-slice branches, merge when green + on-air-validated).
- Read CLAUDE.md (especially the "State ownership" section and the
  conventions/guardrails) and the `3a` item in ARCHITECTURE_REVIEW.md.
- Reference discipline: two slices just landed on this branch that model the
  test-first + green-gated, one-commit-per-step style to follow —
  `crates/core/src/worked.rs` (worked-status producer) and the TX-offset
  ownership in `crates/qso/src/engine.rs`. This task is a sibling engine refactor.
- Use the `ct` daemon (mcp__ct__* — search/outline/lookup) over grep/cat to
  navigate. `cargo` for builds/git.

THE PROBLEM
- The engine's QSO-sequencing decisions are scattered across ~4 functions in
  `crates/qso/src/engine.rs`: `commit_from_cq`, `commit_from_armed`,
  `advance_active`, `resume_from` (find their current locations with ct — the
  file moved recently; the review cited ~lines 444/577/697 plus log/snapshot
  sites ~958/981/1004, now stale). Each matches on role / inbound message kind /
  contest and ends in a silent `_ => None`, so an unhandled combination silently
  DROPS a QSO step instead of erroring.

THE GOAL
- Introduce a typed `Progress` enum + ONE transition table mapping
  `(role, contest, message-kind) -> (reply, next-state, log-action)`. Route all
  four sites through that single table. The match must be EXHAUSTIVE — remove the
  `_ => None` catch-alls so a missing transition is a COMPILE ERROR, not a
  silently dropped step.
- BEHAVIOR-PRESERVING: the engine must sequence QSOs (CQ -> reply -> report ->
  RR73 -> 73 -> log/idle, including the Field Day exchange) exactly as it does
  today. You are consolidating WHERE the decisions are made, not changing them.

DO IT TEST-FIRST
1. BEFORE refactoring, write characterization tests that pin the CURRENT
   sequencing across the matrix — each role (caller/answerer), each contest
   (Standard / ArrlFieldDay), each inbound message kind -> assert the
   (reply, next state, log) the engine produces today. Extend the existing engine
   test suite. Confirm they pass against the current code.
2. THEN introduce `Progress` + the table and route the four sites through it. The
   characterization tests must stay green — that is your proof of
   behavior-preservation. The only intended change is that formerly-silent cases
   are now explicit/compile-checked.

GUARDRAILS
- Do NOT change keying/PTT, slot timing, the TX path, the auto-QSY / TX-offset
  ownership (just landed in engine.rs), the worked-status producer, or any actual
  QSO outcome. Scope is the sequencing-decision consolidation only.
- Green bar before EVERY commit: `cargo build --workspace`,
  `cargo clippy --all-targets -- -D warnings`, `cargo test --workspace`.
- Commit per logical step (characterization tests; then the Progress table; then
  routing each site). Lowercase scoped subjects. Use `::core::` if you need std
  `core` inside `crates/core`.

YOUR CALLS (you own the engine)
- Shape the `Progress` enum and the table's key/value to fit how the FSM actually
  thinks — the `(role, contest, kind) -> (reply, next, log)` shape is a
  suggestion; adjust as the code wants. Decide whether `Progress` replaces or
  wraps the existing State/role types.

VALIDATION (required before merge)
- This is QSO sequencing: tests + an on-air check are the gate. Before merging to
  fd-stabilization, run real QSOs through the full sequence (CQ -> report -> RR73
  -> 73, including a Field Day exchange) and confirm the progression is identical
  to before.
```
