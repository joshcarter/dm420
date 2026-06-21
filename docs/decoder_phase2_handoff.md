# Phase 2 (coherent demod) — work-in-progress handoff

**Status:** ✅ alignment fix landed. Builds green, all `modes` tests pass, clippy clean.
The coherent front-end is implemented, the time-alignment offset is **pinned and baked
in**, all five LLR variants are enabled, and the diagnostic env knobs have been removed.
Headline: **matched 606 / gap 35%** on the 24-slot real set, up from the magnitude
path's 471 / 50%. Remaining Phase-2-adjacent work (FT4 coherent, profiling) and Phase 3
(subtraction) are listed under "Known gaps" below.

See `docs/decoder_sensitivity_plan.md` for the why. This doc is the *where we are*.

## TL;DR for whoever picks this up

1. Phase 2 adds `crates/modes/src/cohere.rs`: per-candidate complex baseband
   (`ft8_downsample`), coherent fine sync (`sync8d`), 32-pt per-symbol FFT, and the
   five WSJT-X LLR variants (`ft8b.f90`). FT8 only; FT4 still uses the magnitude
   path. Wired into `decode.rs::decode_streaming` (coherent tried first, magnitude
   path as a safety fallback).
2. The coherent core is **correct** — `cohere::tests::coherent_decodes_clean_signal`
   decodes a clean signal end-to-end (CRC pass).
3. **The alignment fix (was the open bug):** the per-candidate start `i0_guess =
   (time_offset + time_sub/osr)·32` ran ~**one symbol high** relative to the coherent
   baseband origin — the magnitude waterfall analyzes a 2-symbol Hann frame
   (`nfft = block_size·FREQ_OSR = 2·NSPS`), so its reported start is biased. The
   correction is now applied at the waterfall→baseband call site in
   `decode.rs::decode_candidate_coherent` (`… - SPS2`, i.e. subtract one symbol =
   32 downsampled samples); `fine_sync` stays convention-agnostic and the coarse
   window is back to a tight `±10`.
4. **The empirical offset (not the predicted −16):** a `DM420_TOFF` sweep (variant-a,
   coherent-only, `±10`) showed a **flat optimum plateau across −40..−20 downsampled
   samples, centered at −32 ≡ one full symbol**. The earlier half-window geometric
   estimate of −16 undershot by 2× (it cost ~13 decodes at −16 vs −32). Confirming
   this empirically — rather than trusting the −16 derivation — was the key step.

## Measured results (ab_jt9 on `sample_data/wsjtx_ft8`, 24 slots, denom ≈ 936)

| Configuration | matched | gap | notes |
|---|---:|---:|---|
| Magnitude path (baseline, `main`) | 471 | 50% | what we ship today |
| Coherent (all variants) + fallback, **mis-aligned** `±10` | 478 | 49% | the old branch default — coherent mostly mis-locked, fallback carried it |
| Coherent-only, variant-a, `±40` (the proof, mis-aligned but brute-forced) | 534 | 43% | alignment reachable via a wide search → coherent beats magnitude |
| **Coherent, all 5 variants, aligned (−32), `±10` — SHIPS** | **606** | **35%** | alignment baked in + multi-symbol variants compound |

`ours-only` rose from 2–5 to **15** at the final config. Eyeballed all 15: every one is
a well-formed FT8 message and the majority involve callsigns that appear in jt9's *own*
confirmed decodes elsewhere in the dataset (e.g. `IQ7KM HK3AB -19` recurs across two
consecutive slots = a real QSO progressing). These are genuine decodes jt9 missed, not
CRC false positives — the gate is holding.

`DM420_TOFF` sweep that pinned the offset (each row = aggregate `matched`, `±10`,
variant-a, coherent-only):

| TOFF | −40 | −36 | −32 | −28 | −24 | −22 | −20 | −16 | −12 | −8 | −4 | 0 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| matched | 537 | 535 | 535 | 535 | 536 | 535 | 535 | 522 | 496 | 441 | 352 | 235 |

Reproduce (set `JT9_BIN` — on macOS jt9 lives in the app bundle, not on PATH):
```sh
JT9_BIN=/Applications/wsjtx.app/Contents/MacOS/jt9 \
  cargo run -q -p modes --release --example ab_jt9 -- sample_data/wsjtx_ft8
```

## What was done to land the fix (all complete)

1. **Pinned the offset empirically** via the `DM420_TOFF` sweep above — flat optimum
   plateau −40..−20, centered at **−32 (one symbol)**, *not* the theory's −16.
2. **Baked it in** as `… - SPS2` (one symbol) at the waterfall→baseband call site in
   `decode.rs::decode_candidate_coherent`, with a derivation comment. Deliberately
   **not** inside `fine_sync` — that stays convention-agnostic so the direct-`analyze`
   clean-signal test (which passes a true start) keeps exercising the core honestly.
   Coarse window restored to a tight `±10`.
3. **Re-enabled all five LLR variants** (removed the diagnostic cap).
4. **Ripped out the diagnostic env knobs** (`DM420_COH_ONLY`, `DM420_NV`, `DM420_TW`,
   `DM420_TOFF`, `DM420_NSYNC`) and the `coherent_decodes_clean_signal` `eprintln!`s.
   (`DM420_OSD` from Phase 1 still applies; it is not a Phase-2 diagnostic.)
5. **Re-measured:** `cargo test -p modes` green; `ab_jt9` → 606 / 35%; `ours-only`
   eyeballed and confirmed real (see results note above).

## What was implemented (file by file)

- **`crates/modes/src/cohere.rs`** (new): `Demod` — owns the FFT plans + cached
  slot spectrum. `set_slot` (one 192000-pt real FFT/slot), `downsample` (port of
  `ft8_downsample.f90`: slice the cached spectrum around f0, taper, `cshift` to DC,
  3200-pt inverse → complex 200 Hz baseband), `sync8d` (coherent Costas power,
  port of `sync8d.f90`), `fine_sync` (coarse time → ±2.5 Hz/0.5 Hz freq → fine
  time, `ft8b.f90` 105–153), `symbol_spectra` (32-pt per-symbol FFT keeping phase),
  `hard_sync` (nsync 0..21), `analyze` (→ LLR variants + refined freq/dt).
  `coherent_llrs` builds variants a/b/c/d/e (`ft8b.f90` 186–254): coherent 1/2/3-
  symbol integration, max-log bit metrics, then our `normalize_llr`.
- **`crates/modes/src/fft.rs`**: added `Cfft` (complex fwd+inv via `rustfft`) +
  re-export of `Complex`; round-trip test.
- **`crates/modes/src/decode.rs`**: `decode_candidate_coherent` (BP+OSD+CRC over
  the variants, returns refined freq/dt); `decode_streaming` builds a `Demod` for
  FT8 and tries coherent-then-magnitude; `normalize_llr` and `verify_codeword` made
  `pub(crate)`.
- **`crates/modes/Cargo.toml`** + `Cargo.lock`: added `rustfft = "6"` (direct dep;
  was already transitive via `realfft`).

## Key implementation decisions / gotchas

- **Reused our `normalize_llr`** (`sqrt(24/var)` scaling) on each variant rather
  than WSJT-X's `normalizebmet` + `scalefac=2.83`, because our `bp_decode` is
  calibrated to the former. Confirmed working (clean test).
- **`i0` derived from the candidate, convention-free:** `(time_offset +
  time_sub/osr)·32 − 32` (32 downsampled samples/symbol = NSPS/NDOWN; the trailing
  `−32` is the one-symbol waterfall-window correction). This avoids WSJT-X's
  `xdt+0.5` origin mismatch. The correction is a *constant* (now measured at one
  symbol), not a convention error.
- **`FT8_COSTAS`/`FT8_GRAY` already equal WSJT-X's `icos7`/`graymap`** — reused
  directly.
- **Performance:** ~3 s/slot (75 s for 24). The long FFT is once/slot; per-candidate
  cost dominates (downsample inverse FFT + ~30 `sync8d` + 79 symbol FFTs + up to 5
  BP/OSD). Acceptable for the benchmark and live (decode is off-thread), but worth
  a profile pass once correct — especially trimming `sync8d` work and capping
  variants/OSD per candidate.

## Known gaps / not done

- ~~The offset fix~~ — **done** (−32, baked in; see above).
- ~~Diagnostic knobs + test `eprintln!`s~~ — **done** (removed).
- **Performance/profiling** — still ~3 s/slot; the profile pass (trim `sync8d`, cap
  variants/OSD per candidate) noted above hasn't been done. Fine for the benchmark
  and live (decode is off-thread), but worth revisiting.
- **FT4 coherent path** — not ported; FT4 still magnitude-only. `ab_jt9` is also
  FT8-only (hardcodes `Protocol::Ft8` / `--ft8`), so `sample_data/wsjtx_ft4` reads
  `0/0` until an FT4 arm is added.
- **Phase 3 (multi-pass subtraction)** — depends on this complex machinery; not started.

## Reference source (WSJT-X), for line-level cross-checks

`/Users/josh/Projects/vendor/wsjtx-improved/src/wsjtx/lib/ft8/`: `ft8b.f90`
(fine sync 105–153, symbol FFT 155–162, hard sync 164–184, metrics 186–254,
`normalizebmet` 493–506), `ft8_downsample.f90`, `sync8d.f90`. Constants in
`ft8_params.f90` (NSPS=1920, NDOWN=60, NP2=2812, NN=79).
