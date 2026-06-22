//! Band Scan panel: two-column band blocks + a Scan/Cancel lit key.
//!
//! Fully bus-driven now that the real scanner exists (`core::scan`): the
//! Scan/Cancel key issues `ScannerCommand::StartSurvey`/`Cancel`, and the run
//! status, the highlighted (currently-dwelling) band, and each band's heard/unworked
//! counts all come live from `scanner/state` + `scanner/candidates`. The four bands
//! (40/20/15/10) are a fixed layout per the spec; counts read 0 until a scan has
//! visited them.

use std::time::{SystemTime, UNIX_EPOCH};

use eframe::egui;
use egui::{Align2, CornerRadius, Pos2, Rect, Stroke};
use types::{Band, BandActivity, ScanStatus};

use super::{Panel, PanelCtx};
use crate::chrome::{key_cell, lcd_panel, measure, panel_header, split_block};
use crate::theme::*;

/// The fixed band layout (spec: 40/20 left column, 15/10 right column) — also the
/// band set handed to the scanner on Scan.
const SCAN_BANDS: [Band; 4] = [Band::B40m, Band::B20m, Band::B15m, Band::B10m];

/// Slots dwelled per band/mode. The scanner clamps to ≥2 (even/odd parity); we ask
/// for exactly two.
const DWELL_SLOTS: u8 = 2;

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

/// "just now" / "N min ago" for the last-scan timestamp (epoch ms).
fn ago(then_ms: i64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(then_ms);
    let mins = (now - then_ms).max(0) / 60_000;
    if mins == 0 {
        "just now".to_string()
    } else {
        format!("{mins} min ago")
    }
}

pub struct BandScan;

impl BandScan {
    pub fn new() -> Self {
        Self
    }
}

impl Panel for BandScan {
    fn title(&self) -> &str {
        "Band Scan"
    }

    fn ui(&mut self, ctx: &mut PanelCtx, block: Rect) {
        // Live scanner run state and per-band counts from the bus.
        let state = ctx.bus.scanner();
        let scanning = state
            .as_ref()
            .map(|s| s.status == ScanStatus::Scanning)
            .unwrap_or(false);
        let current = state.as_ref().and_then(|s| s.current);
        let last_scan = state.as_ref().and_then(|s| s.last_scan);
        let activity = ctx.bus.band_activity();

        let pal = ctx.pal;
        let painter = ctx.painter;
        let (header, screen) = split_block(block);

        let status = if scanning {
            let lbl = current.map(band_label).unwrap_or("");
            format!("Scanning {lbl} …")
        } else {
            match last_scan {
                Some(ts) => format!("Last scan: {}", ago(ts.0)),
                None => "Idle".to_string(),
            }
        };
        panel_header(painter, header, pal, "Band Scan", &status, ctx.active);

        // Scan / Cancel button (lit accent key in a recessed track), header-right.
        let label = if scanning { "CANCEL" } else { "SCAN" };
        let cy = header.center().y;
        let cell_w = measure(painter, &tracked(label), heading_bold(9.0)) + 22.0;
        let track_w = cell_w + 4.0;
        let track = Rect::from_min_max(
            Pos2::new(header.right() - track_w, cy - 11.0),
            Pos2::new(header.right(), cy + 11.0),
        );
        lcd_panel(painter, track, pal, 4);
        let cell = Rect::from_min_max(
            Pos2::new(track.left() + 2.0, track.top() + 2.0),
            Pos2::new(track.right() - 2.0, track.bottom() - 2.0),
        );
        let resp = key_cell(
            ctx.ui,
            painter,
            pal,
            cell,
            label,
            true,
            ctx.ui.id().with("scan_btn"),
        );
        if resp.clicked() {
            if scanning {
                ctx.bus.cancel_scan();
            } else {
                ctx.bus.start_scan(SCAN_BANDS.to_vec(), DWELL_SLOTS);
            }
        }

        recessed_screen(painter, screen, pal);

        // Two columns split by a 1px divider; left = [40m,20m], right = [15m,10m].
        let mid = screen.center().x;
        painter.line_segment(
            [
                Pos2::new(mid, screen.top() + 8.0),
                Pos2::new(mid, screen.bottom() - 8.0),
            ],
            Stroke::new(1.0, pal.dim.gamma_multiply(0.4)),
        );
        let left_half = Rect::from_min_max(screen.min, Pos2::new(mid, screen.bottom()));
        let right_half = Rect::from_min_max(Pos2::new(mid, screen.top()), screen.max);
        draw_column(painter, left_half, pal, &activity, current, &[SCAN_BANDS[0], SCAN_BANDS[1]]);
        draw_column(painter, right_half, pal, &activity, current, &[SCAN_BANDS[2], SCAN_BANDS[3]]);
    }
}

/// Draw one column's two band blocks. Each shows the band as a large label with its
/// heard / unworked counts (looked up from `activity`, 0 if not yet scanned); the
/// block highlights when it is the band the scanner is currently dwelling on.
fn draw_column(
    painter: &egui::Painter,
    half: Rect,
    pal: &Palette,
    activity: &[BandActivity],
    current: Option<Band>,
    bands: &[Band; 2],
) {
    const BLOCK_H: f32 = 30.0;
    const BLOCK_GAP: f32 = 7.0;
    let total = BLOCK_H * 2.0 + BLOCK_GAP;
    let top = half.center().y - total / 2.0;
    let content_left = half.left() + 12.0;

    for (slot, &band) in bands.iter().enumerate() {
        let act = activity.iter().find(|a| a.band == band);
        let heard = act.map_or(0, |a| a.stations_seen);
        let unworked = act.map_or(0, |a| a.unworked);
        let active = current == Some(band);
        let by = top + slot as f32 * (BLOCK_H + BLOCK_GAP);
        let bcy = by + BLOCK_H / 2.0;

        if active {
            painter.rect_filled(
                Rect::from_min_max(
                    Pos2::new(content_left, by),
                    Pos2::new(content_left + 2.0, by + BLOCK_H),
                ),
                CornerRadius::ZERO,
                pal.accent,
            );
        }
        let num_x = content_left + 10.0;
        let num_color = if active { pal.accent } else { pal.sub };
        painter.text(
            Pos2::new(num_x, bcy),
            Align2::LEFT_CENTER,
            band_label(band),
            heading_bold(22.0),
            num_color,
        );

        let text_x = num_x + 40.0 + 9.0;
        let n1 = format!("{heard}");
        let w1 = painter
            .text(
                Pos2::new(text_x, bcy - 7.0),
                Align2::LEFT_CENTER,
                &n1,
                mono(11.0),
                pal.legend,
            )
            .width();
        painter.text(
            Pos2::new(text_x + w1 + 3.0, bcy - 7.0),
            Align2::LEFT_CENTER,
            "heard",
            mono(11.0),
            pal.dim,
        );
        let n2 = format!("{unworked}");
        let w2 = painter
            .text(
                Pos2::new(text_x, bcy + 7.0),
                Align2::LEFT_CENTER,
                &n2,
                mono(11.0),
                pal.accent,
            )
            .width();
        painter.text(
            Pos2::new(text_x + w2 + 3.0, bcy + 7.0),
            Align2::LEFT_CENTER,
            "unworked",
            mono(11.0),
            pal.dim,
        );
    }
}
