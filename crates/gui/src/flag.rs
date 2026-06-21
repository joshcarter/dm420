//! Procedural country-flag icons for the Call Sign panel.
//!
//! The bundled tactical font can't render color-emoji flags, so flags are drawn
//! from primitives keyed by ISO 3166-1 alpha-2 code. Common layouts (tricolors,
//! Nordic crosses, discs, hoist triangles, plus a handful of distinctive
//! designs) render as recognizable true-color icons. Emblems (eagles, shields,
//! coats of arms) are omitted — the field/stripe layout is what reads at icon
//! size — so e.g. Kenya and Angola show their stripe fields without the central
//! device. Only the genuinely un-approximable flags (intricate emblems on a
//! plain field, the Nepali pennant) fall back to a neutral ISO chip. Phase 1 of
//! `docs/call_sign_lookup_panel.md`.

use eframe::egui;
use egui::{Align2, Color32, CornerRadius, Mesh, Pos2, Rect, Shape, Stroke};

use crate::theme::{Palette, mono};

// Flag palette — approximate official colors, kept saturated so the icon reads
// as a small splash of color against the monochrome chassis.
const RED: Color32 = Color32::from_rgb(0xC8, 0x10, 0x2E);
const WHITE: Color32 = Color32::from_rgb(0xF2, 0xF2, 0xF2);
const BLACK: Color32 = Color32::from_rgb(0x1A, 0x1A, 0x1A);
const GOLD: Color32 = Color32::from_rgb(0xFF, 0xCC, 0x00);
const NAVY: Color32 = Color32::from_rgb(0x01, 0x21, 0x69);
const BLUE: Color32 = Color32::from_rgb(0x00, 0x55, 0xA4);
const LBLUE: Color32 = Color32::from_rgb(0x6C, 0xA0, 0xDC);
const AZURE: Color32 = Color32::from_rgb(0x00, 0x57, 0xB7);
const GREEN: Color32 = Color32::from_rgb(0x00, 0x7A, 0x3D);
const ORANGE: Color32 = Color32::from_rgb(0xFF, 0x79, 0x00);
const SAFFRON: Color32 = Color32::from_rgb(0xFF, 0x99, 0x33);
const MAROON: Color32 = Color32::from_rgb(0x8A, 0x1C, 0x3B);

/// A flag layout. Slices are top→bottom (`H`) or left→right (`V`), equal bands.
enum Design {
    H(&'static [Color32]),
    V(&'static [Color32]),
    Nordic { field: Color32, cross: Color32 },
    Disc { field: Color32, disc: Color32 },
    /// Full-length upright cross (England/Georgia style).
    Cross { field: Color32, cross: Color32 },
    /// Horizontal bicolor with a triangle on the hoist (Czechia, Philippines).
    TriHoist { top: Color32, bottom: Color32, tri: Color32 },
    /// Single field — an emblem-only flag with the device omitted.
    Solid(Color32),
    /// A field with one centered five-pointed star (Morocco, Vietnam).
    Star { field: Color32, star: Color32 },
    /// Blue/light-blue ensign: a UK canton + stars in the fly (AU, NZ, …).
    Ensign(Color32),
    Swiss,
    Us,
    Canada,
    Uk,
    Brazil,
    Chile,
    Korea,
    Greece,
    China,
    Taiwan,
    Laos,
    /// No design — render the ISO code on a neutral chip.
    Chip,
}

/// Draw the flag for `iso` into `rect` (a thin framed icon). Unknown codes get a
/// labeled chip so the panel always shows *something* keyed to the country.
pub fn draw_flag(painter: &egui::Painter, rect: Rect, iso: &str, pal: &Palette) {
    let p = painter.with_clip_rect(rect);
    match design(iso) {
        Design::H(bands) => bands_h(&p, rect, bands),
        Design::V(bands) => bands_v(&p, rect, bands),
        Design::Nordic { field, cross } => nordic(&p, rect, field, cross),
        Design::Disc { field, disc } => disc_center(&p, rect, field, disc),
        Design::Cross { field, cross } => upright_cross(&p, rect, field, cross),
        Design::TriHoist { top, bottom, tri } => tri_hoist(&p, rect, top, bottom, tri),
        Design::Solid(c) => {
            p.rect_filled(rect, CornerRadius::ZERO, c);
        }
        Design::Star { field, star } => {
            p.rect_filled(rect, CornerRadius::ZERO, field);
            filled_star(&p, rect.center(), rect.height() * 0.3, star);
        }
        Design::Ensign(field) => ensign(&p, rect, field),
        Design::Swiss => swiss(&p, rect),
        Design::Us => us(&p, rect),
        Design::Canada => canada(&p, rect),
        Design::Uk => uk(&p, rect),
        Design::Brazil => brazil(&p, rect),
        Design::Chile => chile(&p, rect),
        Design::Korea => korea(&p, rect),
        Design::Greece => greece(&p, rect),
        Design::China => china(&p, rect),
        Design::Taiwan => taiwan(&p, rect),
        Design::Laos => laos(&p, rect),
        Design::Chip => chip(painter, rect, iso, pal),
    }
    // Frame on top so the band/cross edges sit inside a crisp border.
    painter.rect_stroke(
        rect,
        CornerRadius::ZERO,
        Stroke::new(1.0, pal.dim.gamma_multiply(0.8)),
        egui::StrokeKind::Inside,
    );
}

fn design(iso: &str) -> Design {
    use Design::*;
    match iso {
        // ---- Americas ----
        "US" => Us,
        "CA" => Canada,
        "MX" => V(&[GREEN, WHITE, RED]),
        "PR" => H(&[RED, WHITE, RED, WHITE, RED]),
        "CU" => H(&[AZURE, WHITE, AZURE, WHITE, AZURE]),
        "HT" => H(&[AZURE, RED]),
        "CR" => H(&[AZURE, WHITE, RED, WHITE, AZURE]),
        "SV" | "HN" | "NI" => H(&[AZURE, WHITE, AZURE]),
        "GT" => V(&[LBLUE, WHITE, LBLUE]),
        "BB" => V(&[AZURE, GOLD, AZURE]),
        "VC" => V(&[AZURE, GOLD, GREEN]),
        "BR" => Brazil,
        "AR" => H(&[LBLUE, WHITE, LBLUE]),
        "CL" => Chile,
        "CO" | "EC" | "VE" => H(&[GOLD, AZURE, RED]),
        "PE" => V(&[RED, WHITE, RED]),
        "BO" => H(&[RED, GOLD, GREEN]),
        "PY" => H(&[RED, WHITE, AZURE]),
        "SR" => H(&[GREEN, WHITE, RED, WHITE, GREEN]),
        // ---- Europe ----
        "GB" | "IM" | "JE" | "GG" => Uk,
        "IE" => V(&[GREEN, WHITE, ORANGE]),
        "DE" => H(&[BLACK, RED, GOLD]),
        "FR" => V(&[BLUE, WHITE, RED]),
        "IT" => V(&[GREEN, WHITE, RED]),
        "ES" => H(&[RED, GOLD, RED]),
        "PT" => V(&[GREEN, RED]),
        "NL" => H(&[RED, WHITE, BLUE]),
        "BE" => V(&[BLACK, GOLD, RED]),
        "LU" => H(&[RED, WHITE, LBLUE]),
        "CH" => Swiss,
        "LI" => H(&[AZURE, RED]),
        "AT" => H(&[RED, WHITE, RED]),
        "SE" => Nordic { field: AZURE, cross: GOLD },
        "DK" | "NO" => Nordic { field: RED, cross: WHITE },
        "FI" => Nordic { field: WHITE, cross: AZURE },
        "IS" => Nordic { field: AZURE, cross: WHITE },
        "PL" => H(&[WHITE, RED]),
        "CZ" => TriHoist { top: WHITE, bottom: RED, tri: AZURE },
        "SK" | "SI" | "HR" | "RS" => H(&[WHITE, AZURE, RED]),
        "HU" => H(&[RED, WHITE, GREEN]),
        "RO" | "MD" => V(&[AZURE, GOLD, RED]),
        "BG" => H(&[WHITE, GREEN, RED]),
        "MK" => Disc { field: RED, disc: GOLD },
        "AL" => Solid(RED),
        "GR" => Greece,
        "EE" => H(&[AZURE, BLACK, WHITE]),
        "LV" => H(&[MAROON, WHITE, MAROON]),
        "LT" => H(&[GOLD, GREEN, RED]),
        "UA" => H(&[AZURE, GOLD]),
        "BY" => H(&[RED, RED, GREEN]),
        "RU" => H(&[WHITE, AZURE, RED]),
        // ---- Caucasus / Central Asia ----
        "KZ" => Disc { field: AZURE, disc: GOLD },
        "KG" => Disc { field: RED, disc: GOLD },
        "TJ" => H(&[RED, WHITE, GREEN]),
        "TM" => Solid(GREEN),
        "UZ" => H(&[AZURE, WHITE, GREEN]),
        "GE" => Cross { field: WHITE, cross: RED },
        "AM" => H(&[RED, AZURE, ORANGE]),
        "AZ" => H(&[AZURE, RED, GREEN]),
        // ---- Mediterranean / Middle East ----
        "MT" => V(&[WHITE, RED]),
        "TR" => Disc { field: RED, disc: WHITE },
        "IL" => H(&[AZURE, WHITE, AZURE]),
        "JO" => H(&[BLACK, WHITE, GREEN]),
        "LB" => H(&[RED, WHITE, RED]),
        "SY" | "IQ" | "EG" => H(&[RED, WHITE, BLACK]),
        "IR" => H(&[GREEN, WHITE, RED]),
        "AE" => H(&[GREEN, WHITE, BLACK]),
        "QA" => V(&[WHITE, MAROON]),
        "BH" => V(&[WHITE, RED]),
        "KW" => H(&[GREEN, WHITE, RED]),
        "SA" => Solid(GREEN),
        "BW" => H(&[LBLUE, BLACK, LBLUE]),
        // ---- Africa ----
        "MA" => Star { field: RED, star: GREEN },
        "DZ" => V(&[GREEN, WHITE]),
        "TN" => Disc { field: RED, disc: WHITE },
        "LY" => H(&[RED, BLACK, GREEN]),
        "KE" => H(&[BLACK, RED, GREEN]),
        "NG" => V(&[GREEN, WHITE, GREEN]),
        "GH" => H(&[RED, GOLD, GREEN]),
        "AO" => H(&[RED, BLACK]),
        "MU" => H(&[RED, AZURE, GOLD, GREEN]),
        // ---- Asia ----
        "JP" => Disc { field: WHITE, disc: RED },
        "KR" => Korea,
        "CN" => China,
        "TW" => Taiwan,
        "HK" => Disc { field: RED, disc: WHITE },
        "MO" => Disc { field: GREEN, disc: WHITE },
        "IN" => H(&[SAFFRON, WHITE, GREEN]),
        "PK" => V(&[WHITE, GREEN, GREEN, GREEN]),
        "BD" => Disc { field: GREEN, disc: RED },
        "LA" => Laos,
        "MM" => H(&[GOLD, GREEN, RED]),
        "TH" => H(&[RED, WHITE, AZURE, WHITE, RED]),
        "VN" => Star { field: RED, star: GOLD },
        "SG" => H(&[RED, WHITE]),
        "ID" => H(&[RED, WHITE]),
        "PH" => TriHoist { top: AZURE, bottom: RED, tri: WHITE },
        // ---- Oceania ----
        "AU" | "NZ" | "CK" => Ensign(AZURE),
        "FJ" => Ensign(LBLUE),
        // Emblem-on-plain-field flags and the Nepali pennant: a chip reads
        // cleaner than a bad approximation.
        _ => Chip,
    }
}

fn bands_h(painter: &egui::Painter, rect: Rect, colors: &[Color32]) {
    let n = colors.len() as f32;
    for (i, &c) in colors.iter().enumerate() {
        let y0 = rect.top() + rect.height() * i as f32 / n;
        let y1 = rect.top() + rect.height() * (i as f32 + 1.0) / n;
        painter.rect_filled(
            Rect::from_min_max(Pos2::new(rect.left(), y0), Pos2::new(rect.right(), y1)),
            CornerRadius::ZERO,
            c,
        );
    }
}

fn bands_v(painter: &egui::Painter, rect: Rect, colors: &[Color32]) {
    let n = colors.len() as f32;
    for (i, &c) in colors.iter().enumerate() {
        let x0 = rect.left() + rect.width() * i as f32 / n;
        let x1 = rect.left() + rect.width() * (i as f32 + 1.0) / n;
        painter.rect_filled(
            Rect::from_min_max(Pos2::new(x0, rect.top()), Pos2::new(x1, rect.bottom())),
            CornerRadius::ZERO,
            c,
        );
    }
}

fn nordic(painter: &egui::Painter, rect: Rect, field: Color32, cross: Color32) {
    painter.rect_filled(rect, CornerRadius::ZERO, field);
    let bar = (rect.height() * 0.22).max(2.0);
    // Vertical arm, offset left of center (the canonical Nordic placement).
    let vx = rect.left() + rect.width() * 0.36;
    painter.rect_filled(
        Rect::from_min_max(
            Pos2::new(vx - bar / 2.0, rect.top()),
            Pos2::new(vx + bar / 2.0, rect.bottom()),
        ),
        CornerRadius::ZERO,
        cross,
    );
    let hy = rect.center().y;
    painter.rect_filled(
        Rect::from_min_max(
            Pos2::new(rect.left(), hy - bar / 2.0),
            Pos2::new(rect.right(), hy + bar / 2.0),
        ),
        CornerRadius::ZERO,
        cross,
    );
}

fn upright_cross(painter: &egui::Painter, rect: Rect, field: Color32, cross: Color32) {
    painter.rect_filled(rect, CornerRadius::ZERO, field);
    let c = rect.center();
    let t = rect.height() * 0.18;
    painter.rect_filled(
        Rect::from_min_max(
            Pos2::new(rect.left(), c.y - t),
            Pos2::new(rect.right(), c.y + t),
        ),
        CornerRadius::ZERO,
        cross,
    );
    painter.rect_filled(
        Rect::from_min_max(
            Pos2::new(c.x - t, rect.top()),
            Pos2::new(c.x + t, rect.bottom()),
        ),
        CornerRadius::ZERO,
        cross,
    );
}

fn disc_center(painter: &egui::Painter, rect: Rect, field: Color32, disc: Color32) {
    painter.rect_filled(rect, CornerRadius::ZERO, field);
    painter.circle_filled(rect.center(), rect.height() * 0.3, disc);
}

fn tri_hoist(painter: &egui::Painter, rect: Rect, top: Color32, bottom: Color32, tri: Color32) {
    let c = rect.center();
    painter.rect_filled(
        Rect::from_min_max(rect.min, Pos2::new(rect.right(), c.y)),
        CornerRadius::ZERO,
        top,
    );
    painter.rect_filled(
        Rect::from_min_max(Pos2::new(rect.left(), c.y), rect.max),
        CornerRadius::ZERO,
        bottom,
    );
    let apex = Pos2::new(rect.left() + rect.width() * 0.5, c.y);
    painter.add(Shape::convex_polygon(
        vec![rect.left_top(), rect.left_bottom(), apex],
        tri,
        Stroke::NONE,
    ));
}

fn swiss(painter: &egui::Painter, rect: Rect) {
    painter.rect_filled(rect, CornerRadius::ZERO, RED);
    let c = rect.center();
    let arm = rect.height() * 0.32;
    let t = rect.height() * 0.12;
    painter.rect_filled(
        Rect::from_min_max(Pos2::new(c.x - t, c.y - arm), Pos2::new(c.x + t, c.y + arm)),
        CornerRadius::ZERO,
        WHITE,
    );
    painter.rect_filled(
        Rect::from_min_max(Pos2::new(c.x - arm, c.y - t), Pos2::new(c.x + arm, c.y + t)),
        CornerRadius::ZERO,
        WHITE,
    );
}

fn us(painter: &egui::Painter, rect: Rect) {
    let n = 13usize;
    for i in 0..n {
        let y0 = rect.top() + rect.height() * i as f32 / n as f32;
        let y1 = rect.top() + rect.height() * (i as f32 + 1.0) / n as f32;
        let c = if i % 2 == 0 { RED } else { WHITE };
        painter.rect_filled(
            Rect::from_min_max(Pos2::new(rect.left(), y0), Pos2::new(rect.right(), y1)),
            CornerRadius::ZERO,
            c,
        );
    }
    let canton = Rect::from_min_max(
        rect.min,
        Pos2::new(
            rect.left() + rect.width() * 0.42,
            rect.top() + rect.height() * 7.0 / 13.0,
        ),
    );
    painter.rect_filled(canton, CornerRadius::ZERO, NAVY);
    let (cols, rows) = (5, 4);
    for r in 0..rows {
        for c in 0..cols {
            let x = canton.left() + canton.width() * (c as f32 + 0.5) / cols as f32;
            let y = canton.top() + canton.height() * (r as f32 + 0.5) / rows as f32;
            painter.circle_filled(Pos2::new(x, y), 0.7, WHITE);
        }
    }
}

fn canada(painter: &egui::Painter, rect: Rect) {
    painter.rect_filled(rect, CornerRadius::ZERO, WHITE);
    let q = rect.width() * 0.25;
    painter.rect_filled(
        Rect::from_min_max(rect.min, Pos2::new(rect.left() + q, rect.bottom())),
        CornerRadius::ZERO,
        RED,
    );
    painter.rect_filled(
        Rect::from_min_max(Pos2::new(rect.right() - q, rect.top()), rect.max),
        CornerRadius::ZERO,
        RED,
    );
    // A simplified, symmetric maple leaf centered in the white field.
    let c = rect.center();
    let s = rect.height() * 0.42;
    let half: &[(f32, f32)] = &[
        (0.0, 1.0),
        (0.12, 0.55),
        (0.42, 0.62),
        (0.32, 0.42),
        (0.62, 0.18),
        (0.48, 0.12),
        (0.52, -0.18),
        (0.20, -0.10),
        (0.16, -0.30),
        (0.0, -0.55),
    ];
    let mut ring: Vec<Pos2> = half
        .iter()
        .map(|&(x, y)| Pos2::new(c.x + x * s, c.y - y * s))
        .collect();
    for &(x, y) in half.iter().rev().skip(1) {
        ring.push(Pos2::new(c.x - x * s, c.y - y * s));
    }
    filled_fan(painter, c, &ring, RED);
}

fn uk(painter: &egui::Painter, rect: Rect) {
    painter.rect_filled(rect, CornerRadius::ZERO, NAVY);
    let (tl, tr) = (rect.left_top(), rect.right_top());
    let (bl, br) = (rect.left_bottom(), rect.right_bottom());
    painter.line_segment([tl, br], Stroke::new(rect.height() * 0.22, WHITE));
    painter.line_segment([tr, bl], Stroke::new(rect.height() * 0.22, WHITE));
    painter.line_segment([tl, br], Stroke::new(rect.height() * 0.09, RED));
    painter.line_segment([tr, bl], Stroke::new(rect.height() * 0.09, RED));
    let c = rect.center();
    let wt = rect.height() * 0.30;
    painter.rect_filled(
        Rect::from_min_max(
            Pos2::new(rect.left(), c.y - wt / 2.0),
            Pos2::new(rect.right(), c.y + wt / 2.0),
        ),
        CornerRadius::ZERO,
        WHITE,
    );
    painter.rect_filled(
        Rect::from_min_max(
            Pos2::new(c.x - wt / 2.0, rect.top()),
            Pos2::new(c.x + wt / 2.0, rect.bottom()),
        ),
        CornerRadius::ZERO,
        WHITE,
    );
    let rt = rect.height() * 0.16;
    painter.rect_filled(
        Rect::from_min_max(
            Pos2::new(rect.left(), c.y - rt / 2.0),
            Pos2::new(rect.right(), c.y + rt / 2.0),
        ),
        CornerRadius::ZERO,
        RED,
    );
    painter.rect_filled(
        Rect::from_min_max(
            Pos2::new(c.x - rt / 2.0, rect.top()),
            Pos2::new(c.x + rt / 2.0, rect.bottom()),
        ),
        CornerRadius::ZERO,
        RED,
    );
}

fn brazil(painter: &egui::Painter, rect: Rect) {
    painter.rect_filled(rect, CornerRadius::ZERO, GREEN);
    let c = rect.center();
    let (dx, dy) = (rect.width() * 0.42, rect.height() * 0.42);
    painter.add(Shape::convex_polygon(
        vec![
            Pos2::new(c.x, c.y - dy),
            Pos2::new(c.x + dx, c.y),
            Pos2::new(c.x, c.y + dy),
            Pos2::new(c.x - dx, c.y),
        ],
        GOLD,
        Stroke::NONE,
    ));
    painter.circle_filled(c, rect.height() * 0.16, AZURE);
}

fn chile(painter: &egui::Painter, rect: Rect) {
    let c = rect.center();
    painter.rect_filled(
        Rect::from_min_max(rect.min, Pos2::new(rect.right(), c.y)),
        CornerRadius::ZERO,
        WHITE,
    );
    painter.rect_filled(
        Rect::from_min_max(Pos2::new(rect.left(), c.y), rect.max),
        CornerRadius::ZERO,
        RED,
    );
    let s = rect.height() * 0.5;
    let canton = Rect::from_min_max(rect.min, Pos2::new(rect.left() + s, rect.top() + s));
    painter.rect_filled(canton, CornerRadius::ZERO, AZURE);
    filled_star(painter, canton.center(), s * 0.3, WHITE);
}

fn korea(painter: &egui::Painter, rect: Rect) {
    painter.rect_filled(rect, CornerRadius::ZERO, WHITE);
    let c = rect.center();
    let r = rect.height() * 0.3;
    painter.circle_filled(c, r, RED);
    // Blue lower half of the taegeuk (trigrams omitted).
    let lower = painter.with_clip_rect(Rect::from_min_max(
        Pos2::new(rect.left(), c.y),
        rect.max,
    ));
    lower.circle_filled(c, r, AZURE);
}

fn greece(painter: &egui::Painter, rect: Rect) {
    let n = 9usize;
    for i in 0..n {
        let y0 = rect.top() + rect.height() * i as f32 / n as f32;
        let y1 = rect.top() + rect.height() * (i as f32 + 1.0) / n as f32;
        let c = if i % 2 == 0 { AZURE } else { WHITE };
        painter.rect_filled(
            Rect::from_min_max(Pos2::new(rect.left(), y0), Pos2::new(rect.right(), y1)),
            CornerRadius::ZERO,
            c,
        );
    }
    let h = rect.height() * 5.0 / 9.0;
    let canton = Rect::from_min_max(rect.min, Pos2::new(rect.left() + h, rect.top() + h));
    painter.rect_filled(canton, CornerRadius::ZERO, AZURE);
    let cc = canton.center();
    let t = h * 0.16;
    painter.rect_filled(
        Rect::from_min_max(
            Pos2::new(canton.left(), cc.y - t),
            Pos2::new(canton.right(), cc.y + t),
        ),
        CornerRadius::ZERO,
        WHITE,
    );
    painter.rect_filled(
        Rect::from_min_max(
            Pos2::new(cc.x - t, canton.top()),
            Pos2::new(cc.x + t, canton.bottom()),
        ),
        CornerRadius::ZERO,
        WHITE,
    );
}

fn china(painter: &egui::Painter, rect: Rect) {
    painter.rect_filled(rect, CornerRadius::ZERO, RED);
    let big = Pos2::new(rect.left() + rect.width() * 0.2, rect.top() + rect.height() * 0.3);
    filled_star(painter, big, rect.height() * 0.18, GOLD);
    for &(fx, fy) in &[(0.38, 0.14), (0.46, 0.28), (0.46, 0.46), (0.38, 0.6)] {
        let p = Pos2::new(rect.left() + rect.width() * fx, rect.top() + rect.height() * fy);
        filled_star(painter, p, rect.height() * 0.06, GOLD);
    }
}

fn taiwan(painter: &egui::Painter, rect: Rect) {
    painter.rect_filled(rect, CornerRadius::ZERO, RED);
    let canton = Rect::from_min_max(
        rect.min,
        Pos2::new(
            rect.left() + rect.width() * 0.5,
            rect.top() + rect.height() * 0.5,
        ),
    );
    painter.rect_filled(canton, CornerRadius::ZERO, AZURE);
    painter.circle_filled(canton.center(), canton.height() * 0.32, WHITE);
}

fn laos(painter: &egui::Painter, rect: Rect) {
    painter.rect_filled(rect, CornerRadius::ZERO, RED);
    painter.rect_filled(
        Rect::from_min_max(
            Pos2::new(rect.left(), rect.top() + rect.height() * 0.25),
            Pos2::new(rect.right(), rect.bottom() - rect.height() * 0.25),
        ),
        CornerRadius::ZERO,
        AZURE,
    );
    painter.circle_filled(rect.center(), rect.height() * 0.2, WHITE);
}

fn ensign(painter: &egui::Painter, rect: Rect, field: Color32) {
    painter.rect_filled(rect, CornerRadius::ZERO, field);
    let canton = Rect::from_min_max(
        rect.min,
        Pos2::new(
            rect.left() + rect.width() * 0.5,
            rect.top() + rect.height() * 0.5,
        ),
    );
    uk(painter, canton);
    // A few white stars scattered through the fly.
    for &(fx, fy, s) in &[
        (0.74, 0.62, 0.16),
        (0.86, 0.40, 0.10),
        (0.66, 0.30, 0.10),
        (0.80, 0.80, 0.10),
    ] {
        let p = Pos2::new(rect.left() + rect.width() * fx, rect.top() + rect.height() * fy);
        filled_star(painter, p, rect.height() * s, WHITE);
    }
}

fn chip(painter: &egui::Painter, rect: Rect, iso: &str, pal: &Palette) {
    painter.rect_filled(rect, CornerRadius::ZERO, pal.screen_bg);
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        iso,
        mono((rect.height() * 0.5).clamp(8.0, 13.0)),
        pal.sub,
    );
}

/// Fill a five-pointed star (concave) as a triangle fan from its center.
fn filled_star(painter: &egui::Painter, center: Pos2, outer: f32, color: Color32) {
    use std::f32::consts::PI;
    let mut ring = Vec::with_capacity(10);
    for i in 0..10 {
        let ang = -PI / 2.0 + i as f32 * PI / 5.0;
        let rad = if i % 2 == 0 { outer } else { outer * 0.4 };
        ring.push(Pos2::new(
            center.x + rad * ang.cos(),
            center.y + rad * ang.sin(),
        ));
    }
    filled_fan(painter, center, &ring, color);
}

/// Fill a polygon that is star-shaped about `center` (a triangle fan). Works for
/// concave outlines like the maple leaf and stars, which `convex_polygon` can't.
fn filled_fan(painter: &egui::Painter, center: Pos2, ring: &[Pos2], color: Color32) {
    let mut mesh = Mesh::default();
    let c = mesh.vertices.len() as u32;
    mesh.colored_vertex(center, color);
    for &p in ring {
        mesh.colored_vertex(p, color);
    }
    let n = ring.len() as u32;
    for i in 0..n {
        mesh.add_triangle(c, c + 1 + i, c + 1 + (i + 1) % n);
    }
    painter.add(Shape::mesh(mesh));
}
