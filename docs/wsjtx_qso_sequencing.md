# WSJT-X QSO Auto-Sequencing — Behavior Reference

**Purpose:** Document the automated QSO message flow (CQ → answer → report →
roger → 73) so an independent FT4/FT8 implementation can interoperate with
WSJT-X's auto-sequencer. Covers the normal flow, the ARRL Field Day flow, FT4
vs. FT8 differences, and a map of where every special-event flow lives in the
source for future duplication.

All paths below are in `widgets/mainwindow.cpp` / `widgets/mainwindow.h` unless
noted.

---

## 1. The big picture

WSJT-X's automation is a small state machine driven by *received* decodes. The
operator only ever makes one of two decisions:

- **Call CQ** (select/transmit `Tx6`), or
- **Answer a station** (double-click a decode → starts at `Tx1`).

From there, every incoming decode addressed to the local station advances the
machine one step and selects the next message to send. When a QSO finishes the
machine logs it and (if it was a CQ) returns to calling CQ — hence the
"hands-off" feel.

Three pieces of state do all the work:

| Member | File | Meaning |
|--------|------|---------|
| `m_QSOProgress` | `mainwindow.h:761` | enum: `CALLING, REPLYING, REPORT, ROGER_REPORT, ROGERS, SIGNOFF` |
| `m_ntx` / `txrb1..6` | — | which of the six Tx message slots is queued next |
| `m_specOp` | `mainwindow.h:759` | special-activity mode (normal vs. contest, see §6) |

### Where the logic lives

| Concern | Function | Approx. line |
|---------|----------|--------------|
| Should an auto-reply fire at all? (gating, auto-stop) | `auto_sequence()` | 7473 |
| Decide the response to a received message (the state machine) | `processMessage()` | 8895 |
| Generate the six standard message strings (`Tx1`–`Tx5`) | `genStdMsgs()` | 9541 |
| Generate the CQ string (`Tx6`) | `genCQMsg()` | 9443 |
| Select/queue a Tx slot | `setTxMsg()` | 9432 |
| Reset state at QSO end / clear DX | `clearDX()` | ~9760 |

`auto_sequence()` is the *gatekeeper*: it is called for every decode, checks
the auto-seq checkbox, frequency tolerance, and whether the message is "for us,"
and only then calls `processMessage()`, which is the actual *state machine*.

---

## 2. The six message slots

For local station **W9XYZ** (grid EM48) working **K1ABC**, the standard 77-bit
messages (FT4/FT8/FST4/Q65/MSK144) are:

| Slot | Content | Example | Role |
|------|---------|---------|------|
| `Tx1` | `<his> <mine> <grid>` | `K1ABC W9XYZ EM48` | Answer a CQ |
| `Tx2` | `<his> <mine> <report>` | `K1ABC W9XYZ -07` | Send signal report |
| `Tx3` | `<his> <mine> R<report>` | `K1ABC W9XYZ R-09` | Roger + report |
| `Tx4` | `<his> <mine> RRR`/`RR73` | `K1ABC W9XYZ RR73` | Roger |
| `Tx5` | `<his> <mine> 73` | `K1ABC W9XYZ 73` | Sign off |
| `Tx6` | `CQ <mine> <grid>` | `CQ W9XYZ EM48` | Call CQ |

Reports are formatted `%+2.2d` (e.g. `-07`, `+03`). `RR73` vs `RRR` for `Tx4`
is controlled by `m_send_RR73` (a user setting); `RR73` is the FT8/FT4 default.
Compound/nonstandard calls get `<...>` hashed-call variants (the `t0a`/`t0b`
forms in `genStdMsgs()`), and a confirming `<full call> 73` `Tx5` is generated
when only the base call was known earlier.

---

## 3. Normal FT8/FT4 flow

Two symmetric roles. State shown is the local station's `m_QSOProgress`.

### 3a. As the station answering a CQ (W9XYZ answers K1ABC)

```
            K1ABC transmits          W9XYZ transmits        W9XYZ state after
  ──────────────────────────────────────────────────────────────────────────
  (1)  CQ K1ABC FN42
  (2)  [double-click his CQ]   →   K1ABC W9XYZ EM48  (Tx1)   REPLYING
  (3)  K1ABC W9XYZ -12         →   K1ABC W9XYZ R-09  (Tx3)   ROGER_REPORT
  (4)  K1ABC W9XYZ RR73        →   K1ABC W9XYZ 73    (Tx5)   SIGNOFF  → log
```

Decision points in `processMessage()`:
- Double-clicking the CQ → "just work them": `m_ntx=1`, `m_QSOProgress=REPLYING`.
- Receiving a bare numeric report (`-50..49`) while `>= CALLING` → `setTxMsg(3)`,
  `ROGER_REPORT` (the "no grid on end of msg" branch, ~9320).
- Receiving `RR73`/`RRR`/`R...` while `>= ROGER_REPORT` → send `73` (`Tx5`),
  `SIGNOFF`, and `logQSOTimer.start(0)` (auto-log) or
  `cease_auto_Tx_after_QSO()`.

### 3b. As the station calling CQ (W9XYZ calls, K1ABC answers)

```
            K1ABC transmits          W9XYZ transmits        W9XYZ state after
  ──────────────────────────────────────────────────────────────────────────
  (1)                              CQ W9XYZ EM48     (Tx6)   CALLING
  (2)  W9XYZ K1ABC FN42        →   W9XYZ K1ABC -12   (Tx2)   REPORT
  (3)  W9XYZ K1ABC R-09        →   W9XYZ K1ABC RR73  (Tx4)   ROGERS
  (4)  W9XYZ K1ABC 73          →   [log QSO, → Tx6 CQ again] SIGNOFF
```

Decision points:
- Receiving `<me> <him> <grid>` (4th word matches `grid_regexp`) in normal mode
  → `setTxMsg(2)`, `REPORT` (~9080; the contest modes branch elsewhere — see §6).
- Receiving `R<report>` while `>= REPORT` → `setTxMsg(4)`, `ROGERS` (~9230).
- Receiving `73`/`RR73` while `ROGERS` → log, then `m_ntx=6` (back to CQ).

> Note the asymmetry: the **answering** station replies with grid in `Tx1`, gets
> a report, sends `R+report` (`Tx3`); the **CQ** station gets the grid, sends a
> report (`Tx2`), gets `R+report`, sends `RR73` (`Tx4`). A complete exchange is
> CQ → grid → report → R-report → RR73 → 73.

### Gating, auto-stop, and timing (`auto_sequence()`)

A reply is only generated when **all** hold:
- `m_auto` is on **and** the `cbAutoSeq` checkbox is visible/enabled/checked;
- the decode is "for us" (contains `m_baseCall` / your call / your DX call, or a
  type-2 compound `DE ...` reply on your Rx/Tx offset);
- you are not already past the relevant point (`!m_sentFirst73`, etc.).

`auto_sequence()` also performs an **auto-stop**: if you are replying/reporting
and you hear your QSO partner answering *someone else* (3rd word is a different
base call) within `stop_tolerance` Hz, it clicks Stop to avoid QRM
(`mainwindow.cpp:7510`).

**TX/RX cadence:** transmissions are slot-aligned. `m_txFirst` is set from the
decode time: `nmod = timeInSeconds() mod (2 × TRperiod)`, `txFirst = (nmod != 0)`
— i.e. you answer in the *opposite* slot from the station you heard. TR periods:
**FT8 = 15 s, FT4 = 7.5 s** (also FST4 variable, Q65 variable, MSK144 fast).

### Tx audio frequency offset on answering

When you answer a station, WSJT-X may move your Tx **audio offset** to match the
station you are working — but this is **not** unconditional. The decision is in
`processMessage()` (`mainwindow.cpp:9005`):

```cpp
QString firstcall = message.call();
if(firstcall.length()>=4 and firstcall.mid(0,3)=="CQ ") firstcall="CQ";
if(!m_bFastMode and (!m_config.enable_VHF_features() or m_mode=="FT8" or m_mode=="FT4" or m_mode=="FST4")) {
  // Don't change Tx freq ... also not if a station is calling me, unless CTRL or SHIFT
  if ((Radio::is_callsign(firstcall)
       && firstcall != m_config.my_callsign() && firstcall != m_baseCall
       && firstcall != "DE")
      || "CQ" == firstcall || "QRZ" == firstcall || ctrl || shift) {
    if (((SpecOp::HOUND != m_specOp) || m_mode != "FT8")
        && (!ui->cbHoldTxFreq->isChecked() || shift || ctrl)) {
      ui->TxFreqSpinBox->setValue(frequency);   // Tx offset := the decoded station's offset
    }
    ...
```

where `frequency = message.frequencyOffset()` is the **audio offset** of the
decoded message (not the dial/RF frequency).

Behavior:

- **"Hold Tx Freq" unchecked (the out-of-the-box default — `HoldTxFreq` defaults
  to `false`, `mainwindow.cpp:1969`):** answering a `CQ`/`QRZ` (or working some
  other callsign) sets your Tx offset equal to that station's offset, i.e. you
  reply zero-beat.
- **"Hold Tx Freq" checked:** your Tx offset stays where it is — you reply on
  your own fixed offset.
- **Shift or Ctrl held while double-clicking:** overrides Hold Tx Freq and forces
  the Tx offset to move to the station's offset.

Two important caveats:

1. **Only when *you* initiate.** The Tx offset follows for `CQ`/`QRZ` or when
   working a *different* callsign. When a station **answers your CQ**
   (`firstcall` == your call), it is explicitly excluded, so your Tx offset does
   **not** jump to the caller — you keep transmitting where you called CQ.
2. **Mode/feature gating.** The whole block is skipped in fast modes, and when
   VHF features are enabled unless the mode is FT8/FT4/FST4. For plain FT8/FT4 it
   always applies. Fox/Hound (DXpedition) uses its own Tx-frequency rules
   entirely — randomized/fixed (`mainwindow.cpp:7790–7827`).

> Interop consequence: a WSJT-X operator answering your CQ with default settings
> transmits on **your** offset (zero-beat to your CQ), but a substantial fraction
> of operators run with "Hold Tx Freq" checked and reply on their own offset. Do
> **not** assume a reply returns on the offset you called CQ on — decode across
> the full passband.

---

## 4. FT4 vs. FT8 differences

FT4 and FT8 share the **same 77-bit message set** and the **same state
machine** — `is77BitMode()` (`mainwindow.cpp:15356`) groups
`FT8, FT4, MSK144, FST4, Q65`, and `genStdMsgs()` builds identical strings for
both. The differences are operational:

| Aspect | FT8 | FT4 |
|--------|-----|-----|
| T/R period | 15 s | 7.5 s |
| Default `Tx4` | `RR73` | `RR73` |
| Auto-seq fall-through | returns early after `genStdMsgs` | **not** early-returned: `if (auto_seq && !m_bDoubleClicked && m_mode!="FT4") return;` (`mainwindow.cpp:9426`) lets FT4 continue to the `quick_call` path |
| Setup hook | — | `chkFT4()` (15265) enables `cbAutoSeq` + `respondComboBox`, sets the special-op label |
| RTTY Roundup `TU;` | n/a | FT4 tracks `m_dateTimeRcvdRR73` / `m_dateTimeSentTx3` to allow a combined `TU; <next call>...` `Tx3` when double-clicking a new caller right after `RR73` (`genStdMsgs` ~9637) |
| NCCC Sprint | — | FT4 + `NA_VHF` + `NCCC_Sprint()`: after sending `Tx3` it auto-logs and drops out of auto-Tx (`processMessage` ~9105) |

For pure interop, treat FT4 and FT8 messages identically; only the symbol timing
and 7.5 s slot alignment differ at the protocol level.

---

## 5. ARRL Field Day flow (`m_specOp == FIELD_DAY`)

Field Day replaces the signal-report exchange with the Field Day exchange
(**transmitter count + class letter + ARRL/RAC section**, e.g. `3A WI`),
configured via **Settings → Advanced → Special operating activity → Field Day**,
stored as `m_config.Field_Day_Exchange()`.

### Message differences

- **CQ** (`genCQMsg()` ~9491): the contest tag `FD` is inserted →
  `CQ FD W9XYZ EM48`.
- **`Tx2`/`Tx3`** (`genStdMsgs()` ~9614): `sent = Field_Day_Exchange()` instead
  of a signal report. So:
  - `Tx2` = `K1ABC W9XYZ 3A WI`
  - `Tx3` = `K1ABC W9XYZ R 3A WI`  (note the space: `sent != rpt` → `"R " + sent`)
- `Tx1`, `Tx4` (`RR73`), `Tx5` (`73`) are unchanged.

### Flow

```
            K1ABC transmits          W9XYZ transmits        W9XYZ state after
  ──────────────────────────────────────────────────────────────────────────
  (1)                              CQ FD W9XYZ EM48  (Tx6)   CALLING
  (2)  W9XYZ K1ABC EN37        →   W9XYZ K1ABC 3A WI (Tx2)   REPORT
  (3)  W9XYZ K1ABC R 2B IL     →   W9XYZ K1ABC RR73  (Tx4)   ROGERS
  (4)  W9XYZ K1ABC 73 / (none) →   [log, → Tx6 CQ]           SIGNOFF
```

### Inbound parsing (`processMessage()` ~9050)

A received decode is recognized as a Field Day exchange by `bFieldDay_msg`:

```cpp
QString t0 = t.at(n-2);            // second-to-last word, e.g. "3A" or "R"
QString t1 = t0.right(1);          // last char, the class letter
bool bFieldDay_msg = (t1 >= "A" && t1 <= "F"   // class A..F
                      && t0.size() <= 3        // e.g. "3A", "12E"
                      && n >= 9);              // enough tokens
int m = t0.remove(t1).toInt();
if (m < 1) bFieldDay_msg = false;  // transmitter count must be >= 1
```

When `bFieldDay_msg` and `FIELD_DAY` mode (~9166):
- received exchange begins with `R` (`t0 == "R"`) → `setTxMsg(4)`, `ROGERS`;
- otherwise (plain exchange) → `setTxMsg(3)`, `ROGER_REPORT`.

If a Field Day exchange is received while **not** in Field Day mode, WSJT-X pops
a "Should you switch to ARRL Field Day mode?" prompt
(`processMessage()` ~9035) but does not switch automatically.

> Interop notes: the class letter is `A`–`F`; the transmitter count must be
> ≥ 1; the section is the final token. `n >= 9` means the parser expects the
> full `<call> <call> <count><class> <section>` decode plus the leading
> time/SNR/freq metadata columns of `clean_string()` — i.e. it counts the whole
> decoded line, not just the message words. Match the on-air message
> `K1ABC W9XYZ 3A WI` and `K1ABC W9XYZ R 3A WI` and you will sequence correctly.

---

## 6. Map of all special-event flows (for future duplication)

The special-activity selector is a single enum; every contest flow is a set of
small conditionals keyed off it. To add or mirror another flow, touch the same
five sites.

**The enum** — `Configuration.hpp:318`:

```cpp
//                              0      1       2         3       4      5       6    7        8          9
enum class SpecialOperatingActivity {NONE, NA_VHF, EU_VHF, FIELD_DAY, RTTY, WW_DIGI, FOX, HOUND, ARRL_DIGI, Q65_PILEUP};
```

Aliased as `SpecOp` in `mainwindow.h:119`; the active value is `m_specOp`, loaded
from `m_config.special_op_id()`. The same numbering is exposed over the UDP API
(`Network/NetworkMessage.hpp:195`), so external software can read the mode.

| Site | Function / file | What to change per flow |
|------|-----------------|-------------------------|
| **CQ tag** | `genCQMsg()` ~9483 | `m_cqStr`: `FD`, `RU`, `WW`, `TEST` (and `Contest_Name()` for a custom tag) |
| **Exchange content** | `genStdMsgs()` ~9610 | what `sent` becomes: grid (`NA_VHF`/`WW_DIGI`/`ARRL_DIGI`/`Q65_PILEUP`), `Field_Day_Exchange()`, `RTTY_Exchange()` + RST, EU-VHF serial+grid |
| **Inbound parse + transitions** | `processMessage()` ~9040–9230 | `bFieldDay_msg`, `bRTTY` (529–599), `bEU_VHF_w2` (520001–594000) detectors and their `setTxMsg`/`m_QSOProgress` branches |
| **UI label / setup** | `chkFT4()` 15265 (and equivalents for FT8) | the `labDXped` text per mode |
| **Logging columns** | `on_contest_log_action_triggered()` + `logbook/` | contest log fields |

Configuration accessors for the exchanges: `Field_Day_Exchange()`,
`RTTY_Exchange()`, `Contest_Name()`, `Individual_Contest_Name()`, and
`sbSerialNumber` (serial-number contests: EU VHF, RTTY DX). All in
`Configuration.hpp` ~105.

### Quick reference per flow

| Mode | CQ tag | Exchange (`Tx2`/`Tx3`) | Detector range |
|------|--------|------------------------|----------------|
| `NONE` (normal) | — | signal report `-07` | report `-50..49` |
| `NA_VHF` | `TEST` | grid `EM48` | grid regexp; `setTxMsg(3)` directly to `ROGER_REPORT` |
| `EU_VHF` | `TEST` | `<RST> <serial> <grid>` | `520001..594000` |
| `FIELD_DAY` | `FD` | `Field_Day_Exchange()` e.g. `3A WI` | class A–F, count ≥1 |
| `RTTY` | `RU` | `<RST> <state/serial>` | RST `529..599`; supports `TU;` |
| `WW_DIGI` | `WW` | grid | grid regexp |
| `ARRL_DIGI` | `TEST` | grid | grid regexp |
| `Q65_PILEUP` | (none) | grid | grid regexp |
| `FOX` / `HOUND` | special | DXpedition mode — see below | — |

### Fox/Hound (DXpedition mode) — separate, more invasive

`FOX`/`HOUND` is *not* a simple exchange variant; it changes the whole TX/RX
discipline (Fox transmits first and may send multiple slots/streams; Hound never
transmits first and must be called). It is woven throughout `mainwindow.cpp`
(`foxTest()`, `m_specOp == FOX/HOUND` guards in `auto_sequence()`,
`processMessage()`, `clearDX()`), plus `Network/FoxVerifier.*`. Treat it as a
distinct protocol if you implement it, not a tweak of the normal flow.

---

## 7. Interop checklist for an independent implementation

1. **Match the six message strings exactly** (word order, `R` placement,
   `RR73`/`73`). WSJT-X's parser keys off word position
   (`message_words.at(2)` = your call, `at(3)` = partner, `at(4)` = grid/report).
2. **Send in the correct slot.** Answer in the opposite T/R slot from the
   station you heard; FT8 = 15 s, FT4 = 7.5 s.
3. **Address the partner's base call** in words 2–3 so `auto_sequence()` accepts
   the decode as "for us."
4. **Drive transitions by content, not by your own internal step** — WSJT-X
   re-derives state from each decode, so a correctly-formatted `R-report` will
   advance it regardless of what it expected.
5. **For Field Day**, send `CQ FD ...`, then `<count><class> <section>` as the
   exchange, and `R <count><class> <section>` for the roger; finish with `RR73`
   then `73`.
6. The QSO state is also fed to the Fortran decoder (`commons.h:30`
   `nQSOProgress`) for a-priori ("AP") decoding, which improves weak-signal
   decoding of expected replies — not required for interop, but explains why
   WSJT-X decodes the *expected* next message slightly better.
