//! Waterslide — egui rendering panel (adapted to egui 0.34 from the design
//! handoff reference).
//!
//! Paints the Waterslide visualisation fed by `waterslide_sim`: a scrolling FFT
//! spectrogram on the right half and the matching decoded text scrolling left on
//! the other half, split at "NOW". The panel is handed a target `Rect` (the
//! recessed screen interior) each frame and maps the model's design space into it.
//!
//! Three techniques:
//!   1. spectrogram = a `ColorImage` shifted in place and re-uploaded each frame;
//!   2. decoded text = `Painter::text` at computed scroll positions, clipped;
//!   3. NOW line / graticule / ±Hz scale = `Painter` primitives.
//!
//! Swapping in a real radio later touches only `waterslide_sim` (the two data
//! sources); this rendering code is unchanged.

use eframe::egui::{
    self, Align2, Color32, ColorImage, FontId, Pos2, Rect, Stroke, TextureHandle, TextureOptions,
};

use crate::theme::Palette;
use crate::waterslide_sim::{column_values, Sim, COL_H};

// FFT half is W×H px in model space; 30 s spans W px → PPS px/sec.
const W: usize = 520;
const H: usize = COL_H; // 500
const PPS: f64 = W as f64 / 30.0;

// Design-space x bands inside the screen (text | divider | fft). The fft fills
// to the right edge — the old dedicated ±Hz scale lane is gone; the gridlines
// carry their own labels instead.
const X_TEXT_R: f32 = 520.0; // right edge of the text lane == "now" centre
const X_FFT_L: f32 = 522.0;
const X_FFT_R: f32 = 1042.0;
const DESIGN_W: f32 = 1042.0;

// Outgoing (next-TX) audio frequency. FT8 keeps the dial fixed and tunes the
// transmitted audio within the passband, so this is an offset in Hz clamped to
// a practical window. The indicator is a translucent full-width lane centred on
// the offset with bright accent rules top and bottom.
const TX_LIMIT_HZ: i32 = 1000; // practical ± audio offset for the next transmission
const TX_BAND_HALF: f32 = 8.0; // half-height of the indicator lane, in design px

/// Palette the panel reads. Built from the app's active `Palette` so it flips on
/// the existing light/dark toggle; the colormap is the only new token.
pub struct WaterslideTheme {
    pub accent: Color32,
    pub text: Color32,
    pub dim: Color32,
    pub legend: Color32,
    pub screen_bg: Color32,
    pub grid: Color32,
    pub grid_mid: Color32,
    pub cmap: [Color32; 256], // intensity 0..1 → colour
}

impl WaterslideTheme {
    /// Derive a Waterslide theme from the spike's active palette. The amber
    /// "Martian" colormap is used in both light and dark — a spectrogram is an
    /// inherently dark scientific display, so it reads correctly on either face.
    pub fn from_palette(pal: &Palette) -> Self {
        Self {
            accent: pal.accent,
            text: pal.body,
            dim: pal.dim,
            legend: pal.legend,
            screen_bg: pal.screen_bg,
            grid: pal.dim.gamma_multiply(0.35),
            grid_mid: pal.accent.gamma_multiply(0.55),
            // Dark face → dark-background map (signal = bright). Light face →
            // inverted map: a paper background with signal as dark amber pixels.
            cmap: if pal.is_dark {
                martian_cmap()
            } else {
                martian_cmap_light()
            },
        }
    }
}

/// 6-stop colormap builder. Stops are (position 0..1, [r,g,b]).
pub fn build_cmap(stops: &[(f32, [u8; 3])]) -> [Color32; 256] {
    let mut lut = [Color32::BLACK; 256];
    for i in 0..256 {
        let v = i as f32 / 255.0;
        let mut a = stops[0];
        let mut b = stops[stops.len() - 1];
        for w in stops.windows(2) {
            if v >= w[0].0 && v <= w[1].0 {
                a = w[0];
                b = w[1];
                break;
            }
        }
        let tt = if (b.0 - a.0).abs() < 1e-6 {
            0.0
        } else {
            (v - a.0) / (b.0 - a.0)
        };
        let lerp = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * tt) as u8;
        lut[i] = Color32::from_rgb(lerp(a.1[0], b.1[0]), lerp(a.1[1], b.1[1]), lerp(a.1[2], b.1[2]));
    }
    lut
}

/// The "Martian / graphite" colormap (dark amber): dark background, bright signal.
pub fn martian_cmap() -> [Color32; 256] {
    build_cmap(&[
        (0.00, [8, 6, 4]),
        (0.22, [46, 18, 6]),
        (0.42, [110, 44, 8]),
        (0.60, [186, 86, 12]),
        (0.78, [240, 150, 40]),
        (1.00, [255, 236, 205]),
    ])
}

/// The light / "silver" colormap: the dark map inverted in lightness — a paper
/// background (noise floor blends into the silver screen) with the signal
/// rendered as progressively darker amber/brown. Reads correctly on a light face.
pub fn martian_cmap_light() -> [Color32; 256] {
    build_cmap(&[
        (0.00, [239, 231, 220]), // ≈ silver screen_bg → noise floor disappears
        (0.22, [228, 200, 150]),
        (0.42, [210, 150, 70]),
        (0.60, [180, 100, 20]),
        (0.78, [120, 55, 8]),
        (1.00, [40, 20, 5]), // strongest signal → near-dark brown
    ])
}

pub struct WaterslidePanel {
    sim: Sim,
    // Theme-independent FFT intensities (0..1), W×H row-major, scrolled in place.
    // The colour image is derived from this each frame through the active cmap,
    // so flipping the palette recolours the whole history — no baked-in colours.
    intensity: Vec<f32>,
    image: ColorImage, // W×H upload buffer, rebuilt from `intensity` each frame
    tex: Option<TextureHandle>,
    dx_frac: f64,
    primed: bool,
    // Audio offset (Hz, ±TX_LIMIT_HZ) where the next FT4/FT8 transmission lands.
    // Set by clicking the FFT lane (by vertical position) or by clicking a
    // decoded line (snaps to that station's offset).
    outgoing_off: i32,
}

impl WaterslidePanel {
    pub fn new(start_t: f64) -> Self {
        Self {
            sim: Sim::new(start_t),
            intensity: vec![0.0; W * H],
            image: ColorImage::new([W, H], vec![Color32::BLACK; W * H]),
            tex: None,
            dx_frac: 0.0,
            primed: false,
            outgoing_off: 300,
        }
    }

    /// Map an audio offset (Hz) to a vertical position in 0..H design space.
    /// Mirrors `waterslide_sim::y_of` against this panel's reference height.
    fn y_of(off: i32) -> f32 {
        250.0 - off as f32 / 3000.0 * H as f32
    }

    /// Fill column `cx` of the intensity buffer from the sim at instant `ts`.
    fn write_col(&mut self, cx: usize, ts: f64) {
        let mut vals = [0f32; COL_H];
        let col_id = (ts * PPS).round() as i64;
        column_values(ts, col_id, &mut vals);
        for y in 0..H {
            self.intensity[y * W + cx] = vals[y].clamp(0.0, 1.0);
        }
    }

    fn prime(&mut self) {
        let t = self.sim.t();
        for cx in 0..W {
            self.write_col(cx, t - cx as f64 / PPS);
        }
        self.primed = true;
    }

    fn scroll(&mut self, dt: f64) {
        self.dx_frac += dt * PPS;
        let mut dx = self.dx_frac.floor() as usize;
        if dx == 0 {
            return;
        }
        if dx > W {
            dx = W;
        }
        self.dx_frac -= dx as f64;
        // shift every row right by dx (memmove-safe), newest goes in at the left
        for y in 0..H {
            let row = y * W;
            self.intensity.copy_within(row..row + (W - dx), row + dx);
        }
        let t = self.sim.t();
        for cx in 0..dx {
            self.write_col(cx, t - cx as f64 / PPS);
        }
    }

    /// Map the whole intensity buffer through `cmap` into the upload image. Cheap
    /// (260 k LUT lookups) and done every frame, so a palette flip recolours all
    /// frozen history rather than only the leading edge.
    fn recolor(&mut self, cmap: &[Color32; 256]) {
        for (px, &v) in self.image.pixels.iter_mut().zip(self.intensity.iter()) {
            *px = cmap[(v * 255.0) as usize];
        }
    }

    /// Paint one frame into `rect` (the recessed-screen interior). `dt` is the
    /// frame delta in seconds; `theme` carries colours + colormap for the palette.
    pub fn ui(&mut self, ui: &mut egui::Ui, rect: Rect, dt: f64, theme: &WaterslideTheme) {
        // 1) advance model + spectrogram (intensities only — theme-independent)
        self.sim.advance(dt);
        if !self.primed {
            self.prime();
        } else {
            self.scroll(dt);
        }
        // recolour the full buffer through the active palette's colormap
        self.recolor(&theme.cmap);

        // 2) upload texture
        let img = self.image.clone();
        match &mut self.tex {
            Some(t) => t.set(img, TextureOptions::LINEAR),
            None => {
                self.tex = Some(ui.ctx().load_texture("waterslide", img, TextureOptions::LINEAR))
            }
        }
        let tex = self.tex.as_ref().unwrap();

        // 3) geometry: map design space (0..DESIGN_W, 0..REF_H) into `rect`
        let sx = rect.width() / DESIGN_W;
        let sy = rect.height() / H as f32;
        let p = |x: f32, y: f32| Pos2::new(rect.left() + x * sx, rect.top() + y * sy);
        // Text is sized off the geometric mean of the two axis scales. The design
        // space is wide (1118×500) but a pane can be near-square: keying fonts to
        // `sy` alone bloats them, keying to `sx`/min shrinks them too far. The
        // geometric mean is the area-preserving uniform scale — the middle ground.
        let fscale = (sx * sy).sqrt();

        // Interaction: a click tunes the outgoing (next-TX) audio frequency.
        // A click landing on a decoded line snaps to that station's offset
        // (resolved in the text loop below); any other click sets the offset
        // from the vertical position. The cursor hints the lane is tunable.
        let resp = ui
            .interact(rect, egui::Id::new("waterslide_tx_tune"), egui::Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let click_pos = resp.clicked().then(|| resp.interact_pointer_pos()).flatten();
        let mut snap_off: Option<i32> = None;

        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, theme.screen_bg);

        // FFT image (newest column at its left edge == panel centre)
        let fft_rect = Rect::from_min_max(p(X_FFT_L, 0.0), p(X_FFT_R, H as f32));
        let uv = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0));
        painter.image(tex.id(), fft_rect, uv, Color32::WHITE);

        // graticule: faint horizontal frequency lines across text + fft. Each
        // ±Hz line is labelled just above its own rule (the tuned centre is
        // shown in the panel header, so the 0 line stays unlabelled).
        for off in [1500i32, 1000, 500, 0, -500, -1000, -1500] {
            let y = 250.0 - off as f32 / 3000.0 * H as f32;
            let (col, wdt) = if off == 0 {
                (theme.grid_mid, 1.4)
            } else {
                (theme.grid, 1.0)
            };
            painter.line_segment([p(0.0, y), p(X_FFT_R, y)], Stroke::new(wdt, col));
            if off != 0 {
                let label = format!("{}{}", if off > 0 { "+" } else { "\u{2212}" }, off.abs());
                painter.text(
                    p(X_FFT_R - 8.0, y - 3.0),
                    Align2::RIGHT_BOTTOM,
                    label,
                    FontId::monospace(9.0 * fscale),
                    theme.legend,
                );
            }
        }

        // 4) decoded text lane (left half), clipped, scrolling left
        let text_rect = Rect::from_min_max(p(0.0, 0.0), p(X_TEXT_R, H as f32));
        let tp = ui.painter_at(text_rect);
        let t = self.sim.t();
        for rec in self.sim.records() {
            let x = X_TEXT_R - ((t - rec.td) * PPS) as f32;
            if x < -260.0 {
                continue;
            }
            let pos = p(x, rec.final_y);
            let snr_col = if rec.rsnr > -12 {
                theme.accent
            } else {
                theme.dim
            };
            // message (right-aligned so the newest char sits at "now")
            let msg_rect =
                tp.text(pos, Align2::RIGHT_CENTER, &rec.msg, FontId::monospace(12.0 * fscale), theme.text);
            let msg_w = msg_rect.width();
            // clicking the line snaps the outgoing frequency to its station
            if let Some(cp) = click_pos {
                if msg_rect.expand(4.0).contains(cp) {
                    snap_off = Some(rec.off);
                }
            }
            // reception SNR just to the left of the message
            let snr_pos = Pos2::new(pos.x - msg_w - 8.0 * sx, pos.y);
            tp.text(snr_pos, Align2::RIGHT_CENTER, sgn(rec.rsnr), FontId::monospace(10.5 * fscale), snr_col);
        }

        // 5) NOW divider at the centre
        painter.line_segment([p(521.0, 0.0), p(521.0, H as f32)], Stroke::new(2.0, theme.accent));

        // 6) resolve a click into a new outgoing frequency: snap to a decoded
        // line if one was hit, otherwise read it off the vertical position.
        if let Some(cp) = click_pos {
            let off = snap_off.unwrap_or_else(|| {
                let design_y = (cp.y - rect.top()) / sy;
                ((250.0 - design_y) * (3000.0 / H as f32)).round() as i32
            });
            self.outgoing_off = off.clamp(-TX_LIMIT_HZ, TX_LIMIT_HZ);
        }

        // 7) outgoing-frequency indicator: a translucent full-width lane with
        // bright accent rules top & bottom, centred on the next-TX offset.
        let oy = Self::y_of(self.outgoing_off);
        let band = Rect::from_min_max(p(0.0, oy - TX_BAND_HALF), p(X_FFT_R, oy + TX_BAND_HALF));
        painter.rect_filled(band, 0.0, theme.accent.gamma_multiply(0.10));
        painter.line_segment([band.left_top(), band.right_top()], Stroke::new(1.5, theme.accent));
        painter.line_segment([band.left_bottom(), band.right_bottom()], Stroke::new(1.5, theme.accent));
        let tx_label = format!(
            "TX {}{}",
            if self.outgoing_off >= 0 { "+" } else { "\u{2212}" },
            self.outgoing_off.abs()
        );
        painter.text(
            p(X_FFT_L + 4.0, oy),
            Align2::LEFT_CENTER,
            tx_label,
            FontId::monospace(9.0 * fscale),
            theme.accent,
        );

        ui.ctx().request_repaint(); // keep the animation running
    }
}

fn sgn(n: i32) -> String {
    format!("{}{:02}", if n <= 0 { "\u{2212}" } else { "+" }, n.abs())
}
