//! Contacts panel: a North-America map (relief-shaded land mesh + graticule +
//! range rings + worked spots) over a flat tactical footer (toggles + SNR bars).
//! Owns the four footer toggle states. The map/footer drawing helpers
//! (`over`, `dashed_polyline`, `ellipse_pts`) are single-consumer and live here.

use eframe::egui;
use egui::{
    Align2, Color32, CornerRadius, Mesh, Pos2, Rect, Shape, Stroke, StrokeKind, TextureHandle,
    Vec2,
};

use super::{Panel, PanelCtx};
use crate::chrome::{measure, panel_header};
use crate::geo_data;
use crate::panel_data as pd;
use crate::theme::*;

pub struct Contacts {
    toggles: [bool; 4], // footer DX ONLY / CQ / ALERT / LOG
}

impl Contacts {
    pub fn new() -> Self {
        Self {
            toggles: [true, false, false, true], // DX ONLY + LOG on, per reference
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

        let header = Rect::from_min_max(
            block.min,
            Pos2::new(block.right(), block.top() + pd::HEADER_ROW_H),
        );
        panel_header(painter, header, pal, "Contacts", "N. America · DN70KA");
        painter.text(
            Pos2::new(header.right() - 2.0, header.center().y),
            Align2::RIGHT_CENTER,
            &format!("{} spots", pd::WORKED.len()),
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
        draw_map(painter, screen, pal, ctx.relief);
        self.draw_footer(ctx.ui, painter, footer, pal);
    }
}

impl Contacts {
    /// Flat tactical footer: square toggles (solid = on, hollow = off) + SNR bars.
    fn draw_footer(&mut self, ui: &mut egui::Ui, painter: &egui::Painter, rect: Rect, pal: &Palette) {
        let cy = rect.center().y;
        let labels = ["DX ONLY", "CQ", "ALERT", "LOG"];
        let mut x = rect.left();
        for i in 0..4 {
            let sq = Rect::from_center_size(Pos2::new(x + TOGGLE_SQ * 0.5, cy), Vec2::splat(TOGGLE_SQ));
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
            let label = tracked(labels[i]);
            painter.text(Pos2::new(tx, cy), Align2::LEFT_CENTER, &label, heading(8.5), label_color);
            x = tx + measure(painter, &label, heading(8.5)) + 18.0;
        }

        // SNR bar-graph (right).
        let heights = [5.0, 8.0, 11.0, 14.0, 9.0, 5.0];
        let mut bx = rect.right() - 70.0;
        let base = cy + 7.0;
        for (j, h) in heights.iter().enumerate() {
            let on = j < 4;
            let bar = Rect::from_min_max(Pos2::new(bx, base - h), Pos2::new(bx + 3.0, base));
            let col = if on { pal.accent } else { pal.sub.gamma_multiply(0.45) };
            painter.rect_filled(bar, CornerRadius::ZERO, col);
            bx += 5.0;
        }
        painter.text(Pos2::new(bx + 4.0, cy), Align2::LEFT_CENTER, "SNR", mono(7.5), pal.sub);
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

fn draw_map(painter: &egui::Painter, screen: Rect, pal: &Palette, relief: &TextureHandle) {
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
    let mut pts: Vec<Vec2> = pd::WORKED
        .iter()
        .filter_map(|s| pd::station_lonlat(s.call, s.grid))
        .map(|(lon, lat)| Vec2::new(pd::map_x(lon), pd::map_y(lat)))
        .collect();
    pts.push(Vec2::new(pd::map_x(pd::HOME_LON), pd::map_y(pd::HOME_LAT)));
    let (mut minx, mut miny, mut maxx, mut maxy) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for v in &pts {
        minx = minx.min(v.x); miny = miny.min(v.y);
        maxx = maxx.max(v.x); maxy = maxy.max(v.y);
    }
    // Pad ~8% and guard against a degenerate (single-point) box.
    let bw = (maxx - minx).max(1.0);
    let bh = (maxy - miny).max(1.0);
    minx -= bw * 0.08; maxx += bw * 0.08;
    miny -= bh * 0.08; maxy += bh * 0.08;
    let (bcx, bcy) = ((minx + maxx) * 0.5, (miny + maxy) * 0.5);
    let scale = (content.width() / (maxx - minx)).min(content.height() / (maxy - miny));
    let p = |sx: f32, sy: f32| {
        Pos2::new(content.center().x + (sx - bcx) * scale, content.center().y + (sy - bcy) * scale)
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

    // Land fill is a textured mesh: each vertex carries a UV into the shaded-relief
    // texture, and an opaque base tint that the relief texel multiplies — so the
    // land lightness varies with terrain (mountains shaded, plains flat).
    let land_base = over(pal.map_land, pal.screen_bg);
    let lon_span = pd::RELIEF_LON1 - pd::RELIEF_LON0;
    let lat_span = pd::RELIEF_LAT1 - pd::RELIEF_LAT0;
    let land_pos = project(geo_data::LAND_VERTS);
    let mut land_mesh = Mesh::with_texture(relief.id());
    for (i, &(la, lo)) in geo_data::LAND_VERTS.iter().enumerate() {
        let uv = Pos2::new((lo - pd::RELIEF_LON0) / lon_span, (pd::RELIEF_LAT1 - la) / lat_span);
        land_mesh.vertices.push(egui::epaint::Vertex { pos: land_pos[i], uv, color: land_base });
    }
    land_mesh.indices.extend_from_slice(geo_data::LAND_IDX);
    painter.add(Shape::mesh(land_mesh));
    stroke_rings(&land_pos, geo_data::LAND_RINGS, Stroke::new(sl(0.5).max(0.6), pal.map_coast));

    // Lakes: translucent dark fill punches the land back down to water tone.
    let lake_fill = Color32::from_rgba_unmultiplied(pal.screen_bg.r(), pal.screen_bg.g(), pal.screen_bg.b(), 220);
    let lake_pos = project(geo_data::LAKES_VERTS);
    let mut lake_mesh = Mesh::default();
    for pos in &lake_pos {
        lake_mesh.colored_vertex(*pos, lake_fill);
    }
    for t in geo_data::LAKES_IDX.chunks_exact(3) {
        lake_mesh.add_triangle(t[0], t[1], t[2]);
    }
    painter.add(Shape::mesh(lake_mesh));
    stroke_rings(&lake_pos, geo_data::LAKES_RINGS, Stroke::new(sl(0.4).max(0.5), pal.map_coast.gamma_multiply(0.7)));

    // 2) graticule
    let grat = pal.dim.gamma_multiply(0.25);
    for &lon in pd::MERIDIANS {
        let x = pd::map_x(lon);
        painter.line_segment([p(x, 0.0), p(x, pd::MAP_H)], Stroke::new(0.4, grat));
    }
    for &lat in pd::PARALLELS {
        let y = pd::map_y(lat);
        painter.line_segment([p(0.0, y), p(pd::MAP_W, y)], Stroke::new(0.4, grat));
        painter.text(
            p(2.0, y - 1.5),
            Align2::LEFT_BOTTOM,
            &format!("{lat:.0}°"),
            font(4.6),
            pal.dim.gamma_multiply(0.65),
        );
    }

    // 3) 49°N border (dashed accent), left edge → lon −95
    let by = pd::map_y(pd::BORDER_LAT);
    dashed_polyline(
        painter,
        &[p(0.0, by), p(pd::map_x(pd::BORDER_LON_END), by)],
        Stroke::new(sl(0.5).max(0.6), pal.accent.gamma_multiply(0.5)),
        sl(3.0),
        sl(2.0),
    );

    // 4) range rings (dashed ellipses about home)
    let home = proj(pd::HOME_LON, pd::HOME_LAT);
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

    // 5) worked spots (filled) — position inferred from each station's grid.
    // Marker/label sized in px (with clamp) so they stay readable at any zoom.
    let spot_r = sl(2.4).clamp(2.0, 3.6);
    let label_font = mono(sl(4.8).clamp(5.0, 8.0));
    for s in pd::WORKED {
        let Some((lon, lat)) = pd::station_lonlat(s.call, s.grid) else { continue };
        let pos = proj(lon, lat);
        painter.circle_filled(pos, spot_r, pal.accent);
        // Flip the label to the inboard side near the right/top edges so it stays on-screen.
        let right = pos.x > content.right() - 42.0;
        let near_top = pos.y < content.top() + 12.0;
        let off = Vec2::new(
            if right { -(spot_r + 1.5) } else { spot_r + 1.5 },
            if near_top { spot_r + 5.0 } else { -(spot_r + 1.0) },
        );
        let align = if right { Align2::RIGHT_BOTTOM } else { Align2::LEFT_BOTTOM };
        painter.text(pos + off, align, s.call, label_font.clone(), pal.body);
    }

    // 6) home / QTH marker — the strongest indicator, drawn last so it sits on top.
    let ring_r = sl(4.6).clamp(5.0, 7.0);
    let arm = ring_r + 2.5;
    painter.circle(home, ring_r, Color32::TRANSPARENT, Stroke::new(1.4, pal.accent));
    painter.line_segment([Pos2::new(home.x - arm, home.y), Pos2::new(home.x + arm, home.y)], Stroke::new(1.0, pal.accent));
    painter.line_segment([Pos2::new(home.x, home.y - arm), Pos2::new(home.x, home.y + arm)], Stroke::new(1.0, pal.accent));
    painter.circle_filled(home, (spot_r + 0.8).max(2.6), pal.accent);
    painter.text(Pos2::new(home.x + arm, home.y - arm), Align2::LEFT_BOTTOM, "QTH", heading(sl(4.8).clamp(6.0, 9.0)), pal.accent);
}
