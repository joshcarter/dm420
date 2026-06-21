# FT4 ⇄ FT8 architecture — design notes before implementing FT4 transmit

> **Update — built.** The design below is now implemented and offline-verified: the FT4
> synth is sample-identical to the `ft8_lib` reference (`ft4_cq_1200.wav`, Pearson r = 1.0).
> A first on-air FT4 QSO is the one remaining acceptance test. Original pre-implementation
> notes follow unchanged.

Status as of this writing: **decode is FT8+FT4 (done, tested); encode/synth is FT8-only.**
Receiving FT4 works end-to-end; the QSO engine, interlock granter, audio-TX, slot
clock, GUI mode toggle, and per-mode calling-frequency table are all already mode-aware.
The one real gap is the **FT4 transmit waveform** plus a few cross-cutting fixes. This
doc is the design we agreed to think through *before* writing that code.

## Guiding principle (already implicit in the decoder)

The modes crate already has a consistent philosophy — we should extend it, not invent a
new one:

1. **Mode *facts* (scalars) live on the `Protocol` enum.** Today: `symbol_period()`,
   `slot_time()`, `num_tones()` (`waterfall.rs`).
2. **Shared math is protocol-free.** `crc`, `ldpc`, `text`, `fft`, and the 77-bit
   `message` layer know nothing about FT8 vs FT4.
3. **Direction-specific code branches *locally and symmetrically*** where the protocols
   genuinely differ. The decoder does exactly one `if wf.protocol == Ft4 { … } else { … }`
   in each of `find_candidates`, `extract_llr`, `decode_candidate`. The encoder should
   read the same way, in the same places, inverted.

The goal: reuse the whole spine, and confine FT4's differences to the same handful of
seams the decoder already uses — so a reviewer can read encode and decode side by side.

## The shared spine — reuse unchanged

| Layer | File | Why it's already mode-agnostic |
|---|---|---|
| 77-bit message pack/unpack | `message.rs` | FT8 and FT4 share the **identical** 77-bit message set (`CQ/grid/report/RR73/73`, Field Day, etc.). `encode_message(text) -> [u8;10]` is protocol-free. |
| CRC-14 | `crc.rs` | same polynomial both modes |
| LDPC(174,91) | `ldpc.rs` | `encode174` / `bp_decode` — same code both modes |
| char tables | `text.rs` | shared |
| forward FFT | `fft.rs` | shared |
| STFT waterfall | `waterfall::Monitor` | **already** parameterized by `Protocol` (pulls `symbol_period` → `block_size`/`nfft`) |
| slot clock | `slot.rs` | **already** parameterized by `Protocol` |
| GFSK kernel | `encode::synth_gfsk` / `gfsk_pulse` / `erf` | **already** generic over `bt` + `symbol_period` — FT4 just passes `bt=1.0`, `symbol_period=0.048` |

Notably `synth_gfsk(symbols, f0, bt, symbol_period, sample_rate)` is already the right
shape. The FT8-specific pieces in `encode.rs` are only `ft8_tones` (tone layout) and
`synth_ft8` (hardcoded period/slot/BT + slot centering).

## Where FT8 and FT4 genuinely differ

| Concern | FT8 | FT4 | Lives today |
|---|---|---|---|
| Symbol period | 0.160 s | 0.048 s | `Protocol::symbol_period()` |
| Slot length | 15 s | 7.5 s | `Protocol::slot_time()` |
| FSK order (tones) | 8 | 4 | `Protocol::num_tones()` |
| Bits / symbol | 3 | 2 | implicit (log2 tones); inline in `extract_llr` |
| Gray map | `FT8_GRAY[8]` | `FT4_GRAY[4]` | `constants.rs` + inline branch |
| Costas sync | one `[7]`, ×3 | four `[4]`, ×1 | `constants.rs` + inline branch |
| Sync geometry | `NUM_SYNC=3 LEN=7 OFFSET=36 ND=58` | `NUM_SYNC=4 LEN=4 OFFSET=33 ND=87` | `FT8_*`/`FT4_*` consts in `decode.rs` |
| Channel symbols | 79 | 105 | literal in `find_candidates` |
| Payload whitening | none | XOR `FT4_XOR` | inline branch in `decode_candidate` |
| GFSK BT | 2.0 | 1.0 | `encode.rs` const |
| Lead/trail ramp | env ramp only | explicit ramp symbols (→105) | encode only (not yet written) |

## Recommended design

### 1. Consolidate mode facts onto `Protocol` (one source of truth)

This is the load-bearing improvement and it's what "reuse what should be reused" means
here. The scalar facts the encoder needs are the *same facts the decoder already uses* —
but some currently live as `FT8_*/FT4_*` constants inside `decode.rs`. Pull those onto
`Protocol` so both directions read one place:

```rust
impl Protocol {
    // existing: symbol_period(), slot_time(), num_tones()
    fn bits_per_symbol(self) -> usize;   // 3 / 2   (= log2 num_tones)
    fn channel_symbols(self) -> usize;   // 79 / 105
    fn gfsk_bt(self) -> f32;             // 2.0 / 1.0
    fn whitening(self) -> Option<&'static [u8; 10]>;  // None / Some(&FT4_XOR)
    // sync geometry, currently FT8_*/FT4_* consts in decode.rs:
    fn num_sync(self) -> usize;          // 3 / 4
    fn length_sync(self) -> usize;       // 7 / 4
    fn sync_offset(self) -> usize;       // 36 / 33
    fn data_symbols(self) -> usize;      // 58 / 87   (== ND)
}
```

Keep the raw tables (`FT8_COSTAS`, `FT4_COSTAS`, `FT8_GRAY`, `FT4_GRAY`, `FT4_XOR`) in
`constants.rs`. Do **not** try to force the Costas tables behind one uniform accessor —
they're structurally different shapes (`[u8;7]` vs `[[u8;4];4]`), so the jagged layout
stays as a localized branch (see #2), exactly as in `extract_llr`. Enum methods cover
the *scalars*; the *structural* layout stays a local branch. After this, `decode.rs` can
drop its private `FT8_*/FT4_*` consts and read `wf.protocol.*` — net DRYer, and decode +
encode share the vocabulary.

### 2. The encoder mirrors the decoder (symmetry)

Replace the two FT8-only functions with protocol-driven ones that branch only where
decode branches:

- `tones(payload: &[u8;10], p: Protocol) -> Vec<u8>` — inverse of `extract_llr`:
  - apply whitening (#3),
  - `payload_with_crc` → `encode174` → 174-bit codeword (**shared, unchanged**),
  - lay out Costas + Gray-coded data. The data-symbol channel positions **must** equal
    decode's `extract_llr` mapping (FT4: data symbol `k` at channel index `k+5 / k+9 /
    k+13` for the three thirds; FT8: `k+7 / k+14`). That mapping — plus ft8_lib's
    `gen_ft4` for the Costas/ramp positions — is the authority.
- `synth(payload, p, f0, sr) -> Vec<f32>` — pulls `symbol_period`/`slot_time`/`gfsk_bt`
  from `Protocol`, calls the **existing** `synth_gfsk`, centers in the slot.

Keep `synth_ft8`/`synth_message` as thin wrappers so existing callers/tests don't churn.

### 3. FT4 whitening is the exact inverse of decode

Decode validates the CRC over the **whitened** on-air bits, then un-whitens to read text
(`decode_candidate`: CRC check on `a91`, *then* `payload[i] = a91[i] ^ FT4_XOR[i]`).
So encode must whiten **before** CRC:

```
message payload ──XOR FT4_XOR──▶ whitened ──payload_with_crc──▶ a91 ──encode174──▶ codeword
```

`payload_with_crc` and `encode174` are reused verbatim; the only new step is one XOR
gated by `Protocol::whitening()`. (FT8: `whitening()` is `None`, no XOR — identical path.)

### 4. Crate-boundary separation of concerns

Keep the two mode vocabularies cleanly separated:

- **`modes` speaks `Protocol`** (Ft8/Ft4) and must stay ignorant of the bus types — this
  is what keeps it independently buildable.
- **`bus`/`types` speaks `OverAirMode`** (Ft8/Ft4/Psk31/Rtty/…), a superset.
- **`core` owns the single conversion.** Today `decode.rs:77` hand-maps `Protocol →
  OverAirMode`; `tx.rs` needs the reverse. Add one `fn protocol_of(OverAirMode) ->
  Option<Protocol>` (or `TryFrom`) in `core` and use it in both places. Don't scatter the
  mapping; don't push `OverAirMode` into `modes`.

### 5. Cross-cutting fixes (outside `modes`)

- `lib.rs::synth_message` gains a `Protocol` (or `OverAirMode`) param and dispatches.
  One caller: `core/tx.rs`.
- `core/tx.rs`: delete the `if mode != Ft8 { "not implemented" }` guard; convert mode via
  #4; pass it to `synth_message`. The `mode` already arrives correct from the QSO shell.
- **Slot-relative TX caps:** `MAX_TX` (14 s) and `PTT_WATCHDOG` (15 s) are FT8-sized and
  exceed a whole 7.5 s FT4 slot. Derive both from `slot_period_ms(mode)` (they're
  backstops — normal key-down is playback-driven — but on FT4 a stuck over would bleed
  ~2 slots).
- **FT4 CQ-first mode source:** `qso/shell.rs` tracks `mode` only from inbound decodes,
  defaulting to FT8. Calling CQ on a quiet FT4 frequency before hearing anyone would tag
  the first over FT8. Seed `mode` from the *configured* protocol (`AudioControl`/
  `current_config`), not just decodes. Matters for Field Day (lots of CQ-calling).

## Considered alternative: a `ModeSpec` struct

Instead of enum methods, a `Protocol::spec() -> &'static ModeSpec { period, tones, bits,
costas, gray, xor, bt, … }` consumed by both encode and decode. **Verdict: not now.** For
two modes it's more churn (rewrites decode's working inline branches into spec lookups)
for little gain, and the jagged Costas field is awkward to model. Revisit if/when a third
77-bit mode arrives (FST4, Q65) — then a table-per-mode pays off. The enum-method approach
(#1) is a strict, low-risk step in that direction.

## Test strategy

- **Round-trip** `encode→synth→decode` for FT4, mirroring the FT8 round-trips already in
  `decode.rs` tests; assert message + audio-freq recovered.
- **Reference-vector** check: FT4 tone array for a known message vs ft8_lib output (the
  decoder side already validates against `ft4_cq_1200.wav`).
- Existing `decodes_reference_ft4` + `ft4_slots_are_half_as_long` stay green.
- On-air FT4 QSO (the real acceptance test).

## Touch list

- `modes/src/waterfall.rs` — extend `Protocol` (#1)
- `modes/src/decode.rs` — read new `Protocol` methods; drop private `FT8_*/FT4_*` consts (#1)
- `modes/src/encode.rs` — `tones`/`synth`/`synth_ft4` (+ keep `synth_ft8` wrapper) (#2, #3)
- `modes/src/lib.rs` — `synth_message` gains protocol (#5)
- `modes/src/constants.rs` — unchanged (tables already present)
- `core/src/tx.rs` — drop guard, convert mode, slot-relative caps (#4, #5)
- `core/src/decode.rs` — share the one mode mapping (#4)
- `qso/src/shell.rs` — CQ-first mode source (#5)
