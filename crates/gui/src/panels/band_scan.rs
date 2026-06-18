//! Band Scan panel: two-column band blocks + a Scan/Cancel lit key. The band list
//! and its heard/unworked counts come live from the bus (`scanner/candidates`);
//! the scan-sweep animation is panel-owned view state that self-advances each
//! frame from the frame delta.

use eframe::egui;
use egui::{Align2, CornerRadius, Pos2, Rect, Stroke};
use types::{Band, BandActivity};

use super::{Panel, PanelCtx};
use crate::chrome::{key_cell, lcd_panel, measure, panel_header, split_block};
use crate::theme::*;

const SCAN_DWELL: f32 = 2.5; // seconds per band

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

pub struct BandScan {
    running: bool,
    active_band: usize,
    accum: f32,    // seconds into the current band dwell
    last_min: u32, // "Last scan: N min ago" (0 == just now)
}

impl BandScan {
    pub fn new() -> Self {
        Self {
            running: false,
            active_band: 0,
            accum: 0.0,
            last_min: 4,
        }
    }

    /// Advance the scan clock, stepping bands every `SCAN_DWELL` and stopping
    /// after the last band. `n` is the live band count from the bus.
    fn tick(&mut self, dt: f32, n: usize) {
        if !self.running || n == 0 {
            return;
        }
        self.accum += dt;
        while self.accum >= SCAN_DWELL {
            self.accum -= SCAN_DWELL;
            self.active_band += 1;
            if self.active_band >= n {
                self.running = false;
                self.active_band = 0;
                self.accum = 0.0;
                self.last_min = 0;
                break;
            }
        }
    }
}

impl Panel for BandScan {
    fn title(&self) -> &str {
        "Band Scan"
    }

    fn ui(&mut self, ctx: &mut PanelCtx, block: Rect) {
        let bands = ctx.bus.band_activity();
        self.tick(ctx.dt as f32, bands.len());

        let pal = ctx.pal;
        let painter = ctx.painter;
        let (header, screen) = split_block(block);

        let active_label = bands
            .get(self.active_band)
            .map(|b| band_label(b.band))
            .unwrap_or("");
        let status = if self.running {
            format!("Scanning {active_label} …")
        } else if self.last_min == 0 {
            "Last scan: just now".to_string()
        } else {
            format!("Last scan: {} min ago", self.last_min)
        };
        panel_header(painter, header, pal, "Band Scan", &status, ctx.active);

        // Scan / Cancel button (lit accent key in a recessed track), header-right.
        let label = if self.running { "CANCEL" } else { "SCAN" };
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
            if self.running {
                self.running = false;
                self.accum = 0.0;
            } else {
                self.running = true;
                self.active_band = 0;
                self.accum = 0.0;
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
        self.draw_column(painter, left_half, pal, &bands, &[0, 1]);
        self.draw_column(painter, right_half, pal, &bands, &[2, 3]);
    }
}

impl BandScan {
    fn draw_column(
        &self,
        painter: &egui::Painter,
        half: Rect,
        pal: &Palette,
        bands: &[BandActivity],
        idxs: &[usize; 2],
    ) {
        const BLOCK_H: f32 = 30.0;
        const BLOCK_GAP: f32 = 7.0;
        let total = BLOCK_H * 2.0 + BLOCK_GAP;
        let top = half.center().y - total / 2.0;
        let content_left = half.left() + 12.0;

        for (slot, &bi) in idxs.iter().enumerate() {
            let Some(activity) = bands.get(bi) else {
                continue;
            };
            let band = band_label(activity.band);
            let heard = activity.stations_seen;
            let unworked = activity.unworked;
            let active = self.running && self.active_band == bi;
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
                band,
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
}
