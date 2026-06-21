# Call Sign panel

A read-only instrument that resolves the **currently selected station** into
human-readable identity: **name**, **country**, and a **country flag** icon,
plus the location/signal facts DM420 already knows locally. It turns a bare
callsign — the thing you selected on the waterslide or clicked on the map — into
"who is this, and where are they."

It lives in the **right column, between Band Scan and Contacts** (top-to-bottom:
Log Book · Band Scan · **Call Sign** · Contacts).

## Selection input — already wired

The panel needs **no new selection plumbing**. `PanelCtx.selected_station:
&mut Option<String>` is the shared per-frame selection channel: the Waterfall
("Digital") panel writes it from its current target (`Waterfall::selected_call`),
and the Contacts map writes it on a marker click (`pick_station`). Both of the
user's entry points — "select traffic in the Digital or Contacts panel" — already
funnel into this one `Option<String>`. The Call Sign panel simply **reads** it
each frame and renders a card for that callsign (and clears to a placeholder when
it's `None`, e.g. after a QSO completes and selection is dropped).

Because selection clears on QSO completion (see commit *"lock selection mid-QSO,
deselect on completion"*), the panel should treat `None` as "show the idle
placeholder" rather than holding a stale card. (Open question below: hold the last
card briefly vs. clear immediately.)

## What it shows

For the selected callsign, top to bottom:

- **Callsign** — large, in the mono tactical face, with the operator-vs-peer
  distinction irrelevant here (this is always a remote station).
- **Flag** — the country's flag as an icon next to the callsign or country line.
- **Name** — the operator's name when known (online/cached only; see below).
- **Country / DXCC entity** — the resolved entity name (e.g. "Germany",
  "United States"). Always available offline from the prefix.
- **Location** (all derived locally, no network): the Maidenhead **grid** if we've
  heard one, the grid's lat/lon, and **distance + bearing** from the operator's own
  grid (`PanelCtx.grid`, the same math `panel_data::grid_to_lonlat` already does for
  the map).
- **Signal / status** (from the bus, locally): last-heard time, last SNR, the band
  it was heard on, whether it was **calling CQ**, and **worked / unworked** (per
  band — the Field Day rule). All of this is already carried on `MapSpot`
  (`bus_view::heard_spots()` / `worked_spots()`) and the log book; the panel just
  finds the spot whose `call` matches the selection.

Everything except **name** is available with **zero network access**. That split is
the heart of the design.

## Data sources — the offline / online split

DM420 is offline-first (Field Day, no central database). The lookup is therefore
**two tiers**, and the panel is fully useful with only the first:

### Tier 1 — offline, always available (no network, no accounts)

- **Country + flag from the callsign prefix.** A bundled **DXCC/ITU prefix table**
  maps a callsign's prefix to a DXCC entity + ISO country code (which keys the flag
  icon). This is a pure lookup over static data — **no I/O**. No such table exists
  in the repo today; it must be added as a bundled data asset (see *Crate &
  data placement*). Prefix resolution has well-known edge cases (e.g. `KG4xx` is
  Guantánamo only for 5-char calls; portable suffixes like `/MM`, `/P`; compound
  prefixes) — the table and resolver must handle longest-prefix matching and the
  documented exception ranges, not a naïve two-letter cut.
- **Grid, location, distance/bearing, SNR, band, CQ, worked/unworked** — all from
  the live bus (`MapSpot`) and log book, same data the map already consumes.

### Tier 2 — optional online enrichment (name; better country precision)

- **Name** (and a more authoritative country/QTH) requires an online callbook.
  Recommended providers, in order of friction:
  - **callook.info** — US/FCC only, free, **no auth**. Good zero-config default for
    US Field Day.
  - **HamQTH** — global, free, requires a login (session token).
  - **QRZ.com XML** — global, requires a paid subscription.
- This is **real network I/O** → it belongs in a **`core` service**, never in the
  GUI. The service serves a `CallsignLookup` request over the bus and caches
  results on disk (`~/.dm420/callbook-cache.json`, parallel to the log book) so
  repeated selections and **fully-offline sessions still show previously-seen
  names**. The panel shows the name when resolved and leaves it blank ("—")
  otherwise — never blocking, never erroring into the UI.

**Recommendation:** ship Tier 1 first and complete (it satisfies "country + flag"
and adds genuinely useful local data); add Tier 2 as a follow-on so "name" appears
when the network and a configured provider are present.

## Architecture & crate placement

Consistent with the bus rule (crates talk only over the bus; I/O lives in `core`):

- **New `callbook` crate** (pure, no async/I/O): the DXCC prefix table + resolver
  (`callsign → { dxcc_entity, iso_country, ... }`) and the grid/distance/bearing
  helpers if we want them shared. Pure data + functions, independently buildable,
  reusable by `core`, `gui`, and later `net`. (A GUI-local module is the lazy
  alternative for the MVP, but a small crate keeps it reusable and testable and
  matches the workspace grain.)
- **Tier-1 panel path:** the GUI panel calls `callbook` directly for country/flag
  (pure compute, fine on the UI thread) and reads `BusView` for the local
  spot/log facts. **No new bus messages required for Tier 1.**
- **Tier-2 service:** a `core` callbook service (spawned in `core::spawn`, mocked in
  `mocks::spawn`, mirroring the rig/logbook pattern) that **serves** a
  `CallsignLookup` command and/or publishes resolved identities on a topic. New
  message types go in `types` and `docs/message-catalog.md`:
  - request: `CallsignLookup { call: Callsign }`
  - reply / state: `CallsignInfo { call, name: Option<String>, country,
    iso_country, grid: Option<GridSquare>, source: LookupSource, .. }`
  - The GUI requests via `BusView` (with a `BusView` pump + cache cell, the same
    sync↔async seam every other topic uses) and renders whatever has resolved.

## Flag rendering

The bundled tactical mono font almost certainly can't render color-emoji regional
flags, so don't rely on emoji. Recommended: a **small bundled flag atlas** (a PNG
sprite sheet keyed by ISO country code) loaded once as a `TextureHandle` — the same
mechanism as the existing `relief` texture passed through `PanelCtx` — and sampled
per entity. Decision to settle (below): show flags in **true color** (a small splash
of color in an otherwise monochrome tactical UI) vs. **theme-tinted/desaturated** to
stay within the Martian Hybrid palette. Provide a neutral placeholder tile for
unknown/maritime-mobile/unresolved prefixes.

## Panel chrome, layout & states

Standard panel construction (see `panels/contacts.rs`, `band_scan.rs`):
implement the `Panel` trait, draw a `panel_header(painter, header, pal, "Call
Sign", status, ctx.active)`, body in a `recessed_screen`, mono fonts, palette
colors. It is a **pure-display** panel with no typed input, so override
`takes_keyboard_focus() -> false` (like the map) so clicking it doesn't steal
keyboard focus from the Digital panel.

States to handle explicitly:

- **No selection** (`selected_station == None`): idle placeholder ("Select a
  station").
- **Selected, prefix-resolved**: country + flag + local facts; name area shows "—"
  until/unless Tier 2 resolves it.
- **Selected, unknown prefix** (e.g. odd compound/portable): show the raw callsign,
  a neutral flag tile, and whatever local facts exist; never error.
- **Selected, never heard with a grid**: show country/flag/name but omit the
  location block (no grid to place).

## Tile / keyboard / layout wiring (concrete touchpoints)

Adding a fifth pane touches the small set of places that enumerate panes:

1. **`main.rs build_tree()`** — `insert_pane(Box::new(CallSign::new()))` and add it
   to the right-column `insert_vertical_tile(vec![log, band, callsign, contacts])`,
   with a default share. Like Band Scan / Log, decide whether it's height-pinned or a
   normal resizable pane (recommend a modest fixed-ish height; it's a compact card).
2. **`TreeIds`** — add a `callsign: TileId` field and update construction.
3. **`TreeIds::by_number` + the `Num1..4` key array in `App::ui`** — extend to
   **five** panels. This renumbers the Cmd/Ctrl shortcuts. Proposed: `1` Digital,
   `2` Log, `3` Band, `4` Call Sign, `5` Contacts (Map moves 4→5). Update the
   `selected_station` doc comment ("4 Map") and CLAUDE.md's "Cmd/Ctrl+1..4".
4. **`settings::LayoutShares`** (`{ waterfall, right, log, band, contacts }`) — add a
   `callsign: f32` share, and have `read_layout_shares()` **default it** when loading
   older saved layouts that lack the field (serde default), so existing
   `~/.dm420` layout files still load.
5. **Layout persistence** (`docs/layout_persistence_proposal.md`) — keep the new
   pane's share in the saved/restored set.

## Work breakdown

**Phase 1 — offline panel (MVP, no network):**
1. `callbook` crate: DXCC/ITU prefix table (bundled data) + resolver with
   longest-prefix match and documented exceptions; unit tests over known calls.
2. Flag atlas asset + loader (texture), ISO-code → sprite; neutral placeholder.
3. `panels/call_sign.rs`: read `selected_station`, resolve country/flag via
   `callbook`, pull grid/SNR/band/CQ/worked from `BusView` spots + log, compute
   distance/bearing from `ctx.grid`; render card + all empty/unknown states.
4. Tile/keyboard/layout wiring (the five touchpoints above).
5. `build --workspace`, `clippy -D warnings`; hand off to Josh for visual check.

**Phase 2 — online name enrichment (optional, follow-on):**
6. `types` + `docs/message-catalog.md`: `CallsignLookup` / `CallsignInfo`.
7. `core` callbook service: provider client (callook.info default; HamQTH/QRZ
   behind config), disk cache at `~/.dm420/callbook-cache.json`, supervised/
   non-blocking; mock arm in `mocks`.
8. `BusView` pump + cell for resolved infos; panel renders name when present.
9. Config (env interim, then settings UI): provider choice + credentials.

## Open decisions for Josh

- **Online name in v1, or offline-only first?** (Recommend offline Tier 1 first.)
- **Default provider** when we do Tier 2: callook.info (US, no-auth) vs.
  HamQTH/QRZ (global, needs accounts).
- **Flag style:** true-color vs. theme-tinted/desaturated to fit Martian Hybrid.
- **Keyboard renumber:** OK to move Map from Cmd/Ctrl-4 to -5, inserting Call Sign
  at 4? (Alternative: append Call Sign as 5, keep Map at 4 — but that breaks the
  left-to-right/top-to-bottom ordering of the shortcuts.)
- **Stale card:** on deselect, clear immediately vs. hold the last card briefly.
- **`callbook` as a crate vs. a GUI module** for the Tier-1 resolver.
