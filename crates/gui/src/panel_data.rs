//! Martian FT8 console — exact layout, palette, and fake-data tables.
//!
#![allow(dead_code)] // a few constants/helpers here are for future live-data wiring
//!
//! Pure data + small helpers, no dependencies. Everything here is lifted 1:1
//! from the HTML prototype (`MartianHybrid.dc.html`) so the egui port matches.
//! The geometry is in the prototype's logical pixels at a 960×600 panel; keep
//! the ratios, exact px aren't sacred.

// ============================================================ LAYOUT
pub const PANEL_W: f32 = 960.0;
pub const PANEL_H: f32 = 600.0;
pub const TOPBAR_H: f32 = 46.0;     // full-width metal top bar
pub const GROOVE_H: f32 = 2.0;      // accent groove under the top bar
pub const MAIN_H: f32 = 552.0;      // body height (TOPBAR_H + GROOVE_H + MAIN_H = 600)

pub const LEFT_COL_W: f32 = 470.0;  // waterfall column; padding 8/10/8/14 (t/r/b/l)
pub const VGROOVE_W: f32 = 2.0;     // vertical groove between columns
// right column: flex (fills remainder ≈ 486 wide); padding 8/14/8/12

pub const GAP: f32 = 8.0;           // vertical gap between stacked panels
pub const HEADER_ROW_H: f32 = 24.0; // each panel's title row
pub const HEADER_GAP: f32 = 6.0;    // gap between title row and recessed screen

// Right-column panel heights (top→bottom). MAP is flex and fills the rest (~228).
pub const LOG_H: f32 = 142.0;
// Band Scan is pinned to this exact pixel height (not resizable): tall enough
// for the two band rows (2×30 + 7 gap = 67) plus header/gap and ~9px of screen
// padding top & bottom. See `pin_band_height`.
pub const BANDSCAN_H: f32 = 128.0;
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
// Equirectangular. SVG viewBox is 393×190; latitude window 16°..54°N.
pub const LON0: f32 = -150.0;   // left edge longitude
pub const LAT_TOP: f32 = 54.0;  // top edge latitude
pub const KX: f32 = 0.819;      // cos(midlat) longitude compression
pub const S: f32 = 5.0;         // units per degree of latitude
pub const MAP_W: f32 = 393.0;   // (= (−54 − LON0) * KX * S)
pub const MAP_H: f32 = 190.0;   // (= (LAT_TOP − 16) * S)

#[inline] pub fn map_x(lon: f32) -> f32 { (lon - LON0) * KX * S }
#[inline] pub fn map_y(lat: f32) -> f32 { (LAT_TOP - lat) * S }

pub const HOME_LAT: f32 = 40.00; // Lafayette, CO (QTH) — N0JDC, grid DN70KA
pub const HOME_LON: f32 = -105.10;

// Graticule
pub const MERIDIANS: &[f32] = &[-140.0,-130.0,-120.0,-110.0,-100.0,-90.0,-80.0,-70.0,-60.0];
pub const PARALLELS: &[f32] = &[20.0, 30.0, 40.0, 50.0];
// US/Canada border: dashed line at 49°N from left edge to lon −95 (lakes start).
pub const BORDER_LAT: f32 = 49.0;
pub const BORDER_LON_END: f32 = -95.0;
// Home range rings (great-circle approx as ellipses): for km d,
//   rx = (d / 85.0) * KX * S ,  ry = (d / 111.0) * S
pub const RING_KM: &[f32] = &[750.0, 1500.0];

// ============================================================ TERRAIN (shaded relief)
// The map's depth comes from a baked shaded-relief texture (assets/relief.png,
// see tools/gen_relief.py) sampled by the land mesh. These bounds must match the
// crop box in gen_relief.py so land lon/lat maps to the right texel.
pub const RELIEF_LON0: f32 = -130.0;
pub const RELIEF_LON1: f32 = -58.0;
pub const RELIEF_LAT0: f32 = 8.0;
pub const RELIEF_LAT1: f32 = 62.0;

// ============================================================ MAIDENHEAD GRID → LON/LAT
/// A decoded Maidenhead locator: cell CENTER plus the size of the smallest cell
/// that was parsed (used to spread co-grid stations without leaving the square).
pub struct GridLoc { pub lon: f32, pub lat: f32, pub lon_size: f32, pub lat_size: f32 }

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
    if !(0..18).contains(&field_lon) || !(0..18).contains(&field_lat) { return None; }
    let sq_lon = (g[2] as i32) - b'0' as i32; // 0..9
    let sq_lat = (g[3] as i32) - b'0' as i32;
    if !(0..10).contains(&sq_lon) || !(0..10).contains(&sq_lat) { return None; }

    // SW corner after field + square.
    let mut lon = -180.0 + field_lon as f32 * 20.0 + sq_lon as f32 * 2.0;
    let mut lat = -90.0 + field_lat as f32 * 10.0 + sq_lat as f32 * 1.0;
    let (mut lon_size, mut lat_size) = (2.0_f32, 1.0_f32);

    if g.len() == 6 {
        let sub_lon = (g[4].to_ascii_uppercase() as i32) - b'A' as i32; // A..X
        let sub_lat = (g[5].to_ascii_uppercase() as i32) - b'A' as i32;
        if !(0..24).contains(&sub_lon) || !(0..24).contains(&sub_lat) { return None; }
        lon_size = 2.0 / 24.0; // 5′
        lat_size = 1.0 / 24.0; // 2.5′
        lon += sub_lon as f32 * lon_size;
        lat += sub_lat as f32 * lat_size;
    }
    // Move from SW corner to cell center.
    Some(GridLoc { lon: lon + lon_size * 0.5, lat: lat + lat_size * 0.5, lon_size, lat_size })
}

/// Position a station from its callsign + grid: the grid-cell center plus a small
/// deterministic per-callsign offset (±0.4 of the cell) so co-grid stations don't
/// overlap. Stable across redraws (hash-based, no randomness). `None` if the grid
/// can't be parsed.
pub fn station_lonlat(call: &str, grid: &str) -> Option<(f32, f32)> {
    let GridLoc { lon, lat, lon_size, lat_size } = grid_to_lonlat(grid)?;
    // Two independent hashes (distinct seeds) so the lon/lat offsets don't share a
    // bit window — a single 32-bit FNV word concentrates entropy in its low bits.
    let frac = |h: u32| ((h & 0xffff) as f32 / 65535.0 - 0.5) * 0.8; // −0.4..0.4
    Some((
        lon + frac(fnv1a(call, 0x811c_9dc5)) * lon_size,
        lat + frac(fnv1a(call, 0x517c_c1b7)) * lat_size,
    ))
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

// ============================================================ LOG BOOK SPOTS (fake)
/// A worked station as it appears on the map; position is inferred from `grid`.
pub struct LogSpot { pub call: &'static str, pub grid: &'static str }
// Phase 1: all entries are worked (filled marker). ~13 stations spread across
// North America. Phase 2 adds a separate unworked/heard list with last-heard times.
pub const WORKED: &[LogSpot] = &[
    LogSpot{call:"K7RA", grid:"CN87"}, // Seattle, WA
    LogSpot{call:"K6XX", grid:"CM97"}, // Bay Area, CA
    LogSpot{call:"W7PH", grid:"DM33"}, // Phoenix, AZ
    LogSpot{call:"K0DEN",grid:"DM79"}, // Denver, CO
    LogSpot{call:"K5ED", grid:"EM12"}, // Dallas, TX
    LogSpot{call:"N5JR", grid:"EL29"}, // Houston, TX
    LogSpot{call:"W9XYZ",grid:"EN61"}, // Detroit, MI
    LogSpot{call:"K1ABC",grid:"FN31"}, // Connecticut
    LogSpot{call:"W2NYC",grid:"FN20"}, // New York, NY
    LogSpot{call:"N4FL", grid:"EL96"}, // Miami, FL
    LogSpot{call:"VE3EN",grid:"FN25"}, // Toronto, ON
    LogSpot{call:"VE6AO",grid:"DO21"}, // Calgary, AB
    LogSpot{call:"XE2OK",grid:"DL95"}, // Monterrey, MX
    LogSpot{call:"XE1RC",grid:"EK09"}, // Mexico City, MX
];

/// Great-circle distance (km) — used to label "Best DX".
pub fn haversine_km(la1: f32, lo1: f32, la2: f32, lo2: f32) -> f32 {
    let re = 6371.0_f32;
    let dl = (la2 - la1).to_radians();
    let dn = (lo2 - lo1).to_radians();
    let a = (dl * 0.5).sin().powi(2)
        + la1.to_radians().cos() * la2.to_radians().cos() * (dn * 0.5).sin().powi(2);
    2.0 * re * a.sqrt().min(1.0).asin()
}

// Coastline/land/lakes geometry now lives in `geo_data.rs` (Natural Earth 50m,
// pre-triangulated). See `tools/gen_geo.py` to regenerate.

// ============================================================ WATERFALL DECODE RAIL (fake)
// Left panel: a sideways waterfall image with a decode rail down its right edge.
// Each decode sits at a vertical position derived from its audio frequency (Hz):
//   y_px = ((1 − f / 3000) * RAILH − 7).clamp(2, RAILH − 16)   with RAILH = 438
pub const FMAX_HZ: f32 = 3000.0;
pub const RAILH: f32 = 438.0;
pub const DECODES: &[(f32, &str, &str)] = &[ // (audio_hz, callsign, snr)
    (2680.0,"OH8X","−08"),(2510.0,"JA1NUT","−15"),(2360.0,"K1ABC","−02"),(2200.0,"DL3XYZ","−19"),
    (2050.0,"VK3WE","−21"),(1880.0,"W7GH","−11"),(1720.0,"EA7KW","−17"),(1560.0,"N5JR","−05"),
    (1400.0,"PY2OG","−23"),(1240.0,"G4ABC","−13"),(1080.0,"VE3EN","−09"),(920.0,"ZL2AB","−24"),
    (600.0,"UA9XYZ","−18"),
];

// ============================================================ LOG BOOK (fake, last 4)
pub const LOGS: &[(&str,&str,&str,&str,&str)] = &[ // (utc, call, grid, sent, rcvd)
    ("2358","W7GH","CN94","−11","−09"),("2355","JA1NUT","PM95","−15","−13"),
    ("2351","G4ABC","IO91","−13","−07"),("2347","VE3EN","FN25","−09","−02"),
];

// ============================================================ BAND SCAN (fake)
pub const BANDS: &[(&str,u32,u32)] = &[ // (band, heard, unworked)
    ("40m",23,7),("20m",41,12),("15m",18,9),("10m",6,4),
];
// Columns: left = [40m, 20m], right = [15m, 10m].
// Scan cycles activeBand 0→3 every 2.5 s, then returns to idle ("Last scan: just now").

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: f32, b: f32, tol: f32) -> bool { (a - b).abs() <= tol }

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
        assert_eq!(grid_to_lonlat("fn31").unwrap().lon, grid_to_lonlat("FN31").unwrap().lon);
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
}
