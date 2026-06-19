//! Shared instrument-panel chrome: chassis textures and the small drawing
//! helpers reused across the top bar and every panel. Lower-level color/font
//! tokens live in `theme`; panel-specific helpers live next to their panel.

use eframe::egui;
use egui::{
    Align2, Color32, Mesh, Pos2, Rect, Shape, Stroke, StrokeKind, TextureHandle, TextureOptions,
    Vec2,
};

use crate::panel_data as pd;
use crate::theme::*;

// ---------------------------------------------------------------------------
// Chassis: brushed-metal texture + gradient face.
// ---------------------------------------------------------------------------

pub fn make_brushed(ctx: &egui::Context, pal: &Palette) -> TextureHandle {
    // One light column, one dark column => 2px stripe period when tiled.
    let img = egui::ColorImage::new([2, 1], vec![pal.stripe_light, pal.stripe_dark]);
    ctx.load_texture("brushed", img, TextureOptions::NEAREST_REPEAT)
}

/// Shaded-relief texture baked from GEBCO; see `tools/gen_relief.py`. Sampled by
/// the land mesh to give the map topographic depth. The hillshade is carried in
/// the ALPHA channel (RGB = white): flat terrain → transparent, mountain shadows
/// → opaque. The land mesh composites a tint through this alpha, so the *same*
/// texture can darken (dark theme) or lighten (light theme) the terrain — a plain
/// grayscale multiplier could only ever darken. Theme-independent — load once.
pub fn make_relief(ctx: &egui::Context) -> TextureHandle {
    let bytes = include_bytes!("../assets/relief.png");
    let gray = image::load_from_memory(bytes)
        .expect("decode relief.png")
        .to_luma8();
    let (w, h) = gray.dimensions();
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for p in gray.pixels() {
        // relief.png stores the multiplier (255 = flat); the shade "deficit"
        // (255 - v) becomes the overlay alpha.
        rgba.extend_from_slice(&[255, 255, 255, 255 - p[0]]);
    }
    let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
    ctx.load_texture("relief", img, TextureOptions::LINEAR)
}

/// Paint the chassis: vertical face gradient, then the translucent brushed
/// stripes tiled over it.
pub fn paint_chassis(painter: &egui::Painter, rect: Rect, pal: &Palette, brushed: &TextureHandle) {
    vertical_gradient(painter, rect, pal.face_top, pal.face_bottom);
    let mut mesh = Mesh::with_texture(brushed.id());
    let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(rect.width() / 2.0, 1.0));
    mesh.add_rect_with_uv(rect, uv, Color32::WHITE);
    painter.add(Shape::mesh(mesh));
}

// ---------------------------------------------------------------------------
// Small chrome helpers
// ---------------------------------------------------------------------------

/// Measure rendered text width (for hand-laying labels).
pub fn measure(painter: &egui::Painter, text: &str, font: egui::FontId) -> f32 {
    painter
        .layout_no_wrap(text.to_owned(), font, Color32::WHITE)
        .size()
        .x
}

pub fn shadow(pal: &Palette) -> Color32 {
    if pal.is_dark {
        Color32::from_rgba_unmultiplied(0, 0, 0, 140)
    } else {
        Color32::from_rgba_unmultiplied(255, 255, 255, 120)
    }
}

pub fn clearc() -> Color32 {
    Color32::from_rgba_unmultiplied(0, 0, 0, 0)
}

/// A recessed LCD surface (clock chips, switch tracks, Scan track): lcd
/// gradient plus a short top inset shadow plus a 1px edge ring. No inset-shadow
/// primitive in egui, so we fake the bevel.
pub fn lcd_panel(painter: &egui::Painter, rect: Rect, pal: &Palette, radius: u8) {
    vertical_gradient(painter, rect, pal.lcd_top, pal.lcd_bottom);
    let sh_h = (rect.height() * 0.5).min(9.0);
    let shade = Rect::from_min_size(rect.min, Vec2::new(rect.width(), sh_h));
    let dark = Color32::from_rgba_unmultiplied(0, 0, 0, if pal.is_dark { 130 } else { 70 });
    vertical_gradient(painter, shade, dark, clearc());
    painter.rect_stroke(
        rect,
        corner_radius(radius),
        Stroke::new(1.0, pal.edge),
        StrokeKind::Inside,
    );
}

/// One segmented-control key: lit accent fill + raised highlight when active,
/// transparent when inactive. Returns the click response.
pub fn key_cell(
    ui: &mut egui::Ui,
    painter: &egui::Painter,
    pal: &Palette,
    rect: Rect,
    label: &str,
    active: bool,
    id: egui::Id,
) -> egui::Response {
    key_cell_accent(ui, painter, pal, rect, label, active, pal.accent, id)
}

/// Like [`key_cell`] but with an explicit fill color for the lit state — used
/// where a key needs a non-primary accent (e.g. the Send button turning cyan
/// when armed). Identical geometry/typography so it matches the Scan key.
#[allow(clippy::too_many_arguments)]
pub fn key_cell_accent(
    ui: &mut egui::Ui,
    painter: &egui::Painter,
    pal: &Palette,
    rect: Rect,
    label: &str,
    active: bool,
    accent: Color32,
    id: egui::Id,
) -> egui::Response {
    if active {
        painter.rect_filled(rect, corner_radius(2), accent);
        // raised: 1px top highlight + 1px bottom shadow.
        let hl = Color32::from_rgba_unmultiplied(255, 255, 255, 72);
        let sh = Color32::from_rgba_unmultiplied(0, 0, 0, 115);
        painter.line_segment(
            [
                Pos2::new(rect.left() + 1.5, rect.top() + 0.75),
                Pos2::new(rect.right() - 1.5, rect.top() + 0.75),
            ],
            Stroke::new(1.0, hl),
        );
        painter.line_segment(
            [
                Pos2::new(rect.left() + 1.5, rect.bottom() - 0.5),
                Pos2::new(rect.right() - 1.5, rect.bottom() - 0.5),
            ],
            Stroke::new(1.0, sh),
        );
    }
    let (font, color) = if active {
        (heading_bold(9.0), pal.on_accent)
    } else {
        (heading(9.0), pal.sub)
    };
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        tracked(label),
        font,
        color,
    );
    ui.interact(rect, id, egui::Sense::click())
}

/// A recessed LCD readout chip — a dim micro-label, a glowing value centered in a
/// fixed-width cell, and an optional dim unit suffix — centered horizontally on
/// `center_x`. The visual language matches the header clocks (see `lcd_clock`);
/// `height`/`value_pt`/`readout_w` let it shrink into a tight panel header.
/// Returns the chip's rect.
#[allow(clippy::too_many_arguments)]
pub fn lcd_readout(
    painter: &egui::Painter,
    pal: &Palette,
    center_x: f32,
    cy: f32,
    height: f32,
    label: &str,
    value: &str,
    unit: &str,
    value_pt: f32,
    readout_w: f32,
) -> Rect {
    const PAD_X: f32 = 10.0;
    const GAP: f32 = 7.0;

    let label_t = tracked(label);
    let label_w = measure(painter, &label_t, mono(8.0));
    let unit_w = if unit.is_empty() {
        0.0
    } else {
        GAP + measure(painter, unit, mono(8.0))
    };
    let chip_w = PAD_X + label_w + GAP + readout_w + unit_w + PAD_X;
    let chip = Rect::from_min_max(
        Pos2::new(center_x - chip_w / 2.0, cy - height / 2.0),
        Pos2::new(center_x + chip_w / 2.0, cy + height / 2.0),
    );
    lcd_panel(painter, chip, pal, 3);

    let dim = pal.lcd_text.gamma_multiply(0.6);
    let lx = chip.left() + PAD_X;
    painter.text(Pos2::new(lx, cy), Align2::LEFT_CENTER, &label_t, mono(8.0), dim);
    let cell = Rect::from_min_max(
        Pos2::new(lx + label_w + GAP, chip.top()),
        Pos2::new(lx + label_w + GAP + readout_w, chip.bottom()),
    );
    // faint glow under the value, then the value itself
    painter.text(
        cell.center(),
        Align2::CENTER_CENTER,
        value,
        heading_bold(value_pt),
        pal.accent.gamma_multiply(0.18),
    );
    painter.text(
        cell.center(),
        Align2::CENTER_CENTER,
        value,
        heading_bold(value_pt),
        pal.lcd_text,
    );
    if !unit.is_empty() {
        // Unit suffix in the same dim micro-label font as the leading label.
        painter.text(Pos2::new(cell.right() + GAP, cy), Align2::LEFT_CENTER, unit, mono(8.0), dim);
    }
    chip
}

/// A segmented switch: a recessed track of key cells with an optional micro-label
/// above it, flush to `right_x` and centered vertically on `track_cy`. The whole
/// track is one click target (hit the lit label too); returns the left edge and a
/// per-cell click flag. Used by the top bar (tall, with a micro-label) and panel
/// headers (compact, no label).
#[allow(clippy::too_many_arguments)]
pub fn segmented(
    ui: &mut egui::Ui,
    painter: &egui::Painter,
    pal: &Palette,
    right_x: f32,
    track_cy: f32,
    track_h: f32,
    micro: &str,
    cells: &[(&str, bool)],
    id_src: &str,
) -> (f32, Vec<bool>) {
    const PAD: f32 = 2.0;
    const GAP: f32 = 2.0;
    const CELL_PAD_X: f32 = 11.0;

    let widths: Vec<f32> = cells
        .iter()
        .map(|(t, _)| measure(painter, &tracked(t), heading(9.0)) + CELL_PAD_X * 2.0)
        .collect();
    let track_w: f32 =
        PAD * 2.0 + widths.iter().sum::<f32>() + GAP * (cells.len() as f32 - 1.0);

    let track = Rect::from_min_max(
        Pos2::new(right_x - track_w, track_cy - track_h / 2.0),
        Pos2::new(right_x, track_cy + track_h / 2.0),
    );
    lcd_panel(painter, track, pal, 4);

    if !micro.is_empty() {
        painter.text(
            Pos2::new(track.left(), track.top() - 3.0),
            Align2::LEFT_BOTTOM,
            tracked(micro),
            mono(7.0),
            pal.sub,
        );
    }

    let cell_h = track_h - PAD * 2.0;
    let mut x = track.left() + PAD;
    for (i, ((label, active), w)) in cells.iter().zip(widths.iter()).enumerate() {
        let cell = Rect::from_min_size(Pos2::new(x, track.top() + PAD), Vec2::new(*w, cell_h));
        // Cells are draw-only; the whole track owns the click (below).
        key_cell(ui, painter, pal, cell, label, *active, ui.id().with((id_src, i)));
        x += w + GAP;
    }

    // The entire switch is one toggle target: a click anywhere on the track flips
    // it, so you can hit the lit label (not just the inactive one) to switch.
    // These are binary switches — a click registers on the currently inactive
    // cell. Interacted after the cells so it sits on top and owns the click; the
    // per-cell key_cell responses are discarded.
    let track_resp = ui.interact(track, ui.id().with((id_src, "track")), egui::Sense::click());
    let clicks = cells
        .iter()
        .map(|(_, active)| track_resp.clicked() && !*active)
        .collect();
    (track.left(), clicks)
}

// ---------------------------------------------------------------------------
// Shared panel chrome: header (spine + legend + sub) + the standard block split.
// ---------------------------------------------------------------------------

/// Draw a panel header (focus marker + uppercase legend + sub-label). `active`
/// fills the focus box when this panel holds keyboard focus.
pub fn panel_header(
    painter: &egui::Painter,
    header: Rect,
    pal: &Palette,
    title: &str,
    sub: &str,
    active: bool,
) {
    let cy = header.center().y;
    let after = focus_box(painter, Pos2::new(header.left(), cy), pal, active);
    let tx = after + 8.0;
    let t = tracked(&title.to_uppercase());
    engraved_text(
        painter,
        Pos2::new(tx, cy),
        &t,
        heading(11.0),
        pal.legend,
        shadow(pal),
        Align2::LEFT_CENTER,
    );
    if !sub.is_empty() {
        let sub_x = tx + measure(painter, &t, heading(11.0)) + 10.0;
        painter.text(
            Pos2::new(sub_x, cy),
            Align2::LEFT_CENTER,
            sub,
            mono(8.5),
            pal.sub,
        );
    }
}

/// Split a panel block into (header row, recessed screen) per the standard
/// 24px header + 6px gap + screen recipe.
pub fn split_block(block: Rect) -> (Rect, Rect) {
    let header = Rect::from_min_max(
        block.min,
        Pos2::new(block.right(), block.top() + pd::HEADER_ROW_H),
    );
    let screen = Rect::from_min_max(
        Pos2::new(block.left(), header.bottom() + pd::HEADER_GAP),
        block.max,
    );
    (header, screen)
}
