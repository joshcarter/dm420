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

use std::collections::HashSet;

use super::{Panel, PanelCtx};
use crate::chrome::{panel_header, split_block};
use crate::format::{band_label, mode_label};
use crate::theme::*;

/// At most this many bands fit. When more are configured, the six shortest-wavelength
/// (highest-frequency) ones win and the rest are logged once as hidden.
const MAX_BANDS: usize = 6;
/// The two over-air modes shown per band, top to bottom.
const MODES: [OverAirMode; 2] = [OverAirMode::Ft8, OverAirMode::Ft4];
/// Target height of one band cell (an FT8 + FT4 line pair). Cells cap at this in the
/// panel and the pinned pane height is sized from it, so the rows fill with no slack.
const ROW_H: f32 = 48.0;

/// The grid shape `n` bands lay out into: a single column for ≤3 bands, two columns
/// above that, with `rows = ceil(n / cols)`. The single source for the layout shape,
/// shared by the panel's drawing and the pane-height pin so they always agree.
fn grid_shape(n: usize) -> (usize, usize) {
    let cols = if n > 3 { 2 } else { 1 };
    (cols, n.div_ceil(cols).max(1))
}

/// The pane height that exactly fits `n` bands: the header + gap + one [`ROW_H`] cell
/// per grid row. The Band Status pane is pinned to this each frame
/// ([`crate::pin_band_height`]) so it grows and shrinks with the active-band count
/// rather than sitting at a fixed height. `n` is clamped to [`MAX_BANDS`] (the panel
/// shows at most that many) and to ≥1 (one header+row even with nothing yet).
pub(crate) fn pane_height(n: usize) -> f32 {
    let (_, rows) = grid_shape(n.clamp(1, MAX_BANDS));
    crate::panel_data::HEADER_ROW_H + crate::panel_data::HEADER_GAP + rows as f32 * ROW_H + 2.0
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
        // The producer tracks the full HF universe; narrow to the operator's active
        // bands here so a re-locked selection takes effect live (no restart).
        let active: HashSet<Band> = ctx.bus.active_bands().into_iter().collect();
        let rows: Vec<BandStatusRow> = ctx
            .bus
            .band_status()
            .map(|s| s.rows)
            .unwrap_or_default()
            .into_iter()
            .filter(|r| active.contains(&r.band))
            .collect();
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

        // Lay the bands out in up to two columns, top to bottom. The same shape the
        // pinned pane height ([`pane_height`]) is sized from, so the rows fit exactly.
        let (cols, per_col) = grid_shape(bands.len());
        let col_w = screen.width() / cols as f32;
        let cell_h = (screen.height() / per_col as f32).min(ROW_H);
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
