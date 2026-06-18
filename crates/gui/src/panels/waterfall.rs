//! Waterfall panel: header (FT8 + tuned-frequency readout) + the live Waterslide
//! FFT sim as the screen body + a decode ticker along the bottom.

use eframe::egui;
use egui::{Align2, Color32, ColorImage, Pos2, Rect, TextureHandle, TextureOptions};
use types::{
    Decode, DecodeContent, HealthState, ParsedMessage, QsoPhase, SlotId, SpectrumRow,
    SubsystemHealth, SubsystemId,
};

use app_core::{LineProfile, Protocol, SerialConfig};

use super::{Panel, PanelCtx};
use crate::bus_view::BusView;
use crate::chrome::{key_cell_accent, lcd_panel, measure, panel_header, shadow};
use crate::panel_data as pd;
use crate::send::{Activation, Command, SendState};
use crate::settings::{DEFAULT_BAUD, HardwareConfig, KENWOOD_BAUDS};
use crate::theme::*;
use crate::waterslide_panel::{Target, WaterslidePanel, WaterslideTheme};

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

/// The real-mode click selection tracked by the panel. The live waterslide is
/// draw-only (the mock sim's `ui()` owns selection in mock mode), so in real mode
/// the panel records the clicked target itself.
#[derive(Clone)]
struct RealSel {
    /// Outgoing TX audio offset (Hz, absolute 0..3000) — where the next CQ/answer
    /// transmits. Kept distinct from the dial/centre frequency.
    offset: f32,
    /// The station to work (its base call + the slot its decode landed in, for the
    /// real `DecodeRef`) when a decoded line was clicked rather than bare spectrum.
    target: Option<(String, SlotId)>,
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
    /// Real-mode selection (offset + optional station). Mock mode reads `slide`.
    real_sel: RealSel,
}

impl Waterfall {
    pub fn new() -> Self {
        Self {
            slide: WaterslidePanel::new(7200.0),
            spectro: Spectrogram::new(),
            form: ConfigForm::default(),
            send: SendState::default(),
            vfo_override_hz: None,
            real_sel: RealSel {
                offset: 1500.0,
                target: None,
            },
        }
    }

    /// The bottom "Send" row: `Send:` label, black message box (mirrors the next
    /// outgoing message), and a right-aligned Scan-style lit key — orange `SEND`
    /// when idle/armed, cyan `CANCEL` while transmitting (cyan also signals the
    /// armed state). The box is not a free text field: only `/`/`:` (start a slash
    /// command) and Enter (activate the button) are accepted; see `send.rs`.
    fn draw_send_row(&mut self, ctx: &mut PanelCtx, row: Rect) {
        // The operator's configured station identity. There is no default, so gate
        // operating until a callsign is set (top bar when unlocked, or dm420.toml).
        let (mycall, mygrid) = (ctx.call, ctx.grid);
        let call_set = !mycall.trim().is_empty();

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

        // Where we're pointed. In real mode the panel owns the click selection (the
        // live waterslide is draw-only); in mock mode the sim does. Resolve to a TX
        // offset, the station to work (if a decoded line was clicked), and that
        // decode's slot (threaded into the real `DecodeRef`).
        let (sel_off, sel_call, sel_slot) = if ctx.bus.is_real() {
            match &self.real_sel.target {
                Some((call, slot)) => (self.real_sel.offset, Some(call.clone()), *slot),
                None => (self.real_sel.offset, None, SlotId(0)),
            }
        } else {
            let t = self.slide.outgoing();
            (t.off() as f32, t.station().map(str::to_string), SlotId(0))
        };

        // Keep the buffer mirroring the would-be next message as a preview (unless
        // mid-command); the engine's authored message takes over once it's running.
        let preview = match &sel_call {
            Some(call) => Target::Station {
                call: call.clone(),
                off: sel_off as i32,
            },
            None => Target::Offset(sel_off as i32),
        };
        self.send.refresh_auto(&preview, mycall, mygrid);

        // Live QSO-engine state drives the display and the button.
        let qso = ctx.bus.qso_state();
        let phase = qso.as_ref().map(|s| s.phase).unwrap_or(QsoPhase::Idle);
        let active_qso = !matches!(phase, QsoPhase::Idle);
        // What to show in the box: a command being typed > the engine's queued
        // message > the local preview.
        let display = if self.send.entering {
            self.send.buf.clone()
        } else if !call_set {
            "SET CALLSIGN — unlock (GUI ▸ EDIT) or set dm420.toml".to_string()
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
        let btn_label = if !call_set {
            "SET CALL"
        } else if active_qso {
            "STOP"
        } else {
            "SEND"
        };
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
        // is busy, otherwise arm — answer the selected station (threading its real
        // slot), or call CQ on bare spectrum. The offset/target come from the
        // resolved selection above (real-mode click state, or the mock sim).
        if activate || btn.clicked() {
            match self.send.activate() {
                Activation::Command(Command::SetFrequency(mhz)) => {
                    self.vfo_override_hz = Some((mhz * 1_000_000.0).round() as u64);
                }
                Activation::Toggle => {
                    if !call_set {
                        // No station callsign yet — operating is blocked until one
                        // is set (top bar when unlocked, or dm420.toml).
                    } else if active_qso {
                        ctx.bus.abort_qso();
                    } else if let Some(call) = &sel_call {
                        ctx.bus.answer_station(sel_off, call.clone(), sel_slot);
                    } else {
                        ctx.bus.call_cq(sel_off);
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
                    // Click-to-select on the live waterslide (mock mode selects via
                    // the sim's own `ui()`; the real waterslide is draw-only). We
                    // hit-test here and let `draw_waterslide` resolve the click to a
                    // station (decoded line) or a bare TX offset (empty spectrum).
                    let resp = ctx
                        .ui
                        .interact(body, ctx.ui.id().with("ws_select"), egui::Sense::click())
                        .on_hover_cursor(egui::CursorIcon::PointingHand);
                    let click = resp.clicked().then(|| resp.interact_pointer_pos()).flatten();

                    // Selection feedback: highlight the selected station's lane and
                    // tag it with the live QSO phase (ARMED while waiting for its CQ,
                    // WORKING once the exchange is under way).
                    let phase = ctx
                        .bus
                        .qso_state()
                        .map(|s| s.phase)
                        .unwrap_or(QsoPhase::Idle);
                    let sel_call = self.real_sel.target.as_ref().map(|(c, _)| c.clone());
                    let tag = sel_call.as_deref().map(|c| match phase {
                        QsoPhase::Armed => format!("ARMED ▸ {c}"),
                        QsoPhase::InExchange { .. } => format!("WORKING ▸ {c}"),
                        _ => format!("▸ {c}"),
                    });

                    // Left half: decodes sliding left from centre, drawn over the
                    // spectrogram (graticule, NOW line, and Hz labels included). The
                    // returned selection (if the click resolved to one) is stored.
                    if let Some(sel) = draw_waterslide(
                        painter,
                        body,
                        pal,
                        &ctx.bus.recent_decodes(64),
                        now_ms,
                        click,
                        self.real_sel.offset,
                        sel_call.as_deref(),
                        tag.as_deref(),
                    ) {
                        self.real_sel = sel;
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

/// The station that originated a decode (its base call) plus the slot it landed
/// in, for building a real `DecodeRef`. `None` for free-text/streaming decodes,
/// which can't be worked.
fn decode_station(d: &Decode) -> Option<(String, SlotId)> {
    match &d.content {
        DecodeContent::Slotted { slot, message, .. } => {
            let call = match message {
                ParsedMessage::Cq { caller, .. } => Some(caller.0.clone()),
                ParsedMessage::Exchange { from, .. } => Some(from.0.clone()),
                ParsedMessage::Signoff { from, .. } => Some(from.0.clone()),
                ParsedMessage::Free(_) | ParsedMessage::Raw(_) => None,
            };
            call.map(|c| (c, *slot))
        }
        DecodeContent::Streaming { .. } => None,
    }
}

/// The "waterslide" decode view: each decode placed by audio offset (vertical)
/// and age (horizontal), newest at the centre NOW line and sliding left as it
/// ages. The right (FFT) half is left blank until a spectrum producer is wired.
/// Fed by `BusView::recent_decodes`, i.e. the real decoder's bus stream.
///
/// `click` is the pointer position of a click this frame (if any); a click on a
/// decoded line selects that station, anything else a bare TX offset — returned
/// as the new [`RealSel`]. `tx_off` is the current outgoing offset (marked), and
/// `sel_call`/`tag` highlight + label the selected station's lane.
#[allow(clippy::too_many_arguments)]
fn draw_waterslide(
    painter: &egui::Painter,
    rect: Rect,
    pal: &Palette,
    decodes: &[Decode],
    now_ms: i64,
    click: Option<Pos2>,
    tx_off: f32,
    sel_call: Option<&str>,
    tag: Option<&str>,
) -> Option<RealSel> {
    let painter = painter.with_clip_rect(rect);
    let now_x = rect.center().x; // the NOW line
    let left_w = (now_x - rect.left()).max(1.0);
    let pps = left_w / WS_HISTORY_SECS; // pixels per second of history
    let mut hit: Option<RealSel> = None;

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

    // Selected station's lane: a faint highlight band drawn behind the decodes so
    // the text stays readable on top of it.
    if tag.is_some() {
        let sy = y_of(tx_off);
        painter.rect_filled(
            Rect::from_min_max(
                Pos2::new(rect.left(), sy - 9.0),
                Pos2::new(rect.right(), sy + 9.0),
            ),
            corner_radius(2),
            pal.accent2.gamma_multiply(0.12),
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
        let station = decode_station(d);
        // A decode from the selected station reads in the secondary accent so the
        // whole lane is easy to follow.
        let is_sel = match (&station, sel_call) {
            (Some((c, _)), Some(s)) => c.as_str() == s,
            _ => false,
        };
        let msg_col = if is_sel { pal.accent2 } else { pal.body };
        let snr_col = if is_sel { pal.accent2 } else { pal.accent };
        let msg_rect = painter.text(
            Pos2::new(x, y),
            Align2::LEFT_CENTER,
            decode_text(d),
            mono(11.0),
            msg_col,
        );
        // A click on this line selects the station to work (and snaps the TX offset
        // to it); resolved after the loop. Unparsed lines can't be worked.
        if let Some(cp) = click
            && msg_rect.expand(4.0).contains(cp)
            && let Some((call, slot)) = &station
        {
            hit = Some(RealSel {
                offset: d.offset.0,
                target: Some((call.clone(), *slot)),
            });
        }
        let snr = d.snr_db.map(fmt_snr).unwrap_or_else(|| "   ".into());
        painter.text(
            Pos2::new(msg_rect.left() - 6.0, y),
            Align2::RIGHT_CENTER,
            snr,
            mono(9.5),
            snr_col,
        );
    }

    // Selected lane marker + tag (cyan), or the bare TX-offset marker (accent) when
    // only an offset is picked — so the operator always sees where the next
    // transmission goes and which station is armed.
    let ty = y_of(tx_off);
    match tag {
        Some(tag) => {
            painter.line_segment(
                [Pos2::new(rect.left(), ty), Pos2::new(rect.right(), ty)],
                egui::Stroke::new(1.0, pal.accent2.gamma_multiply(0.6)),
            );
            painter.text(
                Pos2::new(rect.right() - 4.0, ty - 2.0),
                Align2::RIGHT_BOTTOM,
                tag,
                mono(9.5),
                pal.accent2,
            );
        }
        None => {
            painter.line_segment(
                [Pos2::new(now_x, ty), Pos2::new(rect.right(), ty)],
                egui::Stroke::new(1.0, pal.accent.gamma_multiply(0.5)),
            );
            painter.text(
                Pos2::new(rect.right() - 4.0, ty + 2.0),
                Align2::RIGHT_TOP,
                "TX",
                mono(8.5),
                pal.accent,
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

    // Resolve a click into a new selection: a decoded station (captured in the
    // loop) wins; otherwise it's a bare TX offset read off the vertical position.
    if let Some(cp) = click {
        return Some(hit.unwrap_or_else(|| {
            let off = ((rect.bottom() - cp.y) / rect.height() * WS_MAX_HZ).clamp(0.0, WS_MAX_HZ);
            RealSel {
                offset: off,
                target: None,
            }
        }));
    }
    None
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
    /// TX audio output device (the rig's data-in); `None` = system default.
    audio_output: Option<String>,
    port: Option<String>,
    baud: u32,
    profile: LineProfile,
    autodetect: bool,
    protocol: Protocol,
    /// Cached device/port lists for the pickers (refreshed on load / Refresh).
    audio_devices: Vec<String>,
    audio_output_devices: Vec<String>,
    serial_ports: Vec<String>,
}

impl Default for ConfigForm {
    fn default() -> Self {
        Self {
            loaded: false,
            audio_input: None,
            audio_output: None,
            port: None,
            baud: DEFAULT_BAUD,
            profile: LineProfile::Default,
            autodetect: true,
            protocol: Protocol::Ft8,
            audio_devices: Vec::new(),
            audio_output_devices: Vec::new(),
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
        self.audio_output = cfg.audio_output;
        self.port = cfg.serial.port;
        self.baud = cfg.serial.baud;
        self.profile = cfg.serial.profile;
        self.autodetect = cfg.serial.autodetect;
        self.protocol = cfg.protocol;
        self.audio_devices = bus.audio_inputs();
        self.audio_output_devices = bus.audio_outputs();
        self.serial_ports = bus.serial_ports();
        self.loaded = true;
    }

    /// The edited fields as a `HardwareConfig` ready to apply.
    fn to_config(&self) -> HardwareConfig {
        HardwareConfig {
            audio_input: self.audio_input.clone(),
            audio_output: self.audio_output.clone(),
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

                        // TX audio output (the rig's data-in). Independent of
                        // capture, so it's always selectable in real mode.
                        ui.label("Audio output");
                        let out_sel = self
                            .audio_output
                            .clone()
                            .unwrap_or_else(|| "(system default)".into());
                        egui::ComboBox::from_id_salt("audio_output")
                            .selected_text(out_sel)
                            .width(240.0)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut self.audio_output,
                                    None,
                                    "(system default)",
                                );
                                for d in &self.audio_output_devices {
                                    ui.selectable_value(
                                        &mut self.audio_output,
                                        Some(d.clone()),
                                        d,
                                    );
                                }
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
                    self.audio_output_devices = bus.audio_outputs();
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
