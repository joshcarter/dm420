//! Contacts panel: a world map (relief-shaded land mesh + graticule +
//! range rings + station spots) over a flat tactical footer (toggles + SNR bars).
//! Plots worked stations (accent plus, from the log) and heard-but-unworked
//! stations (accent circle, or triangle while calling CQ — dimming with last-heard
//! age, `docs/map_panel.md`). Spots are filtered to the header-selected band so the
//! per-band "worked" rule holds. Bounds auto-fit the plotted spots, so it reads as a
//! regional map when contacts cluster and zooms out to the globe when DX comes in.
//! Owns the footer toggle states and the selected band.
//! The map/footer drawing helpers (`over`, `dashed_polyline`, `ellipse_pts`) are
//! single-consumer and live here.

use std::collections::HashSet;

use eframe::egui;
use egui::{
    Align2, Color32, CornerRadius, Mesh, Pos2, Rect, Shape, Stroke, StrokeKind, TextureHandle, Vec2,
};

use types::Band;

use super::{Panel, PanelCtx};
use crate::bus_view::MapSpot;
use crate::chrome::{measure, panel_header, segmented_select};
use crate::geo_data;
use crate::panel_data as pd;
use crate::theme::*;

/// The bands the map plots, in switcher order. "Worked" is per band (the Field Day
/// rule), so the map shows one band at a time — mirroring the waterslide.
const MAP_BANDS: [Band; 4] = [Band::B40m, Band::B20m, Band::B15m, Band::B10m];
const MAP_BAND_LABELS: [&str; 4] = ["40", "20", "15", "10"];

pub struct Contacts {
    /// Footer toggles: `[0]` recent-only (last 24 h) vs. all logged entries;
    /// `[1]` include heard-but-unworked stations. Per `docs/map_panel.md`.
    toggles: [bool; 2],
    /// The band the map is showing — its spots are filtered to this band, so the
    /// per-band "worked" rule holds (a call worked on another band still reads as
    /// unworked here). Chosen via the header band switcher.
    band: Band,
}

impl Contacts {
    pub fn new() -> Self {
        Self {
            toggles: [true, true], // recent-only + show unworked
            band: Band::B20m,
        }
    }
}

impl Panel for Contacts {
    fn title(&self) -> &str {
        "Contacts"
    }

    fn ui(&mut self, ctx: &mut PanelCtx, block: Rect) {
        let painter = ctx.painter;
        let pal = ctx.pal;

        // Worked stations from the log on the selected band; optionally trimmed to
        // the last 24 h. Band-filtering first keeps "worked" per band — a call
        // logged on another band doesn't count here.
        let band = self.band;
        let now = ctx.bus.now_ms();
        let mut worked = ctx.bus.worked_spots();
        worked.retain(|s| s.band == Some(band));
        if self.toggles[0] {
            let cutoff = now - 24 * 3_600_000;
            worked.retain(|s| s.last_ms >= cutoff);
        }
        // Heard-but-unworked stations on this band, excluding any already worked
        // here (a worked station is shown as a plus, not as a transient). Empty
        // unless the "unworked" toggle is on. Order in the combined list doesn't
        // matter — `draw_map` paints unworked then worked so worked markers sit on
        // top.
        let mut spots = worked;
        if self.toggles[1] {
            let worked_calls: HashSet<String> = spots.iter().map(|s| s.call.clone()).collect();
            spots.extend(
                ctx.bus
                    .heard_spots()
                    .into_iter()
                    .filter(|s| s.band == Some(band))
                    .filter(|s| !worked_calls.contains(&s.call)),
            );
        }
        let spot_count = spots.len();

        // Home is the operator's configured grid, decoded to lon/lat; fall back to
        // the default QTH if the grid can't be parsed.
        let home = pd::grid_to_lonlat(ctx.grid)
            .map(|g| (g.lon, g.lat))
            .unwrap_or((pd::HOME_LON, pd::HOME_LAT));

        let header = Rect::from_min_max(
            block.min,
            Pos2::new(block.right(), block.top() + pd::HEADER_ROW_H),
        );
        panel_header(
            painter,
            header,
            pal,
            "Contacts",
            &format!("World · {}", ctx.grid),
            ctx.active,
        );
        // Band switcher (right cluster): pick which band's spots the map shows, the
        // same per-band partition the waterslide uses. The spot count tucks in just
        // left of it.
        let cy = header.center().y;
        let sel = MAP_BANDS.iter().position(|b| *b == self.band).unwrap_or(0);
        let (sw_left, clicked) = segmented_select(
            ctx.ui,
            painter,
            pal,
            header.right() - 2.0,
            cy,
            18.0,
            "",
            &MAP_BAND_LABELS,
            sel,
            "map_band",
        );
        if let Some(i) = clicked {
            self.band = MAP_BANDS[i];
        }
        painter.text(
            Pos2::new(sw_left - 8.0, cy),
            Align2::RIGHT_CENTER,
            format!("{spot_count} spots"),
            mono(8.5),
            pal.sub,
        );

        let footer = Rect::from_min_max(
            Pos2::new(block.left(), block.bottom() - pd::FOOTER_H),
            block.max,
        );
        let screen = Rect::from_min_max(
            Pos2::new(block.left(), header.bottom() + pd::HEADER_GAP),
            Pos2::new(block.right(), footer.top() - pd::GAP),
        );
        recessed_screen(painter, screen, pal);
        draw_map(
            painter,
            screen,
            pal,
            ctx.relief,
            &spots,
            now,
            home,
            ctx.selected_station.as_deref(),
        );
        self.draw_footer(ctx.ui, painter, footer, pal);
    }
}

impl Contacts {
    /// Flat tactical footer: square toggles (solid = on, hollow = off) + SNR bars.
    fn draw_footer(
        &mut self,
        ui: &mut egui::Ui,
        painter: &egui::Painter,
        rect: Rect,
        pal: &Palette,
    ) {
        let cy = rect.center().y;
        let labels = ["RECENT", "UNWORKED"];
        let mut x = rect.left();
        for (i, label_text) in labels.iter().enumerate() {
            let sq =
                Rect::from_center_size(Pos2::new(x + TOGGLE_SQ * 0.5, cy), Vec2::splat(TOGGLE_SQ));
            let resp = ui.interact(
                sq.expand(2.0),
                ui.id().with(("footer_toggle", i)),
                egui::Sense::click(),
            );
            if resp.clicked() {
                self.toggles[i] = !self.toggles[i];
            }
            if self.toggles[i] {
                painter.rect_filled(sq, CornerRadius::ZERO, pal.accent);
            } else {
                painter.rect_stroke(
                    sq,
                    CornerRadius::ZERO,
                    Stroke::new(TOGGLE_STROKE, pal.sub),
                    StrokeKind::Inside,
                );
            }
            let label_color = if self.toggles[i] { pal.legend } else { pal.sub };
            let tx = sq.right() + 6.0;
            let label = tracked(label_text);
            painter.text(
                Pos2::new(tx, cy),
                Align2::LEFT_CENTER,
                &label,
                heading(8.5),
                label_color,
            );
            x = tx + measure(painter, &label, heading(8.5)) + 18.0;
        }

        // SNR bar-graph (right).
        let heights = [5.0, 8.0, 11.0, 14.0, 9.0, 5.0];
        let mut bx = rect.right() - 70.0;
        let base = cy + 7.0;
        for (j, h) in heights.iter().enumerate() {
            let on = j < 4;
            let bar = Rect::from_min_max(Pos2::new(bx, base - h), Pos2::new(bx + 3.0, base));
            let col = if on {
                pal.accent
            } else {
                pal.sub.gamma_multiply(0.45)
            };
            painter.rect_filled(bar, CornerRadius::ZERO, col);
            bx += 5.0;
        }
        painter.text(
            Pos2::new(bx + 4.0, cy),
            Align2::LEFT_CENTER,
            "SNR",
            mono(7.5),
            pal.sub,
        );
    }
}

/// Composite a translucent foreground over an opaque background → opaque color.
/// `fg`'s channels are already alpha-weighted (egui `Color32` is premultiplied),
/// so only the background is scaled by `(1 − a)`. Requires `bg` fully opaque
/// (`bg.a() == 255`); a translucent `bg` would drop its alpha and mis-tint.
fn over(fg: Color32, bg: Color32) -> Color32 {
    debug_assert_eq!(bg.a(), 255, "over() requires an opaque background");
    let a = fg.a() as f32 / 255.0;
    let m = |f: u8, b: u8| (f as f32 + b as f32 * (1.0 - a)).round().min(255.0) as u8;
    Color32::from_rgb(m(fg.r(), bg.r()), m(fg.g(), bg.g()), m(fg.b(), bg.b()))
}

/// Draw a dashed polyline, keeping dash phase across segment joints.
fn dashed_polyline(painter: &egui::Painter, pts: &[Pos2], stroke: Stroke, dash: f32, gap: f32) {
    let mut drawing = true;
    let mut remaining = dash;
    for w in pts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let seg = b - a;
        let len = seg.length();
        if len < 1e-4 {
            continue;
        }
        let dir = seg / len;
        let mut pos = 0.0;
        let mut start = a;
        while pos < len {
            let step = remaining.min(len - pos);
            let end = a + dir * (pos + step);
            if drawing {
                painter.line_segment([start, end], stroke);
            }
            pos += step;
            remaining -= step;
            start = end;
            if remaining <= 1e-4 {
                drawing = !drawing;
                remaining = if drawing { dash } else { gap };
            }
        }
    }
}

fn ellipse_pts(center: Pos2, rx: f32, ry: f32, n: usize) -> Vec<Pos2> {
    (0..=n)
        .map(|i| {
            let a = i as f32 / n as f32 * std::f32::consts::TAU;
            Pos2::new(center.x + rx * a.cos(), center.y + ry * a.sin())
        })
        .collect()
}

/// The shape of a plotted station marker — the category cue on the map. All are
/// drawn in the accent color; the shape (not the color) distinguishes them.
#[derive(Clone, Copy)]
enum Marker {
    /// Heard but unworked.
    Circle,
    /// Worked (in the log).
    Plus,
    /// Unworked and calling CQ — an answerable caller.
    Triangle,
}

#[allow(clippy::too_many_arguments)]
fn draw_map(
    painter: &egui::Painter,
    screen: Rect,
    pal: &Palette,
    relief: &TextureHandle,
    // Worked (plus) and heard-but-unworked (circle / CQ triangle) stations in one
    // list; the `worked`/`cq` flags pick the marker shape. Worked markers are drawn
    // last so they paint over the heard ones.
    spots: &[MapSpot],
    // Wall-clock now (ms since epoch) — the reference for dimming heard markers.
    now_ms: i64,
    // The operator's home location as `(lon, lat)` — a plotted bounds point and
    // the centre of the range rings.
    home_ll: (f32, f32),
    // The callsign selected in the waterslide, if any. When it matches a plotted
    // spot, a full-screen crosshair marks that station's location on the map.
    selected: Option<&str>,
) {
    if screen.width() < 24.0 || screen.height() < 24.0 {
        return;
    }
    // SVG content area: padding t6 r8 b4 l8.
    let content = Rect::from_min_max(
        Pos2::new(screen.left() + 8.0, screen.top() + 6.0),
        Pos2::new(screen.right() - 8.0, screen.bottom() - 4.0),
    );

    // Dynamic bounds: fit the box (in world/SVG units) spanning every plotted
    // station plus home. Home is included but not centered, so it lands wherever
    // the worked cluster puts it (e.g. contacts to the west → home biased right).
    let mut pts: Vec<Vec2> = spots
        .iter()
        .filter_map(|s| pd::station_lonlat(&s.call, &s.grid))
        .map(|(lon, lat)| Vec2::new(pd::map_x(lon), pd::map_y(lat)))
        .collect();
    pts.push(Vec2::new(pd::map_x(home_ll.0), pd::map_y(home_ll.1)));
    let (mut minx, mut miny, mut maxx, mut maxy) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for v in &pts {
        minx = minx.min(v.x);
        miny = miny.min(v.y);
        maxx = maxx.max(v.x);
        maxy = maxy.max(v.y);
    }
    // Pad ~8%, then enforce a minimum span so a sparse map — e.g. just the home
    // marker before any spots arrive (real mode starts with an empty log + heard
    // set) — settles on a regional view instead of collapsing onto a single point.
    // Without this floor `scale` runs away and the scale-derived graticule font
    // (`font(4.6)` below) requests a multi-thousand-pixel glyph that overflows the
    // font atlas and aborts the app.
    const MIN_SPAN_X: f32 = 400.0; // ~80° lon (the projection is 5 world units/°)
    const MIN_SPAN_Y: f32 = 240.0; // ~48° lat
    let (bcx, bcy) = ((minx + maxx) * 0.5, (miny + maxy) * 0.5);
    let half_x = ((maxx - minx) * 0.54).max(MIN_SPAN_X * 0.5); // 0.54 = ½ span + 8% pad
    let half_y = ((maxy - miny) * 0.54).max(MIN_SPAN_Y * 0.5);
    minx = bcx - half_x;
    maxx = bcx + half_x;
    miny = bcy - half_y;
    maxy = bcy + half_y;
    let scale = (content.width() / (maxx - minx)).min(content.height() / (maxy - miny));
    let p = |sx: f32, sy: f32| {
        Pos2::new(
            content.center().x + (sx - bcx) * scale,
            content.center().y + (sy - bcy) * scale,
        )
    };
    let proj = |lon: f32, lat: f32| p(pd::map_x(lon), pd::map_y(lat));
    let sl = |v: f32| v * scale; // svg length -> px
    let font = |sz: f32| mono(sz * scale);

    let map_painter = painter.with_clip_rect(screen.shrink(2.0));
    let painter = &map_painter;

    // 1) basemap: pre-triangulated land + lakes (Natural Earth 50m, earcut offline).
    let project = |verts: &[(f32, f32)]| -> Vec<Pos2> {
        verts.iter().map(|&(la, lo)| proj(lo, la)).collect()
    };
    let stroke_rings = |pos: &[Pos2], rings: &[(u32, u32)], stroke: Stroke| {
        for &(s, l) in rings {
            let ring = &pos[s as usize..(s + l) as usize];
            let mut closed = ring.to_vec();
            closed.push(ring[0]);
            painter.add(Shape::line(closed, stroke));
        }
    };

    // Land is drawn in two passes: a flat base fill, then a shaded-relief overlay
    // composited on top. The relief texture carries the hillshade as alpha (RGB =
    // white), so the overlay tint decides the direction of the depth cue — a dark
    // tint shades the terrain (dark theme), a light tint highlights it (light
    // theme). Plains have ~0 alpha and read as the flat base in either theme.
    let land_base = over(pal.map_land, pal.screen_bg);
    let land_pos = project(geo_data::LAND_VERTS);
    let mut land_mesh = Mesh::default();
    for &pos in &land_pos {
        land_mesh.colored_vertex(pos, land_base);
    }
    land_mesh.indices.extend_from_slice(geo_data::LAND_IDX);
    painter.add(Shape::mesh(land_mesh));

    let relief_tint = if pal.is_dark {
        Color32::BLACK
    } else {
        Color32::WHITE
    };
    let lon_span = pd::RELIEF_LON1 - pd::RELIEF_LON0;
    let lat_span = pd::RELIEF_LAT1 - pd::RELIEF_LAT0;
    let mut relief_mesh = Mesh::with_texture(relief.id());
    for (i, &(la, lo)) in geo_data::LAND_VERTS.iter().enumerate() {
        let uv = Pos2::new(
            (lo - pd::RELIEF_LON0) / lon_span,
            (pd::RELIEF_LAT1 - la) / lat_span,
        );
        relief_mesh.vertices.push(egui::epaint::Vertex {
            pos: land_pos[i],
            uv,
            color: relief_tint,
        });
    }
    relief_mesh.indices.extend_from_slice(geo_data::LAND_IDX);
    painter.add(Shape::mesh(relief_mesh));
    stroke_rings(
        &land_pos,
        geo_data::LAND_RINGS,
        Stroke::new(sl(0.5).max(0.6), pal.map_coast),
    );

    // Lakes: translucent dark fill punches the land back down to water tone.
    let lake_fill = Color32::from_rgba_unmultiplied(
        pal.screen_bg.r(),
        pal.screen_bg.g(),
        pal.screen_bg.b(),
        220,
    );
    let lake_pos = project(geo_data::LAKES_VERTS);
    let mut lake_mesh = Mesh::default();
    for pos in &lake_pos {
        lake_mesh.colored_vertex(*pos, lake_fill);
    }
    for t in geo_data::LAKES_IDX.chunks_exact(3) {
        lake_mesh.add_triangle(t[0], t[1], t[2]);
    }
    painter.add(Shape::mesh(lake_mesh));
    stroke_rings(
        &lake_pos,
        geo_data::LAKES_RINGS,
        Stroke::new(sl(0.4).max(0.5), pal.map_coast.gamma_multiply(0.7)),
    );

    // 2) graticule
    let grat = pal.dim.gamma_multiply(0.25);
    for &lon in pd::MERIDIANS {
        let x = pd::map_x(lon);
        painter.line_segment([p(x, 0.0), p(x, pd::MAP_H)], Stroke::new(0.4, grat));
    }
    for &lat in pd::PARALLELS {
        let y = pd::map_y(lat);
        painter.line_segment([p(0.0, y), p(pd::MAP_W, y)], Stroke::new(0.4, grat));
        // Pin the label to the visible left edge: at world zoom the map's own left
        // edge (lon −180) is usually off-screen, so anchor in screen space instead.
        painter.text(
            Pos2::new(content.left() + 2.0, p(0.0, y).y - 1.5),
            Align2::LEFT_BOTTOM,
            format!("{lat:.0}°"),
            font(4.6),
            pal.dim.gamma_multiply(0.65),
        );
    }

    // 3) range rings (dashed ellipses about home)
    let home = proj(home_ll.0, home_ll.1);
    for &km in pd::RING_KM {
        let rx = sl((km / 85.0) * pd::KX * pd::S);
        let ry = sl((km / 111.0) * pd::S);
        let pts = ellipse_pts(home, rx, ry, 96);
        dashed_polyline(
            painter,
            &pts,
            Stroke::new(sl(0.45).max(0.6), pal.accent.gamma_multiply(0.32)),
            sl(2.0),
            sl(2.5),
        );
    }

    // 3.5) selection crosshair — when a station selected in the waterslide is
    // plotted here, mark its location with a full-screen horizontal + vertical
    // crosshair so the operator can find it at a glance. Position is taken from the
    // matching spot's grid, so the crosshair lands exactly on its marker. Drawn
    // under the spots (step 4) so the dot and label stay crisp; a highlight ring is
    // added over the spots below.
    let selected_pos = selected.and_then(|call| {
        spots
            .iter()
            .find(|s| s.call.eq_ignore_ascii_case(call))
            .and_then(|s| pd::station_lonlat(&s.call, &s.grid))
            .map(|(lon, lat)| proj(lon, lat))
    });
    if let Some(sp) = selected_pos {
        let cross = Stroke::new(1.8, pal.accent.gamma_multiply(0.6));
        painter.line_segment(
            [Pos2::new(content.left(), sp.y), Pos2::new(content.right(), sp.y)],
            cross,
        );
        painter.line_segment(
            [Pos2::new(sp.x, content.top()), Pos2::new(sp.x, content.bottom())],
            cross,
        );
    }

    // 4) station spots — position inferred from each station's grid; marker/label
    // sized in px (with clamp) so they stay readable at any zoom. Every marker is in
    // the accent color; the *shape* carries the category (`docs/map_panel.md`):
    //   • heard but unworked  → hollow circle
    //   • unworked, calling CQ → triangle (an answerable caller)
    //   • worked (in the log)  → plus sign
    let spot_r = sl(3.4).clamp(3.2, 5.4);
    let stroke_w = (spot_r * 0.42).clamp(1.3, 2.0);
    let label_font = mono(sl(4.8).clamp(5.0, 8.0));
    let plot = |call: &str, lon: f32, lat: f32, kind: Marker, color: Color32, label: Color32| {
        let pos = proj(lon, lat);
        let stroke = Stroke::new(stroke_w, color);
        match kind {
            Marker::Circle => {
                painter.circle_stroke(pos, spot_r, stroke);
            }
            Marker::Plus => {
                painter.line_segment(
                    [Pos2::new(pos.x - spot_r, pos.y), Pos2::new(pos.x + spot_r, pos.y)],
                    stroke,
                );
                painter.line_segment(
                    [Pos2::new(pos.x, pos.y - spot_r), Pos2::new(pos.x, pos.y + spot_r)],
                    stroke,
                );
            }
            Marker::Triangle => {
                // Equilateral-ish, point up, centroid on the spot.
                let tri = vec![
                    Pos2::new(pos.x, pos.y - spot_r),
                    Pos2::new(pos.x - spot_r * 0.92, pos.y + spot_r * 0.72),
                    Pos2::new(pos.x + spot_r * 0.92, pos.y + spot_r * 0.72),
                ];
                painter.add(Shape::closed_line(tri, stroke));
            }
        }
        // Flip the label to the inboard side near the right/top edges so it stays on-screen.
        let right = pos.x > content.right() - 42.0;
        let near_top = pos.y < content.top() + 12.0;
        let off = Vec2::new(
            if right { -(spot_r + 1.5) } else { spot_r + 1.5 },
            if near_top {
                spot_r + 5.0
            } else {
                -(spot_r + 1.0)
            },
        );
        let align = if right {
            Align2::RIGHT_BOTTOM
        } else {
            Align2::LEFT_BOTTOM
        };
        painter.text(pos + off, align, call, label_font.clone(), label);
    };

    // Heard-but-unworked first, then worked, so worked markers paint on top. Heard
    // markers dim with last-heard age (full → 0.2 over the hour; spots older than an
    // hour are filtered upstream); a CQ caller reads as a triangle, others a circle.
    for s in spots.iter().filter(|s| !s.worked) {
        let Some((lon, lat)) = pd::station_lonlat(&s.call, &s.grid) else {
            continue;
        };
        let age = ((now_ms - s.last_ms).max(0) as f32 / 3_600_000.0).clamp(0.0, 1.0);
        let alpha = 1.0 - 0.8 * age;
        let kind = if s.cq { Marker::Triangle } else { Marker::Circle };
        plot(
            &s.call,
            lon,
            lat,
            kind,
            pal.accent.gamma_multiply(alpha),
            pal.sub.gamma_multiply(alpha),
        );
    }
    // Worked → accent plus sign.
    for s in spots.iter().filter(|s| s.worked) {
        let Some((lon, lat)) = pd::station_lonlat(&s.call, &s.grid) else {
            continue;
        };
        plot(&s.call, lon, lat, Marker::Plus, pal.accent, pal.body);
    }

    // Highlight ring around the selected station's marker (over the spots, under
    // the home marker) so the crosshair's target reads clearly.
    if let Some(sp) = selected_pos {
        painter.circle_stroke(sp, spot_r + 2.6, Stroke::new(1.5, pal.accent));
    }

    // 5) home / QTH marker — the strongest indicator, drawn last so it sits on top.
    let ring_r = sl(4.6).clamp(5.0, 7.0);
    let arm = ring_r + 2.5;
    painter.circle(
        home,
        ring_r,
        Color32::TRANSPARENT,
        Stroke::new(1.4, pal.accent),
    );
    painter.line_segment(
        [
            Pos2::new(home.x - arm, home.y),
            Pos2::new(home.x + arm, home.y),
        ],
        Stroke::new(1.0, pal.accent),
    );
    painter.line_segment(
        [
            Pos2::new(home.x, home.y - arm),
            Pos2::new(home.x, home.y + arm),
        ],
        Stroke::new(1.0, pal.accent),
    );
    painter.circle_filled(home, (spot_r + 0.8).max(2.6), pal.accent);
    painter.text(
        Pos2::new(home.x + arm, home.y - arm),
        Align2::LEFT_BOTTOM,
        "QTH",
        heading(sl(4.8).clamp(6.0, 9.0)),
        pal.accent,
    );
}
