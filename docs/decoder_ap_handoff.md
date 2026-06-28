# A-priori (AP) decoding — design + status

> **Status: Phase-2 prototype landed, behind `DM420_AP` (default OFF).** Context-free
> CQ / CQ-FD hypotheses implemented and unit-tested; directed (MyCall) hypothesis
> implemented, needs `DM420_AP_MYCALL`. Measured ON/OFF on the crowded corpora — see
> the end. This doc is the map for finishing it.

## Why AP is the lever

We proved (`decoder-sensitivity` notes, the `jt9 -d1/-d2/-d3` depth sweep) that DM420's
**blind** decoder is already at/above jt9's blind ceiling on a crowded band (≈3% gap vs
`jt9 -d1`). The entire crowded-band gap vs `jt9 -d3` (~24% on FT4, ~350 decodes) is jt9's
**a-priori decoding**: when a blind decode fails, jt9 retries with the bits of a
*hypothesized* field clamped to strong log-likelihoods and lets the CRC accept/reject.
AP is therefore the single highest-value decoder feature for Field Day. It is **separate
from** and **composes on top of** the blind path (coherent + subtraction + OSD + the
noise-relative finder).

## How WSJT-X does it (`lib/ft8/ft8b.f90`)

Passes 1–5 are blind. Passes 6+ are AP. For an AP pass it sets `apmask` (which of the 174
codeword bits are fixed) and overlays `llrz(masked) = apmag * pattern`, with
`apmag = 1.1·max|llr|`, then re-runs the LDPC decode + CRC. Hypotheses (`iaptype`), gated
by `nQSOProgress` (QSO state) and `ncontest` (3 = ARRL Field Day):

| iaptype | fixes | needs |
|---|---|---|
| 1 | addressed call = `CQ`/`CQ FD`/… (bits 1:29) + msg type (75:77) | nothing (context-free) |
| 2 | addressed call = **MyCall** | own callsign |
| 3 | + DX call = **partner** (bits 1:58) | own + partner call |

The Field-Day `mcqfd` pattern and the `ncontest==3` masks are the FD-specific bits.

## How it maps onto DM420 (implemented in `crates/modes/src/ap.rs`)

- **Codeword layout.** DM420's LDPC is systematic: `log174[0..77]` are the 77 message
  bits (`decode_std` reads `n29a` = addressed call + flag from bits `[0,29)`; the `i3`
  message-type bits are `[74,77)`), `[77..91]` the CRC, `[91..174]` parity. `bp_decode`'s
  convention is `s > 0 ⇒ bit 1` (`ldpc.rs:71`), so a `1` bit clamps to `+apmag`, `0` to
  `−apmag`.
- **Deriving the clamp values from our own encoder.** Instead of transcribing WSJT-X's
  `mcqfd` arrays, each hypothesis encodes a *template* message with DM420's
  `encode_message` and reads the fixed field's bits back out
  (`payload_to_codeword_bits`, which applies FT4 whitening so the clamp is in codeword
  space). This is self-consistent with our decoder and correct for both modes by
  construction.
- **The hook.** `ap::try_ap(log174, protocol)` runs only as a **CRC-gated retry after a
  blind miss** — wired at the tail of `decode_candidate` (magnitude) and both
  `decode_candidate_coherent{,_ft4}` (on the first/full-integration LLR variant). Blind
  runs first and unchanged, so AP can only **add** decodes, never regress one. Same CRC
  gate as the blind path is the false-decode control.
- **Knobs.** `DM420_AP=1` enables it (default OFF). `DM420_AP_MYCALL=<call>` adds the
  directed hypothesis. Mirrors the existing `DM420_BASELINE/COHERENT/SUBTRACT/OSD` pattern.

### Hypotheses implemented
1. **CQ FD** — fix addressed-call=`CQ FD` + type. Context-free; the dominant FD case.
2. **CQ** — fix addressed-call=`CQ` + type. Non-FD callers.
3. **MyCall** (only if `DM420_AP_MYCALL` set) — fix addressed-call = my call (bits
   `[0,29)`), type left free so one hypothesis covers both standard replies and FD
   exchanges sent to me.

## What remains (next session)

- **DxCall / partner hypothesis (iaptype 3)** — fix both calls during an active QSO;
  needs the partner call threaded from the `qso` engine / `ContestProfile`. Biggest
  remaining AP yield once in a QSO.
- **Live context wiring** — in the app, `mycall`/grid come from `Station` config and the
  partner from the QSO engine; thread those into `decode_streaming` (today AP reads env).
  The `qso` crate already tracks `nQSOProgress`-equivalent state to gate which hypotheses
  run (WSJT-X uses `nappasses`/`naptypes` for this).
- **Hashed-callsign AP** — resolve `<...>` hashed compound calls against the session
  `CallHash` table and feed as priors.
- **Per-candidate cost** — AP adds `hypotheses × bp_decode(30)` per blind miss; broaden
  from "first LLR variant" to all variants only if the recall/cost trade justifies it.
- **False-decode audit at scale** — AP is the main false-decode risk; the CRC gate holds
  in measurement, but watch `ours-only` as hypotheses grow, and consider the WSJT-X
  `napwid` frequency gate for the directed types.

## Measurement (`ab_jt9`, `DM420_AP` ON vs OFF)

See the session report / the parent thread for the live numbers. Method:
`DM420_AP={0,1} AB_MODE={ft4,ft8} ab_jt9 sample_data/wsjtx_ft{4,8}_crowded`, plus
`DM420_AP_MYCALL=<inferred station call>` for the directed pass. Guardrail: `DM420_AP=0`
is byte-identical to the pre-AP blind path (FT8 `wsjtx_ft8` floor 787/16% holds).
