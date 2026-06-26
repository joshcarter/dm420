# Bug writeup — FD exchange won't encode (`modes` has no Field Day message type)

> Personal working note (see this folder's `CLAUDE.md` — not a source of truth).
> Tracks the 🔴 blocker in `STATUS.md`: "FD exchange won't encode — `modes`
> packer has no Field Day message type." Written 2026-06-26.

> **Implementation status (2026-06-26): landed on `fd-exchange-encode-writeup` and
> validated against the WSJT-X source + decoder.** The plan below is code, contained
> to `crates/modes/`: a pure `arrl_fd` module (section table + `<count><class>`
> grammar + the `FieldDayExchange` pivot) and the wire pack/unpack in `message.rs`,
> dispatched from `encode_message`/`decode`.
>
> **Interop validation (this is the important part):** cross-checking against the
> real `ft8code` immediately caught that my first section table was wrong — built
> from memory, it had bogus entries (`KP4`, `MAR`, `NT`, `TX`), missed real ones
> (`NS`, `TER`, `NTX`, `PE`, `NB`), and treated `isec` as 0-based. I then validated
> against the actual WSJT-X source at `/Users/Shared/wsjtx/lib/77bit/packjt77.f90`
> and corrected it: the table is now the exact 86-entry `csec` array and `isec` is
> **1-based** (wire = table index + 1), matching the source's
> `format(2b28,b1,b4,b3,b7,2b3)` and `isec=i`. Evidence now in CI / on hand:
> - 3 **golden-vector** tests assert our 77-bit payload is byte-identical to
>   `ft8code` (plain, rogered `R`, and an `n3=4` >16-tx count) — no longer ignored.
> - WSJT-X's **`jt9` decodes our synthesized FD signal** end-to-end in both **FT8**
>   (`K1ABC W9XYZ 6A WI`) and **FT4** (`K1ABC W9XYZ R 6A WI`).
> - Our decoder reads the same WSJT-X-valid WAV back into
>   `FieldDay { class: "6A", section: "WI" }` (RX path).
>
> Remaining: a real over-the-air contact (RF, AGC, timing) — code interop is proven.

## TL;DR

The Field Day bare exchange `<his> <mine> <class> <section>` (the over right after
`CQ FD`) **mis-encodes**. `modes::encode_message("K1ABC N0JDC 3A CO")` packs it as
a standard `to/de/report` message: it treats `3A` as a signal report `+03` and
**silently drops the section** (`CO`). On air it transmits — and decodes — as
`K1ABC N0JDC +03`. Every FD contact sends a garbage report instead of the exchange,
so the QSO scores **zero** ARRL FD points.

The fix is to implement WSJT-X's ARRL-FD 77-bit message type (pack **and** unpack)
in `crates/modes/src/message.rs`.

## The problem

### Symptom

```
modes::encode_message("K1ABC N0JDC 3A CO")  ->  transmits/decodes as  "K1ABC N0JDC +03"
```

The class+transmitter token `3A` becomes a report `+03`; the section `CO` vanishes.
`CQ FD W9XYZ EM48` itself encodes fine (handled by the `is_cq_modifier_tok` path);
**only the bare exchange over is broken.**

### Root cause — the encode side

`encode_message` (`crates/modes/src/message.rs:598`) tokenizes the text and only
ever routes into two packers — standard or free text. There is no Field Day branch:

```rust
} else {
    (tok(&toks, 0), tok(&toks, 1), tok(&toks, 2))   // to, de, extra
};
if let Some(p) = encode_std(&to, &de, &extra, hash) { return Some(p); }
```

For `["K1ABC","N0JDC","3A","CO"]` this yields `to="K1ABC"`, `de="N0JDC"`,
`extra="3A"` — **the 4th token `CO` is never read at all.** Then `encode_std` calls
`packgrid("3A")` (`message.rs:470`). `3A` isn't a grid (first char isn't `A`–`R`)
and doesn't start with `R`, so it falls through to the report branch:
`dd_to_int("3A", 3)` (`text.rs:119`) reads the leading digit and stops at the
letter → `3`, which packs as report `MAXGRID4 + 35 + 3`. On decode, `unpackgrid`
turns that back into `int_to_dd(3, 2, true)` = `"+03"`. That is exactly the
`K1ABC N0JDC +03` we see.

### The other half — the decode side is *also* broken

The 77-bit type for ARRL FD already exists in the type table: `get_type` maps
`i3 = 0, n3 = 3|4 → MessageType::ArrlFd` (`message.rs:117`). But `decode` has **no
arm** for it (`message.rs:132`):

```rust
MessageType::FreeText  => decode_free(p),
MessageType::Telemetry => decode_telemetry_hex(p),
_ => return None,          // ArrlFd lands here
```

So even a **correctly** FD-packed message — whether ours once fixed, or one from a
real WSJT-X station on air — decodes to `None` and is dropped. **We currently can't
receive FD exchanges either.** The reason the broken round-trip "succeeds" today is
only because our encoder emits a *Standard* message, which decodes fine — just to
the wrong text. The fix is genuinely **both directions**, which is why `STATUS.md`
lists `decode` alongside `encode_message`/`encode_std`.

### Correction to STATUS.md's shorthand

`STATUS.md` describes this as "i3=3: 4-bit class + section enum." That is
imprecise. The canonical WSJT-X ARRL-FD pack is **i3 = 0, n3 = 3 or 4**, body =

```
n28a(28) + n28b(28) + ir(1) + intx(4) + nclass(3) + isec(7)   = 71 bits
                                              (+ n3(3) + i3(3) = 77)
```

- `n28a` / `n28b` — the two callsigns (28 bits each; FD has **no** per-call `/R`
  `/P` "ip" bit, unlike standard's 29-bit fields).
- `ir` (1 bit) — whether the exchange carries the leading `R` (the rogered `Tx3`).
- `intx` (4 bits) — **transmitter count − 1**. `n3 = 3` ⇒ tx 1–16, `n3 = 4` ⇒
  tx 17–32. So `ntx = intx + 1 + 16·(n3 − 3)`.
- `nclass` (3 bits) — class letter `A`–`F` → 0–5.
- `isec` (7 bits) — ARRL/RAC section index (0–84).

The existing `ArrlFd` enum variant at `i3=0, n3=3|4` already reflects the real
format — STATUS's "i3=3" is just a slip.

## How to replicate in a test

There is **no existing FD encode/decode test.** `message.rs`'s test module only
covers standard / CQ-DX / free-text / CRC / merge; the `synth_message` round-trip
test in `lib.rs:60` only uses `"CQ K1ABC FN42"`. STATUS's "verified via
`synth_message`→`decode` round-trip" was an ad-hoc check, not a committed test. So
replication = **add a new failing test.** Two levels:

### 1. Unit (`message.rs` `tests`) — tightest repro

Mirrors the existing `roundtrip_std` helper:

```rust
#[test]
fn field_day_exchange_roundtrips() {
    let mut h = CallHash::new();
    let p = encode_message("K1ABC N0JDC 3A CO", &mut h).expect("encode");
    assert_eq!(get_type(&p), MessageType::ArrlFd);          // FAILS today: is Standard
    let mut h2 = CallHash::new();
    let (text, ty) = decode(&p, &mut h2).expect("decode");  // FAILS (None) after encoder fix
    assert_eq!(ty, MessageType::ArrlFd);
    assert_eq!(text, "K1ABC N0JDC 3A CO");                  // today: "K1ABC N0JDC +03"
}
```

Add the rogered form too: `"K1ABC N0JDC R 3A CO"` (the `Tx3` over).

### 2. Through `synth_message` for FT8 *and* FT4 (`lib.rs`)

Proves the whole TX path, matching STATUS's framing: synth the audio, `decode()`
it, assert the message string round-trips. Reuse the shape of
`synth_message_is_mode_aware_and_round_trips`.

Run with `cargo test -p modes field_day`. It fails today (text is `+03`, type is
`Standard`) and passes once both the packer and unpacker land.

## Implementation plan

### Scope — fully contained in the `modes` crate

The rest of the FD path is already built: the `qso` engine produces the correct
exchange *strings* (`crates/qso/src/message.rs:93`, `engine.rs:1192`), and
`core::parse` / the GUI already turn inbound FD exchange *strings* into
`ExchangePayload::FieldDay`. The **only** missing link is the 77-bit pack/unpack
that sits behind the existing public seam — `modes::encode_message` /
`modes::decode` (and therefore `modes::synth_message`). So nothing outside
`crates/modes/` changes: no `types`, `bus`, `qso`, `core`, or GUI edits. This is
the architecturally satisfying part — the wire format is owned by the one crate
that owns the wire format, and everyone else already speaks strings.

### Layering — a semantic pivot type between text and bits

The mistake to avoid is what `encode_std` does for grids: smear token-parsing,
table lookups, and bit-twiddling into one function. We split it into two layers
with a small value type in the middle, so each side is independently testable:

```
  human text  <──parse/format──>  FieldDayExchange  <──pack/unpack──>  [u8; 10]
  "K1ABC N0JDC 3A CO"            (semantic fields)                   (77-bit payload)
        └─ arrl_fd.rs (pure: no hash, no bits) ─┘   └─ message.rs (bits + CallHash) ─┘
```

- **`FieldDayExchange`** (new, in `arrl_fd.rs`) is the semantic middle: the parsed
  meaning of an FD over, independent of both the on-screen string and the wire
  bits. Fields: `call_to: String`, `call_de: String`, `rogered: bool`,
  `ntx: u8` (1–32), `class_idx: u8` (0–5 ⇒ A–F), `section_idx: u8` (0-based index
  into `SECTIONS`).
- **Text ↔ semantics** lives in `arrl_fd.rs` and is **pure** — no `CallHash`, no
  bit layout, trivially unit-tested:
  - `FieldDayExchange::parse(toks: &[&str]) -> Option<Self>` — returns `None` for
    anything that isn't a well-formed FD exchange (so callers fall through to the
    standard/free-text packers).
  - `FieldDayExchange::to_text(&self) -> String` — the canonical
    `"<to> <de> [R ]<ntx><class> <section>"` rendering for decode output.
- **Semantics ↔ wire bits** lives in `message.rs` next to its siblings
  (`encode_std`/`decode_std`), because only there do we have `pack28`/`unpack28`
  and `CallHash`:
  - `encode_arrl_fd(ex: &FieldDayExchange, hash: &mut CallHash) -> Option<[u8; 10]>`
  - `decode_arrl_fd(p: &[u8; 10], hash: &mut CallHash) -> Option<String>`

This keeps `arrl_fd.rs` free of bus/hash/bit concerns (it's reference data + pure
parsing) and keeps the bit math encapsulated with the other message types.

### File-by-file

**1. New module `crates/modes/src/arrl_fd.rs`** (declared `mod arrl_fd;` in
`lib.rs`, alongside `mod message;` — matches the crate's flat-module convention):

- `pub(crate) const SECTIONS: [&str; 85]` — the canonical WSJT-X `csec` table, in
  the **exact order** `unpack77.f90` uses. The array index *is* the 7-bit `isec`
  wire value, so the ordering is the interop contract (see "Why WSJT-X order" in
  this doc). A doc comment says so, and warns against "fixing" it to alphabetical
  or aligning it to `gui/panel_data.rs`'s geographic `SECTIONS`.
- `section_index(sec: &str) -> Option<u8>` / `section_name(isec: u8) -> Option<&'static str>`
  — case-insensitive, trimmed; `None` for unknown (lets non-FD text fall through).
- `Class` parsing: `parse_class(tok: &str) -> Option<(u8 /*ntx 1..=32*/, u8 /*idx 0..=5*/)>`
  matching WSJT-X's `bFieldDay_msg` constraints — 1–2 digits, count ≥ 1 and ≤ 32,
  trailing letter `A`–`F`. Rejects `"0A"`, `"33A"`, `"3G"`, `"R"`, `"3"`.
- `FieldDayExchange` + `parse`/`to_text` as above. `parse` accepts exactly the two
  shapes `[to, de, class, sec]` and `[to, de, "R", class, sec]`, requiring both a
  parseable class token **and** a known section — the conjunction is what keeps a
  4-word free-text line from being mis-claimed as FD.

**2. `crates/modes/src/message.rs`** — wire layer + dispatch:

- `encode_arrl_fd(ex, hash)`: `pack28` each call (reuse its first return; FD has no
  per-call `/R`/`/P` ip bit, so the ip return is ignored — a documented limitation,
  compound/portable calls aren't representable in the FD field and aren't in scope);
  bail `None` if either call fails to pack. Derive `intx`/`n3` from `ntx`, `ir` from
  `rogered`, then assemble the fields MSB-first (see layout below).
- `decode_arrl_fd(p, hash)`: load the 77 bits, take the fields MSB-first,
  `unpack28(n28, 0, 0, hash)` each call (ip = 0, i3 = 0 ⇒ no suffix logic), map
  `class_idx`/`section_idx`/`ntx` back, build a `FieldDayExchange`, return
  `to_text()`. `None` if a call won't unpack or `section_name` is out of range.
- Two one-line dispatch hooks:
  - in `encode_message`, *before* the `encode_std` attempt:
    `if let Some(ex) = arrl_fd::FieldDayExchange::parse(&toks) { if let Some(p) = encode_arrl_fd(&ex, hash) { return Some(p); } }`
  - in `decode`'s match: `MessageType::ArrlFd => decode_arrl_fd(p, hash)?,`

**3. Bit assembly** — to avoid the off-by-one shift bugs `encode_std`'s hand-rolled
`>>`/`<<` chain invites, `encode_arrl_fd`/`decode_arrl_fd` use a `u128` accumulator,
pushing/taking fields MSB-first with the field widths spelled out inline. (We don't
retrofit `encode_std` now — no churn — but the FD path gets the clearer technique.)

### Wire-format reference (WSJT-X type 0.3 / 0.4)

```
field   bits  meaning
n28a     28   call_to   (pack28: DE/QRZ/CQ tokens, 22-bit hash, or standard call)
n28b     28   call_de
ir        1   1 ⇒ exchange carries the leading "R" (the rogered Tx3)
intx      4   transmitter count − 1, low nibble  (combine with n3)
nclass    3   class letter A..F → 0..5
isec      7   section index into SECTIONS (0-based)        ← interop-critical
n3        3   3 ⇒ ntx 1..16,  4 ⇒ ntx 17..32   ⇒  ntx = intx + 1 + 16·(n3 − 3)
i3        3   0
        ─────
         77   (+ 3 pad bits to fill 10 bytes)
```

⚠️ **The one spec detail to pin, not guess: is `isec` 0-based?** This plan treats
the wire value as a 0-based index into `SECTIONS` (the natural Rust mapping, and
consistent with how `get_type` already reads the i3/n3 bits as a 0-based enum). An
off-by-one here silently decodes every section as its neighbour — exactly the
score-poisoning failure this whole bug is about. So it is **gated by a golden
vector**, not by reasoning: see the interop test below. If WSJT-X turns out to
write `isec` 1-based, the fix is a single `± 1` isolated inside
`section_index`/`section_name`, and the golden test is what tells us.

> The full ordered `SECTIONS` list (`AB, AK, AL, AR, AZ, BC, CO, … WV, WWA, WY,
> DX` — 85 entries) goes in `arrl_fd.rs`. **Transcribe it from WSJT-X
> `unpack77.f90`'s `csec` array verbatim** rather than from memory or by sorting;
> the order and the exact membership (note `GH`, `KP4`, `PR`, `VI`, `MAR`, the RAC
> sections, trailing `DX`) are the contract.

### Test plan

Four buckets, fast→broad. All `cargo test -p modes`.

1. **`arrl_fd.rs` unit tests — pure, no bits.** `parse_class` accept/reject table
   (`3A`,`12E`,`32F` ok; `0A`,`33A`,`3G`,`3`,`R`,`ABC` rejected). `section_index ∘
   section_name` round-trips all 85 entries; case/space folded (`" co "` → `CO`);
   unknown (`ZZ`) → `None`; `DX` present. `FieldDayExchange::parse` accepts both
   shapes (plain + rogered), sets `rogered` correctly, rejects 3-token input,
   unknown-section input, and a non-FD 4-tuple; `parse` then `to_text` reproduces
   the canonical string.

2. **`message.rs` unit tests — full bit round-trip** (mirrors the existing
   `roundtrip_std` helper):
   - `field_day_exchange_roundtrips`: `encode_message("K1ABC N0JDC 3A CO")` ⇒
     `get_type == ArrlFd` ⇒ `decode` ⇒ `"K1ABC N0JDC 3A CO"`.
   - rogered form `"K1ABC N0JDC R 3A CO"`.
   - `n3 = 4` path: `"K1ABC N0JDC 20A CO"` (ntx 20) round-trips — exercises the
     transmitter-count split.
   - spread of class letters `A`–`F` and multi-char sections (`SCV`, `EMA`, `DX`).
   - **guard / no-regression:** the existing standard cases still encode as
     `Standard` (a report like `"W9XYZ K1ABC R-09"` is *not* hijacked as FD), and a
     4-word free-text line still falls through to free text.

3. **Interop golden vector — the byte-compat gate.** Assert
   `encode_message("K1ABC W9XYZ 6A WI")` equals a fixed `[u8; 10]` captured from
   WSJT-X for that exact message (via `ft8code "K1ABC W9XYZ 6A WI"`, or by decoding
   our synth with `jt9`). This single test pins `isec` 0-vs-1-based, MSB bit order,
   and the `ntx − 1` offset simultaneously — self-consistency can't catch a
   whole-table shift; this can. Captured once, then it's CI.

4. **`lib.rs` synth round-trip, FT8 *and* FT4** (the STATUS "synth_message→decode
   round-trip" item, now committed): synth `"K1ABC N0JDC 3A CO"` in each protocol,
   `decode()` the audio, assert the message comes back. Mirrors
   `synth_message_is_mode_aware_and_round_trips`.

**On-air confirmation (manual, not CI):** the existing `ab_jt9` example diffs our
decoder against `jt9` on captured WAVs — point it at a real FD slot to confirm the
*decode* side against live WSJT-X traffic; `ft8code`/`jt9`-on-synth confirms the
*encode* side. The golden vector bakes the encode check into CI so we don't depend
on having a radio to stay correct.

### Out of scope / follow-ups (tracked separately in `STATUS.md`)

- Compound/portable (`/P`, `/R`) calls in the FD exchange — not representable in
  the 28-bit FD call field; out of scope, documented in `encode_arrl_fd`.
- The separate STATUS item "Log entries carry no FD-vs-normal tag" — unrelated to
  packing; not touched here.
- Closeout: once this lands and the golden vector is green, flip the 🔴 blocker in
  `STATUS.md` to on-air-validation-only.

## References

- `crates/modes/src/message.rs` — `encode_message` (:598), `encode_std` (:503),
  `packgrid` (:470), `pack28` (:433), `decode` (:132), `get_type` (:110),
  `unpack28` (:179), `unpackgrid` (:246), `MessageType` (:19).
- `crates/modes/src/text.rs` — `dd_to_int` (:119), `int_to_dd` (:95).
- `crates/modes/src/lib.rs` — `synth_message` (:40) and its round-trip test (:60).
- `docs/wsjtx_qso_sequencing.md` §5 — the ARRL Field Day flow + inbound parse.
- `STATUS.md` — the 🔴 blocker entry this writeup expands.
