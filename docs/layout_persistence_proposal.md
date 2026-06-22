# Proposal: generalized panel-layout persistence

**Status:** proposal (not yet implemented). Captures the design discussed for
replacing the hand-maintained tile-share persistence with full `egui_tiles`
tree serialization, so saved layouts survive panel **rearrangement** and the
**addition/removal of panels** — neither of which the current scheme supports.

## 1. What's built today (and why it's brittle)

The layout is persisted as a fixed set of **named tile shares**, not as a real
layout structure:

- `settings::LayoutShares` (`crates/gui/src/settings.rs`) is a struct of exactly
  five floats — `waterfall, right, log, band, contacts` — written to the
  `[layout]` table of `~/.dm420/config.toml`.
- `App::current_layout()` reads each share with `share_of(TileId)`; `build_tree()`
  writes each back with `Shares::set_share(...)` at startup
  (`crates/gui/src/main.rs`).
- `TreeIds { root, right, band, waterfall, log, contacts }` hard-codes the exact
  tree shape: a horizontal root split (waterfall | right) whose right child is a
  vertical stack (log / band / contacts).

Three consequences:

1. **It cannot represent anything but the one baked-in shape.** Rearranging
   panes, reordering the right stack, grouping panes into tabs, or adding a new
   panel has no field to live in. A new panel means new struct fields, new
   `TreeIds` members, new `build_tree` wiring, and new save/restore lines.
2. **Band Scan is force-pinned every frame.** `pin_band_height()` overwrites the
   `right` container's `band` share on *every* frame to lock Band Scan to a fixed
   pixel height. So Band Scan's split is intentionally not resizable and cannot
   persist — by construction.
3. **Layout is only written as a side effect of a window-geometry change.** The
   reactive save (`maybe_persist_window`) calls `save_window_layout(...)` only
   when the window size/position/fullscreen settles; the explicit
   `persist_window_layout()` runs on `close_requested`, which (per the code's own
   comment) Cmd+Q does not deliver. Dragging only a panel divider — without
   touching the window — may never be saved, and the per-frame band-pin rewriting
   the right container makes that stack especially fragile to capture.

This is why, in practice, the root left/right split appears to "stick" while the
right-stack splits do not reliably round-trip.

## 2. The generalized design

`egui_tiles::Tree<Pane>` is fully serde-serializable **when `Pane` is**. A
serialized tree captures the entire structure generically: every container, its
kind (linear/grid/tabs), child order, and split ratios (`Shares`). That is
exactly the thing we want to persist.

The panes today are `Box<dyn Panel>` — trait objects holding live view state,
which is not serializable. The standard `egui_tiles` pattern is to **split the
identity of a pane from its runtime state**:

1. **`PanelKind` — a lightweight, serializable pane id.**
   ```rust
   #[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
   pub enum PanelKind { Waterfall, Log, Band, Contacts }
   ```
   The tree becomes `Tree<PanelKind>`. This is the only thing serialized.

2. **Runtime state lives in a side map**, keyed by kind:
   ```rust
   panels: HashMap<PanelKind, Box<dyn Panel>>
   ```
   The `Tactical` behavior borrows this map and, in `pane_ui`, looks up
   `panels.get_mut(kind)` to render. (The behavior already borrows `&self.view`,
   palette, etc.; add `panels: &mut HashMap<PanelKind, Box<dyn Panel>>`.)

3. **Persist by serializing the whole tree.**
   - Serialize `Tree<PanelKind>` to JSON and store it (see §4 on where).
   - On launch, deserialize; if absent/garbled, fall back to `default_tree()`
     (today's baked-in shape).

4. **Save on layout change, not on window movement.** Each frame, after
   `tree.ui(...)`, serialize the tree and compare a cheap hash against the last
   saved hash; if changed, debounce-write. Decouples layout persistence from
   window geometry. (Window geometry stays its own `[window]` save.)

### Migration: added / removed panels

A saved tree is from a *previous build* whose panel set may differ:

- **Unknown kind in the saved tree** (a panel was removed): drop those tiles
  during load and let `egui_tiles` simplification collapse the now-empty
  containers. Because `PanelKind` is an enum, `serde` deserialization of an
  unknown variant fails — so either (a) deserialize into a permissive form and
  filter, or (b) version the format and migrate. Simplest robust approach:
  after deserializing, walk the tree and remove any pane whose kind is not in the
  current `PanelKind` set, then `tree.simplify(...)`.
- **Expected kind missing from the saved tree** (a panel was added since the
  layout was saved): after load, for every `PanelKind` not present as a pane,
  splice it in at a sensible default location (e.g. as a new tab on the right
  stack, or a new split). This is the one piece of real policy to decide.

A small **format version** integer stored alongside the tree lets us route old
layouts through explicit migrations instead of silently discarding them.

## 3. Helpers that must resolve tiles by kind

Several helpers currently key off the fixed `TreeIds`. With a serialized tree the
`TileId`s are regenerated each launch, so these become **kind → TileId lookups**
(scan `tree.tiles` for the pane with the given `PanelKind`):

- `pin_band_height()` — find the Band pane and its parent container. (Or retire
  it entirely; see §5.)
- `enforce_min_width()` — operates on the root horizontal split; find the root
  container generically (it already takes a `TileId`).
- Keyboard focus `TreeIds::by_number()` — map `1..4` to a `PanelKind`, then to
  its current `TileId`.
- Top-bar identity marker — `top_bar()` looks up the Waterfall pane's rect to
  align the callsign; resolve by kind.

A single helper — `fn tile_of(tree, kind) -> Option<TileId>` — covers all of
these.

## 4. Where to store it

Two options:

- **A.** Inline JSON string in the existing `config.toml` `[layout]` table
  (`tree = "..."`). Keeps one config file; the TOML stays hand-editable except
  for that one opaque value.
- **B.** A sibling file `~/.dm420/layout.json`. Cleaner separation; the tree JSON
  is verbose and arguably doesn't belong inside the hand-editable TOML.

Recommendation: **B** (`layout.json`), with the old `[layout]` shares read once as
a fallback/migration source so existing configs aren't lost.

## 5. Open decisions

1. **Keep or drop the Band Scan height-pin?** If we keep `pin_band_height`, Band
   Scan stays a fixed-height row that the user can't resize (its serialized share
   is then cosmetic). If we drop it, Band Scan becomes a normal resizable pane
   remembered like the others. (This was offered as a variant of the work.)
2. **Default placement for newly-added panels** on migration (new tab vs new
   split, and where).
3. **Storage location** (§4).
4. **Reset-to-default affordance** — a control (e.g. in the unlocked/EDIT view)
   to discard the saved tree and rebuild `default_tree()`, since a serialized
   layout can otherwise strand a user in a bad arrangement.

## 6. Prerequisite

`egui_tiles` is currently pulled in without features (`egui_tiles = "0.15.0"` in
the root `Cargo.toml`). Tree serialization requires its **`serde` feature**:

```toml
egui_tiles = { version = "0.15.0", features = ["serde"] }
```

(Pin stays at 0.15.0 per the egui-stack pinning convention in `CLAUDE.md`.)

## 7. Suggested rollout (incremental, each step compiles)

1. Add the `serde` feature; introduce `PanelKind` and the `panels:
   HashMap<PanelKind, Box<dyn Panel>>` side map; change the tree to
   `Tree<PanelKind>` and route `pane_ui` through the map. **No behavior change,
   no persistence yet** — pure refactor, verifiable by build/clippy + a visual
   pass (layout identical to today).
2. Add `tile_of(tree, kind)` and convert `pin_band_height` / `enforce_min_width`
   / `by_number` / top-bar lookup to it. Retire `TreeIds`.
3. Serialize/deserialize the whole tree (with version + fallback to
   `default_tree()`); switch the save trigger to layout-change debounce. Remove
   `LayoutShares` and the `[layout]` share fields (read once for migration).
4. Add migration (unknown-kind drop, missing-kind splice) and the reset-to-default
   control. Only now is rearrange/add-panel fully supported.

Steps 1–2 are safe groundwork even if full serialization lands later.

---

*Cross-references: `crates/gui/src/main.rs` (`build_tree`, `TreeIds`,
`pin_band_height`, `enforce_min_width`, `Tactical`, `current_layout`,
`maybe_persist_window`, `persist_window_layout`), `crates/gui/src/settings.rs`
(`LayoutShares`, `read_layout_shares`, `save_window_layout`),
`crates/gui/src/panels/mod.rs` (`Panel`, `PanelCtx`).*
