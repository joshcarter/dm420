# Phase 2 (coherent demod) — work-in-progress handoff

**Status:** WIP, builds green, all 25 `modes` tests pass. The coherent front-end is
implemented and **proven to beat the magnitude path once a time-alignment bug is
fixed** — but the fix is not yet baked in. This branch carries diagnostic env
knobs (listed below) used to localize the bug; they should be removed before this
merges to `main`.

See `docs/decoder_sensitivity_plan.md` for the why. This doc is the *where we are*.

## TL;DR for whoever picks this up

1. Phase 2 adds `crates/modes/src/cohere.rs`: per-candidate complex baseband
   (`ft8_downsample`), coherent fine sync (`sync8d`), 32-pt per-symbol FFT, and the
   five WSJT-X LLR variants (`ft8b.f90`). FT8 only; FT4 still uses the magnitude
   path. Wired into `decode.rs::decode_streaming` (coherent tried first, magnitude
   path as a safety fallback).
2. The coherent core is **correct** — `cohere::tests::coherent_decodes_clean_signal`
   shows all five variants decode a clean signal with `bp errors=0` + CRC pass.
3. **The bug:** the per-candidate start sample `i0_guess = (time_offset +
   time_sub/osr)·32` is systematically offset from the true downsampled position by
   the magnitude waterfall's 2-symbol Hann-window lead. The coarse-time search
   (`±10` downsampled samples) can't reach it, so fine sync mis-locks on ~half the
   real signals and the demod degrades.
4. **The proof + payoff:** widening the coarse search to `±40` (env `DM420_TW=40`)
   recovers them — **variant-a-only coherent then scores 534 vs the magnitude
   path's 471** (gap 50% → 43%) on the 24-slot real set. Coherent demod *beats*
   magnitude once aligned, exactly as predicted.

## Measured results (ab_jt9 on `sample_data/wsjtx_ft8`, 24 slots, denom = 935)

| Configuration | matched | gap | notes |
|---|---:|---:|---|
| Magnitude path (baseline, `main`) | 471 | 50% | what we ship today |
| Coherent (all variants) + magnitude fallback, `±10` | 478 | 49% | current default on this branch — marginal, because coherent mostly mis-locks and the fallback carries it |
| Coherent-only, `±10` | 254 | 73% | bug exposed: coherent alone is *worse* when mis-aligned |
| Coherent-only, variant-a, `±10` | 235 | — | variant a ≡ magnitude metric, so this isolates extraction/sync |
| **Coherent-only, variant-a, `±40`** | **534** | **43%** | **alignment fixed → coherent beats magnitude** |

`ours-only` (potential false decodes) stayed at 2–5 throughout — the CRC gate holds.

Reproduce (jt9 is on PATH at `/usr/bin/jt9`):
```sh
cargo run -q -p modes --release --example ab_jt9 -- sample_data/wsjtx_ft8
# diagnostics:
DM420_COH_ONLY=1 DM420_NV=1 DM420_TW=40 cargo run -q -p modes --release --example ab_jt9 -- sample_data/wsjtx_ft8
```

## The next step (do this first)

1. **Pin the constant offset.** Sweep `DM420_TOFF` (added to `i0` in
   `cohere::Demod::fine_sync`) with a *tight* window (`DM420_TW=10`), variant-a,
   coherent-only, and read the **aggregate** `matched` (the `=== aggregate ===`
   block — *not* the per-file `matched` lines; my first sweep grep mistakenly read
   a per-file line and produced garbage "matched 24" rows). Theory predicts the
   peak near **`DM420_TOFF=-16`** (the waterfall window centers block *b* at sample
   `b·1920 − 960`, i.e. −960 audio = −16 downsampled samples). Confirm empirically.
   Example sweep:
   ```sh
   for t in -24 -20 -16 -12 -8 0; do
     echo -n "TOFF=$t  "
     DM420_COH_ONLY=1 DM420_NV=1 DM420_TW=10 DM420_TOFF=$t \
       cargo run -q -p modes --release --example ab_jt9 -- sample_data/wsjtx_ft8 \
       2>/dev/null | sed -n '/=== aggregate/,/gap/p' | grep -oE 'matched [0-9]+'
   done
   ```
   (Each run ≈ 75 s; the whole sweep ≈ 8 min.)
2. **Bake the offset into `i0_guess`** (a named const in `cohere.rs`, derived/
   commented), and keep the coarse window tight (`±10`) — wide windows are slow and
   risk locking onto a louder neighbor's Costas on crowded bands.
3. **Re-enable all five variants** and re-measure — expect **> 534** (variant a
   alone already beat magnitude; b/c add coherent multi-symbol gain).
4. **Rip out the diagnostic env knobs** (below) and the `coherent_decodes_clean_signal`
   `eprintln!`s. Keep a single clean wiring.
5. Re-run `cargo test -p modes` (25 tests incl. `fixtures_decode`) and the
   `ab_jt9` gap; watch `ours-only` for any false-decode rise.

## Diagnostic env knobs on this branch (TEMPORARY — remove before merge)

All read in `cohere.rs` / `decode.rs`:

| Var | Default | Meaning |
|---|---|---|
| `DM420_COH_ONLY` | off | disable the magnitude fallback — see coherent's standalone score |
| `DM420_NV` | all | cap how many LLR variants are tried (1 = nsym=1 / variant a only) |
| `DM420_TW` | 10 | coarse-time search half-width, in downsampled samples |
| `DM420_TOFF` | 0 | constant added to the coarse-time start guess `i0` (offset hunt) |
| `DM420_NSYNC` | 4 | hard Costas-sync gate in `analyze` (0 = off; CRC still gates) |

(`DM420_OSD` from Phase 1 still applies.)

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
  time_sub/osr)·32` (32 downsampled samples/symbol = NSPS/NDOWN). This avoids
  WSJT-X's `xdt+0.5` origin mismatch — but it carries the waterfall window offset
  (the bug above), which is a *constant* correction, not a convention error.
- **`FT8_COSTAS`/`FT8_GRAY` already equal WSJT-X's `icos7`/`graymap`** — reused
  directly.
- **Performance:** ~3 s/slot (75 s for 24). The long FFT is once/slot; per-candidate
  cost dominates (downsample inverse FFT + ~30 `sync8d` + 79 symbol FFTs + up to 5
  BP/OSD). Acceptable for the benchmark and live (decode is off-thread), but worth
  a profile pass once correct — especially trimming `sync8d` work and capping
  variants/OSD per candidate.

## Known gaps / not done

- The offset fix (step 1–3 above) — the headline remaining work.
- FT4 coherent path — not ported; FT4 still magnitude-only. `ab_jt9` is also
  FT8-only (hardcodes `Protocol::Ft8` / `--ft8`), so `sample_data/wsjtx_ft4` reads
  `0/0` until an FT4 arm is added.
- Phase 3 (multi-pass subtraction) — depends on this complex machinery; not started.
- Diagnostic knobs + test `eprintln!`s — to be removed.

## Reference source (WSJT-X), for line-level cross-checks

`/home/josh/Projects/wsjtx-improved/src/wsjtx/lib/ft8/`: `ft8b.f90`
(fine sync 105–153, symbol FFT 155–162, hard sync 164–184, metrics 186–254,
`normalizebmet` 493–506), `ft8_downsample.f90`, `sync8d.f90`. Constants in
`ft8_params.f90` (NSPS=1920, NDOWN=60, NP2=2812, NN=79).
