//! Waterfall panel: header (FT8 + tuned-frequency readout) + the live Waterslide
//! FFT sim as the screen body + a decode ticker along the bottom.

use eframe::egui;
use egui::{Align2, Color32, Pos2, Rect};

use super::{Panel, PanelCtx};
use crate::chrome::{measure, panel_header, shadow};
use crate::panel_data as pd;
use crate::theme::*;
use crate::waterslide_panel::{WaterslidePanel, WaterslideTheme};

pub struct Waterfall {
    slide: WaterslidePanel,
}

impl Waterfall {
    pub fn new() -> Self {
        Self {
            slide: WaterslidePanel::new(7200.0),
        }
    }
}

impl Panel for Waterfall {
    fn title(&self) -> &str {
        "Waterfall"
    }

    fn ui(&mut self, ctx: &mut PanelCtx, block: Rect) {
        let painter = ctx.painter;
        let pal = ctx.pal;

        let header = Rect::from_min_max(
            block.min,
            Pos2::new(block.right(), block.top() + pd::HEADER_ROW_H),
        );
        panel_header(painter, header, pal, "FT8", "0–3000 Hz · time → left");
        // right side: prominent tuned-frequency readout
        let cy = header.center().y;
        let mut rx = header.right() - 2.0;
        painter.text(Pos2::new(rx, cy), Align2::RIGHT_CENTER, "MHz", mono(8.5), pal.sub);
        rx -= measure(painter, "MHz", mono(8.5)) + 5.0;
        engraved_text(
            painter,
            Pos2::new(rx, cy),
            "14.074",
            heading_bold(15.0),
            pal.accent,
            shadow(pal),
            Align2::RIGHT_CENTER,
        );

        // ticker (bottom) + screen (fills between header and ticker).
        let ticker = Rect::from_min_max(
            Pos2::new(block.left(), block.bottom() - pd::TICKER_H),
            block.max,
        );
        let screen = Rect::from_min_max(
            Pos2::new(block.left(), header.bottom() + pd::HEADER_GAP),
            Pos2::new(block.right(), ticker.top() - pd::GAP),
        );
        recessed_screen(painter, screen, pal);

        // Live Waterslide simulation as the screen body (inset to keep brackets).
        if screen.width() > 24.0 && screen.height() > 24.0 {
            let body = screen.shrink(8.0);
            let theme = WaterslideTheme::from_palette(pal);
            let mut child = ctx.ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(body)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            child.set_clip_rect(screen.shrink(2.0));
            self.slide.ui(&mut child, body, ctx.dt, &theme);
        }

        draw_ticker(painter, ticker, pal);
    }
}

fn draw_ticker(painter: &egui::Painter, rect: Rect, pal: &Palette) {
    let cy = rect.center().y;
    let segments: [(&str, Color32, egui::FontId); 4] = [
        ("235945", pal.sub, mono(9.0)),
        ("  −02  ", pal.accent, heading_bold(9.0)),
        ("K1ABC W9XYZ EM48", pal.legend, mono(9.0)),
        ("  ·  235945 −17 CQ EA7KW IM67", pal.sub, mono(9.0)),
    ];
    let painter = painter.with_clip_rect(rect);
    let mut x = rect.left();
    for (text, color, font) in segments {
        let w = measure(&painter, text, font.clone());
        painter.text(Pos2::new(x, cy), Align2::LEFT_CENTER, text, font, color);
        x += w;
    }
}
