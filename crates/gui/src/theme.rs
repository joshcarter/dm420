//! "Martian Hybrid" theme: palettes, geometry tokens, and the shared low-level
//! painters that draw the instrument-panel chrome (gradients, recessed screens,
//! corner brackets, spine headers). Everything that reads color goes through a
//! `Palette`, so the light/dark flip is a single struct swap.

use egui::{
    Color32, CornerRadius, FontFamily, FontId, Mesh, Pos2, Rect, Shape, Stroke, StrokeKind, Vec2,
};

/// Named font family for headings/legends (Chakra Petch). Body/data text uses
/// `FontFamily::Monospace`, which we remap to IBM Plex Mono.
pub fn heading_family() -> FontFamily {
    FontFamily::Name("heading".into())
}

/// Heavier heading family (Chakra Petch 700) for callsigns, band numerals, the
/// clock readouts, and lit toggle keys — the design's weight-700 elements.
pub fn heading_bold_family() -> FontFamily {
    FontFamily::Name("heading_bold".into())
}

pub fn heading(size: f32) -> FontId {
    FontId::new(size, heading_family())
}

pub fn heading_bold(size: f32) -> FontId {
    FontId::new(size, heading_bold_family())
}

pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

// ---------------------------------------------------------------------------
// Geometry tokens (logical px). Ratios matter more than exact values.
// ---------------------------------------------------------------------------

#[allow(dead_code)] // chassis corner radius token (window uses OS rounding)
pub const CHASSIS_RADIUS: u8 = 4;
pub const BRACKET_ARM: f32 = 9.0;
pub const BRACKET_STROKE: f32 = 1.5;
pub const SPINE_W: f32 = 3.0;
pub const SPINE_H: f32 = 14.0;
pub const TOGGLE_SQ: f32 = 10.0;
pub const TOGGLE_STROKE: f32 = 1.5;
// Panel/layout geometry (TOPBAR_H, FOOTER_H, header rows, gaps) lives in
// `panel_data`, which owns the 960×600 panel layout.

// ---------------------------------------------------------------------------
// Palette
// ---------------------------------------------------------------------------

/// Every color the custom chrome reads. Two instances exist: GRAPHITE and SILVER.
#[derive(Clone, Copy)]
pub struct Palette {
    pub face_top: Color32,
    pub face_bottom: Color32,
    pub edge: Color32,
    pub stripe_light: Color32,
    pub stripe_dark: Color32,
    pub legend: Color32,
    pub sub: Color32,
    pub accent: Color32,
    pub screen_bg: Color32,
    pub ring: Color32,
    pub body: Color32,
    pub dim: Color32,
    pub lcd_top: Color32,
    pub lcd_bottom: Color32,
    pub lcd_text: Color32,
    /// Text drawn ON an accent fill (lit toggle keys / Scan button).
    pub on_accent: Color32,
    /// Contacts-map land polygon fill + coastline stroke.
    pub map_land: Color32,
    pub map_coast: Color32,
    pub is_dark: bool,
}

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}
const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied_const(r, g, b, a)
}

/// DARK — "graphite"
pub const GRAPHITE: Palette = Palette {
    face_top: rgb(0x2C, 0x28, 0x23),
    face_bottom: rgb(0x17, 0x13, 0x10),
    edge: rgb(0x10, 0x0C, 0x08),
    stripe_light: rgba(255, 255, 255, 6), // ~0.022
    stripe_dark: rgba(0, 0, 0, 8),         // ~0.03
    legend: rgb(0xF6, 0xE6, 0xCF),
    sub: rgba(202, 180, 150, 184), // 0.72
    accent: rgb(0xF7, 0x92, 0x0F),
    screen_bg: rgb(0x08, 0x06, 0x04),
    ring: rgba(247, 146, 15, 102), // 0.40
    body: rgb(0xF4, 0xEE, 0xE6),
    dim: rgba(205, 175, 140, 153), // 0.60
    lcd_top: rgb(0x1C, 0x14, 0x07),
    lcd_bottom: rgb(0x0D, 0x0A, 0x04),
    lcd_text: rgb(0xFF, 0xB2, 0x4D),
    on_accent: rgb(0x1D, 0x14, 0x08),
    map_land: rgba(247, 211, 162, 165), // warm tan base; relief texel shades it
    map_coast: rgba(247, 160, 60, 102), // rgba(...,0.40)
    is_dark: true,
};

/// LIGHT — "silver"
pub const SILVER: Palette = Palette {
    face_top: rgb(0xE9, 0xE4, 0xD9),
    face_bottom: rgb(0xCD, 0xC6, 0xB7),
    edge: rgb(0xA3, 0x98, 0x80),
    stripe_light: rgba(255, 255, 255, 140), // 0.55
    stripe_dark: rgba(0, 0, 0, 9),            // 0.035
    legend: rgb(0x36, 0x26, 0x0F),
    sub: rgba(95, 68, 32, 199), // 0.78
    accent: rgb(0xC2, 0x66, 0x0F),
    screen_bg: rgb(0xEF, 0xE7, 0xDC),
    ring: rgba(150, 80, 10, 107), // 0.42
    body: rgb(0x24, 0x18, 0x08),
    dim: rgba(120, 80, 40, 158), // 0.62
    lcd_top: rgb(0xE8, 0xDC, 0xC0),
    lcd_bottom: rgb(0xD4, 0xC6, 0x9E),
    lcd_text: rgb(0x3A, 0x2A, 0x10),
    on_accent: rgb(0xFD, 0xF6, 0xEC),
    map_land: rgba(150, 96, 36, 110), // warm tan base; relief texel shades it
    map_coast: rgba(150, 80, 10, 115), // rgba(...,0.45)
    is_dark: false,
};

// ---------------------------------------------------------------------------
// Shared painters
// ---------------------------------------------------------------------------

/// Vertical two-stop gradient via a vertex-colored quad mesh. egui has no
/// gradient primitive, so this is the workhorse for face + LCD + sheen.
pub fn vertical_gradient(painter: &egui::Painter, rect: Rect, top: Color32, bottom: Color32) {
    let mut mesh = Mesh::default();
    mesh.colored_vertex(rect.left_top(), top);
    mesh.colored_vertex(rect.right_top(), top);
    mesh.colored_vertex(rect.right_bottom(), bottom);
    mesh.colored_vertex(rect.left_bottom(), bottom);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(Shape::mesh(mesh));
}

/// Four L-shaped corner brackets, flush to the rect corners (0px inset).
pub fn corner_brackets(painter: &egui::Painter, rect: Rect, accent: Color32) {
    let s = Stroke::new(BRACKET_STROKE, accent);
    let a = BRACKET_ARM;
    let inset = BRACKET_STROKE * 0.5; // keep the stroke fully inside the rect
    let (l, r, t, b) = (
        rect.left() + inset,
        rect.right() - inset,
        rect.top() + inset,
        rect.bottom() - inset,
    );
    // top-left
    painter.line_segment([Pos2::new(l, t), Pos2::new(l + a, t)], s);
    painter.line_segment([Pos2::new(l, t), Pos2::new(l, t + a)], s);
    // top-right
    painter.line_segment([Pos2::new(r, t), Pos2::new(r - a, t)], s);
    painter.line_segment([Pos2::new(r, t), Pos2::new(r, t + a)], s);
    // bottom-left
    painter.line_segment([Pos2::new(l, b), Pos2::new(l + a, b)], s);
    painter.line_segment([Pos2::new(l, b), Pos2::new(l, b - a)], s);
    // bottom-right
    painter.line_segment([Pos2::new(r, b), Pos2::new(r - a, b)], s);
    painter.line_segment([Pos2::new(r, b), Pos2::new(r, b - a)], s);
}

/// The recessed "screen": flat fill, 1px accent ring, and a short top-edge
/// gradient that fakes the inset/recessed bevel (no inset-shadow primitive in
/// egui). Then the four corner brackets on top.
pub fn recessed_screen(painter: &egui::Painter, rect: Rect, pal: &Palette) {
    painter.rect_filled(rect, CornerRadius::ZERO, pal.screen_bg);

    // Top-edge shadow gradient: darker at the top fading to transparent — reads
    // as the screen sitting below the chassis lip.
    let shade_h = (rect.height() * 0.18).min(22.0);
    if shade_h > 1.0 {
        let shade = Rect::from_min_size(rect.min, Vec2::new(rect.width(), shade_h));
        let dark = Color32::from_rgba_unmultiplied(0, 0, 0, if pal.is_dark { 120 } else { 60 });
        let clear = Color32::from_rgba_unmultiplied(0, 0, 0, 0);
        vertical_gradient(painter, shade, dark, clear);
    }

    // 1px accent ring, painted inside the rect edge.
    painter.rect_stroke(
        rect,
        CornerRadius::ZERO,
        Stroke::new(1.0, pal.ring),
        StrokeKind::Inside,
    );

    corner_brackets(painter, rect, pal.accent);
}

/// Draw the 3px accent spine bar at `left_center`, returning x where text starts.
pub fn spine(painter: &egui::Painter, left_center: Pos2, accent: Color32) -> f32 {
    let rect = Rect::from_center_size(
        Pos2::new(left_center.x + SPINE_W * 0.5, left_center.y),
        Vec2::new(SPINE_W, SPINE_H),
    );
    painter.rect_filled(rect, CornerRadius::ZERO, accent);
    rect.right()
}

/// "Engraved" legend: draw text twice with a 1px offset for a faux text-shadow.
/// Returns the galley rect. Letter-spacing is faked by inserting thin spaces.
pub fn engraved_text(
    painter: &egui::Painter,
    pos: Pos2,
    text: &str,
    font: FontId,
    color: Color32,
    shadow: Color32,
    anchor: egui::Align2,
) -> Rect {
    painter.text(pos + Vec2::new(0.0, 1.0), anchor, text, font.clone(), shadow);
    painter.text(pos, anchor, text, font, color)
}

/// Approximate CSS letter-spacing on tracked caps by interleaving thin spaces.
/// egui `RichText` has no letter-spacing; this is the cheap workaround.
pub fn tracked(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 {
            out.push('\u{2009}'); // thin space
        }
        out.push(ch);
    }
    out
}

pub fn corner_radius(r: u8) -> CornerRadius {
    CornerRadius::same(r)
}
