//! Waterfall panel: header (FT8 + tuned-frequency readout) + the live Waterslide
//! FFT sim as the screen body + a decode ticker along the bottom.

use eframe::egui;
use egui::{Align2, Color32, ColorImage, Pos2, Rect, TextureHandle, TextureOptions};
use types::{
    Decode, DecodeContent, HealthState, ParsedMessage, SpectrumRow, SubsystemHealth, SubsystemId,
};

use app_core::{KENWOOD_BAUDS, LineProfile, Protocol, SerialConfig};

use super::{Panel, PanelCtx};
use crate::bus_view::BusView;
use crate::chrome::{measure, panel_header, shadow};
use crate::panel_data as pd;
use crate::settings::HardwareConfig;
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
    form: ConfigForm,
}

impl Waterfall {
    pub fn new() -> Self {
        Self {
            slide: WaterslidePanel::new(7200.0),
            spectro: Spectrogram::new(),
            form: ConfigForm::default(),
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
        // When the rig is faulted, don't show a (possibly stale) frequency as if
        // it were live — show a dashed, dimmed placeholder instead.
        let rig_fault = ctx.bus.is_real()
            && ctx
                .bus
                .health(SubsystemId::Rig)
                .map(|h| h.is_faulted())
                .unwrap_or(false);
        let (vfo_text, vfo_col) = if rig_fault {
            ("---.---".to_string(), pal.dim)
        } else {
            let vfo_hz = ctx.bus.rig_state().map(|r| r.vfo.0).unwrap_or(14_074_000);
            (format!("{:.3}", vfo_hz as f64 / 1_000_000.0), pal.accent)
        };
        engraved_text(
            painter,
            Pos2::new(rx, cy),
            &vfo_text,
            heading_bold(15.0),
            vfo_col,
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

        let body_big = screen.width() > 24.0 && screen.height() > 24.0;

        if ctx.unlocked {
            // Unlocked (GUI EDIT): the screen body becomes the radio/audio settings
            // form. Real mode only — the form drives live hardware; under mocks
            // there's nothing to configure.
            if body_big {
                if ctx.bus.is_real() {
                    let body = screen.shrink(10.0);
                    let mut child = ctx.ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(body)
                            .layout(egui::Layout::top_down(egui::Align::Min)),
                    );
                    child.set_clip_rect(screen.shrink(2.0));
                    self.form.ui(&mut child, ctx.bus, pal);
                } else {
                    draw_centered_note(
                        painter,
                        screen,
                        pal,
                        "RADIO SETUP",
                        "available in real mode — launch with DM420_REAL=1",
                    );
                }
            }
        } else {
            // Locked: normal operating view. Re-sync the form to the applied config
            // the next time it's opened.
            self.form.loaded = false;

            // When the capture device is missing or disconnected, the spectrogram
            // and decode rail have no live data — show the fault here instead of a
            // frozen or empty screen. The supervisor keeps reconnecting underneath.
            let audio_fault = ctx
                .bus
                .is_real()
                .then(|| ctx.bus.health(SubsystemId::Audio))
                .flatten()
                .filter(SubsystemHealth::is_faulted);

            if let Some(health) = audio_fault {
                if body_big {
                    draw_fault_body(painter, screen, pal, &health);
                }
            } else if ctx.bus.is_real() {
                // Real mode: no FFT/spectrum producer is wired yet (the decoder
                // publishes `Decode`s, not `SpectrumRow`s), so we render the decodes
                // themselves in waterslide form — placed by audio offset (vertical)
                // and age (horizontal), NOW at centre, the FFT (right) half blank.
                if body_big {
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
            } else if body_big {
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

// =====================================================================
// Radio / audio settings form (shown when the panel is unlocked)
// =====================================================================

/// Editable radio + audio settings shown in the unlocked FT8 panel body. Seeded
/// from the currently-applied config the first time it opens (and on Revert);
/// Apply pushes it to the live producers via [`BusView::apply_config`], which
/// reconnect with the new settings.
struct ConfigForm {
    /// Whether the fields have been seeded from the applied config yet.
    loaded: bool,
    audio_input: Option<String>,
    port: Option<String>,
    baud: u32,
    profile: LineProfile,
    autodetect: bool,
    protocol: Protocol,
    /// Cached device/port lists for the pickers (refreshed on load / Refresh).
    audio_devices: Vec<String>,
    serial_ports: Vec<String>,
}

impl Default for ConfigForm {
    fn default() -> Self {
        Self {
            loaded: false,
            audio_input: None,
            port: None,
            baud: 19_200,
            profile: LineProfile::Default,
            autodetect: true,
            protocol: Protocol::Ft8,
            audio_devices: Vec::new(),
            serial_ports: Vec::new(),
        }
    }
}

impl ConfigForm {
    /// Seed the editable fields from the currently-applied config and refresh the
    /// device/port lists.
    fn load(&mut self, bus: &BusView) {
        let cfg = bus.current_config();
        self.audio_input = cfg.audio_input;
        self.port = cfg.serial.port;
        self.baud = cfg.serial.baud;
        self.profile = cfg.serial.profile;
        self.autodetect = cfg.serial.autodetect;
        self.protocol = cfg.protocol;
        self.audio_devices = bus.audio_inputs();
        self.serial_ports = bus.serial_ports();
        self.loaded = true;
    }

    /// The edited fields as a `HardwareConfig` ready to apply.
    fn to_config(&self) -> HardwareConfig {
        HardwareConfig {
            audio_input: self.audio_input.clone(),
            serial: SerialConfig {
                port: self.port.clone(),
                baud: self.baud,
                profile: self.profile,
                autodetect: self.autodetect,
            },
            protocol: self.protocol,
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, bus: &BusView, pal: &Palette) {
        if !self.loaded {
            self.load(bus);
        }
        ui.spacing_mut().item_spacing = egui::vec2(10.0, 8.0);
        ui.label(egui::RichText::new("RADIO SETUP").color(pal.legend).strong());

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Grid::new("radio_setup_grid")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("Audio input");
                        let sel = self
                            .audio_input
                            .clone()
                            .unwrap_or_else(|| "(system default)".into());
                        egui::ComboBox::from_id_salt("audio_input")
                            .selected_text(sel)
                            .width(240.0)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.audio_input, None, "(system default)");
                                for d in &self.audio_devices {
                                    ui.selectable_value(&mut self.audio_input, Some(d.clone()), d);
                                }
                            });
                        ui.end_row();

                        ui.label("Mode");
                        egui::ComboBox::from_id_salt("mode")
                            .selected_text(proto_label(self.protocol))
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.protocol, Protocol::Ft8, "FT8");
                                ui.selectable_value(&mut self.protocol, Protocol::Ft4, "FT4");
                            });
                        ui.end_row();

                        ui.label("Rig port");
                        ui.checkbox(&mut self.autodetect, "Autodetect port / baud");
                        ui.end_row();
                    });

                // Manual serial fields are disabled (greyed) while autodetect is on.
                ui.add_enabled_ui(!self.autodetect, |ui| {
                    egui::Grid::new("serial_grid")
                        .num_columns(2)
                        .spacing([12.0, 8.0])
                        .show(ui, |ui| {
                            ui.label("Port");
                            let sel = self.port.clone().unwrap_or_else(|| {
                                if self.serial_ports.is_empty() {
                                    "(no ports found)".into()
                                } else {
                                    "(select port)".into()
                                }
                            });
                            egui::ComboBox::from_id_salt("port")
                                .selected_text(sel)
                                .width(240.0)
                                .show_ui(ui, |ui| {
                                    for p in &self.serial_ports {
                                        ui.selectable_value(&mut self.port, Some(p.clone()), p);
                                    }
                                });
                            ui.end_row();

                            ui.label("Baud");
                            egui::ComboBox::from_id_salt("baud")
                                .selected_text(self.baud.to_string())
                                .show_ui(ui, |ui| {
                                    for &b in KENWOOD_BAUDS {
                                        ui.selectable_value(&mut self.baud, b, b.to_string());
                                    }
                                });
                            ui.end_row();

                            ui.label("Flow");
                            egui::ComboBox::from_id_salt("flow")
                                .selected_text(profile_label(self.profile))
                                .show_ui(ui, |ui| {
                                    for p in [
                                        LineProfile::Default,
                                        LineProfile::AssertDtrRts,
                                        LineProfile::HardwareFlow,
                                    ] {
                                        ui.selectable_value(&mut self.profile, p, profile_label(p));
                                    }
                                });
                            ui.end_row();
                        });
                });

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Apply").clicked() {
                        bus.apply_config(self.to_config());
                    }
                    if ui.button("Refresh devices").clicked() {
                        self.audio_devices = bus.audio_inputs();
                        self.serial_ports = bus.serial_ports();
                    }
                    if ui.button("Revert").clicked() {
                        self.loaded = false;
                    }
                });
            });
    }
}

fn proto_label(p: Protocol) -> &'static str {
    match p {
        Protocol::Ft8 => "FT8",
        Protocol::Ft4 => "FT4",
    }
}

fn profile_label(p: LineProfile) -> &'static str {
    match p {
        LineProfile::Default => "None (default)",
        LineProfile::AssertDtrRts => "DTR/RTS",
        LineProfile::HardwareFlow => "RTS/CTS (hardware)",
    }
}

/// A simple two-line centred note in the screen body (used when the settings form
/// has nothing to drive, e.g. mock mode).
fn draw_centered_note(painter: &egui::Painter, screen: Rect, pal: &Palette, title: &str, detail: &str) {
    let painter = painter.with_clip_rect(screen.shrink(6.0));
    let c = screen.center();
    painter.text(
        Pos2::new(c.x, c.y - 8.0),
        Align2::CENTER_CENTER,
        title,
        heading_bold(14.0),
        pal.accent,
    );
    painter.text(
        Pos2::new(c.x, c.y + 12.0),
        Align2::CENTER_CENTER,
        detail,
        mono(9.5),
        pal.sub,
    );
}

/// Fault placeholder for the screen body: a centred status line plus the
/// producer's reason, shown when the audio subsystem is down/degraded so the
/// panel reads as "no signal because the device is gone", not "band is quiet".
fn draw_fault_body(painter: &egui::Painter, screen: Rect, pal: &Palette, health: &SubsystemHealth) {
    let painter = painter.with_clip_rect(screen.shrink(6.0));
    let c = screen.center();
    let title = match health.state {
        HealthState::Down(_) => "AUDIO OFFLINE",
        HealthState::Degraded(_) => "AUDIO DEGRADED",
        HealthState::Healthy => return,
    };
    painter.text(
        Pos2::new(c.x, c.y - 12.0),
        Align2::CENTER_CENTER,
        title,
        heading_bold(15.0),
        pal.accent,
    );
    if let Some(reason) = health.reason() {
        painter.text(
            Pos2::new(c.x, c.y + 10.0),
            Align2::CENTER_CENTER,
            reason,
            mono(10.0),
            pal.sub,
        );
    }
    painter.text(
        Pos2::new(c.x, c.y + 28.0),
        Align2::CENTER_CENTER,
        "reconnecting…",
        mono(9.0),
        pal.dim,
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
