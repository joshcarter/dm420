# TX audio offset hold — proposal

TBD: I'm not sold on double-click as the right UI. Maybe something
like tab.

**Status:** proposal (not yet built). Author conversation: N0JDC.

## Motivation

Sometimes it's best to pick a clear space in the spectrum and transmit there
regardless of where the other stations are — so long as everyone is inside each
other's audio passband. Once an operator has found a clean slot, they want to
**CQ or answer other stations while staying at that offset**, instead of the
default behavior of zero-beating each station they answer.

This is conceptually WSJT-X's "Hold Tx Freq."

## Naming

The project already uses **locked/unlocked** for the global operate/configure
posture, and there is a separate selection-lock while a QSO is armed. To avoid
collision this feature is called **"offset hold" / "held"** in code, with a small
lock glyph in the UI.

## How offset works today (relevant facts)

- **Waterslide click** (`crates/gui/src/panels/waterfall.rs`) resolves to a
  `RealSel { offset, target, resume }`. A bare-spectrum click reads the offset off
  the vertical position; clicking a decoded line snaps `offset` to that station and
  sets `target`.
- **SEND** publishes `Selection { outgoing }` and a `QsoCommand`
  (`CallCq` / `Start{target}` / `Resume{…, offset}`) via `crates/gui/src/bus_view.rs`.
- **Engine** (`crates/qso/src/engine.rs`):
  - CQ side (`commit_from_cq`): transmits at **our** offset. ✅ already what we want.
  - Resume (`resume_from`): transmits at the **offset the GUI passed**. ✅ already
    controllable.
  - **Armed→answering (`commit_from_armed`, engine.rs:276): snaps to the *target's*
    CQ offset.** ⛔ This is the one place that overrides a held offset.
- **Decode→QSO matching is by callsign + addressing, never by offset**
  (engine.rs:505–516, `addressed()`). So "stations answering at other offsets"
  already works: when we CQ at a held offset and someone replies on their own
  frequency, we still recognize them by call and keep TXing at our offset. This
  needs **verification, not new code**.
- **Double-click** currently resets the zoom level (`apply_view_gestures`,
  waterfall.rs:798). That gesture gets repurposed (zoom-reset removed — the operator
  can always zoom out).

## Interaction model

State: `held: Option<f32>` on the `Waterfall` panel.
`None` = today's free-follow mode; `Some(hz)` = pinned.

**While NOT held:**

| Gesture | Action |
|---|---|
| single-click bare spectrum | set offset to click (today's behavior) |
| single-click decoded line | prime station + snap offset (today's behavior) |
| **double-click anywhere** | **set `held = Some(<offset at click / line's offset>)`** (replaces zoom-reset) |

**While held:**

| Gesture | Action |
|---|---|
| **single-click decoded line** | **prime station only; offset stays pinned** (new) |
| single-click bare spectrum | **unlock** (`held = None`) + set offset to click (back to today's mode) |
| double-click anywhere | **move** the held offset to the new click; stays held |

**Open default (may change):** double-click on a decoded *line* pins to that line's
frequency but does **not** prime the station — priming stays a single-click action,
keeping double-click meaning exactly "pick + lock offset."

## Changes

### 1. `crates/types/src/lib.rs` — `Selection`

Add `pub hold: bool`. Touches a struct flagged `[Joel/Josh own]` for its stability
key — adding a field is low-risk, but **flag for W4LL review** per the convention on
`types` changes. Update the `Selection` test (lib.rs:910) and
`docs/message-catalog.md:164`.

### 2. `crates/qso/src/engine.rs` — honor hold on the one path that snaps

- Store `hold: bool` on `Engine`, set from `Event::Select(sel)` (alongside the
  existing `self.outgoing = sel.outgoing` at line 176).
- In `commit_from_armed`, replace `offset: *offset` (line 276) with
  `offset: if self.hold { self.outgoing } else { *offset }`.
- That is the whole engine change. The CQ and resume paths already use our/GUI
  offset.

### 3. `crates/gui/src/bus_view.rs` — thread the flag

- `publish_selection(offset_hz, hold, target)` sets `Selection.hold`.
- `call_cq` / `answer_station` / `resume_qso` take a `hold: bool` and forward it.
  (`resume_qso` already passes the offset; when held, the caller passes the held
  offset.)

### 4. `crates/gui/src/panels/waterfall.rs` — UI behavior + indicator

- Add `held: Option<f32>` to `Waterfall`.
- In `ui()`, compute a `double_click` pointer position alongside `click` (gate both
  on not-armed, same as today).
- Remove the double-click branch from `apply_view_gestures` (zoom reset goes away).
- Resolve clicks per the matrix above: double-click sets/moves `held`; single-click
  bare while held clears `held`; single-click line while held primes without touching
  offset. When `held.is_some()`, force `real_sel.offset = held` so SEND publishes the
  pinned value.
- Pass `held.is_some()` into the `call_cq/answer_station/resume_qso` calls in
  `draw_send_row`.
- **Visual:** render the outgoing-frequency lane (waterfall.rs:1434) with a distinct
  "pinned" treatment (lock glyph / solid accent border) when held, so the operator
  can see the offset is locked.

### 5. Docs + TODO

- `docs/qso_flow.md` (gesture table ~75–77, offset policy §5/§173): document
  offset-hold and that it overrides the answer-side zero-beat snap.
- `docs/waterslide_panel.md` (~49–50): document double-click-to-pin,
  single-click-to-release, and the indicator; update "held for the duration of a QSO"
  to note the new persistent hold.
- `TODO.md`: track this; note the FT4 PTT watchdog is unaffected.
- **Out of scope for v1:** mirroring the gesture in **mock mode**
  (`waterslide_panel.rs`, the `Target` enum + its own double-click). Real mode is the
  on-air path; mock parity is a possible follow-up.

## Risks / things to watch

- **egui double-vs-single click:** a double-click also emits a single `clicked()`
  (the first press). Make double-click authoritative so a pin doesn't briefly
  unlock/retune in the same gesture — defer the single-click action one frame or
  suppress it when `double_clicked()` fires.
- **Held offset must survive across QSOs** (that's the point — "CQ or answer others
  while staying put"). The existing post-QSO cleanup only clears `target`, not the
  offset, so this is consistent; ensure `held` is untouched by QSO completion.
- **Session-only** (not persisted to `config.toml`), matching how zoom isn't
  persisted. Could be persisted later if wanted.

## Verification

- `cargo build --workspace`, `cargo test --workspace` (engine has unit tests around
  the commit paths — add one for held `commit_from_armed`),
  `cargo clippy --all-targets -- -D warnings`.
- Per project convention, build + clippy here and hand the visual confirmation (pin
  indicator, gesture feel) to the author to run.
