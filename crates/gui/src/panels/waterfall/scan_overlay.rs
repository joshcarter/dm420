//! The SCANNING-mode overlay for the Digital panel: a hatched border around the
//! inner screen with the word SCANNING interrupting the top and bottom edges. Drawn
//! while the band scanner is sweeping — the panel still shows the live waterslide +
//! decodes, but is non-interactive (see `Panel::ui`), and this frames it so the
//! operator can't mistake "scanning, hands off" for normal operation.

use eframe::egui;
use egui::{Color32, FontId, Rect};

use super::render::{draw_hatch_v, draw_tx_hatch_row};

/// Thickness (px) of the hatch strips. ~50% beefier than the ARMED/TRANSMITTING
/// rows so the "scanning, hands off" frame is unmistakable.
const STRIP_H: f32 = 9.0;

/// How far the border is inset from the screen edge, so the hatch stands clear of
/// the recessed-screen frame instead of overdrawing it — making it read as a
/// distinct band rather than fringe on the bezel.
const INSET: f32 = 6.0;

/// Draw the scanning border just inside `rect`: the word SCANNING centered on the
/// top and bottom edges with hatch on either side (reusing the TX-row helper), and
/// vertical hatch strips down the left and right edges between them.
pub(super) fn draw_scan_border(painter: &egui::Painter, rect: Rect, font: FontId, color: Color32) {
    let rect = rect.shrink(INSET);
    let top_y = rect.top() + STRIP_H * 0.5;
    let bot_y = rect.bottom() - STRIP_H * 0.5;
    draw_tx_hatch_row(
        painter,
        rect.left(),
        rect.right(),
        top_y,
        "SCANNING",
        font.clone(),
        color,
        STRIP_H,
    );
    draw_tx_hatch_row(
        painter,
        rect.left(),
        rect.right(),
        bot_y,
        "SCANNING",
        font,
        color,
        STRIP_H,
    );
    // Left + right strips run between the two rows so they don't overdraw the labels.
    let left = Rect::from_min_max(
        egui::pos2(rect.left(), top_y),
        egui::pos2(rect.left() + STRIP_H, bot_y),
    );
    let right = Rect::from_min_max(
        egui::pos2(rect.right() - STRIP_H, top_y),
        egui::pos2(rect.right(), bot_y),
    );
    draw_hatch_v(painter, left, color);
    draw_hatch_v(painter, right, color);
}
