//! Waterfall panel: header (Digital mode + freq readout + FT8/FT4 toggle) + the live Waterslide
//! FFT sim as the screen body + a decode ticker along the bottom.

use eframe::egui;
use egui::{Align2, Color32, ColorImage, Pos2, Rect, TextureHandle, TextureOptions};
use types::{
    Decode, DecodeContent, ExchangePayload, HealthState, OverAirMode, ParsedMessage, QsoPhase,
    Signoff, SlotId, SpectrumRow, SubsystemHealth, SubsystemId,
};

use app_core::{LineProfile, Protocol, SerialConfig};

use super::{Panel, PanelCtx};
use crate::bus_view::BusView;
use crate::chrome::{key_cell_accent, lcd_panel, measure, panel_header};
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
            ParsedMessage::Exchange { to, from, payload } => {
                format!("{} {} {}", to.0, from.0, fmt_payload(payload))
            }
            ParsedMessage::Signoff { to, from, kind } => {
                format!("{} {} {}", to.0, from.0, fmt_signoff(*kind))
            }
            ParsedMessage::Free(s) | ParsedMessage::Raw(s) => s.clone(),
        },
        DecodeContent::Streaming { text } => text.clone(),
    }
}

/// The exchange body as WSJT-X renders it: grid verbatim, reports as `%+2.2d`
/// (`-07`, `+05`), the roger form prefixed `R`, Field Day as `[R ]<class> <section>`.
fn fmt_payload(p: &ExchangePayload) -> String {
    match p {
        ExchangePayload::Grid(g) => g.0.clone(),
        ExchangePayload::Report(r) => format!("{r:+03}"),
        ExchangePayload::RogerReport(r) => format!("R{r:+03}"),
        ExchangePayload::FieldDay {
            class,
            section,
            rogered,
        } => format!("{}{class} {}", if *rogered { "R " } else { "" }, section.0),
    }
}

/// A sign-off rendered as its on-air token (not always `73`).
fn fmt_signoff(kind: Signoff) -> &'static str {
    match kind {
        Signoff::Rrr => "RRR",
        Signoff::Rr73 => "RR73",
        Signoff::Seven3 => "73",
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
    /// Dial frequency set via the `/f` / `/b` commands (Hz), shown in the header
    /// in place of the rig readout. Mock-mode feedback only — in real mode those
    /// commands retune the rig and the header tracks the resulting `RigState`.
    vfo_override_hz: Option<u64>,
    /// Real-mode selection (offset + optional station). Mock mode reads `slide`.
    real_sel: RealSel,
    /// The message latched on the air for the current over, held in the Send box
    /// until the transmission finishes — even after the engine has advanced its
    /// `next_tx` or gone idle (the final 73/RR73 keeps showing while it plays out).
    /// `None` when we're not transmitting. See `draw_send_row`.
    tx_hold: Option<String>,
    /// The engine's `next_tx` text observed on the previous frame, so when an over
    /// starts we can latch the message being sent even though the engine may have
    /// already stepped to idle by the time the own-TX waterfall reaches the GUI.
    last_next_tx: Option<String>,
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
            tx_hold: None,
            last_next_tx: None,
        }
    }

    /// The bottom "Send" row: `Send:` label, recessed message box (mirrors the
    /// next outgoing message, themed to the screen background), and a right-aligned
    /// Scan-style lit key — orange `SEND` when idle/armed, cyan `CANCEL` while
    /// transmitting (cyan also signals the armed state). The box is not a free text
    /// field: only `/`/`:` (start a slash command) and Enter (activate the button)
    /// are accepted; see `send.rs`.
    fn draw_send_row(&mut self, ctx: &mut PanelCtx, row: Rect) {
        // The operator's configured station identity. There is no default, so gate
        // operating until a callsign is set (top bar when unlocked, or the config file).
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
        // The engine's queued message for the next over, if any.
        let next_tx = qso
            .as_ref()
            .and_then(|s| s.next_tx.as_ref())
            .map(|m| m.text.clone());

        // Hold the on-air message in the box for the whole over. We're transmitting
        // while a fresh own-TX waterfall column is arriving (the same signal the
        // screen uses to swap to the own-TX waterfall). On the first frame of an
        // over we latch the message being sent — preferring the live `next_tx`,
        // else the one shown last frame, since by the time the own-TX column reaches
        // the GUI the engine may already have stepped to idle (the final 73/RR73).
        let now_ms = chrono::Utc::now().timestamp_millis();
        let transmitting = ctx.bus.tx_spectrum().is_some_and(|r| now_ms - r.t.0 < 500);
        if transmitting {
            if self.tx_hold.is_none() {
                self.tx_hold = next_tx.clone().or_else(|| self.last_next_tx.clone());
            }
        } else {
            self.tx_hold = None;
        }
        self.last_next_tx = next_tx.clone();

        // Treat an in-flight over as active even if the engine has already gone
        // idle, so the box stays highlighted and the button reads STOP until the
        // over finishes playing out.
        let active_qso = !matches!(phase, QsoPhase::Idle) || self.tx_hold.is_some();
        // What to show in the box: a command being typed > the message on the air >
        // the engine's queued message > the local preview.
        let display = if self.send.entering {
            self.send.buf.clone()
        } else if !call_set {
            "SET CALLSIGN — unlock (GUI ▸ EDIT) or set the config file".to_string()
        } else if let Some(text) = self.tx_hold.clone().or(next_tx) {
            text
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

        // Recessed message box (themed to the screen background) with a 1px edge;
        // text vertically centered.
        painter.rect_filled(box_rect, corner_radius(2), pal.screen_bg);
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
                Activation::Command(cmd) => self.apply_command(ctx, cmd),
                Activation::Toggle => {
                    if !call_set {
                        // No station callsign yet — operating is blocked until one
                        // is set (top bar when unlocked, or the config file).
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

    /// Apply a parsed slash command. `/f` takes an explicit dial frequency; `/b`
    /// resolves a band to its calling frequency for the *current* over-air mode
    /// (FT8 and FT4 differ — e.g. 20 m is 14.074 vs 14.080). In real mode the rig
    /// is retuned and the header tracks the resulting `RigState`; in mock mode
    /// there's no rig, so we set the local display override for feedback.
    fn apply_command(&mut self, ctx: &PanelCtx, cmd: Command) {
        let hz = match cmd {
            Command::SetFrequency(mhz) => Some((mhz * 1_000_000.0).round() as u64),
            Command::SetBand(band) => {
                let mode = ctx
                    .bus
                    .spectrum()
                    .map(|s| s.mode)
                    .unwrap_or(OverAirMode::Ft8);
                crate::send::calling_freq_hz(band, mode)
            }
        };
        if let Some(hz) = hz {
            if ctx.bus.is_real() {
                ctx.bus.set_freq(hz);
            } else {
                self.vfo_override_hz = Some(hz);
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
        panel_header(painter, header, pal, "Digital", "", ctx.active);

        // Right cluster, laid out right-to-left: the FT8/FT4 mode toggle, then the
        // tuned-frequency readout styled like the header clocks.
        let cy = header.center().y;
        let proto = ctx.bus.current_config().protocol;
        let (_mode_left, mode_clicks) = crate::chrome::segmented(
            ctx.ui,
            painter,
            pal,
            header.right() - 2.0,
            cy,
            20.0,
            "",
            &[
                ("FT8", proto == Protocol::Ft8),
                ("FT4", proto == Protocol::Ft4),
            ],
            "sw_mode",
        );
        if mode_clicks[0] {
            ctx.bus.set_protocol(Protocol::Ft8);
        }
        if mode_clicks[1] {
            ctx.bus.set_protocol(Protocol::Ft4);
        }

        // Tuned-frequency readout (FREQ chip), centered in the header bar like the
        // top-bar clocks. When the rig is faulted, show a dashed placeholder rather
        // than a stale freq.
        let rig_fault = ctx.bus.is_real()
            && ctx
                .bus
                .health(SubsystemId::Rig)
                .map(|h| h.is_faulted())
                .unwrap_or(false);
        let vfo_text = if rig_fault {
            "---.---.--".to_string()
        } else {
            let hz = self
                .vfo_override_hz
                .or_else(|| ctx.bus.rig_state().map(|r| r.vfo.0))
                .unwrap_or(14_074_000);
            // MHz.kHz.daHz grouping, matching the rig's front panel (10 Hz step).
            format!(
                "{}.{:03}.{:02}",
                hz / 1_000_000,
                hz % 1_000_000 / 1_000,
                hz % 1_000 / 10
            )
        };
        crate::chrome::lcd_readout(
            painter,
            pal,
            header.center().x,
            cy,
            20.0,
            "FREQ",
            &vfo_text,
            "MHz",
            13.0,
            80.0,
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
                        "available in real mode — relaunch without DM420_MOCK=1",
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
                    // Persist the audio + serial settings to the config file, then apply.
                    crate::settings::save_hardware_config(&edited);
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
                    // each spanning `ws_secs`, so the scroll rates match.
                    let now_x = body.center().x;
                    let right = Rect::from_min_max(Pos2::new(now_x, body.top()), body.max);
                    let cmap = if pal.is_dark {
                        crate::waterslide_panel::martian_cmap()
                    } else {
                        crate::waterslide_panel::martian_cmap_light()
                    };
                    // While keyed, show our own-TX waterfall (the outgoing signal at
                    // its true offset) in place of the RX one, which is meaningless
                    // during an over. A fresh own-TX column means we're transmitting;
                    // otherwise fall back to the RX waterfall. Both share the buffer,
                    // so the timeline reads RX … my over … RX as it scrolls.
                    let tx_col = ctx.bus.tx_spectrum().filter(|r| now_ms - r.t.0 < 500);
                    let column = tx_col.or_else(|| ctx.bus.spectrum());
                    // Scroll speed: set so one decode line clears as the next slot's
                    // lands. Both halves share this span so their pixels-per-second
                    // match (see `ws_history_secs`). FT4's shorter slots scroll faster.
                    let protocol = ctx.bus.current_config().protocol;
                    let ws_secs = ws_history_secs(painter, body, protocol);
                    self.spectro
                        .update_and_paint(ctx.ui, right, ctx.dt, ws_secs, column.as_ref(), &cmap);
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
                    // returned selection (if the click resolved to one) is stored. The
                    // TX lane is sized to the on-air signal and tinted by armed state.
                    let bandwidth_hz = signal_bandwidth_hz(protocol);
                    let armed = !matches!(phase, QsoPhase::Idle);
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
                        bandwidth_hz,
                        ws_secs,
                        app_core::slot_period(protocol) as f32,
                        armed,
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

        // Publish the selected station so other panels (the Contacts map) can
        // highlight it. Real mode reads the live `real_sel`; mock mode the sim's
        // outgoing target. `None` when a bare offset or nothing is selected.
        *ctx.selected_station = self.selected_call(ctx.bus.is_real());
    }
}

impl Waterfall {
    /// The callsign currently selected in the waterslide (the station to work), or
    /// `None` when the selection is a bare spectrum offset or nothing is selected.
    /// Real mode reads `real_sel`; mock mode the simulation's outgoing target.
    fn selected_call(&self, real: bool) -> Option<String> {
        if real {
            self.real_sel.target.as_ref().map(|(c, _)| c.clone())
        } else {
            self.slide.outgoing().station().map(str::to_owned)
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
/// each spans the same `history_secs`, so the on-screen pixels-per-second match.
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
        history_secs: f32,
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
        self.dx_frac += dt * (self.w as f64 / history_secs as f64);
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
/// A representative decode line (SNR + a typical exchange) used to gauge how wide
/// one message renders in the current font. The font is monospaced, so this is an
/// exact stand-in for any same-length line; longer/shorter real messages vary a
/// little, which is fine — the scroll speed only needs to be approximately right.
const WS_REF_MSG: &str = "−15 KX1ABC W4XYZ R−12";

/// Seconds of decode history the left (text) half spans (NOW at centre → oldest at
/// left), chosen so one message clears as the next renders into place. We render
/// `WS_REF_MSG` in the current font to get its pixel width `msg_w`, then scroll at
/// `msg_w / slot_period` px/s — i.e. a message travels its own width in one slot,
/// so the previous slot's line has moved off NOW before the next lands on top of
/// it. That makes `history_secs = (left_w / msg_w) * slot_period`: faster for FT4
/// (7.5 s slots), slower for FT8 (15 s), and it tracks font/window size so roughly
/// the same number of messages always fit across the half.
fn ws_history_secs(painter: &egui::Painter, body: Rect, protocol: Protocol) -> f32 {
    let msg_pt = (WS_MSG_FONT_MAX * body.height() / WS_REF_H).clamp(MIN_FONT_PT, WS_MSG_FONT_MAX);
    let msg_w = measure(painter, WS_REF_MSG, mono(msg_pt)).max(1.0);
    let left_w = (body.width() * 0.5).max(1.0);
    (left_w / msg_w) * app_core::slot_period(protocol) as f32
}

/// Decode-text size on the waterslide scales with pane height, clamped to this
/// band: `MIN_FONT_PT` (the app-wide floor) up to `WS_MSG_FONT_MAX`. The size is
/// tuned against `WS_REF_H` — at that pane height the message renders at the
/// `WS_MSG_FONT_MAX` ceiling. Both `ws_history_secs` and `draw_waterslide` derive
/// the font size from this constant, so scroll calibration tracks any change to it.
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
/// decoded line selects that station (snapping to its *true* audio offset, never
/// the de-collided text position), anything else a bare TX offset — returned as
/// the new [`RealSel`]. `tx_off` is the current outgoing offset (marked as the TX
/// lane), `sel_call`/`tag` highlight + label the selected station's lane, and
/// `bandwidth_hz`/`armed` size and tint that lane to the on-air signal.
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
    bandwidth_hz: f32,
    history_secs: f32,
    slot_secs: f32,
    armed: bool,
) -> Option<RealSel> {
    let painter = painter.with_clip_rect(rect);
    let now_x = rect.center().x; // the NOW line
    let left_w = (now_x - rect.left()).max(1.0);
    let pps = left_w / history_secs; // pixels per second of history
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

    // Slot-interval dividers: a thin accent rule at each transmit-slot boundary,
    // marching out of NOW as the lane scrolls. Boundaries align to real slot starts
    // (multiples of the slot period in UTC), so a rule lands where each decode
    // column does — separating the messages into discrete intervals. The spectrogram
    // (right half) mirrors the text lane about NOW, so each boundary draws twice:
    // left at `now_x - age·pps`, and its mirror over the spectrogram at `now_x + age·pps`.
    let slot_ms = (slot_secs as f64 * 1000.0) as i64;
    if slot_ms > 0 {
        let stroke = egui::Stroke::new(1.0, pal.accent.gamma_multiply(0.5));
        let mut t = (now_ms / slot_ms) * slot_ms; // most recent boundary ≤ now
        loop {
            let dx = ((now_ms - t) as f32 / 1000.0) * pps; // distance from NOW
            let (xl, xr) = (now_x - dx, now_x + dx);
            if xl < rect.left() {
                break;
            }
            painter.line_segment([Pos2::new(xl, rect.top()), Pos2::new(xl, rect.bottom())], stroke);
            if xr <= rect.right() {
                painter.line_segment([Pos2::new(xr, rect.top()), Pos2::new(xr, rect.bottom())], stroke);
            }
            t -= slot_ms;
        }
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

    // Text size scales with pane height but is clamped to a readable band, so a
    // tall pane doesn't bloat it and a short one doesn't shrink it past legibility
    // (`MIN_FONT_PT` is the app-wide floor — see `theme.rs`). The de-collision
    // gap below is keyed to the resulting line height.
    let msg_pt = (WS_MSG_FONT_MAX * rect.height() / WS_REF_H).clamp(MIN_FONT_PT, WS_MSG_FONT_MAX);
    let snr_pt = (msg_pt - 1.5).max(MIN_FONT_PT);
    let line_h = msg_pt * 1.25; // minimum vertical spacing between two decode centres

    // The decode/TX offset is the signal's *lowest* tone, so its energy sits
    // `bandwidth/2` above the offset. Centre the text there to line it up with
    // the spectrogram trace (and the TX lane). The click target stays the raw
    // offset (the second pass snaps to `d.offset`), not this visual centre.
    let half_bw = bandwidth_hz * 0.5;

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
        let station = decode_station(d);
        // A decode from the selected station reads in the secondary accent so the
        // whole lane is easy to follow. All decodes render at full brightness (no
        // SNR-based dimming).
        let is_sel = match (&station, sel_call) {
            (Some((c, _)), Some(s)) => c.as_str() == s,
            _ => false,
        };
        let msg_col = if is_sel { pal.accent2 } else { pal.body };
        let snr_col = if is_sel { pal.accent2 } else { pal.accent };
        let msg_rect = painter.text(
            Pos2::new(p.x, p.final_y),
            Align2::LEFT_CENTER,
            decode_text(d),
            msg_font.clone(),
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
            Pos2::new(msg_rect.left() - 6.0, p.final_y),
            Align2::RIGHT_CENTER,
            snr,
            snr_font.clone(),
            snr_col,
        );
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
    // SEND/STOP. Labelled with the selected station's QSO phase (`tag`) or a
    // bare offset.
    let lane = if armed { pal.accent2 } else { pal.accent };
    let bottom = y_of(tx_off);
    let top = y_of(tx_off + bandwidth_hz).min(bottom - 3.0); // floor at 3px so it stays visible
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
    let tx_label = match tag {
        Some(tag) => format!("{tag}  {} Hz", tx_off as i32),
        None => format!("\u{25B6} TX {} Hz", tx_off as i32),
    };
    painter.text(
        Pos2::new(rect.left() + 4.0, band.top() - 1.0),
        Align2::LEFT_BOTTOM,
        tx_label,
        snr_font,
        lane,
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
        // Capture the chosen device's stable USB identity (vid/pid/serial) so a
        // later replug — which renumbers the `/dev/cu.usbserial-*` path — still
        // resolves to the same radio. The path is kept as a fallback hint.
        let (usb_vid, usb_pid, usb_serial) = match self.port.as_deref() {
            Some(p) if !p.is_empty() => app_core::usb_identity_for_port(p),
            _ => (None, None, None),
        };
        HardwareConfig {
            audio_input: self.audio_input.clone(),
            audio_output: self.audio_output.clone(),
            serial: SerialConfig {
                port: self.port.clone(),
                usb_serial,
                usb_vid,
                usb_pid,
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
