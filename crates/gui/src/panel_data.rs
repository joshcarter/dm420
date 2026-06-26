//! Martian FT8 console — exact layout, palette, and fake-data tables.
//!
//! Pure data + small helpers, no dependencies. Everything here is lifted 1:1
//! from the HTML prototype (`MartianHybrid.dc.html`) so the egui port matches.
//! The geometry is in the prototype's logical pixels at a 960×600 panel; keep
//! the ratios, exact px aren't sacred.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::geo_data;

// ============================================================ LAYOUT
pub const PANEL_W: f32 = 960.0;
pub const PANEL_H: f32 = 600.0;
pub const TOPBAR_H: f32 = 46.0; // full-width metal top bar
pub const GROOVE_H: f32 = 2.0; // accent groove under the top bar
#[allow(dead_code)] // documents the 600px body-height budget; not yet read at runtime
pub const MAIN_H: f32 = 552.0; // body height (TOPBAR_H + GROOVE_H + MAIN_H = 600)

pub const LEFT_COL_W: f32 = 470.0; // waterfall column; padding 8/10/8/14 (t/r/b/l)
pub const VGROOVE_W: f32 = 2.0; // vertical groove between columns
// right column: flex (fills remainder ≈ 486 wide); padding 8/14/8/12

pub const GAP: f32 = 8.0; // vertical gap between stacked panels
pub const HEADER_ROW_H: f32 = 24.0; // each panel's title row
pub const HEADER_GAP: f32 = 6.0; // gap between title row and recessed screen

// Right-column panel heights (top→bottom). MAP is flex and fills the rest (~228).
pub const LOG_H: f32 = 142.0;
// Band Status is pinned to this exact pixel height (not resizable): tall enough for
// up to three band rows per column, each carrying an FT8 + FT4 line, plus the header.
// See `pin_band_height`.
pub const BANDSCAN_H: f32 = 176.0;
// Call Sign card: header/gap + callsign/flag + two info lines (country · grid ·
// distance, then the message exchange). Compact; a normal resizable pane.
pub const CALLSIGN_H: f32 = 128.0;
pub const FOOTER_H: f32 = 30.0;
// No panel column may be dragged narrower than this. See `enforce_min_width`.
pub const MIN_PANEL_W: f32 = 256.0;
// Left column: header(24) + screen(flex) + ticker(30, gap 8). Ticker height is
// matched to FOOTER so the waterfall + contacts recessed screens bottom-align.
pub const TICKER_H: f32 = 30.0;

// Recessed-screen corner brackets: arm 9px, stroke 1.5px, accent, flush to corner.
// Panel title "spine" bar: 3px wide × 14px tall, accent.

// ============================================================ PALETTE
// The egui port keeps all colors in `theme::Palette` (Color32 + gradient stops),
// so this file is data-only. The reference solid-color table lived here; it now
// lives in `theme.rs` (GRAPHITE / SILVER). For reference, the solid values were:
//   DARK : accent F7920F, text F4EEE6, legend F6E6CF, sub CAB496(.72),
//          dim CDAF8C(.60), screen_bg 080604, edge 100C08, lcd FFB24D, on_accent 1D1408
//   LIGHT: accent C2660F, text 241808, legend 36260F, sub 5F4420(.78),
//          dim 785028(.62), screen_bg EFE7DC, edge A39880, lcd 3A2A10, on_accent FDF6EC
//
// Map land fill / coastline stroke (RGBA), per theme:
//   DARK : fill rgba(255,238,214,0.055)  stroke rgba(247,160,60,0.40)
//   LIGHT: fill rgba(95,62,20,0.10)      stroke rgba(150,80,10,0.45)

// Fonts: Chakra Petch (headings/legends/numerals, 600–700, tracked, UPPERCASE),
//        IBM Plex Mono (all data/body, 400–600). Both OFL — vendor the TTFs.

// ============================================================ MAP PROJECTION
// Plate carrée (equirectangular, no longitude compression) over the WHOLE WORLD,
// so the map can auto-fit any cluster of contacts on Earth. `map_x`/`map_y` give
// world units; `draw_map` recomputes scale + offset each frame to fit the
// plotted points, so these full-globe constants are just the unit system.
pub const LON0: f32 = -180.0; // left edge longitude
pub const LAT_TOP: f32 = 90.0; // top edge latitude
pub const KX: f32 = 1.0; // no longitude compression (true plate carrée)
pub const S: f32 = 5.0; // units per degree
pub const MAP_W: f32 = 1800.0; // (= (180 − LON0) * KX * S)
pub const MAP_H: f32 = 900.0; // (= (LAT_TOP − (−90)) * S)

#[inline]
pub fn map_x(lon: f32) -> f32 {
    (lon - LON0) * KX * S
}
#[inline]
pub fn map_y(lat: f32) -> f32 {
    (LAT_TOP - lat) * S
}

// Fallback QTH (Lafayette, CO ≈ grid DN70KA), used by the Contacts map only when
// the operator's configured grid can't be decoded. Normally home is derived from
// that grid via `grid_to_lonlat`.
pub const HOME_LAT: f32 = 40.00;
pub const HOME_LON: f32 = -105.10;

// Graticule: world meridians/parallels every 30° (edges ±180/±90 omitted).
pub const MERIDIANS: &[f32] = &[
    -150.0, -120.0, -90.0, -60.0, -30.0, 0.0, 30.0, 60.0, 90.0, 120.0, 150.0,
];
pub const PARALLELS: &[f32] = &[-60.0, -30.0, 0.0, 30.0, 60.0];
// Home range rings (great-circle approx as ellipses); the 85 km/° is lon spacing
// at the home latitude (111·cos 40°): for km d, rx = (d / 85.0) * KX * S,
//   ry = (d / 111.0) * S.
pub const RING_KM: &[f32] = &[750.0, 1500.0];

// ============================================================ TERRAIN (shaded relief)
// The map's depth comes from a baked shaded-relief texture (assets/relief.png,
// see tools/gen_relief.py) sampled by the land mesh. These bounds must match the
// crop box in gen_relief.py so land lon/lat maps to the right texel.
pub const RELIEF_LON0: f32 = -180.0;
pub const RELIEF_LON1: f32 = 180.0;
pub const RELIEF_LAT0: f32 = -90.0;
pub const RELIEF_LAT1: f32 = 90.0;

// ============================================================ MAIDENHEAD GRID → LON/LAT
/// A decoded Maidenhead locator: cell CENTER plus the size of the smallest cell
/// that was parsed (used to spread co-grid stations without leaving the square).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridLoc {
    pub lon: f32,
    pub lat: f32,
    pub lon_size: f32,
    pub lat_size: f32,
}

/// Parse a Maidenhead grid (e.g. `FN31`, `DN70KA`, `DN70KA12`) to a `GridLoc` at
/// the cell center. Accepts the 4-, 6-, and 8-char forms FT8 carries, decoding to
/// subsquare (6-char) precision and ignoring any extended-square tail. Returns
/// `None` for malformed input so callers can skip stations they can't position.
pub fn grid_to_lonlat(grid: &str) -> Option<GridLoc> {
    let g = grid.trim().as_bytes();
    let g = match g.len() {
        4 => g,
        6 | 8 => &g[..6],
        _ => return None,
    };
    let field_lon = (g[0].to_ascii_uppercase() as i32) - b'A' as i32; // A..R
    let field_lat = (g[1].to_ascii_uppercase() as i32) - b'A' as i32;
    if !(0..18).contains(&field_lon) || !(0..18).contains(&field_lat) {
        return None;
    }
    let sq_lon = (g[2] as i32) - b'0' as i32; // 0..9
    let sq_lat = (g[3] as i32) - b'0' as i32;
    if !(0..10).contains(&sq_lon) || !(0..10).contains(&sq_lat) {
        return None;
    }

    // SW corner after field + square.
    let mut lon = -180.0 + field_lon as f32 * 20.0 + sq_lon as f32 * 2.0;
    let mut lat = -90.0 + field_lat as f32 * 10.0 + sq_lat as f32 * 1.0;
    let (mut lon_size, mut lat_size) = (2.0_f32, 1.0_f32);

    if g.len() == 6 {
        let sub_lon = (g[4].to_ascii_uppercase() as i32) - b'A' as i32; // A..X
        let sub_lat = (g[5].to_ascii_uppercase() as i32) - b'A' as i32;
        if !(0..24).contains(&sub_lon) || !(0..24).contains(&sub_lat) {
            return None;
        }
        lon_size = 2.0 / 24.0; // 5′
        lat_size = 1.0 / 24.0; // 2.5′
        lon += sub_lon as f32 * lon_size;
        lat += sub_lat as f32 * lat_size;
    }
    // Move from SW corner to cell center.
    Some(GridLoc {
        lon: lon + lon_size * 0.5,
        lat: lat + lat_size * 0.5,
        lon_size,
        lat_size,
    })
}

/// Spread a station within a located region (grid cell or section bounds): the
/// region center plus a small deterministic per-callsign offset (±0.4 of the
/// region) so co-located stations don't overlap. Stable across redraws
/// (hash-based, no randomness).
fn spread(call: &str, loc: GridLoc) -> (f32, f32) {
    let GridLoc {
        lon,
        lat,
        lon_size,
        lat_size,
    } = loc;
    // Two independent hashes (distinct seeds) so the lon/lat offsets don't share a
    // bit window — a single 32-bit FNV word concentrates entropy in its low bits.
    let frac = |h: u32| ((h & 0xffff) as f32 / 65535.0 - 0.5) * 0.8; // −0.4..0.4
    (
        lon + frac(fnv1a(call, 0x811c_9dc5)) * lon_size,
        lat + frac(fnv1a(call, 0x517c_c1b7)) * lat_size,
    )
}

/// Position a station from its callsign + grid. `None` if the grid can't be parsed.
#[allow(dead_code)] // exercised by the unit tests; live callers go through `place_station`
pub fn station_lonlat(call: &str, grid: &str) -> Option<(f32, f32)> {
    place(call, &Locator::Grid(grid.to_string()))
}

/// Where a map spot's location comes from. A grid (precise) or an ARRL/RAC section
/// (coarse — a Field Day responder sends only its section, never a grid). Strings,
/// not the `types` newtypes, to keep this module dependency-free.
#[derive(Clone, Debug, PartialEq)]
pub enum Locator {
    Grid(String),
    Section(String),
}

/// Position a station from whatever locator we have. Sections place at the
/// section's regional centroid with the spread scaled to the section's extent, so
/// co-section stations scatter across the region rather than stacking on one
/// point. `None` if the locator can't be resolved.
pub fn place_station(call: &str, loc: &Locator) -> Option<(f32, f32)> {
    place(call, loc)
}

/// Memo of resolved spots: `(call, locator-key)` → position (or `None` if unplaceable).
type PlacedCache = HashMap<(String, String), Option<(f32, f32)>>;

thread_local! {
    /// Memoized resolved positions, keyed by `(call, locator)`. Placement is a pure
    /// deterministic function, so this is just a cache: it skips the snap-to-land
    /// search on every redraw (and per `docs/map_panel.md`, a station's chosen spot
    /// must *stay put* once picked — same input, same output, guaranteed). The egui
    /// UI is single-threaded, so a `thread_local` is enough.
    static PLACED: RefCell<PlacedCache> = RefCell::new(HashMap::new());
}

/// Resolve a station's map position: locator → region, spread within it, then snap
/// off water onto land within the region (`docs/map_panel.md`). Memoized so the
/// search runs once per station and the spot is stable across frames.
fn place(call: &str, loc: &Locator) -> Option<(f32, f32)> {
    let key = (
        call.to_string(),
        match loc {
            Locator::Grid(g) => format!("g:{}", g.to_ascii_uppercase()),
            Locator::Section(s) => format!("s:{}", s.trim().to_ascii_uppercase()),
        },
    );
    if let Some(cached) = PLACED.with(|m| m.borrow().get(&key).copied()) {
        return cached;
    }
    let region = match loc {
        Locator::Grid(g) => grid_to_lonlat(g),
        Locator::Section(s) => section_to_lonlat(s),
    };
    let out = region.map(|r| snap_to_land(spread(call, r), r));
    PLACED.with(|m| m.borrow_mut().insert(key, out));
    out
}

// ============================================================ SNAP-TO-LAND
/// If `(lon, lat)` already sits on land, keep it. Otherwise relocate it to the
/// nearest land point *within the region* (`docs/map_panel.md`: an approximate
/// position over water is moved onto land inside its locator). Search is a lattice
/// over the same ±0.4-of-region envelope the spread uses, so a snapped marker never
/// leaves the cell/section. Returns the original point if the whole region is water
/// (best effort — e.g. a mid-ocean grid cell).
fn snap_to_land(point: (f32, f32), region: GridLoc) -> (f32, f32) {
    let (lon, lat) = point;
    if point_on_land(lon, lat) {
        return point;
    }
    let half_lon = 0.4 * region.lon_size;
    let half_lat = 0.4 * region.lat_size;
    const N: i32 = 8; // (2N+1)² = 289 candidate points across the region
    let mut best: Option<(f32, f32)> = None;
    let mut best_d = f32::MAX;
    for iy in -N..=N {
        for ix in -N..=N {
            let cx = region.lon + half_lon * (ix as f32 / N as f32);
            let cy = region.lat + half_lat * (iy as f32 / N as f32);
            if point_on_land(cx, cy) {
                let d = (cx - lon).powi(2) + (cy - lat).powi(2);
                if d < best_d {
                    best_d = d;
                    best = Some((cx, cy));
                }
            }
        }
    }
    best.unwrap_or(point)
}

/// Is `(lon, lat)` on land? True inside the land mesh but outside every lake — the
/// same triangulated geometry the Contacts map fills (land) and overlays as water
/// (lakes), so snapping agrees with what's drawn.
fn point_on_land(lon: f32, lat: f32) -> bool {
    in_mesh(lon, lat, geo_data::LAND_VERTS, geo_data::LAND_IDX)
        && !in_mesh(lon, lat, geo_data::LAKES_VERTS, geo_data::LAKES_IDX)
}

/// Point-in-mesh test: true if `(lon, lat)` falls inside any triangle. `verts` are
/// `(lat, lon)`; `idx` lists triangles as index triples. A bounding-box reject per
/// triangle keeps this cheap despite the world-scale mesh.
fn in_mesh(lon: f32, lat: f32, verts: &[(f32, f32)], idx: &[u32]) -> bool {
    for t in idx.chunks_exact(3) {
        let a = verts[t[0] as usize];
        let b = verts[t[1] as usize];
        let c = verts[t[2] as usize];
        // verts are (lat, lon): x = lon = .1, y = lat = .0.
        let (ax, ay) = (a.1, a.0);
        let (bx, by) = (b.1, b.0);
        let (cx, cy) = (c.1, c.0);
        // Cheap AABB reject before the full edge test.
        if lon < ax.min(bx).min(cx)
            || lon > ax.max(bx).max(cx)
            || lat < ay.min(by).min(cy)
            || lat > ay.max(by).max(cy)
        {
            continue;
        }
        if point_in_tri(lon, lat, ax, ay, bx, by, cx, cy) {
            return true;
        }
    }
    false
}

/// Standard half-plane sign test for point `(px, py)` against triangle `a, b, c`.
/// Boundary points count as inside.
#[allow(clippy::too_many_arguments)]
fn point_in_tri(
    px: f32,
    py: f32,
    ax: f32,
    ay: f32,
    bx: f32,
    by: f32,
    cx: f32,
    cy: f32,
) -> bool {
    let sign = |px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32| {
        (px - bx) * (ay - by) - (ax - bx) * (py - by)
    };
    let d1 = sign(px, py, ax, ay, bx, by);
    let d2 = sign(px, py, bx, by, cx, cy);
    let d3 = sign(px, py, cx, cy, ax, ay);
    let neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(neg && pos)
}

// ====================================================== ARRL/RAC SECTION → LON/LAT
/// ARRL/RAC Field Day sections → an approximate region: `(abbr, center_lon,
/// center_lat, lon_extent, lat_extent)` in degrees. Coarse on purpose — a section
/// is a state-or-larger region, so the extent feeds the same per-callsign spread
/// as a grid cell, scattering co-section stations across the region instead of
/// stacking them. Centroids are eyeballed, good enough to land a marker in the
/// right state/province (all a section warrants). Covers the 70 ARRL + 13 RAC
/// sections; `DX` and bare-class traffic carry no location and resolve to `None`.
const SECTIONS: &[(&str, f32, f32, f32, f32)] = &[
    // --- ARRL: whole-state sections ---
    ("AL", -86.8, 32.8, 3.0, 3.0),
    ("AK", -150.0, 63.0, 16.0, 6.0),
    ("AZ", -111.7, 34.3, 4.0, 4.0),
    ("AR", -92.4, 34.8, 3.0, 2.5),
    ("CO", -105.5, 39.0, 5.0, 3.0),
    ("CT", -72.7, 41.6, 1.5, 1.0),
    ("DE", -75.5, 39.0, 0.6, 1.2),
    ("GA", -83.5, 32.7, 3.0, 3.0),
    ("ID", -114.5, 44.4, 5.0, 6.0),
    ("IL", -89.2, 40.0, 3.0, 5.0),
    ("IN", -86.3, 39.9, 2.5, 4.0),
    ("IA", -93.5, 42.0, 5.0, 2.5),
    ("KS", -98.3, 38.5, 6.0, 2.5),
    ("KY", -85.3, 37.5, 5.0, 2.0),
    ("LA", -92.0, 31.0, 3.5, 3.0),
    ("ME", -69.2, 45.4, 3.0, 3.5),
    ("MI", -85.0, 44.3, 5.0, 5.0),
    ("MN", -94.3, 46.3, 6.0, 5.0),
    ("MS", -89.7, 32.7, 2.5, 4.0),
    ("MO", -92.5, 38.4, 5.0, 3.5),
    ("MT", -109.5, 47.0, 11.0, 4.0),
    ("NE", -99.8, 41.5, 8.0, 2.5),
    ("NV", -116.9, 39.5, 4.0, 7.0),
    ("NH", -71.6, 43.7, 1.5, 3.0),
    ("NM", -106.1, 34.5, 6.0, 5.0),
    ("NC", -79.4, 35.5, 8.0, 2.5),
    ("ND", -100.5, 47.5, 7.0, 2.5),
    ("OH", -82.8, 40.3, 4.0, 3.5),
    ("OK", -97.5, 35.5, 7.0, 2.5),
    ("OR", -120.6, 44.0, 7.0, 4.0),
    ("RI", -71.5, 41.7, 0.7, 0.8),
    ("SC", -80.9, 33.9, 4.0, 2.5),
    ("SD", -100.3, 44.4, 7.0, 2.5),
    ("TN", -86.3, 35.8, 9.0, 1.8),
    ("UT", -111.7, 39.3, 3.5, 5.0),
    ("VT", -72.7, 44.1, 1.0, 3.0),
    ("VA", -78.7, 37.5, 7.0, 2.5),
    ("WV", -80.6, 38.6, 4.0, 3.0),
    ("WI", -89.8, 44.6, 4.0, 4.5),
    ("WY", -107.5, 43.0, 7.0, 3.5),
    // --- ARRL: multi-section states ---
    ("EPA", -76.0, 40.9, 3.0, 1.5), // Eastern Pennsylvania
    ("WPA", -79.7, 41.0, 2.5, 1.8), // Western Pennsylvania
    ("MDC", -76.8, 39.0, 3.0, 1.0), // Maryland-DC
    ("NNJ", -74.4, 40.8, 0.8, 1.0), // Northern New Jersey
    ("SNJ", -74.7, 39.5, 0.9, 1.0), // Southern New Jersey
    ("ENY", -73.9, 42.6, 1.2, 2.0), // Eastern New York
    ("NNY", -75.0, 44.2, 2.0, 1.2), // Northern New York
    ("NLI", -73.3, 40.8, 1.2, 0.5), // NYC / Long Island
    ("EMA", -71.0, 42.4, 1.2, 0.8), // Eastern Massachusetts
    ("WMA", -72.6, 42.4, 1.2, 0.8), // Western Massachusetts
    ("EWA", -118.5, 47.3, 3.0, 2.0), // Eastern Washington
    ("WWA", -122.3, 47.5, 1.5, 2.5), // Western Washington
    ("SF", -122.6, 38.3, 0.8, 1.0), // San Francisco
    ("EB", -122.0, 37.8, 0.8, 0.8), // East Bay
    ("SCV", -121.8, 37.2, 0.8, 0.8), // Santa Clara Valley
    ("SJV", -119.8, 36.5, 2.0, 3.0), // San Joaquin Valley
    ("SV", -121.5, 39.3, 1.5, 2.5), // Sacramento Valley
    ("LAX", -118.3, 34.1, 1.0, 0.8), // Los Angeles
    ("ORG", -117.8, 33.7, 0.6, 0.6), // Orange
    ("SB", -119.8, 34.7, 2.0, 1.2), // Santa Barbara
    ("SDG", -116.9, 33.0, 1.5, 1.2), // San Diego
    ("NTX", -97.0, 33.0, 4.0, 2.0), // North Texas
    ("STX", -98.5, 29.0, 4.0, 3.0), // South Texas
    ("WTX", -102.0, 31.5, 4.0, 3.0), // West Texas
    ("NFL", -82.5, 30.0, 4.0, 1.5), // Northern Florida
    ("SFL", -80.5, 26.3, 1.5, 1.5), // Southern Florida
    ("WCF", -82.2, 28.0, 1.0, 1.5), // West Central Florida
    ("PR", -66.5, 18.2, 1.2, 0.4),  // Puerto Rico
    ("VI", -64.8, 18.0, 0.4, 0.3),  // Virgin Islands
    ("PAC", -157.9, 21.3, 3.0, 2.0), // Pacific (Hawaii)
    // --- RAC: Canadian sections ---
    ("MAR", -64.0, 45.5, 4.0, 2.0), // Maritime (NS/NB/PEI)
    ("NL", -57.0, 49.0, 6.0, 4.0),  // Newfoundland/Labrador
    ("QC", -72.0, 47.0, 10.0, 5.0), // Quebec
    ("ONE", -76.5, 45.2, 2.0, 1.5), // Ontario East
    ("ONN", -85.0, 49.0, 10.0, 4.0), // Ontario North
    ("ONS", -81.0, 43.2, 2.5, 1.5), // Ontario South
    ("GTA", -79.4, 43.7, 1.0, 0.6), // Greater Toronto Area
    ("MB", -98.0, 53.0, 6.0, 6.0),  // Manitoba
    ("SK", -106.0, 53.0, 7.0, 6.0), // Saskatchewan
    ("AB", -114.0, 53.5, 6.0, 6.0), // Alberta
    ("BC", -123.0, 52.0, 8.0, 7.0), // British Columbia
    ("NT", -120.0, 64.0, 30.0, 8.0), // Northern Territories (YT/NWT/NU)
];

/// Resolve an ARRL/RAC section abbreviation to its region centroid + extent.
/// Case-insensitive. `None` for unknown sections (e.g. `DX`) so callers skip them.
pub fn section_to_lonlat(section: &str) -> Option<GridLoc> {
    let s = section.trim().to_ascii_uppercase();
    SECTIONS
        .iter()
        .find(|(abbr, ..)| *abbr == s)
        .map(|&(_, lon, lat, lon_size, lat_size)| GridLoc {
            lon,
            lat,
            lon_size,
            lat_size,
        })
}

/// FNV-1a 32-bit hash from an explicit offset basis (`seed`) — fast and stable,
/// used only to derive deterministic positional jitter for co-grid callsigns.
#[inline]
fn fnv1a(s: &str, seed: u32) -> u32 {
    let mut h = seed;
    for b in s.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

// Coastline/land/lakes geometry now lives in `geo_data.rs` (Natural Earth 10m,
// pre-triangulated). See `tools/gen_geo.py` to regenerate.

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn grid_centers() {
        // DN70 (home field/square) → center ≈ −105.0 / 40.5
        let g = grid_to_lonlat("DN70").unwrap();
        assert!(near(g.lon, -105.0, 0.01), "lon {}", g.lon);
        assert!(near(g.lat, 40.5, 0.01), "lat {}", g.lat);
        // FN31 (Connecticut) → center ≈ −73.0 / 41.5
        let g = grid_to_lonlat("FN31").unwrap();
        assert!(near(g.lon, -73.0, 0.01), "lon {}", g.lon);
        assert!(near(g.lat, 41.5, 0.01), "lat {}", g.lat);
        // 6-char subsquare narrows the cell to an exact center inside the square.
        let s = grid_to_lonlat("DN70KA").unwrap();
        assert!(s.lon_size < 0.1 && s.lat_size < 0.05);
        assert!(near(s.lon, -105.125, 0.001), "lon {}", s.lon);
        assert!(near(s.lat, 40.0208, 0.001), "lat {}", s.lat);
        // 8-char extended locators decode to 6-char precision (tail ignored).
        let e = grid_to_lonlat("DN70KA12").unwrap();
        assert!(near(e.lon, s.lon, 1e-6) && near(e.lat, s.lat, 1e-6));
        // Case-insensitive: lowercase field/subsquare letters decode identically.
        assert_eq!(
            grid_to_lonlat("fn31").unwrap().lon,
            grid_to_lonlat("FN31").unwrap().lon
        );
    }

    #[test]
    fn grid_rejects_malformed() {
        // Includes a 6-char with an out-of-range subsquare letter (Z = 25, valid is A..X).
        for bad in ["", "F", "FN3", "FN3X", "FN311", "ZZ99", "F931", "FN31ZZ"] {
            assert!(grid_to_lonlat(bad).is_none(), "expected None for {bad:?}");
        }
    }

    #[test]
    fn station_offset_stable_and_in_cell() {
        let g = grid_to_lonlat("FN31").unwrap();
        let a = station_lonlat("K1ABC", "FN31").unwrap();
        let b = station_lonlat("K1ABC", "FN31").unwrap();
        assert_eq!(a, b, "must be deterministic across calls");
        // Offset stays within ±0.4 of the cell, so the point never leaves the square.
        assert!((a.0 - g.lon).abs() <= 0.4 * g.lon_size + 1e-4);
        assert!((a.1 - g.lat).abs() <= 0.4 * g.lat_size + 1e-4);
        // Different callsigns in the same grid get different spots.
        assert_ne!(a, station_lonlat("W2NYC", "FN31").unwrap());
        assert!(station_lonlat("NOGRID", "ZZ99").is_none());
    }

    #[test]
    fn section_known_and_case_insensitive() {
        let wi = section_to_lonlat("WI").unwrap();
        // Wisconsin centroid lands in the upper Midwest, north of the equator.
        assert!(wi.lon < -80.0 && wi.lon > -100.0);
        assert!(wi.lat > 40.0 && wi.lat < 50.0);
        // Section lookup folds case and trims.
        assert_eq!(section_to_lonlat(" wi "), Some(wi));
        // Unknown sections (e.g. DX) and grids resolve to None.
        assert!(section_to_lonlat("DX").is_none());
        assert!(section_to_lonlat("ZZZ").is_none());
    }

    #[test]
    fn place_station_spreads_within_section() {
        let region = section_to_lonlat("CO").unwrap();
        let a = place_station("N0JDC", &Locator::Section("CO".into())).unwrap();
        let b = place_station("N0JDC", &Locator::Section("CO".into())).unwrap();
        assert_eq!(a, b, "section placement is deterministic");
        // Stays within ±0.4 of the section extent — the marker never leaves the region.
        assert!((a.0 - region.lon).abs() <= 0.4 * region.lon_size + 1e-4);
        assert!((a.1 - region.lat).abs() <= 0.4 * region.lat_size + 1e-4);
        // Distinct calls in one section scatter; a grid locator still routes correctly.
        assert_ne!(a, place_station("W4LL", &Locator::Section("CO".into())).unwrap());
        assert_eq!(
            place_station("K1ABC", &Locator::Grid("FN31".into())),
            station_lonlat("K1ABC", "FN31"),
        );
        assert!(place_station("X", &Locator::Section("DX".into())).is_none());
    }

    #[test]
    fn point_on_land_classifies_known_points() {
        // Denver, CO — solidly inland.
        assert!(point_on_land(-104.99, 39.74));
        // Mid–Pacific and mid–Atlantic — open ocean.
        assert!(!point_on_land(-140.0, 30.0));
        assert!(!point_on_land(-40.0, 35.0));
    }

    #[test]
    fn snap_relocates_water_point_onto_land() {
        // A region centred off the California coast (open water) but wide enough that
        // its eastern edge reaches the mainland.
        let region = GridLoc {
            lon: -125.0,
            lat: 37.0,
            lon_size: 8.0,
            lat_size: 2.0,
        };
        assert!(!point_on_land(region.lon, region.lat), "centre must be water");
        let snapped = snap_to_land((region.lon, region.lat), region);
        assert!(point_on_land(snapped.0, snapped.1), "snapped point is on land");
        // Stays inside the ±0.4 region envelope.
        assert!((snapped.0 - region.lon).abs() <= 0.4 * region.lon_size + 1e-4);
        assert!((snapped.1 - region.lat).abs() <= 0.4 * region.lat_size + 1e-4);
    }

    #[test]
    fn snap_keeps_point_when_region_is_all_water() {
        // A small mid-ocean region: nothing to snap to, so the point is unchanged.
        let region = GridLoc {
            lon: -150.0,
            lat: 30.0,
            lon_size: 2.0,
            lat_size: 2.0,
        };
        let p = (region.lon, region.lat);
        assert_eq!(snap_to_land(p, region), p);
    }
}
