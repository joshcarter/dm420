# Closing the FT8 decode-sensitivity gap

**Status:** Phase 1 (OSD) + Phase 2 (subtraction) done. **Owner:** TBD.
**Metric:** the `ab_jt9` example.

> **Progress** (controlled A/B on 24 real busy-band FT8 slots vs `jt9 -d 3`):
> - Baseline (no enhancements): matched **450**, gap **52%**.
> - + Phase 1 OSD (`3f14b7d`): matched **471**, gap **50%**.
> - + Phase 2 subtraction (`d2e0128`): matched **480**, gap **49%**.
>
> No rise in false decodes across either phase (`ours-only` steady at 5).
> Subtraction (+9) came in below the masking bucket's promise: it **converges by
> pass 3** (6 passes == 3), so the limiter is fit *quality* on close-spaced
> signals, not pass count — a per-symbol fit cross-talks when two signals sit a
> few Hz apart, which is exactly the masking case. The likely next lever is a
> frequency-refined, more-global fit (see Phase 2 notes). Per-slot FT8 decode
> ~20 ms → ~120 ms, well inside the ~2 s window; FT4 is single-pass and unaffected.

## The problem, measured

On 17 real busy-band 20 m slots (the clean `sample_data/wsjtx_captures/gain_0.5`
set — `gain_1.0` clips on every slot and was rejected), our decoder recovers
**~49 % of what `jt9 -d 3` decodes** (315 vs 641), as a near-perfect *subset*:
only 2 of our decodes in 17 files were ones `jt9` missed. So accuracy is fine;
we are simply **half as sensitive** on crowded bands.

Reproduce the baseline any time:

```sh
export JT9_BIN=/Applications/wsjtx.app/Contents/MacOS/jt9
cargo run -p modes --example ab_jt9 -- sample_data/wsjtx_captures/gain_0.5
```

### What we miss, by SNR (328 missed signals)

```
        ≤ -20:   7
     -20..-15:  74
     -15..-10:  91
      -10..-5: 100   ← largest bucket
       -5..0:  47
   ≥ 0 dB:      9
```

A clean, isolated FT8 signal decodes with plain belief-propagation down to
roughly **−18 dB**. So the split by *mechanism* is:

- **≥ −10 dB (≈48 %): should have decoded and didn't → masking.** A louder
  neighbor within a few Hz corrupts the soft symbols. Fix: **multi-pass
  subtraction**.
- **< −15 dB (≈25 %): genuinely below BP's reach → sensitivity.** Fix: **OSD**.
- **−15..−10 (≈27 %): mixed**, helped by both.

This matches the synthetic `crowd_recall` finding (masking is the single largest
mechanism) and extends it (real bands add a deep-weak tail synthetic scenes
didn't expose). Both upgrades are justified; neither alone closes a 51 % gap.

## Where the decoder stands (attach points)

All in `crates/modes/src`. FT8 is **LDPC(174, 91)**, systematic — codeword =
91 message/CRC bits + 83 parity (`ldpc.rs` `N=174 M=83 K=91`, `encode174`).

Single-pass pipeline, `decode.rs::decode_streaming` (l.435):

1. `Monitor::new` + `process` → builds `Waterfall.mag` (a `Vec<u8>`, **magnitude
   only — phase is discarded**) from the time-domain `samples`.
2. `find_candidates` (l.272) → Costas sync over the grid, keep top
   `MAX_CANDIDATES=140` with score ≥ `MIN_SCORE=10`.
3. Per candidate: `extract_llr` → `normalize_llr` → `bp_decode(&llr, 25)`
   returns `(plain, min_errors)`; `decode_candidate` (l.393) **drops the
   candidate the instant `min_errors > 0`**, then CRC-checks.
4. Dedup by payload (`HashSet<[u8;10]>`), unpack, emit.

Two facts shape the work:

- **The `min_errors > 0` drop at `decode_candidate:397` is the exact OSD hook.**
  When BP gets close (small nonzero `min_errors`) we throw the result away; OSD
  re-derives a codeword from the reliable bits instead.
- **`Waterfall.mag` is magnitude-only**, so subtraction cannot operate on the
  waterfall — it must re-synthesize the decoded signal in the *time domain*,
  subtract from `samples`, and rebuild the `Monitor`. We already have the
  forward synthesizer (`encode::synth_ft8` / `ft8_tones`).

## Plan

Sequenced for incremental, independently-measurable delivery. **Re-run `ab_jt9`
after each phase** — the aggregate gap % is the gate.

### Phase 1 — OSD backstop ✅ done (commit `3f14b7d`)

Implemented in `modes/src/osd.rs`, wired at `decode_candidate`. Order-1 + order-2
(pairs among the 20 least-reliable basis bits); best few candidates handed to the
CRC gate. `DM420_OSD=0` disables it. Result: +21 matched on the 24-slot FT8 set,
no false-decode increase. Possible later tuning: `OSD_MAX_ERRORS` gate, `LAMBDA`,
`CRC_TRIES`.

**Why first:** self-contained, ~400–600 lines, *no pipeline refactor*. It bolts
onto the LLRs `bp_decode` already consumes and the systematic generator we
already have (`LDPC_GENERATOR`). Lower risk than subtraction, and it pays for
itself on the ~25 % deep-weak tail immediately.

**Algorithm (order-1/2 reprocessing, the `osd174_91` / `ft8_lib osd.c` method):**

1. In `decode_candidate`, when `bp_decode` returns `min_errors` in
   `1..=THRESH` (start ~), run OSD on the *normalized LLRs* instead of
   returning `None`.
2. Sort the 174 bit positions by `|LLR|` descending (most reliable first).
3. Form the generator in these permuted columns and Gaussian-eliminate to get
   91 independent most-reliable basis positions (systematic re-encode set).
4. Hard-decide those 91, re-encode to a full codeword. Then **order-`i`
   reprocessing**: flip small combinations of the least-reliable of the 91
   (order 1, optionally 2), re-encode each, and keep the codeword with the
   smallest soft (weighted-Hamming) distance to the LLRs.
5. CRC-check the winner exactly as today (`crc::extract/compute_crc`). Accept on
   match.

**New code:** `modes/src/osd.rs` (the reprocessing + a GF(2) eliminate over the
generator). Reuse `LDPC_GENERATOR`, `crc`, `pack_bits`. Wire one call site in
`decode_candidate`.

**Risks / notes:** OSD order-2 is O(91²) re-encodes per candidate — cap the
order and gate it on `min_errors ≤ THRESH` so we only pay it on near-misses, not
all 140 candidates. A subtly wrong column-elimination silently loses dB — see
validation.

**Expected:** recovers a meaningful slice of the < −12 dB tail; modest help in
the −15..−10 mix.

### Phase 2 — multi-pass subtraction ✅ v1 done (commit `d2e0128`)

Implemented in `modes/src/subtract.rs`; `decode_streaming` now loops decode →
subtract → rebuild waterfall → decode (FT8 only; `DM420_SUBTRACT=0` off,
`DM420_SUBTRACT_PASSES` tunes). v1 uses a **per-symbol** complex-gain fit with a
±480-sample timing search. Result: matched 471 → 480, no false-decode rise.

**Why v1 underdelivered, and the higher-quality fit (Phase 2.1).** The +9 is below
the masking bucket's promise because a per-symbol fit **cross-talks for
close-spaced signals** — two tones a few Hz apart aren't orthogonal over one
0.16 s symbol, so fitting the strong signal partly absorbs (and partly leaves) the
weak neighbor: exactly the masking case we care about. Over the *full* 12.6 s
transmission they separate cleanly (3 Hz × 12.6 s ≈ 38 cycles), so the fix is a
**frequency-refined, more-global fit** (what WSJT-X's `subtractft8` does):
1. **Refine frequency to sub-Hz** before fitting — a small search (or quadratic
   interpolation of the sync correlation) around the candidate's ±1.5 Hz `f0`.
2. With accurate `f0`, fit a **global complex gain** (or a lightly-smoothed
   slow-varying gain to still track fading), so close neighbors stay orthogonal
   and the subtraction is clean.
3. Re-measure with `ab_jt9`; converged by pass 3 already, so this is pure quality.

Compute budget is ample (~120 ms/slot now vs a ~2 s window), so the extra search
is affordable. This is the most promising remaining lever on the masking bucket.

**Why second:** biggest mechanism bucket (~48 % masked), but it needs a pipeline
refactor and time-domain re-synthesis with amplitude/phase/frequency fit — more
moving parts. Its value is *multiplied* once OSD exists, because each subtraction
pass ends in a BP+OSD decode of a cleaner residual.

**Refactor:** pull "build `Monitor` → `find_candidates` → decode loop" out of
`decode_streaming` into a reusable `fn decode_pass(samples) -> Vec<(Decode,
Candidate)>`. Drive it from a loop:

```
passes = 3
for _ in 0..passes:
    results = decode_pass(&residual)          # strongest-first
    emit newly-seen results
    for (decode, cand) in results:
        wave = resynth(decode, cand)          # estimated f0, dt, complex gain
        residual -= wave                       # subtract from time-domain audio
    if nothing new this pass: break
```

**The hard part — `resynth` / fit.** We have `synth_ft8` (the right tones), but a
clean subtraction needs the received signal's **frequency, time offset, and a
per-symbol complex amplitude** (WSJT-X fits a smoothed complex gain across the
~79 symbols). Start simple — one global complex gain estimated by correlating
`synth_ft8` against `samples` at the candidate's `(freq_hz, dt)` — then refine to
per-symbol gains if residual energy is too high. This estimation quality *is* the
feature; a poor fit leaves residual that masks as badly as the original.

**New code:** `modes/src/subtract.rs` (fit + subtract); refactor in `decode.rs`.

**Expected:** the large recovery — the ≥ −10 dB "should-have-decoded" misses,
plus weak signals freed by removing their maskers.

### Phase 3 — a-priori (AP) decoding (defer)

Couples the decoder to the `qso` crate's contact state (expected next message).
Narrow payoff, real integration cost. Revisit only if Phases 1–2 leave a gap and
the QSO-engine hooks exist. Not scheduled.

### Cheap knobs to test alongside (low effort)

- Raise `MAX_CANDIDATES` / lower `MIN_SCORE` and re-measure — probably *not* the
  bottleneck (140 ≈ ft8_lib default) but it's a one-line experiment.
- Confirm `LDPC_ITERS=25` isn't starving convergence vs WSJT-X.

## Validation (the part that actually matters)

Translation is easy; proving we didn't silently lose dB is the work.

1. **No regressions:** `cargo test -p modes` (the `fixtures_decode` cross-checks
   against ft8_lib-generated WAVs must still pass) and `crowd_recall` recall must
   not drop.
2. **Real gain:** `ab_jt9` aggregate gap % on the gain_0.5 set after each phase.
   Target trajectory: 51 % → meaningfully lower after Phase 1 → close to `jt9`
   after Phase 2.
3. **No false decodes:** watch `ours-only` in `ab_jt9` — it is 2 today. OSD and a
   bad subtraction fit are the two things that can manufacture false decodes; the
   CRC gate should hold them to ~0. Any rise is a red flag.
4. **Capture more corpus** at different bands/times so the metric isn't overfit
   to one 4-minute window.

## Aside: our own capture path

`gain_1.0` clipped every slot (100 % peak, intermod) — bad for *our* decoder too.
When DM420 owns audio capture, default to a conservative input level (gain_0.5
peaked ~65 %, no clipping) and surface a clipping warning. Tracked separately
from the decoder work.
