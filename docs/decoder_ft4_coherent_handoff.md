# FT4 coherent decode — handoff for the next session

> **STATUS: DONE** (commits `fc5b620` Phases 1-3 + `53adefb` Phase 4, on `main`).
> FT4 decode is now coherent + multi-pass, matching the FT8 architecture. On the
> expanded 47-slot corpus (`sample_data/wsjtx_ft4`, denom 319): magnitude 240/gap
> 25% → coherent 269/gap 16% → **coherent + subtraction 288 / gap 10%** — better
> than FT8's 16%. FT8 unchanged at 787/16% (no regression). `ours-only` 32, all
> clean. Implemented as a **sibling module `cohere_ft4.rs`** (not a cohere.rs
> generalization — the FT4 DSP diverges enough that a sibling kept the FT8 path
> byte-identical). FT4 i0 offset measured via `DM420_FT4_TOFF` sweep → baked −14.
> New `DM420_COHERENT=0` A/B knob. The original plan follows for reference.

**Goal:** bring the FT4 decoder up to the FT8 decoder's sensitivity by porting the
same coherent front-end + multi-pass subtraction that took FT8 from gap 50% → 16%.
FT4 decode is currently **magnitude-only, single-pass** (no coherent demod, no
subtraction) — exactly where FT8 was before Phase 2.

This mirrors `docs/decoder_phase2_handoff.md` (the FT8 coherent handoff). Read that
first — the FT4 work is the same shape with FT4 parameters and FT4's bit-metric step.

## Where things stand (as of this handoff)

- **FT8** (on `main`): coherent front-end (`cohere.rs`) + multi-pass subtraction
  (`subtract.rs`), composed in `decode_streaming`. `ab_jt9` FT8 = **matched 787 / gap 16%**.
- **FT4 transmit**: implemented (Joel) and merged — `encode::ft4_tones` + generic
  `synth`, wired through `core/tx.rs`. **Untested on air.**
- **`Protocol` refactor** (Joel, merged): per-mode facts live on `Protocol` in
  `waterfall.rs` — `num_tones`, `bits_per_symbol`, `channel_symbols`, `data_symbols`,
  `num_sync`, `length_sync`, `sync_offset`, `gfsk_bt`, `whitening`, `symbol_period`,
  `slot_time`. This is the abstraction the FT4 coherent port should build on.
- **FT4 decode**: magnitude-only single-pass. In `decode_streaming` the coherent path
  and the subtraction loop are both gated `protocol == Protocol::Ft8`.

## FT4 baseline (the target to beat)

`ab_jt9` now has an `AB_MODE=ft8|ft4` knob and a parser that handles FT4's `+` sync
marker (FT8 uses `~`). Measure any time:

```sh
JT9_BIN=/Applications/wsjtx.app/Contents/MacOS/jt9 \
  AB_MODE=ft4 cargo run -q -p modes --release --example ab_jt9 -- sample_data/wsjtx_ft4
```

Baseline on `sample_data/wsjtx_ft4` (14 slots), magnitude-only FT4 vs `jt9 --ft4 -d 3`:

```
matched 69  ours-only 0  jt9-only 29  → gap 29/98 = 30%
```

So FT4 magnitude-only already recovers ~70% of jt9 as a perfectly clean subset
(zero false decodes) — a *better* starting point than FT8's was (gap 50%). The
coherent + subtraction port should close most of the remaining 30%. (FT4 sample
WAVs are ~6 s / 12 kHz / mono; FT4 slot period is 7.5 s. Denominator is small —
14 slots, 98 jt9 decodes — so consider capturing more FT4 corpus for a firmer number.)

## The port plan (mirrors the FT8 Phase 2 work)

WSJT-X's FT4 decoder is coherent with the **same architecture as FT8** — the FT8
port in `cohere.rs` is the template. Reference: `/Users/josh/Projects/vendor/
wsjtx-improved/src/wsjtx/lib/ft4/` (`ft4_downsample.f90`, `sync4d.f90`,
`get_ft4_bitmetrics.f90`, `subtractft4.f90`, `ft4_params.f90`). See
[[wsjtx-reference-source]].

1. **Generalize `cohere.rs` from FT8-hardcoded to `Protocol`-parameterized.** Today
   it hardcodes `NSPS=1920`, `NN=79`, 8 tones, `FT8_COSTAS`/`FT8_GRAY`. Drive these
   from `Protocol` facts + a small per-mode geometry table. FT8 must stay byte-identical
   (re-run FT8 `ab_jt9` = 787 to confirm no regression).
2. **Add FT4 geometry.** `NSPS=576`, `NDOWN=18` → **`NSS = 576/18 = 32` downsampled
   samples/symbol — the same 32 as FT8**, so `SPS2` is unchanged. `NN=103` (105 with
   the two ramp symbols), 4 tones, `NFFT1=2304`, `NMAX=79488`. Costas: four 4-symbol
   blocks `icos4a/b/c/d` at symbol indices 1–4 / 34–37 / 67–70 / 100–103 (note the
   leading ramp at index 0; `ft4_sync_score` already carries the `1 +` offset).
   `FT4_GRAY = [0,1,3,2]`, `FT4_COSTAS` already in `constants.rs`.
3. **Port the FT4 DSP** (FT4-specific, structurally identical to the FT8 versions):
   - `ft4_downsample` — note it uses a **flat-top** window (`bw_flat=4·baud`,
     `bw_transition=0.5·baud`), not FT8's cosine taper.
   - `sync4d` — coherent Costas sync over the four 4-symbol arrays (cf. `sync8d`).
   - `get_ft4_bitmetrics` — per-symbol 4-pt complex FFT (`cs(0:3,k)`), hard-sync gate
     (`nsync` 0–16, bad if < 8), then **coherent sequences of 1/2/4 symbols** producing
     5 bitmetric variants. This is the one piece that differs structurally from FT8's
     `ft8b` (which integrates nsym 1/2/3 over 8-tone symbols); FT4 works over
     4-symbol / 8-bit groups (256 sequences). Reuse our `normalize_llr` + BP + OSD +
     CRC gate, exactly as the FT8 coherent path does.
4. **Wire FT4 into the coherent path** — drop the `protocol == Protocol::Ft8` gate in
   `decode_candidate_coherent`/`decode_streaming` so FT4 candidates take coherent-first
   with the magnitude fallback.
5. **FT4 Phase 3** — port `subtractft4.f90` (FT4 has its own) and enable the multi-pass
   subtraction loop for FT4 (currently FT8-only via `protocol == Ft8 && subtract_enabled()`).

## Critical gotchas (learned the hard way on FT8)

- **The start-sample (`i0`) alignment offset must be measured empirically, NOT derived.**
  On FT8 the magnitude waterfall's 2-symbol Hann frame put the candidate start one full
  symbol (−32 downsampled) off; the "obvious" half-window estimate (−16) was wrong and
  cost ~13 decodes. FT4 has its own convention (ramp symbol at index 0, the `1 +` sync
  offset, NSPS=576) — **do a `DM420_TOFF`-style sweep for FT4 before baking in any
  constant.** The FT8 correction lives in `decode_candidate_coherent` as `… − SPS2`;
  FT4 needs its own measured value. Keep the correction at the call site, not inside
  `fine_sync` (so the clean-signal test keeps exercising the core honestly).
- **Vet `ours-only` after enabling.** Subtraction can manufacture false decodes from a
  bad fit. On FT8, eyeball each new `ours-only` (recurring callsigns / well-formed =
  real jt9 miss; garbled/invalid-prefix calls = false). Watch the count.
- **FT4 whitening is already handled** — `verify_codeword` un-whitens via
  `protocol.whitening()` (FT4 → `FT4_XOR`). Don't double-apply.
- **Coherent is tried first, magnitude is the fallback** — so coherent can only add
  decodes, never regress. Keep that invariant for FT4 too.

## Measurement / done criteria

- `cargo test -p modes` green (incl. a FT4 coherent clean-signal round-trip — add one
  mirroring `cohere::tests::coherent_decodes_clean_signal`).
- `clippy --all-targets -D warnings` clean.
- FT8 `ab_jt9` still 787 (no regression from the cohere generalization).
- FT4 `ab_jt9` (`AB_MODE=ft4`) gap meaningfully below the baseline above; `ours-only`
  stays clean.

Related: [[decoder-sensitivity-work]], [[wsjtx-reference-source]].
