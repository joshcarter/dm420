# Bug writeup — FD exchange won't encode (`modes` has no Field Day message type)

> Personal working note (see this folder's `CLAUDE.md` — not a source of truth).
> Tracks the 🔴 blocker in `STATUS.md`: "FD exchange won't encode — `modes`
> packer has no Field Day message type." Written 2026-06-26.

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

## How we'll fix it

All self-contained in `crates/modes/src/message.rs`, mirroring WSJT-X's pack:

1. **Section table.** Add the canonical WSJT-X 85-entry ordered section array
   (`AB, AK, AL, … WY, DX`) as a `const` — index ↔ 7-bit `isec`.
   ⚠️ This must be the **WSJT-X order**, not `gui/panel_data.rs`'s `SECTIONS`
   (that table is map-centric/geographic and only carries lon/lat — wrong order
   for on-air interop). The canonical list does not exist anywhere in the repo
   yet; it has to be added.

2. **Encoder** — `encode_arrl_fd(call_to, call_de, rogered, ntx, class, section)`:
   - Reuse `pack28`'s first return (the 28-bit call value) for both calls — FD has
     no per-call `/R`/`/P` ip bit, so ignore the ip return.
   - Map class `A`–`F` → 0–5; look up `section` → `isec`; compute
     `intx = (ntx − 1) & 0xF` and `n3 = 3 + (ntx − 1)/16`; set `ir` from the
     leading `R`.
   - Bit-pack into `p[0..10]`:
     `n28a(28) | n28b(28) | ir(1) | intx(4) | nclass(3) | isec(7) | n3(3) | i3=0(3)`.

3. **Router** — in `encode_message`, detect the FD shape *before* falling to
   `encode_std`: tokens of form `[to, de, <count><classLetter>, section]` and
   `[to, de, "R", <count><classLetter>, section]`, where the class token is
   `1–2 digits + A–F`. Parse `3A` → (ntx=3, class=A), call `encode_arrl_fd`.
   (The `CQ FD …` opener already works via the `is_cq_modifier_tok` path — leave it.)

4. **Decoder** — add `MessageType::ArrlFd => decode_arrl_fd(p, hash)` to `decode`
   (`message.rs:140`). Extract the fields with the FD bit boundaries (not
   `decode_std`'s 29-bit layout), `unpack28(n28, 0, 0, hash)` for each call,
   reverse the class/section/ntx mapping, and rebuild
   `"<to> <de> [R ]<ntx><class> <section>"`.

5. **Tests** — commit the failing tests above; they become the regression guard.
   Worth adding a **fixed-payload** assertion against a known WSJT-X byte pattern
   for `K1ABC W9XYZ 6A WI` (the classic example) so we are byte-compatible with
   real radios, not just self-consistent.

### Why this is the only missing link

The rest of the FD path is already built: the `qso` engine builds the correct
exchange *strings* (`crates/qso/src/message.rs:93`, `engine.rs:1192`), and
`core::parse` / the GUI already parse inbound FD exchanges into
`ExchangePayload::FieldDay`. The only gap is the 77-bit pack/unpack itself in
`modes`. Distinct from the now-fixed send-box/engine config split.

## References

- `crates/modes/src/message.rs` — `encode_message` (:598), `encode_std` (:503),
  `packgrid` (:470), `pack28` (:433), `decode` (:132), `get_type` (:110),
  `unpack28` (:179), `unpackgrid` (:246), `MessageType` (:19).
- `crates/modes/src/text.rs` — `dd_to_int` (:119), `int_to_dd` (:95).
- `crates/modes/src/lib.rs` — `synth_message` (:40) and its round-trip test (:60).
- `docs/wsjtx_qso_sequencing.md` §5 — the ARRL Field Day flow + inbound parse.
- `STATUS.md` — the 🔴 blocker entry this writeup expands.
