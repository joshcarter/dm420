# QSO Flow — Operator Model & Auto-Sequencing

How DM420 runs an FT4/FT8 contact end to end: what the operator does, what
the software does automatically, and how the QSO engine sequences messages.

**Baseline:** the on-air mechanics — the six message slots, slot/timing
alignment, content-driven state transitions, and the Field Day variant — follow
WSJT-X. See [`wsjtx_qso_sequencing.md`](wsjtx_qso_sequencing.md) for the
authoritative behavior reference. **Design rule: duplicate WSJT-X except where
we are specifically trying to improve.** This area will evolve; decisions marked
*(revisit)* below are deliberately deferred.

Bus message types referenced here live in
[`message-catalog.md`](message-catalog.md) §5 (Selection + QSO) and §9
(cross-station gossip).

---

## 1. The operator's two actions

In normal operation the operator only ever does one of two things, both from the
waterslide panel:

1. **Call CQ.** Select a part of the audio spectrogram with few signals, press
   **Enter**. The software calls CQ at that offset and works whoever answers.
2. **Answer a station.** Select an existing station, press **Enter**. The
   software *arms* to that station and answers it the next time it calls CQ —
   even if that station is currently mid-QSO with someone else.

There is also a recovery gesture for one specific case:

3. **Pick up a contact mid-stream (resume).** Click a decode that is *addressed
   to you* (`<my call> <their call> …`) and press **Enter**. Use this when a
   station answers a call you'd already disarmed from — e.g. you armed to them,
   they didn't reply, you disarmed to look elsewhere, and *then* their answer
   came in. Arming (action 2) wouldn't help here: it waits for the target's next
   **CQ**, which a station answering *you* won't send. Resume instead commits the
   contact at once, deriving your role and the next message from the clicked
   line's content (the same content-driven logic as §3), and answers in the
   opposite slot. A plain CQ is still action 2, not a resume.

Everything after that — slot timing, message selection, advancing the exchange,
logging — is automatic. The outgoing-message text box at the bottom of the panel
is normally pre-filled with the next auto-generated message (the CQ string or
the next message in the exchange).

**Enter is an arm/disarm toggle.** Pressing Enter again while armed (whether
armed to CQ or to answer, before or during a transmission) **disarms** and stops
transmitting. This is the single Stop control; there is no separate abort key in
v1.

### The text box

The box exists for two reasons beyond showing the queued message: typed
**slash commands** to the software (e.g. `/f 14.074`, `/b 20`) and, eventually,
freeform text for modes like PSK31. For v1 its behavior is:

- Leading `/` → interpret the rest as a software command (see
  [`radio_control.md`](radio_control.md)). We may include `:` as an
  alias to `/` for indicating commands.
- **Enter** → arm / send the queued message on the next interval (or disarm if
  already armed).
- Any other typed text → ignored.

There is intentionally **no manual hand-editing of FT8 message content in v1**
(no typing `AGN` / `PSE RPT`). Because sequencing is content-driven (§3), manual
nudges are rarely needed; this may be revisited.

---

## 2. Bus mapping

| Operator gesture | `Selection` (selection/{id}/active) | Command (qso/{id}/command) |
|---|---|---|
| Click empty spectrum + Enter | `outgoing = offset`, `target = None` | `CallCq` |
| Click a station + Enter | `outgoing = <set on their CQ>`, `target = Some(DecodeRef)` | `Start { target }` |
| Click a line addressed to us + Enter | `outgoing = their offset`, `target = Some(DecodeRef)` | `Resume { target, message, snr, offset }` |
| Enter again while armed | — | `Abort` |

The engine reflects progress on `qso/{id}/state` (`QsoState`); the panel renders
from it. The "send on the next interval" timing is the engine's job (QSO engine
+ clock), never the UI's — the UI emits a command and reflects state.

**Engine state.** Mirror WSJT-X's `m_QSOProgress`
(`CALLING, REPLYING, REPORT, ROGER_REPORT, ROGERS, SIGNOFF`) and re-derive
transitions from received content, not from an internal step counter (WSJT-X
interop checklist #4). Two implications for the catalog's `QsoPhase`:

- It needs an **`Armed { target }`** phase for the wait-for-CQ state in §4 —
  the catalog enum (`Idle, Calling, InExchange{step}, Complete, TimedOut`) has
  no equivalent today. *(open: §7)*
- `InExchange { step }` is **display-only** — the authoritative *internal* state is
  now the WSJT-X-style `Progress` enum the engine derives from content and carries on
  `Active.progress` (landed, item 3a). The **published** phase is still `step`;
  surfacing `progress` on the bus is a follow-up. *(was: flag to Joel)*

---

## 3. The two role flows

Both flows are the standard WSJT-X exchange; see
[`wsjtx_qso_sequencing.md`](wsjtx_qso_sequencing.md) §3 (normal) and §5 (Field
Day) for the per-slot message strings and the inbound-parse transitions we
match. Summarized:

**Answering a station** (we are W9XYZ, working K1ABC):
`Tx1 grid → (rcv report) → Tx3 R-report → (rcv RR73) → Tx5 73`. Log on RR73
received.

**Calling CQ** (we call, K1ABC answers):
`Tx6 CQ → (rcv grid) → Tx2 report → (rcv R-report) → Tx4 RR73 → (rcv 73)`. Log
on RR73 sent.

**Field Day** is *not* just "swap the report for the exchange." Two things change
versus the two flows above (`wsjtx_qso_sequencing.md` §5): the **grid step
(`Tx1`) is skipped**, and the **`RR73`/`73` roles — and therefore the logging
trigger — reverse**. The CQ string becomes `CQ FD …` (grid retained), the
exchange is `<count><class> <section>` (e.g. `3A WI`), and no signal report is
sent.

- **Answering a station** (we are W9XYZ, working K1ABC): open directly with the
  bare exchange — there is no grid opener —
  `Tx2 exchange → (rcv R-exchange) → Tx4 RR73`. **We** send `RR73` and **log on
  RR73 sent**; there is no final `73` to wait for.
- **Calling CQ** (we call, K1ABC answers):
  `Tx6 CQ FD → (rcv exchange) → Tx3 R-exchange → (rcv RR73) → Tx5 73`. We send
  the combined roger+exchange (`Tx3` = `R 3A WI`, one message that both rogers
  the partner and sends our own exchange) and the final `73`; **log on RR73
  received**.

We send `RR73` (accept both `RR73` and `RRR` inbound) whenever we hold the roger
slot — in Field Day that is the *answering* side, the mirror of normal mode.

**Slot alignment.** Transmit in the *opposite* T/R slot from the station we
heard (FT8 = 15 s, FT4 = 7.5 s). Match the six message strings exactly and
address the partner's base call in words 2–3 so a WSJT-X partner's
`auto_sequence()` accepts our decodes as "for us."

---

## 4. Answering: the armed / wait-for-CQ model

This is DM420's main departure from WSJT-X, which replies immediately on
double-click and never waits for a busy station. Our model:

1. Operator selects a station and presses Enter → engine enters
   **`Armed { target }`**. We publish our intent on the bus (§6) and otherwise
   stay **receive-only** — no transmissions while armed and waiting.
2. When the target **calls CQ**, we answer: snap our Tx offset to the target's
   offset (§5), transmit our opener in the opposite slot (`Tx1` grid in normal
   mode; `Tx2` exchange in Field Day, where the grid step is skipped — §3), and
   proceed through the exchange.
3. **If the target answers someone else** (we lost the race — their next Tx
   addresses a different call): **stop transmitting immediately** (good-citizen
   QRM avoidance, matching WSJT-X auto-stop) and **re-arm** to wait for their
   next CQ. We do not give up automatically.
4. **No arm timeout** — the operator is present and disarms with Enter. *(We
   may add one later if unattended operation matters.)*

While armed-to-answer, the radio is receive-only; "call CQ" and "answer" are
mutually exclusive — picking one is picking a mode, not queuing both.

> **Tail-ending** (calling a station the instant it sends `RR73`/`73` to its
> current partner, before it CQs) is **out of scope for v1** — *(revisit)* after
> observing more on-air Field Day traffic. v1 waits strictly for CQ.

---

## 5. Audio offset selection

- **Answering:** snap our Tx offset to the answered station's offset (reply
  zero-beat). This matches observed Field Day practice and WSJT-X's
  default ("Hold Tx Freq" off).
- **Calling CQ:** transmit at the clear offset the operator selected.
- **During an exchange:** hold the Tx offset for the whole QSO; the outgoing
  indicator locks while in `Calling` / `Armed` / exchange phases. A completed
  CQ-initiated QSO resumes CQ on the **same** held offset (§6).

> *(revisit)* A future version may auto-suggest an open part of the audio
> spectrum (least-occupied lane) from recent decode density instead of relying
> on the operator's pick.

---

## 6. Multiple callers, dupes, and cross-station coordination

**Multiple stations answer our CQ in one slot.** Auto-select the **highest-SNR**
caller that is **not a dupe** and **not being worked by another operator** on the
network. The panel must:

- Highlight **all** answering stations in the secondary highlight color.
- Allow manual override via **number keys**, ordered **top-to-bottom** (1 = top
  caller, 2 = next, …). A manual pick may override the dupe/peer exclusions.

**Dupe / worked status** comes from the merged logbook enrichment
(`WorkedStatus`): a station worked by *any* operator on the network counts as a
dupe for the group (Field Day is one group entry). Auto-pick excludes
`WorkedByMe` and `WorkedByNetwork`; manual selection may still work them.

**Cross-station coordination (bus).** Two responsibilities on the gossip layer
([`message-catalog.md`](message-catalog.md) §9):

1. **Publish our intent** — set `WorkingTarget` (radio, band, offset, call) the
   moment we arm to a station or commit to a caller, so peers avoid us.
2. **Consume peers' intent** — exclude a station another operator is currently
   working from our auto-pick (and flag it in the UI).

---

## 7. Lifecycle decisions (resolved)

| Topic | Decision |
|---|---|
| State derivation | Duplicate WSJT-X; re-derive from received content, not an internal counter. Evolves over time. |
| Lost the race (target works someone else) | Stop transmitting, re-arm, wait for next CQ. |
| Arm timeout | None (operator present). |
| Tx watchdog | None for now. |
| Enter while armed | Disarm / stop transmitting. |
| Repeat policy | Keep repeating the current message every period until content advances it or the operator disarms. |
| CQ with no answer | Keep calling CQ indefinitely. |
| When to log | **Normal:** `RR73` **received** (we answered) or `RR73` **sent** (we called CQ). **Field Day mirrors this:** we answered → log on `RR73` **sent** (our `Tx4`); we called CQ → log on `RR73` **received** (we then send the final `Tx5` `73`). |
| After completed QSO | Resume CQ if we started by calling CQ (same offset); go **idle** if we were answering a station. |
| `RR73` vs `RRR` | Send `RR73`; accept both inbound. |

## 8. Open / revisit-later

- **Tail-ending** vs. strict wait-for-CQ — needs more on-air observation (§4).
- **Incomplete QSO** (timed out mid-exchange): whether to log a partial or
  discard it — undecided.
- **Arm timeout / Tx watchdog** — deferred; may return for unattended use.
- **Auto-suggest an open Tx offset** — future (§5).
- **Manual FT8 message editing** in the text box — out of v1, may revisit (§1).
- **Protocol completeness** *(Joel seam)*: compound/nonstandard & hashed `<…>`
  callsigns (common in Field Day), FT4-at-launch vs. FT8-first, and a-priori
  (AP) decoding of the expected next message. Not interop blockers, but pending.
