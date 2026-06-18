//! Waterfall panel: header (FT8 + tuned-frequency readout) + the live Waterslide
//! FFT sim as the screen body + a decode ticker along the bottom.

use eframe::egui;
use egui::{Align2, Color32, ColorImage, Pos2, Rect, TextureHandle, TextureOptions};
use types::{
    Decode, DecodeContent, HealthState, ParsedMessage, QsoPhase, SpectrumRow, SubsystemHealth,
    SubsystemId,
};

use app_core::{LineProfile, Protocol, SerialConfig};

use super::{Panel, PanelCtx};
use crate::bus_view::BusView;
use crate::chrome::{key_cell_accent, lcd_panel, measure, panel_header, shadow};
use crate::panel_data as pd;
use crate::send::{Activation, Command, SendState};
use crate::settings::{DEFAULT_BAUD, HardwareConfig, KENWOOD_BAUDS};
use crate::theme::*;
use crate::waterslide_panel::{Target, WaterslidePanel, WaterslideTheme, target_call};

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
    /// Send-row text-box / slash-command state. The transmit lifecycle itself
    /// lives in the QSO engine (`QsoState`), which this row renders and commands.
    send: SendState,
    /// Dial frequency set via the `/f` command (MHz→Hz), shown in the header in
    /// place of the rig readout. Mock feedback until rig wiring lands.
    vfo_override_hz: Option<u64>,
}

impl Waterfall {
    pub fn new() -> Self {
        Self {
            slide: WaterslidePanel::new(7200.0),
            spectro: Spectrogram::new(),
            form: ConfigForm::default(),
            send: SendState::default(),
            vfo_override_hz: None,
        }
    }

    /// The bottom "Send" row: `Send:` label, black message box (mirrors the next
    /// outgoing message), and a right-aligned Scan-style lit key — orange `SEND`
    /// when idle/armed, cyan `CANCEL` while transmitting (cyan also signals the
    /// armed state). The box is not a free text field: only `/`/`:` (start a slash
    /// command) and Enter (activate the button) are accepted; see `send.rs`.
    fn draw_send_row(&mut self, ctx: &mut PanelCtx, row: Rect) {
        // The operator's configured station identity (top-bar, env-seeded).
        let (mycall, mygrid) = (ctx.call, ctx.grid);

        // Keyboard: only the active panel acts on typed input / Enter. `/` or `:`
        // begins a slash command; Backspace/Escape edit/abort it; Enter activates
        // (run the command, else toggle arm). Anything else is ignored.
        let mut activate = false;
        if ctx.active {
            let events = ctx.ui.input(|i| i.events.clone());
            for ev in &events {
                match ev {
                    egui::Event::Text(t) => self.send.type_text(t),
                    egui::Event::Key {
                        key: egui::Key::Enter,
                        pressed: true,
                        ..
                    } => activate = true,
                    egui::Event::Key {
                        key: egui::Key::Backspace,
                        pressed: true,
                        ..
                    } => self.send.backspace(),
                    egui::Event::Key {
                        key: egui::Key::Escape,
                        pressed: true,
                        ..
                    } => self.send.escape(),
                    _ => {}
                }
            }
        }

        // Keep the buffer mirroring the would-be next message as a preview (unless
        // mid-command); the engine's authored message takes over once it's running.
        let target = self.slide.outgoing().clone();
        self.send.refresh_auto(&target, mycall, mygrid);

        // Live QSO-engine state drives the display and the button.
        let qso = ctx.bus.qso_state();
        let phase = qso.as_ref().map(|s| s.phase).unwrap_or(QsoPhase::Idle);
        let active_qso = !matches!(phase, QsoPhase::Idle);
        // What to show in the box: a command being typed > the engine's queued
        // message > the local preview.
        let display = if self.send.entering {
            self.send.buf.clone()
        } else if let Some(text) = qso
            .as_ref()
            .and_then(|s| s.next_tx.as_ref())
            .map(|m| &m.text)
        {
            text.clone()
        } else {
            self.send.buf.clone()
        };

        let pal = ctx.pal;
        let painter = ctx.painter;

        // Layout: [Send:] [────── box ──────] [ SEND ]
        let pad = 8.0;
        let label = "Send:";
        let label_font = mono(11.0);
        let label_w = measure(painter, label, label_font.clone());
        let cy = row.center().y;

        painter.text(
            Pos2::new(row.left() + pad, cy),
            Align2::LEFT_CENTER,
            label,
            label_font,
            pal.sub,
        );

        // Scan-style lit key (lcd track + key_cell), sized to its label. SEND when
        // idle; STOP once the engine is armed/calling/in an exchange (the single
        // Stop control).
        let btn_label = if active_qso { "STOP" } else { "SEND" };
        let cell_w = measure(painter, &tracked(btn_label), heading_bold(9.0)) + 22.0;
        let track_w = cell_w + 4.0;
        let track = Rect::from_min_max(
            Pos2::new(row.right() - pad - track_w, cy - 11.0),
            Pos2::new(row.right() - pad, cy + 11.0),
        );
        let box_left = row.left() + pad + label_w + pad;
        let box_rect = Rect::from_min_max(
            Pos2::new(box_left, cy - 11.0),
            Pos2::new(track.left() - pad, cy + 11.0),
        );

        // Black message box with a 1px edge; text vertically centered.
        painter.rect_filled(box_rect, corner_radius(2), Color32::BLACK);
        painter.rect_stroke(
            box_rect,
            corner_radius(2),
            egui::Stroke::new(1.0, pal.edge),
            egui::StrokeKind::Inside,
        );
        let text_color = if active_qso { pal.accent2 } else { pal.body };
        painter.with_clip_rect(box_rect).text(
            Pos2::new(box_rect.left() + 6.0, cy),
            Align2::LEFT_CENTER,
            &display,
            egui::FontId::monospace(12.0),
            text_color,
        );

        // Lit key in its recessed track. Cyan fill while armed/transmitting.
        lcd_panel(painter, track, pal, 4);
        let cell = Rect::from_min_max(
            Pos2::new(track.left() + 2.0, track.top() + 2.0),
            Pos2::new(track.right() - 2.0, track.bottom() - 2.0),
        );
        let accent = if active_qso { pal.accent2 } else { pal.accent };
        let btn = key_cell_accent(
            ctx.ui,
            painter,
            pal,
            cell,
            btn_label,
            true,
            accent,
            ctx.ui.id().with("ft8_send_btn"),
        );

        // Enter or a button click activates: apply a slash command, else toggle
        // the engine. Toggle resolves against the live phase: abort if the engine
        // is busy, otherwise arm — answer the selected station, or call CQ on bare
        // spectrum. Offsets carry the sign the waterslide uses (audio offset Hz).
        if activate || btn.clicked() {
            match self.send.activate() {
                Activation::Command(Command::SetFrequency(mhz)) => {
                    self.vfo_override_hz = Some((mhz * 1_000_000.0).round() as u64);
                }
                Activation::Toggle => {
                    if active_qso {
                        ctx.bus.abort_qso();
                    } else if let Some(call) = target.station() {
                        ctx.bus
                            .answer_station(target.off() as f32, call.to_string());
                    } else {
                        ctx.bus.call_cq(target.off() as f32);
                    }
                }
                Activation::None => {}
            }
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
        panel_header(
            painter,
            header,
            pal,
            "FT8",
            "0–3000 Hz · time → left",
            ctx.active,
        );
        // right side: prominent tuned-frequency readout
        let cy = header.center().y;
        let mut rx = header.right() - 2.0;
        painter.text(
            Pos2::new(rx, cy),
            Align2::RIGHT_CENTER,
            "MHz",
            mono(8.5),
            pal.sub,
        );
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
            let vfo_hz = self
                .vfo_override_hz
                .or_else(|| ctx.bus.rig_state().map(|r| r.vfo.0))
                .unwrap_or(14_074_000);
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

        // send row (bottom) + screen (fills between header and the send row).
        let send_row = Rect::from_min_max(
            Pos2::new(block.left(), block.bottom() - pd::TICKER_H),
            block.max,
        );
        let screen = Rect::from_min_max(
            Pos2::new(block.left(), header.bottom() + pd::HEADER_GAP),
            Pos2::new(block.right(), send_row.top() - pd::GAP),
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
            // Locked: re-locking the GUI commits any edits made while unlocked.
            // `form.loaded` is set only after the form was shown (real mode), so
            // this fires once on the unlock→lock transition. Only apply on an
            // actual change, so re-locking without edits doesn't force a reconnect.
            if self.form.loaded {
                let edited = self.form.to_config();
                if edited != ctx.bus.current_config() {
                    ctx.bus.apply_config(edited);
                }
                self.form.loaded = false; // re-sync to applied config on next unlock
            }

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
                    self.spectro.update_and_paint(
                        ctx.ui,
                        right,
                        ctx.dt,
                        ctx.bus.spectrum().as_ref(),
                        &cmap,
                    );
                    // Left half: decodes sliding left from centre, drawn over the
                    // spectrogram (graticule, NOW line, and Hz labels included). A
                    // click selects a station/offset; feed it to the send row's target.
                    let outgoing = self.slide.outgoing().clone();
                    let armed = ctx
                        .bus
                        .qso_state()
                        .map(|s| !matches!(s.phase, QsoPhase::Idle))
                        .unwrap_or(false);
                    let tx = TxLane {
                        target: &outgoing,
                        bandwidth_hz: signal_bandwidth_hz(ctx.bus.current_config().protocol),
                        armed,
                    };
                    if let Some(t) =
                        draw_waterslide(ctx.ui, body, pal, &ctx.bus.recent_decodes(64), now_ms, tx)
                    {
                        self.slide.set_outgoing(t);
                    }
                    ctx.ui
                        .ctx()
                        .request_repaint_after(std::time::Duration::from_millis(33));
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

        // The send row is the operating control, shown only when locked. When
        // unlocked the bottom strip is the settings/edit surface, not the radio.
        // The transmit lifecycle now lives in the QSO engine, which the row reads
        // and commands (no local arm cadence to step).
        if !ctx.unlocked {
            self.draw_send_row(ctx, send_row);
        }
    }
}

/// Audio-offset axis span (Hz): FT8/FT4 decodes land in roughly 0..3000 Hz.
const WS_MAX_HZ: f32 = 3000.0;

/// Occupied bandwidth (Hz) of one transmission in the given mode — `num_tones ×
/// tone_spacing` (FT8: 8 × 6.25 ≈ 50 Hz; FT4: 4 × 20.83 ≈ 83 Hz). Used to size
/// the outgoing-frequency lane so it matches a real signal's footprint.
fn signal_bandwidth_hz(protocol: Protocol) -> f32 {
    match protocol {
        Protocol::Ft8 => 50.0,
        Protocol::Ft4 => 83.0,
    }
}

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
                self.intensity
                    .copy_within(base..base + (self.w - dx), base + dx);
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
            None => {
                self.tex = Some(
                    ui.ctx()
                        .load_texture("spectrogram", img, TextureOptions::LINEAR),
                )
            }
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

/// Decode-text size on the waterslide scales with pane height, clamped to this
/// band: `MIN_FONT_PT` (the app-wide floor) up to `WS_MSG_FONT_MAX`. The size is
/// tuned against `WS_REF_H` — at that pane height the message renders at 12 pt.
const WS_MSG_FONT_MAX: f32 = 12.0;
const WS_REF_H: f32 = 460.0;

/// A decode positioned on the waterslide. `true_y` is the audio-offset lane (the
/// frequency the signal actually sits on); `final_y` is where the text is drawn
/// after de-collision. Click-to-tune must read `true_y`/the offset, never `final_y`.
struct Placed {
    idx: usize, // index into the `decodes` slice
    x: f32,
    true_y: f32,
    final_y: f32,
    slot: i64, // decode timestamp (FT8 slot start) — decodes sharing one form a column
}

/// The next-TX lane overlaid on the waterslide: where it sits (`target`), how
/// wide the on-air signal is (`bandwidth_hz`), and whether we're armed to
/// transmit — which tints it cyan (armed) vs amber (idle), mirroring SEND/STOP.
struct TxLane<'a> {
    target: &'a Target,
    bandwidth_hz: f32,
    armed: bool,
}

/// The "waterslide" decode view: each decode placed by audio offset (vertical)
/// and age (horizontal), newest at the centre NOW line and sliding left as it
/// ages. The right (FFT) half is left blank until a spectrum producer is wired.
/// Fed by `BusView::recent_decodes`, i.e. the real decoder's bus stream.
///
/// `tx` is the current next-TX target, drawn as a lane so the operator sees
/// where/whom they're set to call. Returns `Some(target)` when the operator
/// clicked: a decoded line snaps to that station (call + its true audio offset),
/// priming a reply; bare spectrum snaps to a plain offset. The snap always reads
/// the decode's *true* audio offset, never the de-collided text position.
fn draw_waterslide(
    ui: &egui::Ui,
    rect: Rect,
    pal: &Palette,
    decodes: &[Decode],
    now_ms: i64,
    tx: TxLane,
) -> Option<Target> {
    // A click anywhere in the body tunes the next TX; landing on a decoded line
    // snaps to that station. The cursor hints the lane is interactive.
    let resp = ui
        .interact(rect, ui.id().with("ws_live_tune"), egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let click_pos = resp.clicked().then(|| resp.interact_pointer_pos()).flatten();
    // A click that hits a decoded line records (call, true-offset); resolved after
    // the draw loop has the text rects to hit-test against.
    let mut snap: Option<(Option<String>, i32)> = None;

    // Same layer as `ctx.painter` (which is `ui.painter().clone()`) and the
    // spectrogram drawn just before — clipped to the body rect.
    let painter = ui.painter_at(rect);
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

    // Text size scales with pane height but is clamped to a readable band, so a
    // tall pane doesn't bloat it and a short one doesn't shrink it past legibility
    // (`MIN_FONT_PT` is the app-wide floor — see `theme.rs`). The de-collision
    // gap below is keyed to the resulting line height.
    let msg_pt = (12.0 * rect.height() / WS_REF_H).clamp(MIN_FONT_PT, WS_MSG_FONT_MAX);
    let snr_pt = (msg_pt - 1.5).max(MIN_FONT_PT);
    let line_h = msg_pt * 1.25; // minimum vertical spacing between two decode centres

    // The decode/TX offset is the signal's *lowest* tone, so its energy sits
    // `bandwidth/2` above the offset. Centre the text there to line it up with
    // the spectrogram trace (and the TX lane). The click target stays the raw
    // offset (the second pass snaps to `d.offset`), not this visual centre.
    let half_bw = tx.bandwidth_hz * 0.5;

    // First pass: gather every on-screen decode with its *true* y (the signal's
    // vertical centre). `final_y` starts equal and is nudged below for legibility;
    // the true y is kept so we can draw a leader back to it when the text bumps.
    let mut placed: Vec<Placed> = Vec::new();
    for (idx, d) in decodes.iter().enumerate() {
        let age = (now_ms - d.t.0) as f32 / 1000.0;
        if age < 0.0 {
            continue; // a clock skew put it in the (blank) future half
        }
        let x = now_x - age * pps;
        if x < rect.left() {
            continue; // scrolled off the left edge
        }
        let ty = y_of(d.offset.0 + half_bw);
        placed.push(Placed {
            idx,
            x,
            true_y: ty,
            final_y: ty,
            slot: d.t.0,
        });
    }

    // De-collide one slot-column at a time: decodes sharing a slot land at the
    // same x, so two close offsets overlap. We bump them apart *within* the
    // column (sorted top-down, pushed down by `line_h`, then the whole column
    // shifted up if it overflows the bottom) — never rearranging across columns,
    // so the rest of the display stays put (cf. DM780's full-reflow superbrowser).
    placed.sort_by(|a, b| a.slot.cmp(&b.slot).then(a.true_y.total_cmp(&b.true_y)));
    let mut i = 0;
    while i < placed.len() {
        let slot = placed[i].slot;
        let mut j = i + 1;
        while j < placed.len() && placed[j].slot == slot {
            j += 1;
        }
        for k in (i + 1)..j {
            let floor = placed[k - 1].final_y + line_h;
            if placed[k].final_y < floor {
                placed[k].final_y = floor;
            }
        }
        let overflow = placed[j - 1].final_y - (rect.bottom() - line_h * 0.5);
        if overflow > 0.0 {
            let top = rect.top() + line_h * 0.5;
            for p in &mut placed[i..j] {
                p.final_y = (p.final_y - overflow).max(top);
            }
        }
        i = j;
    }

    // Second pass: draw. Each message is left-aligned so it starts at its decode
    // time and reads rightward; the SNR sits just to its left. Both ride `final_y`.
    let msg_font = mono(msg_pt);
    let snr_font = mono(snr_pt);
    for p in &placed {
        let d = &decodes[p.idx];
        let strong = d.snr_db.map(|s| s > -12).unwrap_or(false);
        let msg_col = if strong { pal.body } else { pal.dim };
        let snr_col = if strong { pal.accent } else { pal.dim };
        let msg_rect = painter.text(
            Pos2::new(p.x, p.final_y),
            Align2::LEFT_CENTER,
            decode_text(d),
            msg_font.clone(),
            msg_col,
        );
        let snr = d.snr_db.map(fmt_snr).unwrap_or_else(|| "   ".into());
        painter.text(
            Pos2::new(msg_rect.left() - 6.0, p.final_y),
            Align2::RIGHT_CENTER,
            snr,
            snr_font.clone(),
            snr_col,
        );
        // Clicking this line targets its station, snapping to the *true* audio
        // offset (`d.offset`), never `final_y`. A free-text line with no callsign
        // still tunes the offset.
        if let Some(cp) = click_pos
            && msg_rect.expand(4.0).contains(cp)
        {
            snap = Some((target_call(&decode_text(d)), d.offset.0 as i32));
        }
        // Bumped off its lane: draw a faint leader from the true audio centre to
        // the shifted text so the eye still maps the row to its real frequency.
        if (p.final_y - p.true_y).abs() > 1.0 {
            let leader = snr_col.gamma_multiply(0.5);
            painter.line_segment(
                [Pos2::new(p.x - 4.0, p.true_y), Pos2::new(p.x - 4.0, p.final_y)],
                egui::Stroke::new(1.0, leader),
            );
            painter.line_segment(
                [Pos2::new(p.x - 6.0, p.true_y), Pos2::new(p.x - 2.0, p.true_y)],
                egui::Stroke::new(1.0, leader),
            );
        }
    }

    // NOW line at the centre.
    painter.line_segment(
        [
            Pos2::new(now_x, rect.top()),
            Pos2::new(now_x, rect.bottom()),
        ],
        egui::Stroke::new(2.0, pal.accent),
    );

    // Outgoing-frequency lane (matches the mock-mode indicator): a translucent
    // full-width band with bright rules top and bottom. It spans
    // [offset, offset + bandwidth] — the decode/TX offset is the signal's
    // *lowest* tone, so the energy sits above it (FT8 ≈ 50 Hz, FT4 ≈ 83 Hz);
    // basing the band there lines it up with the traffic on the spectrogram.
    // Cyan while armed to transmit, amber otherwise — same convention as
    // SEND/STOP. Labelled with the station being called (or a bare offset).
    let lane = if tx.armed { pal.accent2 } else { pal.accent };
    let off = tx.target.off() as f32;
    let bottom = y_of(off);
    let top = y_of(off + tx.bandwidth_hz).min(bottom - 3.0); // floor at 3px so it stays visible
    let band = Rect::from_min_max(Pos2::new(rect.left(), top), Pos2::new(rect.right(), bottom));
    painter.rect_filled(band, 0.0, lane.gamma_multiply(0.10));
    painter.line_segment(
        [band.left_top(), band.right_top()],
        egui::Stroke::new(1.5, lane),
    );
    painter.line_segment(
        [band.left_bottom(), band.right_bottom()],
        egui::Stroke::new(1.5, lane),
    );
    let tx_label = match tx.target {
        Target::Station { call, off } => format!("\u{25B6} {call}  {off} Hz"),
        Target::Offset(off) => format!("\u{25B6} TX {off} Hz"),
    };
    painter.text(
        Pos2::new(rect.left() + 4.0, band.top() - 1.0),
        Align2::LEFT_BOTTOM,
        tx_label,
        snr_font,
        lane,
    );

    // Resolve a click into the new outgoing target: a decoded line becomes a
    // Station (prime a reply to that call); empty spectrum becomes a bare Offset
    // read off the vertical position. Offsets are clamped to the visible axis.
    click_pos.map(|cp| match snap {
        Some((Some(call), off)) => Target::Station { call, off },
        Some((None, off)) => Target::Offset(off),
        None => {
            let off = ((rect.bottom() - cp.y) / rect.height() * WS_MAX_HZ).round();
            Target::Offset(off.clamp(0.0, WS_MAX_HZ) as i32)
        }
    })
}

// =====================================================================
// Radio / audio settings form (shown when the panel is unlocked)
// =====================================================================

/// Editable radio + audio settings shown in the unlocked FT8 panel body. Seeded
/// from the currently-applied config when the panel is unlocked; the edits are
/// committed when the GUI is re-locked (see the locked branch in `Panel::ui`),
/// which pushes them to the live producers via [`BusView::apply_config`].
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
            baud: DEFAULT_BAUD,
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
        ui.label(
            egui::RichText::new("RADIO SETUP")
                .color(pal.legend)
                .strong(),
        );

        // Audio device + decode mode are pushed to the live capture producer; in
        // WAV replay (or rig-only) there is none, so they're fixed at startup and
        // shown read-only rather than letting the operator edit dead controls.
        let live_audio = bus.has_live_audio();

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
                        ui.add_enabled_ui(live_audio, |ui| {
                            egui::ComboBox::from_id_salt("audio_input")
                                .selected_text(sel)
                                .width(240.0)
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(
                                        &mut self.audio_input,
                                        None,
                                        "(system default)",
                                    );
                                    for d in &self.audio_devices {
                                        ui.selectable_value(
                                            &mut self.audio_input,
                                            Some(d.clone()),
                                            d,
                                        );
                                    }
                                });
                        });
                        ui.end_row();

                        ui.label("Mode");
                        ui.add_enabled_ui(live_audio, |ui| {
                            egui::ComboBox::from_id_salt("mode")
                                .selected_text(proto_label(self.protocol))
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut self.protocol, Protocol::Ft8, "FT8");
                                    ui.selectable_value(&mut self.protocol, Protocol::Ft4, "FT4");
                                });
                        });
                        ui.end_row();

                        if !live_audio {
                            ui.label("");
                            ui.label(
                                egui::RichText::new("WAV replay — set at startup")
                                    .color(pal.sub)
                                    .italics(),
                            );
                            ui.end_row();
                        }

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
                if ui.button("Refresh devices").clicked() {
                    self.audio_devices = bus.audio_inputs();
                    self.serial_ports = bus.serial_ports();
                }
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new("Changes take effect when you lock the GUI.")
                        .color(pal.sub)
                        .italics(),
                );
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
fn draw_centered_note(
    painter: &egui::Painter,
    screen: Rect,
    pal: &Palette,
    title: &str,
    detail: &str,
) {
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
