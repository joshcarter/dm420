//! Call Sign panel: resolves the currently selected station into country + flag
//! and the location/signal facts DM420 already knows locally — distance and
//! bearing from the operator's grid, plus the reports exchanged if we've worked
//! them. Pure display, fed entirely offline (callsign prefix table + the live
//! bus); no network. Phase 1 of `docs/call_sign_lookup_panel.md`.

use eframe::egui;
use egui::{Align2, Pos2, Rect};

use super::{Panel, PanelCtx};
use crate::chrome::{panel_header, split_block};
use crate::flag;
use crate::panel_data as pd;
use crate::theme::*;

pub struct CallSign;

impl CallSign {
    pub fn new() -> Self {
        Self
    }
}

impl Panel for CallSign {
    fn title(&self) -> &str {
        "Call Sign"
    }

    /// Pure display — never steal keyboard focus from the Digital panel on click.
    fn takes_keyboard_focus(&self) -> bool {
        false
    }

    fn ui(&mut self, ctx: &mut PanelCtx, block: Rect) {
        let pal = ctx.pal;
        let painter = ctx.painter;
        let (header, screen) = split_block(block);

        let call = ctx
            .selected_station
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty());

        let status = call.unwrap_or("");
        panel_header(painter, header, pal, "Call Sign", status, ctx.active);
        recessed_screen(painter, screen, pal);

        let Some(call) = call else {
            painter.text(
                screen.center(),
                Align2::CENTER_CENTER,
                "SELECT A STATION",
                mono(11.0),
                pal.dim,
            );
            return;
        };

        let left = screen.left() + 14.0;
        let right = screen.right() - 14.0;

        // ---- identity: callsign + flag + country ----
        painter.text(
            Pos2::new(left, screen.top() + 20.0),
            Align2::LEFT_CENTER,
            call.to_uppercase(),
            heading_bold(24.0),
            pal.accent,
        );

        let country = callbook::lookup(call);
        let iso = country.map(|c| c.iso).unwrap_or("");
        let flag_w = 42.0;
        let flag_h = 28.0;
        let flag_rect = Rect::from_min_size(
            Pos2::new(right - flag_w, screen.top() + 7.0),
            egui::vec2(flag_w, flag_h),
        );
        if iso.is_empty() {
            flag::draw_flag(painter, flag_rect, "?", pal);
        } else {
            flag::draw_flag(painter, flag_rect, iso, pal);
        }

        // ---- local facts from the bus: grid (heard or logged) + worked exchange ----
        let grid = ctx
            .bus
            .heard_spots()
            .into_iter()
            .chain(ctx.bus.worked_spots())
            .find(|s| s.call.eq_ignore_ascii_case(call))
            .map(|s| s.grid)
            .filter(|g| !g.is_empty());

        let worked = ctx
            .bus
            .recent_logs(500)
            .into_iter()
            .find(|e| e.call.0.eq_ignore_ascii_case(call));
        let grid = grid.or_else(|| {
            worked
                .as_ref()
                .and_then(|e| e.grid.as_ref())
                .map(|g| g.0.clone())
                .filter(|g| !g.is_empty())
        });

        // Line 1: country · grid · distance — only the parts we actually know,
        // joined into one row so it stays legible at minimum panel height.
        let mut parts: Vec<String> = vec![
            country
                .map(|c| c.name.to_string())
                .unwrap_or_else(|| "Unknown prefix".into()),
        ];
        if let Some(g) = &grid {
            parts.push(g.clone());
        }
        if let (Some(them), Some(home)) = (
            grid.as_deref().and_then(pd::grid_to_lonlat),
            pd::grid_to_lonlat(ctx.grid),
        ) {
            let km = haversine_km(
                (home.lon as f64, home.lat as f64),
                (them.lon as f64, them.lat as f64),
            );
            parts.push(format!(
                "{} mi / {} km",
                group((km * 0.621_371).round() as i64),
                group(km.round() as i64)
            ));
        }
        painter.text(
            Pos2::new(left, screen.top() + 46.0),
            Align2::LEFT_CENTER,
            parts.join("  ·  "),
            mono(12.0),
            pal.legend,
        );

        // Line 2: the message exchange — the FT8 signal report or the ARRL Field
        // Day class + section — shown TX then RX, when the call is in the log.
        let exchange = match worked.as_ref() {
            Some(e) if !e.exchange_sent.is_empty() || !e.exchange_rcvd.is_empty() => {
                let part = |s: &str| if s.is_empty() { "—".to_string() } else { s.to_string() };
                format!(
                    "TX {}  ·  RX {}",
                    part(&e.exchange_sent),
                    part(&e.exchange_rcvd)
                )
            }
            _ => "not worked".to_string(),
        };
        painter.text(
            Pos2::new(left, screen.top() + 66.0),
            Align2::LEFT_CENTER,
            exchange,
            mono(12.0),
            if worked.is_some() { pal.accent } else { pal.dim },
        );
    }
}

/// Great-circle distance in km between two `(lon, lat)` points (degrees).
fn haversine_km(from: (f64, f64), to: (f64, f64)) -> f64 {
    const R: f64 = 6371.0;
    let (lon1, lat1) = (from.0.to_radians(), from.1.to_radians());
    let (lon2, lat2) = (to.0.to_radians(), to.1.to_radians());
    let dlat = lat2 - lat1;
    let dlon = lon2 - lon1;
    let a = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
    2.0 * R * a.sqrt().asin()
}

/// Group an integer with thousands separators (`5432` → `5,432`).
fn group(n: i64) -> String {
    let s = n.abs().to_string();
    let mut out = String::new();
    let bytes = s.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*b as char);
    }
    if n < 0 { format!("-{out}") } else { out }
}
