# Closing the FT8 decode-sensitivity gap

**Status:** Phase 1 (OSD) done — but it barely moved the metric, and *why* it
didn't reframes the whole plan. **Owner:** TBD. **Metric:** the `ab_jt9` example
(now runnable on Linux — `jt9` is on `PATH` at `/usr/bin/jt9`).

> **The reframe (2026-06).** The original plan diagnosed the gap as **masking
> (subtraction) + deep-weak (OSD)** and treated our receive front-end as already
> at WSJT-X parity. **It is not.** We are a faithful `ft8_lib` port, which means a
> **strictly non-coherent, magnitude-only** front-end. WSJT-X's biggest weak-signal
> wins (per-candidate fine sync + *coherent* multi-symbol integration) live *before*
> the LDPC/OSD stage this plan originally focused on. OSD, subtraction, and BP all
> consume the front-end's LLRs — and ours are ~2–3 dB degraded before any backstop
> sees them. That is why OSD landed with a shrug. **The front-end is now Phase 2,
> ahead of subtraction.**

## Measured baseline (reproduce any time)

`jt9` is installed locally, so the A/B benchmark runs on this machine:

```sh
cargo run -q -p modes --release --example ab_jt9 -- sample_data/wsjtx_ft8
```

Current result on the 24 real busy-band 20 m FT8 slots in `sample_data/wsjtx_ft8`:

```
matched 471   ours-only 5   jt9-only 464   → gap 464/935 = 50%
```

We recover **~half** of what `jt9 -d 3` decodes, as a near-perfect *subset* (only
5 ours-only across 24 files — accuracy is fine; we are simply half as sensitive on
crowded bands). This reproduces the earlier macOS measurement (450→471 across the
OSD A/B), so the metric is stable across platforms.

### What we miss, by SNR (464 missed signals)

```
        ≤ -20:   22
     -20..-15:  125
     -15..-10:  137   ← largest bucket
      -10..-5:  117
       -5..0:   47
       ≥ 0 dB:  16
```

**262 of 464 misses (56 %) are below −10 dB.** That is the deep-weak tail, the
zone where coherent demodulation and fine sync pay off most — *not* the loud-
neighbour-masking zone the original plan emphasized. (The older bucketing put the
mode at −10..−5; this fresh run, on the current decoder with OSD already on, shows
the mass has shifted weak. OSD nibbled the loud near-misses; the weak tail it
can't reach because the LLRs feeding it are non-coherent.)

### Two independent probes of the mechanism

- **Isolated weak-signal floor** (`crowd_recall --n 6`, one SNR, well-separated):
  our recall falls off at **−18 to −19 dB** and is 0 % by −21 dB — exactly where
  `jt9` is *still decoding*. With **zero crowding** we are ~2–3 dB short. That gap
  is pure front-end (coherent demod + fine sync); masking cannot explain it.
- **Synthetic crowding** (`crowd_recall --n 40`): isolated >20 Hz decodes **100 %**,
  overlap ≤5 Hz drops to **25 %**, masked-by-louder-neighbour to **0 %**. Masking
  is real too — it is the *second* lever, not the first.

Both gaps are real; the front-end one is upstream of everything and was omitted.

## The two decoders, side by side

We are a clean-room Rust port of `ft8_lib` (Kārlis Goba). The C source is not
vendored; see `crates/modes/ATTRIBUTION.md`. WSJT-X's FT8 front-end
(`wsjtx-improved/src/wsjtx/lib/ft8/`) has a stack of machinery that runs **before**
the LDPC decode:

| Stage | WSJT-X (`ft8b.f90` / `ft8_downsample.f90` / `sync8.f90`) | Ours (`crates/modes`) | dB at stake |
|---|---|---|---|
| Coarse sync | non-coherent, 3.125 Hz × 0.04 s grid, Costas power-ratio, **≤1000** candidates | non-coherent, 3.125 Hz × 0.08 s grid, Costas contrast, **140** candidates (`MAX_CANDIDATES`) | small |
| Per-candidate extraction | downconvert to **complex** 200 Hz baseband, ~62.5 Hz wide, carrier at DC (`ft8_downsample`) | none — reads magnitudes off the shared display waterfall | enables the rest |
| **Fine sync** | refine to **0.5 Hz / 5 ms**, re-derotate (`twkfreq1`), re-downsample (`ft8b` 109–153) | none — demod at the coarse grid bin | **~1–2 dB** |
| **Coherent demod** | complex symbol FFT; **coherent 2- and 3-symbol integration** (sum complex *then* `abs`) — `ft8b` 186–234; source calls this "the main weak-signal gain" | magnitude-only, 1 symbol (`decode.rs::extract_llr`) | **~2–3 dB (biggest)** |
| LLR variants | 5 metric vectors (a/b/c/d/e) + AP-injected passes | 1 (max-log over tone dB) | — |
| Multi-pass subtraction | **3 outer passes**, complex-envelope fit (gain+phase, slowly varying), re-sync on residual (`subtractft8.f90`) | none — single pass | crowded-band lever |
| OSD | order-2, up to `maxosd` times, inside each pass | order-2 once on BP near-miss (`osd.rs`) | already have it |

**The architectural root cause:** `waterfall.rs:152` stores only `re²+im²` →
dB → `u8`. Phase is discarded at the very first stage and never recovered. The
same magnitude waterfall feeds both the waterslide display *and* the decoder.
That reuse is elegant but it **structurally precludes coherent demodulation** —
the single biggest lever. Closing the gap means the decoder gets its **own
per-candidate complex extraction path**, separate from the display waterfall.

## Where the decoder stands (attach points)

All in `crates/modes/src`. FT8 is **LDPC(174, 91)**, systematic.
`decode.rs::decode_streaming` (single pass):

1. `Monitor::new` + `process` → `Waterfall.mag` (`Vec<u8>`, **magnitude only**).
2. `find_candidates` → Costas sync over the 2×2 grid, top `MAX_CANDIDATES=140`,
   score ≥ `MIN_SCORE=10`.
3. Per candidate: `extract_llr` (magnitude max-log) → `normalize_llr` →
   `bp_decode`; on BP near-miss (`errors ≤ OSD_MAX_ERRORS`) → `osd::osd_decode`;
   CRC gate (`verify_codeword`).
4. Dedup by payload, unpack, emit.

Two facts shape the work:

- **The whole LLR path is fed by `Waterfall.mag`** (magnitude). Coherent demod and
  subtraction both need the **complex** signal, so both require re-synthesis /
  re-extraction from the *time-domain* `samples` — a new path. We already have the
  forward synthesizer (`encode::synth_ft8` / `ft8_tones`) and the FFT (`fft.rs`).
- The complex-extraction machinery Phase 2 builds (downconvert → fine sync →
  complex symbol FFT) is **the same machinery Phase 3 (subtraction) needs** for its
  complex-envelope fit. Phase 2 is the foundation, not a detour.

## Plan

Sequenced for incremental, independently-measurable delivery. **Re-run `ab_jt9`
after each phase** — the aggregate gap % is the gate.

### Phase 1 — OSD backstop ✅ done (commit `3f14b7d`)

`osd.rs` (order-0/1/2, most-reliable-basis via Gauss-Jordan, soft-distance rank,
CRC gate), wired at `decode_candidate`. `DM420_OSD=0` disables it. Result: +21
matched on the 24-slot set, no false-decode rise. **Modest by design** — it is a
weak-signal backstop bolted onto non-coherent LLRs. It cannot reach the −15 dB
tail because the soft information handed to it is already ~2–3 dB degraded. Leave
it on; revisit `OSD_MAX_ERRORS` / `LAMBDA` only after Phase 2 lifts the LLRs.

### Phase 2 — per-candidate coherent demodulation (NEW — the missing foundation)

**Why first now:** it is upstream of BP, OSD, *and* subtraction, and the evidence
(56 % of misses below −10 dB; a ~2–3 dB shortfall on *isolated* signals) points
straight at it. It is the lever WSJT-X's own source names as the main weak-signal
gain, and we cannot do it at all today because we threw the phase away.

Build a decoder-owned complex path, ported from `ft8b.f90` + `ft8_downsample.f90`:

1. **Complex baseband extraction** per candidate: from the time-domain `samples`,
   downconvert to a complex stream centered on the candidate frequency (carrier →
   DC), low-pass to ~62.5 Hz, decimate to ~200 Hz (≈32 samples/symbol). One cached
   forward FFT of the whole slot, sliced per candidate (the `ft8_downsample`
   trick), keeps this cheap.
2. **Fine sync** on that complex stream: search ±2.5 Hz in 0.5 Hz steps and ±a few
   samples in time using a coherent sync metric (`sync8d`-style
   `Σ cd·conj(csync)`), re-derotate to the best frequency, re-extract. Target
   **0.5 Hz / 5 ms** precision.
3. **Coherent symbol demod:** complex-FFT each of the 79 symbols (32-pt), keep the
   8 complex tone amplitudes. Build LLRs from **coherent 1-, 2-, and 3-symbol
   integration** (sum the complex tone amplitudes across adjacent symbols *before*
   `abs`). Produce the metric variants and feed each through BP+OSD as additional
   passes, keeping the first CRC-valid decode.

**New code:** `modes/src/cohere.rs` (downsample + fine sync + complex demod) and a
new per-candidate decode path in `decode.rs` that uses it instead of the
magnitude-waterfall LLRs. The magnitude `Waterfall` stays for `find_candidates`
and the display.

**Risks / validation:** a subtly wrong derotation or symbol-window offset silently
costs the dB you ported for — it will not crash, it will just decode fewer weak
signals. Gate it with `cargo test -p modes` (the `fixtures_decode` cross-checks)
and the isolated-floor probe: the floor should move from ~−18 dB toward ~−21 dB.

**Expected:** recovers a meaningful slice of the −15..−10 and −20..−15 buckets
(the 262-signal weak tail), and lifts every signal ~2–3 dB so Phase 3's fits start
from cleaner decodes.

### Phase 3 — multi-pass subtraction (the crowded-band lever)

**Why after Phase 2:** biggest *masking* bucket, but its value multiplies once the
front-end is coherent — each subtraction pass ends in a BP+OSD decode of a cleaner
residual, and the complex-envelope fit reuses Phase 2's baseband machinery. Doing
it before Phase 2 means fitting subtractions from a degraded estimate (poor fit →
residual that masks as badly as the original).

Pull "build candidates → decode loop" into a reusable `decode_pass(samples)` and
drive it from a 3-pass loop. After each decode, fit a **slowly-varying complex
amplitude** (gain *and* phase across the 79 symbols, à la `subtractft8`: demod to
baseband against the unit-amplitude reference, low-pass the complex envelope,
subtract `2·Re(envelope·ref)` from the time-domain audio), then re-sync on the
residual. Optionally refine DT per WSJT-X's `lrefinedt`.

**New code:** `modes/src/subtract.rs`; refactor in `decode.rs`.

**Expected:** the −10..0 dB "should-have-decoded" misses, plus weak signals freed
by removing their maskers.

### Phase 4 — a-priori (AP) decoding (defer)

Couples the decoder to the `qso` crate's contact state (expected next message).
Narrow payoff, real integration cost, and the `qso` crate is still a stub. Revisit
only if Phases 2–3 leave a gap.

### Cheap knobs to test alongside (low effort, do first / in parallel)

- Raise `MAX_CANDIDATES` 140 → 300 and lower `MIN_SCORE`; WSJT-X keeps **1000**.
  One-line experiment — probably small, but rule it in/out cheaply.
- Sweep `TIME_OSR`/`FREQ_OSR` (currently 2/2). WSJT-X coarse-syncs at ¼-symbol
  (we use ½). Cheap to try; the real time/freq precision win is Phase 2's fine
  sync, so don't expect much here.
- Confirm `LDPC_ITERS=25` isn't starving convergence (WSJT-X uses 30 with an
  early-stop).

## Validation (the part that actually matters)

Translation is easy; proving we didn't silently lose dB is the work.

1. **No regressions:** `cargo test -p modes` (the `fixtures_decode` cross-checks
   against generated WAVs must still pass) and `crowd_recall` recall must not drop.
2. **Real gain:** `ab_jt9` aggregate gap % on `sample_data/wsjtx_ft8` after each
   phase. Target: 50 % → meaningfully lower after Phase 2 → close to `jt9` after
   Phase 3.
3. **No false decodes:** watch `ours-only` in `ab_jt9` (5 today). A bad fine-sync
   derotation or subtraction fit are the two things that can manufacture false
   decodes; the CRC gate should hold them near 0. Any rise is a red flag.
4. **Isolated-floor probe:** `crowd_recall --n 6` at a single SNR, swept −15..−21,
   tracks the front-end gain independent of masking. Phase 2 should push the floor
   from ~−18 toward ~−21 dB.

### Benchmark gaps to fix

- **`ab_jt9` is FT8-only.** It hardcodes `Protocol::Ft8` and `JT9_ARGS=--ft8 -d 3`,
  so `sample_data/wsjtx_ft4` reports `0/0`. Add an FT4 arm (decode with
  `Protocol::Ft4`, run `jt9 --ft4`) before claiming any FT4 result.
- Capture more corpus at different bands/times so the metric isn't overfit to one
  ~4-minute 20 m window.

## Strategic note: port vs. subprocess

Phases 2–3 reproduce `ft8b` + `ft8_downsample` + `subtractft8` + fine sync —
~1000+ lines of dense DSP where a subtle numeric slip silently costs sensitivity.
If full WSJT-X parity is a hard Field-Day requirement, the rational alternative is
to **shell out to `jt9`** (now installed): full sensitivity, zero porting, at the
cost of a Fortran/FFTW dependency and a heavier integration seam. The phased port
is the right call *if* we want an embeddable, dependency-light decoder we control;
the subprocess is the right call *if* we just need the decodes. Decide this before
committing to Phase 2, because it determines whether Phase 2 is worth building at
all.

## Aside: our own capture path

`gain_1.0`-style hot captures clip and intermod (bad for our decoder too). When
DM420 owns audio capture, default to a conservative input level and surface a
clipping warning. Tracked separately from the decoder work.
