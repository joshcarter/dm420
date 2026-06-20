# Attribution & licensing — FT8/FT4 decoder (READ THIS)

**The `modes` crate is a from-scratch Rust implementation of the FT8/FT4
digital modes. Its algorithms were reverse-engineered / ported from the C
reference decoder [`ft8_lib`](https://github.com/kgoba/ft8_lib) by Kārlis Goba,
which is MIT-licensed. We owe two attribution obligations and must honor them:**

## 1. ft8_lib — MIT License (Kārlis Goba)

The decode/encode/LDPC/sync/message-packing **algorithms** in this crate were
derived from `ft8_lib`. MIT permits this (use, modify, port, distribute) **on
the condition that the copyright notice and permission notice are retained**.
The full license text is in [`THIRD_PARTY_LICENSES/ft8_lib-LICENSE.txt`](THIRD_PARTY_LICENSES/ft8_lib-LICENSE.txt).

> MIT License — Copyright (c) 2018 Kārlis Goba

**Obligation:** keep that license file in the repo and this notice intact. If we
ever ship a binary, the MIT notice must travel with it.

## 2. FT8/FT4 protocol constants — WSJT-X (Franke / Taylor)

The fixed numeric tables in [`src/constants.rs`](src/constants.rs) — the Costas
sync arrays, the Gray map, the **LDPC(174,91)** parity/generator matrices, the
CRC-14 polynomial, and the FT4 whitening sequence — are the *interoperable
on-air specification* of FT8/FT4, designed by Steven Franke (K9AN) and Joe
Taylor (K1JT) and published in WSJT-X (GPLv3). They are facts that any
independent implementation must reproduce bit-for-bit to decode real signals.
We transcribe them as data (cross-checked against ft8_lib's MIT copy); all logic
that consumes them is our own.

## What is and isn't ours

- **Ours (written from scratch):** the STFT waterfall, Costas sync search,
  soft-symbol LLR extraction, the LDPC belief-propagation decoder, CRC, GFSK
  synthesis, message pack/unpack, the slot clock, and all kenctl integration.
  These are independent Rust ports of the *algorithms*, not copies of the C source.
- **Third-party (crates):** the forward FFT is the [`realfft`](https://crates.io/crates/realfft)
  crate (MIT) on [`rustfft`](https://crates.io/crates/rustfft) (MIT OR Apache-2.0).
  It replaced our original hand-rolled radix-2 + Bluestein FFT (~10x faster). Both
  are permissively licensed; ship their notices with any distribution.
- **Not ours:** the protocol constant tables (WSJT-X spec) and the algorithmic
  approach (ft8_lib, MIT). Credit and license preserved as above.

## POC reminder

This is a proof-of-concept that will fold into a larger application. **Before
distributing anything built on this crate, re-confirm both obligations above are
satisfied** (MIT notice shipped; protocol constants credited). If the larger app
has stricter licensing needs, the clean path is a fully independent
re-derivation of the constants from the FT8/FT4 spec documents.
