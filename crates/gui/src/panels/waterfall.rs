//! Waterfall panel: header (FT8 + tuned-frequency readout) + the live Waterslide
//! FFT sim as the screen body + a decode ticker along the bottom.

use eframe::egui;
use egui::{Align2, Color32, ColorImage, Pos2, Rect, TextureHandle, TextureOptions};
use types::{Decode, DecodeContent, ParsedMessage, SpectrumRow};

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
    spectro: Spectrogram,
}

impl Waterfall {
    pub fn new() -> Self {
        Self {
            slide: WaterslidePanel::new(7200.0),
            spectro: Spectrogram::new(),
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

        if ctx.bus.is_real() {
            // Real mode: no FFT/spectrum producer is wired yet (the decoder
            // publishes `Decode`s, not `SpectrumRow`s), so we render the decodes
            // themselves in waterslide form — placed by audio offset (vertical)
            // and age (horizontal), NOW at centre, the FFT (right) half left blank.
            if screen.width() > 24.0 && screen.height() > 24.0 {
                let body = screen.shrink(8.0);
                let now_ms = chrono::Utc::now().timestamp_millis();
                // Right half: real scrolling spectrogram (brightness = intensity),
                // flowing right as the decode text flows left. Equal-width halves
                // each spanning WS_HISTORY_SECS, so the scroll rates match.
                let now_x = body.center().x;
                let right = Rect::from_min_max(Pos2::new(now_x, body.top()), body.max);
                let cmap = if pal.is_dark {
                    crate::waterslide_panel::martian_cmap()
                } else {
                    crate::waterslide_panel::martian_cmap_light()
                };
                self.spectro
                    .update_and_paint(ctx.ui, right, ctx.dt, ctx.bus.spectrum().as_ref(), &cmap);
                // Left half: decodes sliding left from centre, drawn over the
                // spectrogram (graticule, NOW line, and Hz labels included).
                draw_waterslide(painter, body, pal, &ctx.bus.recent_decodes(64), now_ms);
                ctx.ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
            }
        } else if screen.width() > 24.0 && screen.height() > 24.0 {
            // Mock mode only: Live Waterslide simulation as the screen body
            // (inset to keep brackets).
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

/// Audio-offset axis span (Hz): FT8/FT4 decodes land in roughly 0..3000 Hz.
const WS_MAX_HZ: f32 = 3000.0;

/// Internal spectrogram texture: width = history columns, height = frequency bins.
const SPECTRO_COLS: usize = 512;
const SPECTRO_MAX_H: usize = 512;

/// A scrolling spectrogram texture for the right half. Newest column sits at the
/// NOW line; older columns flow right, brightness = signal intensity. It scrolls
/// at the same rate the decode text moves left: both halves are equal width and
/// each spans `WS_HISTORY_SECS`, so the on-screen pixels-per-second match.
struct Spectrogram {
    w: usize,
    h: usize,
    /// `h*w` row-major intensities; row 0 = top (high freq), col 0 = newest.
    intensity: Vec<u8>,
    image: ColorImage,
    tex: Option<TextureHandle>,
    dx_frac: f64,
}

impl Spectrogram {
    fn new() -> Self {
        Self {
            w: 0,
            h: 0,
            intensity: Vec::new(),
            image: ColorImage::new([1, 1], vec![Color32::BLACK]),
            tex: None,
            dx_frac: 0.0,
        }
    }

    /// (Re)allocate buffers when the bin count changes.
    fn ensure_size(&mut self, h: usize) {
        if self.w == SPECTRO_COLS && self.h == h {
            return;
        }
        self.w = SPECTRO_COLS;
        self.h = h;
        self.intensity = vec![0u8; self.w * h];
        self.image = ColorImage::new([self.w, h], vec![Color32::BLACK; self.w * h]);
        self.tex = None;
    }

    /// Write a freshly-arrived column into texture column `col`. Bin 0 (lowest
    /// freq) maps to the bottom row so the axis matches the decode side.
    fn write_col(&mut self, col: usize, mags: &[u8]) {
        for r in 0..self.h {
            let bin = self.h - 1 - r;
            self.intensity[r * self.w + col] = mags.get(bin).copied().unwrap_or(0);
        }
    }

    /// Advance the scroll by `dt`, fill the newly-exposed columns with `latest`,
    /// recolour through `cmap`, and blit into `rect` (the right half).
    fn update_and_paint(
        &mut self,
        ui: &egui::Ui,
        rect: Rect,
        dt: f64,
        latest: Option<&SpectrumRow>,
        cmap: &[Color32; 256],
    ) {
        if let Some(row) = latest {
            self.ensure_size(row.mags.len().clamp(1, SPECTRO_MAX_H));
        }
        if self.w == 0 || self.h == 0 {
            return;
        }

        // Scroll right by whole columns; carry the fraction across frames.
        self.dx_frac += dt * (self.w as f64 / WS_HISTORY_SECS as f64);
        let mut dx = self.dx_frac.floor() as usize;
        if dx > 0 {
            self.dx_frac -= dx as f64;
            dx = dx.min(self.w);
            for r in 0..self.h {
                let base = r * self.w;
                self.intensity.copy_within(base..base + (self.w - dx), base + dx);
            }
            for c in 0..dx {
                match latest {
                    Some(row) => self.write_col(c, &row.mags),
                    None => {
                        for r in 0..self.h {
                            self.intensity[r * self.w + c] = 0;
                        }
                    }
                }
            }
        }

        for (px, &v) in self.image.pixels.iter_mut().zip(self.intensity.iter()) {
            *px = cmap[v as usize];
        }
        let img = self.image.clone();
        match &mut self.tex {
            Some(t) => t.set(img, TextureOptions::LINEAR),
            None => self.tex = Some(ui.ctx().load_texture("spectrogram", img, TextureOptions::LINEAR)),
        }
        let uv = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0));
        ui.painter_at(rect)
            .image(self.tex.as_ref().unwrap().id(), rect, uv, Color32::WHITE);
    }
}
/// Seconds of history the left (text) half spans: NOW at centre → oldest at left.
/// Tuned so a decode column travels well clear of centre within one FT8 slot
/// (~15 s); a smaller value scrolls faster, a larger one shows more history.
const WS_HISTORY_SECS: f32 = 45.0;

/// The "waterslide" decode view: each decode placed by audio offset (vertical)
/// and age (horizontal), newest at the centre NOW line and sliding left as it
/// ages. The right (FFT) half is left blank until a spectrum producer is wired.
/// Fed by `BusView::recent_decodes`, i.e. the real decoder's bus stream.
fn draw_waterslide(
    painter: &egui::Painter,
    rect: Rect,
    pal: &Palette,
    decodes: &[Decode],
    now_ms: i64,
) {
    let painter = painter.with_clip_rect(rect);
    let now_x = rect.center().x; // the NOW line
    let left_w = (now_x - rect.left()).max(1.0);
    let pps = left_w / WS_HISTORY_SECS; // pixels per second of history

    // Audio offset → vertical position (low Hz at bottom, high Hz at top).
    let y_of = |off: f32| rect.bottom() - (off / WS_MAX_HZ).clamp(0.0, 1.0) * rect.height();

    // Faint frequency graticule with Hz labels (parked at the right edge, in the
    // otherwise-blank FFT half).
    for hz in [500.0f32, 1000.0, 1500.0, 2000.0, 2500.0] {
        let y = y_of(hz);
        painter.line_segment(
            [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
            egui::Stroke::new(1.0, pal.dim.gamma_multiply(0.35)),
        );
        painter.text(
            Pos2::new(rect.right() - 4.0, y - 2.0),
            Align2::RIGHT_BOTTOM,
            format!("{hz:.0}"),
            mono(8.5),
            pal.sub,
        );
    }

    // Decodes: newest at NOW, older to the left. Each message is left-aligned so
    // it starts at its decode time and reads rightward; the SNR sits just to its left.
    for d in decodes {
        let age = (now_ms - d.t.0) as f32 / 1000.0;
        if age < 0.0 {
            continue; // a clock skew put it in the (blank) future half
        }
        let x = now_x - age * pps;
        if x < rect.left() {
            continue; // scrolled off the left edge
        }
        let y = y_of(d.offset.0);
        let strong = d.snr_db.map(|s| s > -12).unwrap_or(false);
        let msg_col = if strong { pal.body } else { pal.dim };
        let snr_col = if strong { pal.accent } else { pal.dim };
        let msg_rect = painter.text(
            Pos2::new(x, y),
            Align2::LEFT_CENTER,
            decode_text(d),
            mono(11.0),
            msg_col,
        );
        let snr = d.snr_db.map(fmt_snr).unwrap_or_else(|| "   ".into());
        painter.text(
            Pos2::new(msg_rect.left() - 6.0, y),
            Align2::RIGHT_CENTER,
            snr,
            mono(9.5),
            snr_col,
        );
    }

    // NOW line at the centre.
    painter.line_segment(
        [Pos2::new(now_x, rect.top()), Pos2::new(now_x, rect.bottom())],
        egui::Stroke::new(2.0, pal.accent),
    );
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
