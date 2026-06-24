//! Band Scan panel: two-column band blocks, each with FT8/FT4 toggles + per-mode
//! counts, plus a Scan/Cancel lit key.
//!
//! Bus-driven (real scanner, `core::scan`): the Scan/Cancel key issues
//! `ScannerCommand::StartSurvey`/`Cancel`, and the run status, elapsed time, the
//! currently-dwelling band, and each band's per-mode counts come from
//! `scanner/state` + `scanner/candidates`. Each band carries two toggles (FT8, FT4)
//! to its right so the operator can skip bands/modes; only enabled `(band, mode)`
//! stops are scanned. Counts are cumulative over the scan and split into **heard** /
//! **cq** (cq ⊆ heard) / **unworked**, per band and mode.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use eframe::egui;
use egui::{Align2, CornerRadius, Pos2, Rect, Stroke};
use types::{Band, BandActivity, OverAirMode, ScanStatus};

use super::{Panel, PanelCtx};
use crate::chrome::{key_cell, lcd_panel, measure, panel_header, split_block};
use crate::format::mode_label;
use crate::theme::*;

/// The fixed band layout (spec: 40/20 left column, 15/10 right column).
const SCAN_BANDS: [Band; 4] = [Band::B40m, Band::B20m, Band::B15m, Band::B10m];
/// The two mode toggles per band (top FT8, bottom FT4).
const SCAN_MODES: [OverAirMode; 2] = [OverAirMode::Ft8, OverAirMode::Ft4];
/// Slots dwelled per stop. The scanner clamps to ≥2 (even/odd parity).
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

/// Elapsed `mm:ss`.
fn fmt_elapsed(d: Duration) -> String {
    let s = d.as_secs();
    format!("{:02}:{:02}", s / 60, s % 60)
}

pub struct BandScan {
    /// Per-band, per-mode enable toggles (`[band][mode]`), default all on. Only
    /// enabled `(band, mode)` stops are sent to the scanner.
    enabled: [[bool; 2]; 4],
    /// When the current scan started (panel-local, for the elapsed clock).
    scan_start: Option<Instant>,
}

impl BandScan {
    pub fn new() -> Self {
        Self {
            enabled: [[true; 2]; 4],
            scan_start: None,
        }
    }
}

impl Panel for BandScan {
    fn title(&self) -> &str {
        "Band Scan"
    }

    fn ui(&mut self, ctx: &mut PanelCtx, block: Rect) {
        let state = ctx.bus.scanner();
        let scanning = state
            .as_ref()
            .map(|s| s.status == ScanStatus::Scanning)
            .unwrap_or(false);
        let current = state.as_ref().and_then(|s| s.current);
        let current_mode = state.as_ref().and_then(|s| s.current_mode);
        let last_scan = state.as_ref().and_then(|s| s.last_scan);
        let activity = ctx.bus.band_activity();

        // Panel-local elapsed clock: start it when scanning begins, clear when it ends.
        if scanning {
            if self.scan_start.is_none() {
                self.scan_start = Some(Instant::now());
            }
            ctx.ui.ctx().request_repaint_after(Duration::from_millis(500));
        } else {
            self.scan_start = None;
        }

        let pal = ctx.pal;
        let painter = ctx.painter;
        let (header, screen) = split_block(block);

        let status = if scanning {
            let lbl = current.map(band_label).unwrap_or("");
            let elapsed = self
                .scan_start
                .map(|s| fmt_elapsed(s.elapsed()))
                .unwrap_or_default();
            format!("Scanning {lbl} … {elapsed}")
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
        if key_cell(ctx.ui, painter, pal, cell, label, true, ctx.ui.id().with("scan_btn")).clicked() {
            if scanning {
                ctx.bus.cancel_scan();
            } else {
                let stops = self.selected_stops();
                if !stops.is_empty() {
                    ctx.bus.start_scan(stops, DWELL_SLOTS);
                }
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
        let left = Rect::from_min_max(screen.min, Pos2::new(mid, screen.bottom()));
        let right = Rect::from_min_max(Pos2::new(mid, screen.top()), screen.max);
        self.draw_column(ctx, left, current, current_mode, scanning, &activity, [0, 1]);
        self.draw_column(ctx, right, current, current_mode, scanning, &activity, [2, 3]);
    }
}

impl BandScan {
    /// The enabled `(band, mode)` stops, from the toggles.
    fn selected_stops(&self) -> Vec<(Band, OverAirMode)> {
        let mut stops = Vec::new();
        for (bi, &band) in SCAN_BANDS.iter().enumerate() {
            for (mi, &mode) in SCAN_MODES.iter().enumerate() {
                if self.enabled[bi][mi] {
                    stops.push((band, mode));
                }
            }
        }
        stops
    }

    /// Draw one column's two band blocks: the band label, its two FT8/FT4 toggles to
    /// the right, and a per-mode `heard / cq / unworked` line beside each toggle. The
    /// currently-dwelling band is accented. `idxs` are indices into [`SCAN_BANDS`].
    #[allow(clippy::too_many_arguments)]
    fn draw_column(
        &mut self,
        ctx: &mut PanelCtx,
        half: Rect,
        current: Option<Band>,
        current_mode: Option<OverAirMode>,
        scanning: bool,
        activity: &[BandActivity],
        idxs: [usize; 2],
    ) {
        const BLOCK_H: f32 = 36.0;
        const BLOCK_GAP: f32 = 10.0;
        let pal = ctx.pal;
        let painter = ctx.painter;
        let total = BLOCK_H * 2.0 + BLOCK_GAP;
        let top = half.center().y - total / 2.0;
        let content_left = half.left() + 14.0;
        let label_x = content_left + 8.0;
        let tog_x = content_left + 58.0;
        let tog_w = 34.0;
        let tog_h = 15.0;
        let counts_x = tog_x + tog_w + 10.0;

        for (slot, &bi) in idxs.iter().enumerate() {
            let band = SCAN_BANDS[bi];
            let active = current == Some(band);
            let by = top + slot as f32 * (BLOCK_H + BLOCK_GAP);

            // Accent bar + accent label for the band currently being dwelled on.
            if active {
                painter.rect_filled(
                    Rect::from_min_max(
                        Pos2::new(content_left - 2.0, by),
                        Pos2::new(content_left, by + BLOCK_H),
                    ),
                    CornerRadius::ZERO,
                    pal.accent,
                );
            }
            painter.text(
                Pos2::new(label_x, by + BLOCK_H / 2.0),
                Align2::LEFT_CENTER,
                band_label(band),
                heading_bold(20.0),
                if active { pal.accent } else { pal.sub },
            );

            // Two mode rows: toggle on the left, its counts to the right.
            for (mi, &mode) in SCAN_MODES.iter().enumerate() {
                let line_cy = by + (mi as f32 + 0.5) * (BLOCK_H / 2.0);
                let on = self.enabled[bi][mi];

                let tog = Rect::from_min_max(
                    Pos2::new(tog_x, line_cy - tog_h / 2.0),
                    Pos2::new(tog_x + tog_w, line_cy + tog_h / 2.0),
                );
                let id = ctx.ui.id().with(("scan_tog", bi, mi));
                if key_cell(ctx.ui, painter, pal, tog, mode_label(mode), on, id).clicked() {
                    self.enabled[bi][mi] = !on;
                    // Apply the change live to a running sweep (no count reset); when
                    // idle it just takes effect on the next Scan.
                    if scanning {
                        ctx.bus.set_stops(self.selected_stops());
                    }
                }

                let (text, color) = if on {
                    let act = activity.iter().find(|a| a.band == band && a.mode == mode);
                    let heard = act.map_or(0, |a| a.heard);
                    let cq = act.map_or(0, |a| a.cq);
                    let unworked = act.map_or(0, |a| a.unworked);
                    // The stop being dwelled right now reads bright white; the current
                    // band's other mode is dimmed, so it's clear which we're scanning.
                    let c = if active && current_mode == Some(mode) {
                        pal.body
                    } else if active {
                        pal.dim
                    } else {
                        pal.sub
                    };
                    (format!("{heard} heard · {cq} cq · {unworked} unwkd"), c)
                } else {
                    ("— off".to_string(), pal.dim)
                };
                painter.text(
                    Pos2::new(counts_x, line_cy),
                    Align2::LEFT_CENTER,
                    &text,
                    mono(11.0),
                    color,
                );
            }
        }
    }
}
