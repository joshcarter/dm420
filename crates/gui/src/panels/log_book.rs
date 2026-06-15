//! Log Book panel: a 4-row QSO table of the most recent contacts, read live from
//! the bus (`logbook/entries`). No view state.

use eframe::egui;
use egui::{Align2, Pos2, Stroke};

use super::{Panel, PanelCtx};
use crate::chrome::{panel_header, split_block};
use crate::theme::*;

/// Format a UTC millisecond timestamp as the `HHMM` the table shows.
fn hhmm(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|t| t.format("%H%M").to_string())
        .unwrap_or_else(|| "----".into())
}

pub struct LogBook;

impl LogBook {
    pub fn new() -> Self {
        Self
    }
}

impl Panel for LogBook {
    fn title(&self) -> &str {
        "Log Book"
    }

    fn ui(&mut self, ctx: &mut PanelCtx, block: egui::Rect) {
        let painter = ctx.painter;
        let pal = ctx.pal;
        let logs = ctx.bus.recent_logs(4);
        let (header, screen) = split_block(block);
        panel_header(painter, header, pal, "Log Book", "last 4 · FT8");
        painter.text(
            Pos2::new(header.right() - 2.0, header.center().y),
            Align2::RIGHT_CENTER,
            format!("{} QSO", ctx.bus.log_count()),
            heading(9.0),
            pal.legend,
        );
        recessed_screen(painter, screen, pal);

        let l = screen.left() + 12.0;
        let r = screen.right() - 12.0;
        if r <= l {
            return;
        }
        let x_call = l + 50.0;
        let x_grid = r - 48.0 - 48.0 - 60.0;
        let x_snt = r - 48.0; // right edge of Snt column
        let x_rcv = r; // right edge of Rcv column

        let hy = screen.top() + 10.0;
        let dimf = mono(8.0);
        for (text, x, align) in [
            ("UTC", l, Align2::LEFT_CENTER),
            ("CALL", x_call, Align2::LEFT_CENTER),
            ("GRID", x_grid, Align2::LEFT_CENTER),
            ("SNT", x_snt, Align2::RIGHT_CENTER),
            ("RCV", x_rcv, Align2::RIGHT_CENTER),
        ] {
            painter.text(Pos2::new(x, hy), align, tracked(text), dimf.clone(), pal.dim);
        }
        let sep_y = screen.top() + 19.0;
        painter.line_segment(
            [Pos2::new(l, sep_y), Pos2::new(r, sep_y)],
            Stroke::new(1.0, pal.dim.gamma_multiply(0.4)),
        );

        for (i, e) in logs.iter().enumerate() {
            let ry = sep_y + 11.0 + i as f32 * 22.0;
            let grid = e.grid.as_ref().map(|g| g.0.as_str()).unwrap_or("----");
            painter.text(
                Pos2::new(l, ry),
                Align2::LEFT_CENTER,
                hhmm(e.time.0),
                mono(10.0),
                pal.dim,
            );
            painter.text(
                Pos2::new(x_call, ry),
                Align2::LEFT_CENTER,
                tracked(&e.call.0),
                heading(10.0),
                pal.body,
            );
            painter.text(Pos2::new(x_grid, ry), Align2::LEFT_CENTER, grid, mono(10.0), pal.dim);
            painter.text(
                Pos2::new(x_snt, ry),
                Align2::RIGHT_CENTER,
                &e.exchange_sent,
                mono(10.0),
                pal.body,
            );
            painter.text(
                Pos2::new(x_rcv, ry),
                Align2::RIGHT_CENTER,
                &e.exchange_rcvd,
                mono(10.0),
                pal.accent,
            );
            if i + 1 < logs.len() {
                let ly = ry + 11.0;
                painter.line_segment(
                    [Pos2::new(l, ly), Pos2::new(r, ly)],
                    Stroke::new(1.0, pal.dim.gamma_multiply(0.22)),
                );
            }
        }
    }
}
