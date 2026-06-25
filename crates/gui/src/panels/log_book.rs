//! Log Book panel: a QSO table of the most recent contacts, read live from the
//! bus (`logbook/entries`). The row count is not fixed — it grows to fill the
//! panel's vertical space, showing as many newest-first entries as fit. No view
//! state.

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
        const ROW_H: f32 = 22.0;
        const BOTTOM_PAD: f32 = 8.0; // keep the last row clear of the screen bezel
        let (header, screen) = split_block(block);

        // The table fills the panel: rows start just below the column-header rule
        // and step every ROW_H, so the screen's height decides how many recent
        // QSOs fit. Size to that, then pull exactly that many newest entries — the
        // count tracks the user's split drag instead of being pinned at 4.
        let sep_y = screen.top() + 19.0; // column-header underline
        let first_row_y = sep_y + 11.0; // center of the first data row
        let capacity = if screen.bottom() - BOTTOM_PAD >= first_row_y {
            (((screen.bottom() - BOTTOM_PAD - first_row_y) / ROW_H).floor() as usize) + 1
        } else {
            0
        };
        let logs = ctx.bus.recent_logs(capacity);
        panel_header(
            painter,
            header,
            pal,
            "Log Book",
            &format!("last {} · FT8", logs.len()),
            ctx.active,
        );
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
            painter.text(
                Pos2::new(x, hy),
                align,
                tracked(text),
                dimf.clone(),
                pal.dim,
            );
        }
        painter.line_segment(
            [Pos2::new(l, sep_y), Pos2::new(r, sep_y)],
            Stroke::new(1.0, pal.dim.gamma_multiply(0.4)),
        );

        // Our own multi-op identity: a row whose `id.origin` differs is another
        // operator's contact, gossiped onto the shared logbook over the LAN.
        let my_id = ctx.bus.my_station_id();
        for (i, e) in logs.iter().enumerate() {
            let ry = first_row_y + i as f32 * ROW_H;
            let grid = e.grid.as_ref().map(|g| g.0.as_str()).unwrap_or("----");
            // Mine vs. peer: a peer's row renders identically to mine — same colors,
            // no tint — except for a single ↔-led station-id badge past the callsign,
            // the same "accent2 = someone else, not me" language the waterslide's
            // deconfliction overlay uses (heard ≠ mine), dialed back to one quiet marker.
            let mine = e.id.origin.0.as_str() == my_id;
            painter.text(
                Pos2::new(l, ry),
                Align2::LEFT_CENTER,
                hhmm(e.time.0),
                mono(10.0),
                pal.dim,
            );
            let call_rect = painter.text(
                Pos2::new(x_call, ry),
                Align2::LEFT_CENTER,
                tracked(&e.call.0),
                heading(10.0),
                pal.body,
            );
            // Peer rows get a ↔-prefixed badge of the author's station id just past
            // the callsign — the overlay's "↔ = a peer, not us" marker. Drawn only
            // when it clears the GRID column; in a too-narrow panel the bare arrow
            // still flags the row.
            if !mine {
                let badge_x = call_rect.right() + 6.0;
                let full = format!("\u{2194} {}", e.id.origin.0);
                let galley = painter.layout_no_wrap(full, mono(8.0), pal.accent2);
                let badge = if badge_x + galley.size().x <= x_grid - 6.0 {
                    galley
                } else {
                    painter.layout_no_wrap("\u{2194}".to_owned(), mono(8.0), pal.accent2)
                };
                painter.galley(
                    Pos2::new(badge_x, ry - badge.size().y * 0.5),
                    badge,
                    pal.accent2,
                );
            }
            painter.text(
                Pos2::new(x_grid, ry),
                Align2::LEFT_CENTER,
                grid,
                mono(10.0),
                pal.dim,
            );
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
