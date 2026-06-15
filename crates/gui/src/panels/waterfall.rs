//! Waterfall panel: header (FT8 + tuned-frequency readout) + the live Waterslide
//! FFT sim as the screen body + a decode ticker along the bottom.

use eframe::egui;
use egui::{Align2, Color32, Pos2, Rect};
use types::{Decode, DecodeContent, ParsedMessage};

use super::{Panel, PanelCtx};
use crate::chrome::{measure, panel_header, shadow};
use crate::panel_data as pd;
use crate::theme::*;
use crate::waterslide_panel::{WaterslidePanel, WaterslideTheme};

/// `HHMMSS` for a UTC millisecond timestamp.
fn hhmmss(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|t| t.format("%H%M%S").to_string())
        .unwrap_or_else(|| "------".into())
}

/// SNR like the rest of the console: Unicode minus, two digits.
fn fmt_snr(snr: i8) -> String {
    let sign = if snr < 0 { '−' } else { '+' };
    format!("{sign}{:02}", snr.unsigned_abs())
}

/// The human-readable body of a decode (`CQ EA7KW IM67`, an exchange, etc.).
fn decode_text(d: &Decode) -> String {
    match &d.content {
        DecodeContent::Slotted { message, .. } => match message {
            ParsedMessage::Cq { caller, grid, .. } => match grid {
                Some(g) => format!("CQ {} {}", caller.0, g.0),
                None => format!("CQ {}", caller.0),
            },
            ParsedMessage::Exchange { to, from, .. } => format!("{} {}", to.0, from.0),
            ParsedMessage::Signoff { to, from, .. } => format!("{} {} 73", to.0, from.0),
            ParsedMessage::Free(s) | ParsedMessage::Raw(s) => s.clone(),
        },
        DecodeContent::Streaming { text } => text.clone(),
    }
}

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
        let vfo_hz = ctx.bus.rig_state().map(|r| r.vfo.0).unwrap_or(14_074_000);
        let vfo_mhz = format!("{:.3}", vfo_hz as f64 / 1_000_000.0);
        engraved_text(
            painter,
            Pos2::new(rx, cy),
            &vfo_mhz,
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

        draw_ticker(painter, ticker, pal, &ctx.bus.recent_decodes(4));
    }
}

/// The bottom rail: most recent decode shown prominently (time · snr · message),
/// then older ones trailing off dimmer. Empty until the first decode lands.
fn draw_ticker(painter: &egui::Painter, rect: Rect, pal: &Palette, decodes: &[Decode]) {
    let cy = rect.center().y;
    let painter = painter.with_clip_rect(rect);
    let mut x = rect.left();

    let draw = |x: &mut f32, text: &str, color: Color32, font: egui::FontId| {
        let w = measure(&painter, text, font.clone());
        painter.text(Pos2::new(*x, cy), Align2::LEFT_CENTER, text, font, color);
        *x += w;
    };

    for (i, d) in decodes.iter().enumerate() {
        let lead = if i == 0 { pal.legend } else { pal.sub };
        let snr = d.snr_db.map(fmt_snr).unwrap_or_else(|| "   ".into());
        if i > 0 {
            draw(&mut x, "  ·  ", pal.sub, mono(9.0));
        }
        draw(&mut x, &hhmmss(d.t.0), pal.sub, mono(9.0));
        draw(&mut x, &format!("  {snr}  "), pal.accent, heading_bold(9.0));
        draw(&mut x, &decode_text(d), lead, mono(9.0));
    }
}
