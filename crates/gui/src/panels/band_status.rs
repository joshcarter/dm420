//! Band Status panel — a read-only, always-on view of who's active on the
//! configured bands/modes: **heard / calling CQ / unworked** counts per
//! `(band, mode)`, aggregated by the `core::band_status` producer from every decode
//! source (local RX, the band scan, and — once the beacon ships them — peers). No
//! controls: scanning is operated from the Digital panel; this panel only displays.
//!
//! Layout (per the spec): up to six bands; the mode toggles are now plain `FT8`/`FT4`
//! labels and each mode line is the compact triple `heard · cq · unworked`, with the
//! header line doubling as the legend. A `(band, mode)` with no configured stop
//! (e.g. 160 m FT4, which has no calling frequency) reads as a dash.

use eframe::egui;
use egui::{Align2, Pos2, Rect};
use types::{Band, BandStatusRow, OverAirMode, calling_freq};

use super::{Panel, PanelCtx};
use crate::chrome::{panel_header, split_block};
use crate::format::mode_label;
use crate::theme::*;

/// At most this many bands fit. When more are configured, the six shortest-wavelength
/// (highest-frequency) ones win and the rest are logged once as hidden.
const MAX_BANDS: usize = 6;
/// The two over-air modes shown per band, top to bottom.
const MODES: [OverAirMode; 2] = [OverAirMode::Ft8, OverAirMode::Ft4];

/// Short display label for a band, e.g. `"40m"`.
fn band_label(b: Band) -> &'static str {
    match b {
        Band::B160m => "160m",
        Band::B80m => "80m",
        Band::B40m => "40m",
        Band::B30m => "30m",
        Band::B20m => "20m",
        Band::B17m => "17m",
        Band::B15m => "15m",
        Band::B12m => "12m",
        Band::B10m => "10m",
        Band::B6m => "6m",
    }
}

pub struct BandStatusPanel {
    /// One-shot guard so the ">6 bands configured" warning is logged once, not every
    /// frame.
    warned: bool,
}

impl BandStatusPanel {
    pub fn new() -> Self {
        Self { warned: false }
    }

    /// Draw one band's block: the band label on the left, then an `FT8`/`FT4` line
    /// each carrying the `heard · cq · unworked` triple (or a dash when that
    /// `(band, mode)` isn't a configured stop).
    fn draw_band(painter: &egui::Painter, pal: &Palette, cell: Rect, band: Band, rows: &[BandStatusRow]) {
        painter.text(
            Pos2::new(cell.left() + 6.0, cell.center().y),
            Align2::LEFT_CENTER,
            band_label(band),
            heading_bold(18.0),
            pal.body,
        );
        let mode_x = cell.left() + 58.0;
        let data_x = mode_x + 34.0;
        for (i, &mode) in MODES.iter().enumerate() {
            let cy = cell.top() + (i as f32 + 0.5) * (cell.height() * 0.5);
            painter.text(
                Pos2::new(mode_x, cy),
                Align2::LEFT_CENTER,
                mode_label(mode),
                mono(11.0),
                pal.sub,
            );
            let (text, color) = match rows.iter().find(|r| r.band == band && r.mode == mode) {
                Some(r) => (format!("{} · {} · {}", r.heard, r.cq, r.unworked), pal.body),
                None => ("—".to_string(), pal.dim),
            };
            painter.text(Pos2::new(data_x, cy), Align2::LEFT_CENTER, &text, mono(12.0), color);
        }
    }
}

impl Panel for BandStatusPanel {
    fn title(&self) -> &str {
        "Band Status"
    }

    fn takes_keyboard_focus(&self) -> bool {
        false
    }

    fn ui(&mut self, ctx: &mut PanelCtx, block: Rect) {
        let rows = ctx.bus.band_status().map(|s| s.rows).unwrap_or_default();
        let pal = ctx.pal;
        let painter = ctx.painter;
        let (header, screen) = split_block(block);
        // The header line doubles as the legend for the `X · Y · Z` triples below.
        panel_header(
            painter,
            header,
            pal,
            "Band Status",
            "heard · calling CQ · unworked",
            ctx.active,
        );
        recessed_screen(painter, screen, pal);

        // Distinct configured bands, ascending by frequency. When more than MAX_BANDS
        // are configured, keep the shortest-wavelength (highest-frequency) ones and
        // log the longer-wavelength remainder once.
        let mut bands: Vec<Band> = Vec::new();
        for r in &rows {
            if !bands.contains(&r.band) {
                bands.push(r.band);
            }
        }
        bands.sort_by_key(|b| calling_freq(*b, OverAirMode::Ft8).map_or(0, |f| f.0));
        if bands.len() > MAX_BANDS {
            let drop = bands.len() - MAX_BANDS;
            if !self.warned {
                self.warned = true;
                let hidden: Vec<&str> = bands[..drop].iter().map(|b| band_label(*b)).collect();
                tracing::warn!(
                    "Band Status shows at most {MAX_BANDS} bands; hiding {} (scanned but not displayed)",
                    hidden.join(", ")
                );
            }
            bands.drain(..drop);
        }
        if bands.is_empty() {
            return;
        }

        // Lay the bands out in up to two columns, top to bottom.
        let cols = if bands.len() > 3 { 2 } else { 1 };
        let per_col = bands.len().div_ceil(cols);
        let col_w = screen.width() / cols as f32;
        let cell_h = (screen.height() / per_col as f32).min(48.0);
        let y0 = screen.top() + (screen.height() - cell_h * per_col as f32).max(0.0) * 0.5;
        for (i, &band) in bands.iter().enumerate() {
            let col = i / per_col;
            let row = i % per_col;
            let cell = Rect::from_min_size(
                Pos2::new(screen.left() + col as f32 * col_w + 8.0, y0 + row as f32 * cell_h),
                egui::vec2(col_w - 16.0, cell_h),
            );
            Self::draw_band(painter, pal, cell, band, &rows);
        }
    }
}
