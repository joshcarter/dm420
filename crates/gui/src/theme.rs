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

/// App-wide floor for any text whose size is *computed at runtime* (e.g. the
/// waterslide decode lanes, or log-book rows scaled to a small pane). Below this
/// our monospace data text stops being readable, so every size-scaling call
/// clamps to this value. Fixed-size legends don't need it — they're authored
/// above the floor already.
pub const MIN_FONT_PT: f32 = 8.0;

// ---------------------------------------------------------------------------
// Geometry tokens (logical px). Ratios matter more than exact values.
// ---------------------------------------------------------------------------

#[allow(dead_code)] // chassis corner radius token (window uses OS rounding)
pub const CHASSIS_RADIUS: u8 = 4;
pub const BRACKET_ARM: f32 = 9.0;
pub const BRACKET_STROKE: f32 = 1.5;
/// Header focus marker: a 9px square at the start of each panel header. Hollow
/// (accent stroke) by default; filled with a faint glow when the panel holds
/// keyboard focus. Replaces the old 3×14 spine bar.
pub const FOCUS_BOX_SZ: f32 = 9.0;
pub const FOCUS_BOX_STROKE: f32 = 1.5;
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
    /// Secondary accent (cyan/blue). Used where the primary amber would read as
    /// "active/normal" but we need a distinct state — e.g. the Send button armed
    /// vs. idle, or an unworked station calling CQ.
    pub accent2: Color32,
    /// Transmit accent (electric violet). The "we are keyed and on
    /// the air" state — distinct from amber (idle) and accent2 (armed). Used by
    /// the Digital panel's corner brackets, NOW divider, TX lane, Send key, and
    /// the outgoing-message text.
    pub accent3: Color32,
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
    stripe_dark: rgba(0, 0, 0, 8),        // ~0.03
    legend: rgb(0xF6, 0xE6, 0xCF),
    sub: rgba(202, 180, 150, 184), // 0.72
    accent: rgb(0xF7, 0x92, 0x0F),
    accent2: rgb(0x3A, 0xD0, 0xE0), // bright cyan, reads against the dark face
    accent3: rgb(0x3A, 0xD0, 0xE0), // TODO: same as accent2 for now — TX/keyed state, dark face
    screen_bg: rgb(0x08, 0x06, 0x04),
    ring: rgba(247, 146, 15, 102), // 0.40
    body: rgb(0xF4, 0xEE, 0xE6),
    dim: rgba(205, 175, 140, 153), // 0.60
    lcd_top: rgb(0x1C, 0x14, 0x07),
    lcd_bottom: rgb(0x0D, 0x0A, 0x04),
    lcd_text: rgb(0xFF, 0xB2, 0x4D),
    on_accent: rgb(0x1D, 0x14, 0x08),
    map_land: rgba(247, 211, 162, 66), // warm tan base; relief texel shades it
    map_coast: rgba(247, 160, 60, 102), // rgba(...,0.40)
    is_dark: true,
};

/// LIGHT — "Daylight Color": a brushed-silver chassis with near-black ink legends,
/// a deep burnt-orange accent, and the "Martian Ink" spectral waterfall. The
/// high-contrast daytime theme; values come from the authoritative handoff in
/// `design_handoff_daylight_color/README.md`.
pub const SILVER: Palette = Palette {
    face_top: rgb(0xF3, 0xF4, 0xF1),
    face_bottom: rgb(0xD9, 0xDB, 0xD5),
    edge: rgb(0x9A, 0x9C, 0x97),
    stripe_light: rgba(255, 255, 255, 153), // 0.60
    stripe_dark: rgba(0, 0, 0, 11),         // 0.045
    legend: rgb(0x19, 0x1C, 0x20),          // near-black ink
    sub: rgba(64, 70, 78, 230),             // 0.90
    accent: rgb(0xB8, 0x53, 0x0A),  // deep burnt orange — brackets, NOW line, ticks
    accent2: rgb(0x0E, 0x6F, 0x7A), // dark cyan — armed state on the silver face
    accent3: rgb(0x9E, 0x1C, 0x22), // dark red — TX/keyed state on the silver face
    screen_bg: rgb(0xF0, 0xEC, 0xE2), // paper
    ring: rgba(120, 90, 40, 102),     // 0.40 — recessed-screen hairline frame
    body: rgb(0x20, 0x24, 0x2A),      // dark ink, in-screen content
    dim: rgba(92, 100, 110, 184),     // 0.72
    lcd_top: rgb(0xE9, 0xE2, 0xD2),
    lcd_bottom: rgb(0xD2, 0xC9, 0xB2),
    lcd_text: rgb(0x2A, 0x20, 0x10),
    on_accent: rgb(0xFD, 0xF6, 0xEC), // near-white text on the burnt-orange fill
    map_land: rgba(60, 52, 32, 26),   // 0.10
    map_coast: rgba(120, 72, 16, 128), // 0.50
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
    recessed_screen_accent(painter, rect, pal, pal.accent);
}

/// Like [`recessed_screen`], but with the corner brackets drawn in an explicit
/// `accent` instead of the palette's amber. Lets the Digital panel tint its
/// frame by operating state (amber idle / accent2 armed / accent3 transmitting).
pub fn recessed_screen_accent(
    painter: &egui::Painter,
    rect: Rect,
    pal: &Palette,
    accent: Color32,
) {
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

    corner_brackets(painter, rect, accent);
}

/// Draw the header focus marker at `left_center` (its left-center point),
/// returning the x where header text should start. A 9px square: hollow with a
/// 1.5px accent stroke when `active` is false, or filled with a faint accent
/// glow when this panel holds keyboard focus. Uses the **accent** (amber), never
/// the cyan/blue transmit color, so focus reads as distinct from live state.
pub fn focus_box(painter: &egui::Painter, left_center: Pos2, pal: &Palette, active: bool) -> f32 {
    let rect = Rect::from_min_size(
        Pos2::new(left_center.x, left_center.y - FOCUS_BOX_SZ * 0.5),
        Vec2::splat(FOCUS_BOX_SZ),
    );
    if active {
        // Soft accent halo: a few expanded translucent rings behind the box.
        for i in 1..=3 {
            painter.rect_stroke(
                rect.expand(i as f32 * 1.5),
                CornerRadius::ZERO,
                Stroke::new(1.0, pal.accent.gamma_multiply(0.16)),
                StrokeKind::Outside,
            );
        }
        painter.rect_filled(rect, CornerRadius::ZERO, pal.accent);
    } else {
        painter.rect_stroke(
            rect,
            CornerRadius::ZERO,
            Stroke::new(FOCUS_BOX_STROKE, pal.accent),
            StrokeKind::Inside,
        );
    }
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
    painter.text(
        pos + Vec2::new(0.0, 1.0),
        anchor,
        text,
        font.clone(),
        shadow,
    );
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
