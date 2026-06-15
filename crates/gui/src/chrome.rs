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

/// Shaded-relief texture (grayscale multiplier) baked from GEBCO; see
/// `tools/gen_relief.py`. Sampled by the land mesh to give the map topographic
/// depth. Theme-independent — load once.
pub fn make_relief(ctx: &egui::Context) -> TextureHandle {
    let bytes = include_bytes!("../assets/relief.png");
    let gray = image::load_from_memory(bytes)
        .expect("decode relief.png")
        .to_luma8();
    let (w, h) = gray.dimensions();
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for p in gray.pixels() {
        let v = p[0];
        rgba.extend_from_slice(&[v, v, v, 255]);
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

/// A recessed LCD surface (clock chips, switch tracks, Scan track): lcd gradient
/// + a short top inset shadow + a 1px edge ring. No inset-shadow primitive in
/// egui, so we fake the bevel.
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
    if active {
        painter.rect_filled(rect, corner_radius(2), pal.accent);
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
        &tracked(label),
        font,
        color,
    );
    ui.interact(rect, id, egui::Sense::click())
}

// ---------------------------------------------------------------------------
// Shared panel chrome: header (spine + legend + sub) + the standard block split.
// ---------------------------------------------------------------------------

/// Draw a panel header (spine + uppercase legend + sub-label).
pub fn panel_header(painter: &egui::Painter, header: Rect, pal: &Palette, title: &str, sub: &str) {
    let cy = header.center().y;
    let after = spine(painter, Pos2::new(header.left(), cy), pal.accent);
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
