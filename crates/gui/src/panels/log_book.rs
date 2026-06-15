//! Log Book panel: a 4-row QSO table read from `panel_data::LOGS`. No view state.

use eframe::egui;
use egui::{Align2, Pos2, Stroke};

use super::{Panel, PanelCtx};
use crate::chrome::{panel_header, split_block};
use crate::panel_data as pd;
use crate::theme::*;

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
        let (header, screen) = split_block(block);
        panel_header(painter, header, pal, "Log Book", "last 4 · FT8");
        painter.text(
            Pos2::new(header.right() - 2.0, header.center().y),
            Align2::RIGHT_CENTER,
            "312 QSO",
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
            painter.text(Pos2::new(x, hy), align, &tracked(text), dimf.clone(), pal.dim);
        }
        let sep_y = screen.top() + 19.0;
        painter.line_segment(
            [Pos2::new(l, sep_y), Pos2::new(r, sep_y)],
            Stroke::new(1.0, pal.dim.gamma_multiply(0.4)),
        );

        for (i, (utc, call, grid, snt, rcv)) in pd::LOGS.iter().enumerate() {
            let ry = sep_y + 11.0 + i as f32 * 22.0;
            painter.text(Pos2::new(l, ry), Align2::LEFT_CENTER, utc, mono(10.0), pal.dim);
            painter.text(
                Pos2::new(x_call, ry),
                Align2::LEFT_CENTER,
                &tracked(call),
                heading(10.0),
                pal.body,
            );
            painter.text(Pos2::new(x_grid, ry), Align2::LEFT_CENTER, grid, mono(10.0), pal.dim);
            painter.text(Pos2::new(x_snt, ry), Align2::RIGHT_CENTER, snt, mono(10.0), pal.body);
            painter.text(Pos2::new(x_rcv, ry), Align2::RIGHT_CENTER, rcv, mono(10.0), pal.accent);
            if i + 1 < pd::LOGS.len() {
                let ly = ry + 11.0;
                painter.line_segment(
                    [Pos2::new(l, ly), Pos2::new(r, ly)],
                    Stroke::new(1.0, pal.dim.gamma_multiply(0.22)),
                );
            }
        }
    }
}
