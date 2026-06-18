# FT8 Decoders Compared: `ft8_lib` vs. WSJT-X — and a Path to Close the Gap

**Purpose:** Evaluate two open implementations of the FT8 weak-signal decoder for
reuse in a new application, and propose an incremental way to get most of
WSJT-X's superior sensitivity without porting its entire Fortran codebase.

**Context:** FT8 is an amateur-radio digital mode for weak-signal contacts. A
"decoder" turns ~15 seconds of received audio into decoded text messages.
Decode *sensitivity* — how weak a signal can still be pulled out, and how many
signals are recovered from a crowded band — is the headline quality metric.
WSJT-X is the reference application; `ft8_lib` is the leading independent codec.

All line counts below are measured from the actual source trees.

---

## 1. The two decoders at a glance

| | **`ft8_lib`** | **WSJT-X FT8 decoder** |
|---|---|---|
| Author / origin | Kārlis Goba (YL3JG), clean-room | Franke/Taylor et al. (K9AN/K1JT), reference implementation |
| Language | C (MIT) | Fortran 90 (GPLv3) |
| Codec size | ~3,500 lines (`ft8/`), self-contained | ~3,000–5,000 lines across the decode chain |
| External deps | KISS FFT (vendored) | FFTW, shared `packjt77` message lib |
| Decode algorithm | Belief-propagation (BP) LDPC **only** | BP **+ OSD + multi-pass subtraction + a-priori + averaging** |
| Sensitivity | Good; baseline | **Gold standard** (several dB better in hard conditions) |
| Integration | Tiny library, links in-process, runs on a $4 microcontroller | Designed to run as a subprocess with a large shared-memory interface |
| Portability | Excellent — runs on Raspberry Pi Pico, STM32F7 (~200 KB RAM), ported to C# | Tied to a Fortran toolchain + FFTW |

**One-sentence summary:** `ft8_lib` is small, clean, portable, and easy to
embed, but decodes fewer weak signals; WSJT-X decodes more weak signals but is a
large, Fortran-bound research codebase.

---

## 2. Why WSJT-X decodes more — the sensitivity machinery

WSJT-X's advantage is not a single trick but a stack of them. Each is visible in
the source:

1. **OSD — Ordered-Statistics Decoding** (`lib/ft8/osd174_91.f90`, ~409 lines).
   A backstop that runs when belief-propagation fails. It re-derives the most
   likely codeword from the most-reliable bits. **This is the single biggest
   weak-signal win.** The algorithm is published (not WSJT-X-specific magic).

2. **Multi-pass subtraction** (`subtractft8.f90` + a pass loop:
   `npasses = 5 + 2*nappasses(...)`). Decode the strongest signal, subtract its
   waveform from the audio, and decode again — recovering weaker signals that
   were masked by stronger neighbors. Crucial on crowded bands.

3. **A-priori (AP) decoding** (`naptypes(nQSOProgress, …)`, `apsym`, `iaptype`).
   The decoder uses knowledge of *what message it expects next given the state of
   the contact* (e.g. "I just sent a report, so I'm likely to receive an R+report
   from this specific callsign") to constrain and complete marginal decodes.
   Note: this couples the decoder to the application's QSO state machine.

4. **Averaging + depth control** (`ft8_a7`/`ft8_a8d`, `ndepth`). Combine repeated
   copies of a message and trade CPU for depth.

`ft8_lib` implements **none** of these (verified by source inspection — it is
BP/LDPC decode only). That is the entire gap, and it is well-understood rather
than mysterious.

---

## 3. Why not just use the WSJT-X decoder?

Two viable ways to get its sensitivity, both with real costs:

- **Run it as a subprocess / FFI the existing binary.** Full sensitivity, zero
  decoder porting. Cost: you take on the Fortran toolchain and WSJT-X's large,
  undocumented shared-memory interface (a ~27 MB audio+params struct with an IPC
  handshake) — exactly the heavyweight coupling a clean rewrite is trying to
  shed.

- **Port the whole decoder to another language.** This is a major undertaking,
  and translation is the *easy* part. The hard part is **validation**: a subtle
  numerical divergence does not crash — it silently costs ~1 dB of sensitivity,
  which is the very thing you ported for. Proving parity requires statistical
  testing across many recordings at controlled signal-to-noise ratios. It also
  drags in `packjt77`, FFTW, sync, subtraction, and AP logic — realistically
  3,000–5,000 lines of dense, terse, research-grade Fortran.

Neither is attractive as a first move.

---

## 4. Proposal: start from `ft8_lib`, add the sensitivity tricks incrementally

Rather than choose between "small but less sensitive" and "sensitive but heavy,"
treat the gap as a list of well-understood, independently testable upgrades and
add them to `ft8_lib` in priority order:

| Priority | Upgrade | Approx. size | Sensitivity payoff | Notes |
|---|---|---|---|---|
| **1** | **OSD backstop** | ~400 lines | **Largest single win** | Published algorithm; self-contained; bolts onto `ft8_lib`'s existing BP stage. Can be implemented from the literature, not only from the Fortran. |
| 2 | Multi-pass subtraction | moderate | Big on crowded bands | Needs a waveform re-synth + subtract loop around the existing decode. |
| 3 | A-priori (AP) decoding | moderate | Helps marginal/expected messages | Requires hints from the application's QSO state machine — more integration work. |

The key point for whoever picks this up: **OSD alone closes most of the gap**, it
is the smallest of the three, it is a documented algorithm with other open
implementations to reference, and it attaches cleanly to `ft8_lib` because
`ft8_lib` already produces the bit-reliability information (log-likelihoods) that
OSD consumes. The other two are optional follow-ons.

### Recommended sequence

1. **Integrate `ft8_lib` as-is** and get a working end-to-end decode path.
2. **Measure the real gap.** Run both decoders on the same set of off-air
   recordings (WSJT-X ships `.wav` samples; `ft8_lib` ships test vectors) and
   compare decode counts at various SNRs. This number decides whether any further
   work is justified for the intended use.
3. **If the gap matters, add OSD first.** Re-measure. Stop here if satisfied.
4. **If still needed,** add subtraction, then AP decoding.
5. **Fallback:** if full parity is required without the porting investment, run
   the unmodified WSJT-X decoder as a subprocess and skip steps 3–4.

---

## 5. Bottom line

- `ft8_lib` and the WSJT-X decoder differ by a **known, bounded set of
  algorithms** — primarily OSD, plus subtraction and a-priori decoding — not by
  some opaque quality difference.
- The pragmatic path is **not** to port WSJT-X wholesale (high effort, dominated
  by validation risk), but to **start from `ft8_lib` and add OSD**, which
  delivers most of the sensitivity gain for a fraction of the work and attaches
  cleanly to what `ft8_lib` already computes.
- **Let a measured comparison on real recordings drive the decision** about
  whether to add OSD at all — for many operating styles, baseline `ft8_lib` is
  already sufficient.

### References / source pointers

- `ft8_lib`: <https://github.com/kgoba/ft8_lib> — `ft8/decode.c` (BP decode),
  `ft8/ldpc.c`, `ft8/message.c` (77-bit message packing).
- WSJT-X decode chain (in `lib/ft8/`): `ft8_decode.f90` (orchestrator),
  `ft8b.f90` (per-candidate decoder + pass/AP logic), `decode174_91.f90`,
  `bpdecode174_91.f90` (BP), `osd174_91.f90` (**OSD**), `subtractft8.f90`
  (subtraction), `ft8apset.f90` (a-priori setup).
- OSD background: K9AN/K1JT QEX articles on the FT4/FT8 protocols, and the
  general ordered-statistics-decoding literature.
