# Proposal: Migrate the spectrogram/decoder FFT to `realfft` / `rustfft`

**Status: DONE (2026-06-20).** Migrated to `realfft`. Validated by an A/B harness
(both backends live-switchable) on real on-air traffic: identical decode yield,
STFT ~10× faster (~63 ms → ~6 ms per FT8 slot). The A/B scaffolding was then
removed; `realfft` is now the only decoder FFT. Decode parity is locked in by
`crates/modes/tests/fixtures_decode.rs` (+ `fft::tests::matches_naive_dft`). The
historical proposal is kept below for rationale.
**Author:** (drafted with Claude)
**Scope:** `crates/modes/src/fft.rs`, `crates/modes/src/waterfall.rs`

## TL;DR

Replace the hand-rolled Bluestein chirp-z FFT in `crates/modes/src/fft.rs`
with [`realfft`](https://crates.io/crates/realfft) (built on
[`rustfft`](https://crates.io/crates/rustfft), the de-facto standard Rust FFT).

This is **worth doing** for code reduction, speed, and battle-tested
correctness — but it is **not just a spectrogram change**. The same FFT feeds
the FT8/FT4 decoder, so it carries decode-regression risk and must be validated
against decode rate, not just visual output. It is **not urgent**.

## Why migrate

- **It deletes ~160 lines of tricky DSP.** `fft.rs` is a hand-rolled radix-2
  Cooley–Tukey plus a Bluestein chirp-z wrapper for arbitrary `N`. That's a
  correctness surface we currently own and maintain ourselves.
- **`rustfft` covers the arbitrary-`N` need natively.** Our transform size is
  `nfft = block_size * freq_osr` — for FT8 at 12 kHz that's `1920 * 2 = 3840 =
  2^8 · 15`, not a power of two. `rustfft` handles any `N` via mixed-radix plus
  its own internal Bluestein, is SIMD-accelerated, and caches twiddle factors in
  a reusable planner.
- **`realfft` is purpose-built for our exact call.** We only ever do a forward
  real-input transform (`Bluestein::forward_real`). `realfft` exploits the
  real-input symmetry for roughly 2× the speed and half the memory of a full
  complex FFT, and returns the `N/2 + 1` non-redundant bins.

## Why this is more than a cosmetic change

The FFT is **the front-end of the decoder**, not just the waterfall display.
`crates/modes/src/waterfall.rs` (`Monitor`) computes the magnitude grid, and
`crates/modes/src/decode.rs` consumes that same grid for:

- `find_candidates` — candidate detection
- `ft8_sync_score` / `ft4_sync_score` — Costas sync scoring
- `extract_llr` — soft-decision LLRs into the LDPC decoder

So any change to the FFT's numerical output ripples into sync scores and LLRs,
which can shift the decode rate. This is the thing to protect.

## What must be preserved exactly

The current magnitude pipeline in `waterfall.rs::process` is:

```rust
let mag2 = re[src] * re[src] + im[src] * im[src];
let db   = 10.0 * (1e-12 + mag2).log10();
let scaled = (2.0 * db + 240.0) as i32;
wf.mag[offset] = scaled.clamp(0, 255) as u8;   // 0.5 dB per step, u8
```

Things that must match after the swap:

1. **FFT normalization / scaling.** Our Bluestein returns an *unnormalized*
   forward transform. `rustfft`/`realfft` are also unnormalized, but confirm the
   scale factor matches — any constant gain shifts `db` by a fixed offset and
   moves the `2*db + 240` u8 quantization, perturbing every downstream
   threshold. If the scale differs, fold the correction into the `+240` offset
   rather than scaling per-bin.
2. **Bin ordering and the `freq_osr` interleave.** Today the code indexes
   `src = bin * freq_osr + freq_sub` into the full complex output. `realfft`
   returns only `N/2 + 1` bins (we only use `min_bin..max_bin`, well inside that
   range), so re-derive the indexing carefully.
3. **Hann window.** Unchanged — windowing happens before the FFT and is
   independent of the FFT implementation. Keep it identical.
4. **`f32` vs `f64`.** The current code accumulates the chirp-z in `f64`
   internally and returns `f32`. `rustfft` is generic; use `f32` to match the
   existing precision and the rest of the pipeline.

## How to validate

The acceptance gate is **decode parity on recorded audio**, not "the waterfall
looks the same."

1. **Golden decode corpus.** Collect a handful of real recorded slots (15 s FT8,
   7.5 s FT4 mono WAV at 12 kHz) spanning weak-signal and crowded-band
   conditions. If we don't have recordings on hand, WSJT-X sample `.wav` files
   are a good public source. Decode each with the *current* code and save the
   set of decoded messages + per-decode SNR/score as the golden output.
2. **Before/after decode count.** Run the same corpus through the migrated FFT.
   The pass condition: **decode count is equal or higher**, and the set of
   decoded messages is a superset of (or identical to) the golden set. A net
   loss of any decode is a regression to investigate before merging.
3. **Bit-level magnitude diff (fast inner-loop check).** For one slot, dump the
   `wf.mag` u8 grid from old and new and compare. Expect near-identical values
   (off-by-one at the u8 quantization boundary is fine; large or systematic
   deltas indicate a scaling mismatch — see "What must be preserved," item 1).
4. **A unit test in `fft.rs`.** Add a test that checks the new wrapper against a
   naive O(N²) DFT for a couple of representative `N` (including the non-power-of-
   two 3840) on a known input — sinusoids plus an impulse. This pins the
   implementation independently of the decoder. (If the existing `fft.rs` already
   has such tests, port them.)
5. **Performance sanity (optional).** Benchmark `Monitor::process` throughput
   before/after to confirm the expected speedup and no regression. This is a
   nice-to-have; correctness is the gate.

## Cost / effort

- Add `realfft` (+ transitive `rustfft`) to the `modes` crate dependencies.
- Replace `Bluestein` with a cached `RealFftPlanner` / `R2cFft` instance held in
  `Monitor`. Adjust the `process` bin indexing for the `N/2 + 1` output layout.
- Wire up the validation corpus + decode-parity check above.

Estimate: ~half a day including validation, dominated by assembling the decode
corpus and confirming scaling parity — not the code swap itself.

## Recommendation

Do it, but gate the merge on decode parity against a recorded corpus. It's a
clean win for maintainability and speed with no user-visible behavior change
*if* the magnitude scaling is preserved. Given it touches the decoder front-end,
it should not be rushed in alongside unrelated changes.
