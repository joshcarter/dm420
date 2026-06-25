//! Waterfall panel: header (Digital mode + freq readout + FT8/FT4 toggle) + the live Waterslide
//! FFT sim as the screen body + a decode ticker along the bottom.

use eframe::egui;
use egui::{Align2, Color32, ColorImage, Pos2, Rect, TextureHandle, TextureOptions};
use std::collections::{HashMap, HashSet};

use types::{
    AbsHz, Band, Callsign, Decode, DecodeContent, DecodeRef, HealthState, OffsetHz, OverAirMode,
    ParsedMessage, QsoPhase, Selection, SelectionContext, SlotId, SpectrumRow, SubsystemHealth,
    SubsystemId,
};

use app_core::{LineProfile, Protocol, SerialConfig};

use super::{Panel, PanelCtx};
use crate::bus_view::BusView;
use crate::chrome::{key_cell_accent, lcd_panel, measure, panel_header};
use crate::format::{decode_text, fmt_snr};
use crate::panel_data as pd;
use crate::send::{Activation, Command, SendState};
use crate::settings::{DEFAULT_BAUD, HardwareConfig, KENWOOD_BAUDS};
use crate::theme::*;
use crate::waterslide_panel::Target;

/// What a waterslide click *resolved to* — the click-resolution payload returned by
/// `draw_waterslide`, not stored on the panel. The panel turns it into a selection on
/// the bus ([`BusView::select`]); the single owner of "who is selected" is the
/// published `Selection`, not this.
#[derive(Clone, Default)]
struct RealSel {
    /// The station to work (its base call + the slot its decode landed in, for the
    /// real `DecodeRef`) when a decoded line was clicked rather than bare spectrum.
    target: Option<(String, SlotId)>,
    /// Set when the clicked line is addressed to *us* (`<my call> <their call> …`):
    /// the parsed message + its SNR, so SEND picks the contact up mid-stream
    /// ([`BusView::resume_qso`]) instead of arming and waiting for a CQ.
    resume: Option<(ParsedMessage, i8)>,
}

/// A resolved waterslide click: the audio offset under the cursor plus the selection
/// it lands on (a decoded station, or a bare offset with no target). The panel
/// publishes this as a `Selection`; its handler then places the TX offset.
struct WsClick {
    offset: f32,
    sel: RealSel,
}

pub struct Waterfall {
    spectro: Spectrogram,
    form: ConfigForm,
    /// Send-row text-box / slash-command state. The transmit lifecycle itself
    /// lives in the QSO engine (`QsoState`), which this row renders and commands.
    send: SendState,
    /// The last selection this panel's operating handler acted on, for *new*-selection
    /// detection: when the published `Selection` differs from this, the handler applies
    /// its response (place the TX offset, retune if out-of-passband + unlocked) once,
    /// then records it here so it never re-fires every frame.
    applied_selection: Option<Selection>,
    /// Panel-local resume hint for the selected decode: when the clicked line is
    /// addressed to *us*, the parsed message + its SNR so SEND resumes mid-stream
    /// instead of arming for a CQ. Tagged with the `DecodeRef` it belongs to so a
    /// later selection change (e.g. a map pick) can't misapply it — it's used only
    /// while it matches the current selection's target. (It can't ride in `Selection`,
    /// which carries who + where, not a `ParsedMessage`.)
    resume: Option<(DecodeRef, ParsedMessage, i8)>,
    /// The QSO phase observed last frame, so a completed contact deselects exactly
    /// once on the edge into `Complete` (rather than republishing every frame).
    prev_phase: Option<QsoPhase>,
    /// The message latched on the air for the current over, held in the Send box
    /// until the transmission finishes — even after the engine has advanced its
    /// `next_tx` or gone idle (the final 73/RR73 keeps showing while it plays out).
    /// `None` when we're not transmitting. See `draw_send_row`.
    tx_hold: Option<String>,
    /// Stable digit assignments: callsign (upper-cased) → (digit index 0..9,
    /// slot-boundary ms when assigned). Updated once per slot boundary; held
    /// across frames so numbers don't shuffle within a slot. The timestamp lets
    /// the retain logic drop entries purely by age, independent of whether the
    /// station's decode is still in `recent_decodes()`.
    cq_assignments: HashMap<String, (usize, i64)>,
    /// Slot-boundary timestamp (ms since epoch) when `cq_assignments` was last
    /// updated. A change here triggers the drop-and-fill logic.
    last_assigned_slot_ms: i64,
    /// Per-frame resolved shortcuts: index = digit assignment, value = the best
    /// current `Decode` for that callsign (or `None` if no recent decode).
    /// Always exactly 10 elements; rebuilt each frame from `cq_assignments`.
    cq_shortcuts: Vec<Option<Decode>>,
    /// The engine's `next_tx` text observed on the previous frame, so when an over
    /// starts we can latch the message being sent even though the engine may have
    /// already stepped to idle by the time the own-TX waterfall reaches the GUI.
    last_next_tx: Option<String>,
    /// Waterslide split preference: `false` centers NOW (1:1 decode/spectrogram),
    /// `true` widens the decode side to 2/3 (`WS_DECODE_WIDE_FRAC`). Loaded from
    /// the config file at startup, toggled live from the unlocked EDIT surface.
    wide_decode: bool,
    /// Frequency-axis view window (Hz): the lowest visible audio offset and the
    /// span shown top-to-bottom. The decode side and the spectrogram share it.
    /// Default is the full `[0, WS_MAX_HZ]`; scroll-wheel zooms (to the cursor),
    /// drag pans, double-click resets. View-only — no offset *state* is rescaled,
    /// only how Hz maps to screen rows. Not persisted across runs.
    view_lo_hz: f32,
    view_span_hz: f32,
    /// AUTO QSY enabled (UI mirror of the engine's `auto_hop`; pushed via the bus).
    /// After 3 unanswered CQs the engine hops to the lane finder's best offset.
    auto_hop: bool,
    /// The clock slot at which we last fed the engine a hop offset, so we recompute
    /// the lane once per slot (after that slot's decodes settle), not every frame.
    last_hop_feed_slot: Option<SlotId>,
}

impl Waterfall {
    pub fn new() -> Self {
        Self {
            spectro: Spectrogram::new(),
            form: ConfigForm::default(),
            send: SendState::default(),
            applied_selection: None,
            resume: None,
            prev_phase: None,
            tx_hold: None,
            cq_assignments: HashMap::new(),
            last_assigned_slot_ms: 0,
            cq_shortcuts: vec![None; 10],
            last_next_tx: None,
            wide_decode: crate::settings::read_waterslide_wide(),
            view_lo_hz: 0.0,
            view_span_hz: WS_MAX_HZ,
            auto_hop: false,
            last_hop_feed_slot: None,
        }
    }

    /// The clear-lane finder's current best CQ offset for the live mode, or `None`
    /// if there's nothing to score. Shared by the FIND CQ button and the auto-QSY
    /// offset feed.
    fn best_cq_offset(ctx: &PanelCtx, now_ms: i64) -> Option<f32> {
        let bw = signal_bandwidth_hz(ctx.bus.current_config().protocol);
        let rows = ctx.bus.recent_spectrum();
        let decodes = ctx.bus.recent_decodes();
        crate::lane_finder::pick_cq_offset(&rows, &decodes, bw, now_ms)
    }

    /// Apply a *new* selection's operating response — the single place a selection
    /// (this panel's own clicks/digits or a Contacts-map pick) turns into a TX-offset
    /// move and an optional dial retune. Called once per new selection by the handler
    /// in `ui`. The engine gates the offset on the lock; the retune is gated here on
    /// `!offset_locked`, so a locked selection never tunes — it only selects.
    fn apply_selection(&self, ctx: &PanelCtx, sel: &Selection) {
        // Audio-offset window we'll snap onto rather than retune. Conservative: keep
        // the station comfortably mid-passband (away from the filter edges).
        const SNAP_LO: i64 = 300;
        const SNAP_HI: i64 = 2500;
        match &sel.context {
            // A lane already inside the current passband (a waterslide click, a bare
            // offset, or CLEAR QSY): just place the TX offset. The engine ignores the
            // move while locked, so a locked click still selects without retuning.
            Some(SelectionContext::Passband(off)) => ctx.bus.set_tx_offset(off.0),
            // A map pick at a known absolute frequency. The Digital panel owns the
            // passband decision (the map has none): snap onto the offset when the
            // station is reachable in the current passband; otherwise retune the dial
            // so it lands at 1500 Hz audio — but only when the offset is unlocked.
            // Locked + out-of-passband ⇒ select-only (no offset move, no retune).
            Some(SelectionContext::AbsFreq(abs)) => {
                let Some(vfo) = ctx.bus.rig_state().map(|r| r.vfo.0) else {
                    return;
                };
                let candidate = abs.0 as i64 - vfo as i64;
                if (SNAP_LO..=SNAP_HI).contains(&candidate) {
                    ctx.bus.set_tx_offset(candidate as f32);
                } else if !ctx.bus.offset_locked() {
                    let new_dial = (abs.0 as i64 - 1500).max(0) as u64;
                    ctx.bus.set_freq(new_dial);
                    ctx.bus.set_tx_offset(1500.0);
                }
            }
            // Select-by-call with no known frequency (a worked-only map spot): select
            // the station, move nothing. Enter still arms (the engine matches on call).
            None => {}
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
        // (run the command, else toggle arm). Digit keys [1..9, 0] fire the CQ
        // shortcut for the ranked station at that slot when disarmed.
        // Digit shortcuts are suppressed while a QSO is in progress or the TX
        // hold is active — assignments keep tracking, but keys don't fire.
        let shortcuts_active = self.tx_hold.is_none()
            && ctx
                .bus
                .qso_state()
                .is_none_or(|s| matches!(s.phase, QsoPhase::Idle));

        // `now_ms` is hoisted here (rather than after the QSO block below) so the
        // Tab and Q key handlers can pass it to `best_cq_offset` synchronously.
        let now_ms = chrono::Utc::now().timestamp_millis();
        // The QSO engine owns the TX offset + its lock; the panel reads them, never a
        // local copy. The Tab/Q/digit handlers and the selection below act on these.
        let locked = ctx.bus.offset_locked();
        let tx_off = ctx.bus.tx_offset().unwrap_or(1500.0);

        let mut activate = false;
        if ctx.active {
            let events = ctx.ui.input(|i| i.events.clone());
            // Track whether a key was consumed as a shortcut so its companion
            // Event::Text doesn't also flow into the slash-command buffer.
            let mut digit_consumed = false;
            let mut tab_consumed = false;
            for ev in &events {
                // Tab — toggle the TX offset lock (the engine owns the lock).
                // Guard: no modifiers, NOT mid-command, not armed (shortcuts_active).
                if let egui::Event::Key { key: egui::Key::Tab, pressed: true, modifiers, .. } = ev
                    && !modifiers.any()
                    && !self.send.entering
                    && shortcuts_active
                {
                    ctx.bus.set_offset_lock(!locked);
                    tab_consumed = true;
                    digit_consumed = true;
                    continue;
                }
                // Q — Clear QSY: jump offset to the clearest available lane. No lock
                // guard — the engine ignores the move while locked. Guard: no
                // modifiers, NOT mid-command, not armed.
                if let egui::Event::Key { key: egui::Key::Q, pressed: true, modifiers, .. } = ev
                    && !modifiers.any()
                    && !self.send.entering
                    && shortcuts_active
                {
                    if let Some(off) = Self::best_cq_offset(ctx, now_ms) {
                        // Deselect onto a clear lane: a bare-offset selection. The
                        // handler places the offset (engine ignores it while locked).
                        ctx.bus
                            .select(None, Some(SelectionContext::Passband(OffsetHz(off))));
                        self.resume = None;
                    }
                    digit_consumed = true;
                    continue;
                }
                // Digit shortcut: no modifiers, not mid-command, not armed.
                if let egui::Event::Key { key, pressed: true, modifiers, .. } = ev
                    && !modifiers.any()
                    && !self.send.entering
                    && shortcuts_active
                    && let Some(idx) = digit_key_index(*key)
                {
                    digit_consumed = true;
                    if let Some(d) = self.cq_shortcuts[idx].clone()
                        && let Some((call, slot)) = decode_station(&d)
                    {
                        // Select + arm in one gesture. Emit the selection (who + the
                        // station's own lane); the handler places the offset, which the
                        // engine ignores while locked — so a locked digit arms there
                        // without moving the TX frequency, an unlocked one snaps to the
                        // station. Then arm (Start carries only who, not the offset).
                        let target = DecodeRef {
                            radio: app_core::radio_id(),
                            slot,
                            call: Some(Callsign(call)),
                        };
                        ctx.bus
                            .select(Some(target.clone()), Some(SelectionContext::Passband(d.offset)));
                        self.resume = None;
                        ctx.bus.answer_station(target);
                    }
                    continue;
                }
                match ev {
                    egui::Event::Text(t) if !digit_consumed => self.send.type_text(t),
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
            // Remove Tab from egui's input queue so focus traversal doesn't also
            // fire and move focus to a header button when we handled Tab as a lock.
            if tab_consumed {
                ctx.ui.input_mut(|i| {
                    i.events
                        .retain(|e| !matches!(e, egui::Event::Key { key: egui::Key::Tab, pressed: true, .. }));
                });
            }
        }

        // Where we're pointed. The selection is the single owner
        // (`selection/{id}/active`), written by this panel's clicks/digits and the
        // Contacts map; read it back for the arm target and the highlight. The resume
        // hint can't round-trip through it (it carries a `ParsedMessage`), so it's kept
        // panel-local, tagged with its target so a later selection change can't
        // misapply it. The TX offset is engine-owned (`tx_off`).
        let sel_target = ctx.bus.selection().and_then(|s| s.target);
        let sel_call = sel_target.as_ref().and_then(|t| t.call.clone()).map(|c| c.0);
        let sel_resume = self
            .resume
            .as_ref()
            .filter(|(r, _, _)| Some(r) == sel_target.as_ref())
            .map(|(_, m, s)| (m.clone(), *s));

        // Keep the buffer mirroring the would-be next message as a preview (unless
        // mid-command); the engine's authored message takes over once it's running.
        let preview = match &sel_call {
            Some(call) => Target::Station {
                call: call.clone(),
                off: tx_off as i32,
            },
            None => Target::Offset(tx_off as i32),
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
        // AUTO QSY moved to unlocked config form; QSY CLEAR is now Tab/q/`/clear`.
        let pad = 8.0;
        let label = "Send:";
        let label_font = mono(11.0);
        let label_w = measure(painter, label, label_font.clone());
        let cy = row.center().y;
        let label_x = row.left() + pad;

        painter.text(
            Pos2::new(label_x, cy),
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
        let box_left = label_x + label_w + pad;
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
        // Body before arming, accent2 once armed, accent3 while actually keyed —
        // the same three-state progression as the Send key and the panel frame.
        let text_color = if transmitting {
            pal.accent3
        } else if active_qso {
            pal.accent2
        } else {
            pal.body
        };
        painter.with_clip_rect(box_rect).text(
            Pos2::new(box_rect.left() + 6.0, cy),
            Align2::LEFT_CENTER,
            &display,
            egui::FontId::monospace(12.0),
            text_color,
        );

        // Lit key in its recessed track. Amber idle, accent2 while armed, accent3
        // (pink/red) once we're actually keyed on the air — matching the panel frame.
        lcd_panel(painter, track, pal, 4);
        let cell = Rect::from_min_max(
            Pos2::new(track.left() + 2.0, track.top() + 2.0),
            Pos2::new(track.right() - 2.0, track.bottom() - 2.0),
        );
        let accent = if transmitting {
            pal.accent3
        } else if active_qso {
            pal.accent2
        } else {
            pal.accent
        };
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
        // spectrum. The target comes from the published selection above; the TX offset
        // is already placed (engine-owned), so arming carries only *who*.
        if activate || btn.clicked() {
            match self.send.activate() {
                Activation::Command(cmd) => self.apply_command(ctx, cmd),
                Activation::Toggle => {
                    if !call_set {
                        // No station callsign yet — operating is blocked until one
                        // is set (top bar when unlocked, or the config file).
                    } else if active_qso {
                        ctx.bus.abort_qso();
                    } else if let Some(target) = &sel_target {
                        // A line addressed to us (resume) picks the contact up
                        // mid-stream; otherwise arm the DM420 wait-for-CQ way.
                        if let Some((message, snr)) = &sel_resume {
                            ctx.bus.resume_qso(
                                target.clone(),
                                message.clone(),
                                *snr,
                                OffsetHz(tx_off),
                            );
                        } else {
                            ctx.bus.answer_station(target.clone());
                        }
                    } else {
                        ctx.bus.call_cq();
                    }
                }
                Activation::None => {}
            }
        }

        // While auto-QSY is on, keep the engine's hop target fresh: feed it the
        // current best CQ lane once per clock slot (after that slot's decodes settle),
        // not every frame. No lock guard — the engine won't hop while locked.
        if self.auto_hop {
            let slot = ctx.bus.clock().map(|c| c.slot);
            if slot != self.last_hop_feed_slot {
                self.last_hop_feed_slot = slot;
                if let Some(off) = Self::best_cq_offset(ctx, now_ms) {
                    ctx.bus.set_cq_hop_offset(off);
                }
            }
        }
    }

    /// Apply a parsed slash command. `/f` takes an explicit dial frequency; `/b`
    /// resolves a band to its calling frequency for the *current* over-air mode
    /// (FT8 and FT4 differ — e.g. 20 m is 14.074 vs 14.080). The rig is retuned and
    /// the header tracks the resulting `RigState`.
    fn apply_command(&mut self, ctx: &PanelCtx, cmd: Command) {
        match cmd {
            Command::ClearQsy => {
                // Deselect onto a clear lane: a bare-offset selection. No lock guard —
                // the handler places the offset and the engine ignores it while locked.
                let now_ms = chrono::Utc::now().timestamp_millis();
                if let Some(off) = Self::best_cq_offset(ctx, now_ms) {
                    ctx.bus
                        .select(None, Some(SelectionContext::Passband(OffsetHz(off))));
                    self.resume = None;
                }
            }
            cmd => {
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
                    Command::ClearQsy => unreachable!(),
                };
                if let Some(hz) = hz {
                    ctx.bus.set_freq(hz);
                }
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

        // The single selection → operating handler. Both this panel's own gestures
        // (waterslide clicks, digit shortcuts, CLEAR QSY) and the Contacts map write
        // the selection (`selection/{id}/active`) via `BusView::select`; we observe
        // the published value — the single owner — and, on a *new* selection, apply
        // the operating response *once*: place the engine's TX offset (engine-gated by
        // the lock) and, only when the station is outside the current passband *and*
        // the offset is unlocked, retune the dial. Selecting always works regardless
        // of lock; locked just means select-only (no tune).
        let selection = ctx.bus.selection();
        if selection != self.applied_selection {
            self.applied_selection = selection.clone();
            if let Some(sel) = &selection {
                self.apply_selection(ctx, sel);
            }
        }

        // Hoist operating state so both the header chrome (CLEAR QSY button) and the
        // body chrome (frame tint, TX lane) share the same snapshot this frame.
        let op_phase = ctx
            .bus
            .qso_state()
            .map(|s| s.phase)
            .unwrap_or(QsoPhase::Idle);
        // A completed contact (final 73 sent) deselects the worked station so the send
        // box reverts to the default CQ. Clear the selection once, on the edge into
        // `Complete`; the audio offset is left where it is (a bare deselect).
        if op_phase == QsoPhase::Complete && self.prev_phase != Some(QsoPhase::Complete) {
            ctx.bus.select(None, None);
            self.resume = None;
        }
        self.prev_phase = Some(op_phase);
        let op_armed = !matches!(op_phase, QsoPhase::Idle);
        let now_ms = chrono::Utc::now().timestamp_millis();
        let op_transmitting = ctx.bus.tx_spectrum().is_some_and(|r| now_ms - r.t.0 < 500);
        // Engine-owned TX offset + lock — the single source of truth for the TX lane,
        // the CLEAR QSY enable, and the LOCKED button.
        let locked = ctx.bus.offset_locked();
        let tx_off = ctx.bus.tx_offset().unwrap_or(1500.0);

        let header = Rect::from_min_max(
            block.min,
            Pos2::new(block.right(), block.top() + pd::HEADER_ROW_H),
        );
        panel_header(painter, header, pal, "Digital", "", ctx.active);

        // Header layout: [Digital] [FT8/FT4] · · · [FREQ] · · · [CLEAR QSY]
        let cy = header.center().y;
        let proto = ctx.bus.current_config().protocol;

        // FT8/FT4 toggle — anchored just right of the "Digital" title text.
        // Replicate segmented's internal cell sizing (CELL_PAD_X=11, PAD=2, GAP=2)
        // to compute the track width for left-anchoring.
        let title_right = header.left()
            + FOCUS_BOX_SZ
            + 8.0
            + measure(painter, &tracked("DIGITAL"), heading(11.0));
        let ft8_cell_w = measure(painter, &tracked("FT8"), heading(9.0)) + 22.0;
        let ft4_cell_w = measure(painter, &tracked("FT4"), heading(9.0)) + 22.0;
        let mode_track_w = 4.0 + ft8_cell_w + ft4_cell_w + 2.0;
        let (_mode_left, mode_clicks) = crate::chrome::segmented(
            ctx.ui,
            painter,
            pal,
            title_right + 8.0 + mode_track_w,
            cy,
            20.0,
            "",
            &[
                ("FT8", proto == Protocol::Ft8),
                ("FT4", proto == Protocol::Ft4),
            ],
            "sw_mode",
        );

        // CLEAR QSY button — right-anchored in the header. Uses the same
        // lcd_panel + key_cell_accent (active=true) pattern as SEND and SCAN.
        let clear_cell_w = measure(painter, &tracked("CLEAR QSY"), heading_bold(9.0)) + 14.0;
        let clear_track_w = clear_cell_w + 4.0;
        let clear_track = Rect::from_center_size(
            Pos2::new(header.right() - 2.0 - clear_track_w * 0.5, cy),
            egui::Vec2::new(clear_track_w, 20.0),
        );
        lcd_panel(painter, clear_track, pal, 4);
        let clear_cell = Rect::from_min_max(
            Pos2::new(clear_track.left() + 2.0, clear_track.top() + 2.0),
            Pos2::new(clear_track.right() - 2.0, clear_track.bottom() - 2.0),
        );
        let clear_resp = key_cell_accent(
            ctx.ui,
            painter,
            pal,
            clear_cell,
            "CLEAR QSY",
            !locked,
            pal.accent,
            ctx.ui.id().with("header_clear_qsy"),
        );
        // No lock click-guard — the engine ignores the move while locked; the button
        // is just dimmed (above) so the operator sees it's frozen. `!op_armed` stays
        // (don't QSY mid-contact).
        if clear_resp.clicked() && !op_armed
            && let Some(off) = Self::best_cq_offset(ctx, now_ms) {
                // Deselect onto a clear lane (bare-offset selection); handler places it.
                ctx.bus
                    .select(None, Some(SelectionContext::Passband(OffsetHz(off))));
                self.resume = None;
            }
        // When the mode actually changes, also retune to the calling frequency for
        // the new mode on the current band (FT8 and FT4 use different dial freqs).
        let new_mode_if_changed = if mode_clicks[0] && proto != Protocol::Ft8 {
            Some((Protocol::Ft8, OverAirMode::Ft8))
        } else if mode_clicks[1] && proto != Protocol::Ft4 {
            Some((Protocol::Ft4, OverAirMode::Ft4))
        } else {
            None
        };
        if let Some((new_proto, new_mode)) = new_mode_if_changed {
            ctx.bus.set_protocol(new_proto);
            let vfo_hz = ctx.bus.rig_state().map(|r| r.vfo.0);
            if let Some(band) = vfo_hz.and_then(|hz| Band::from_hz(AbsHz(hz)))
                && let Some(hz) = crate::send::calling_freq_hz(band, new_mode) {
                    ctx.bus.set_freq(hz);
                }
        }

        // Tuned-frequency readout (FREQ chip), centered in the header bar like the
        // top-bar clocks. When the rig is faulted, show a dashed placeholder rather
        // than a stale freq.
        let rig_fault = ctx
            .bus
            .health(SubsystemId::Rig)
            .map(|h| h.is_faulted())
            .unwrap_or(false);
        let vfo_text = if rig_fault {
            "---.---.--".to_string()
        } else {
            let hz = ctx
                .bus
                .rig_state()
                .map(|r| r.vfo.0)
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
        // Operating state tints the panel frame so it's obvious at a glance: amber
        // when idle, accent2 (blue/cyan) once armed to transmit, accent3 (pink/red)
        // while actually keyed on the air. The corner brackets, the NOW divider, and
        // the TX lane all read this so they agree. Only when locked (operating) — the
        // unlocked screen is the radio-setup form, where state tinting is meaningless.
        // (op_phase / op_armed / now_ms / op_transmitting hoisted before the header.)
        let op_accent = if ctx.unlocked {
            pal.accent
        } else if op_transmitting {
            pal.accent3
        } else if op_armed {
            pal.accent2
        } else {
            pal.accent
        };
        recessed_screen_accent(painter, screen, pal, op_accent);

        let body_big = screen.width() > 24.0 && screen.height() > 24.0;

        if ctx.unlocked {
            // Unlocked (GUI EDIT): the screen body becomes the radio/audio settings
            // form, which drives live hardware.
            if body_big {
                let body = screen.shrink(10.0);
                let mut child = ctx.ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(body)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                child.set_clip_rect(screen.shrink(2.0));
                self.form.ui(&mut child, ctx.bus, pal, &mut self.wide_decode, &mut self.auto_hop);
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
                .health(SubsystemId::Audio)
                .filter(SubsystemHealth::is_faulted);

            if let Some(health) = audio_fault {
                if body_big {
                    draw_fault_body(painter, screen, pal, &health);
                }
            } else {
                // Real mode: no FFT/spectrum producer is wired yet (the decoder
                // publishes `Decode`s, not `SpectrumRow`s), so we render the decodes
                // themselves in waterslide form — placed by audio offset (vertical)
                // and age (horizontal), NOW at centre, the FFT (right) half blank.
                if body_big {
                    let body = screen.shrink(8.0);
                    let now_ms = chrono::Utc::now().timestamp_millis();
                    // One interaction over the whole waterslide body, sensing both
                    // clicks (station select / TX offset) and drags (pan). Created up
                    // front so the frequency-view gestures resolve *before* the
                    // spectrogram and decodes paint — they share the view window.
                    let resp = ctx.ui.interact(
                        body,
                        ctx.ui.id().with("ws_select"),
                        egui::Sense::click_and_drag(),
                    );
                    self.apply_view_gestures(ctx.ui, body, &resp);
                    let (lo_frac, hi_frac) = self.view_fracs();
                    // Right half: real scrolling spectrogram (brightness = intensity),
                    // flowing right as the decode text flows left. NOW sits at
                    // `now_frac` across (1/2 centered, or 2/3 in the wide-decode
                    // split); both sides still span the same `ws_secs`, so at 2:1 the
                    // spectrogram is just compressed into its narrower 1/3 (the
                    // pixels-per-second differ, the time window doesn't).
                    let now_frac = if self.wide_decode {
                        WS_DECODE_WIDE_FRAC
                    } else {
                        0.5
                    };
                    let now_x = body.left() + body.width() * now_frac;
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
                    // lands. Both halves share this time span (the decode side sizes
                    // it — see `ws_history_secs`); at 1:1 the pixels-per-second match,
                    // at 2:1 they differ. FT4's shorter slots scroll faster. The
                    // spectrogram scrolls off the same `now_ms` wall-clock the decode
                    // text is placed against, so the two axes can't drift apart.
                    let protocol = ctx.bus.current_config().protocol;
                    let ws_secs = ws_history_secs(painter, body, protocol, now_frac);
                    self.spectro.update_and_paint(
                        ctx.ui,
                        right,
                        now_ms,
                        ws_secs,
                        column.as_ref(),
                        &cmap,
                        lo_frac,
                        hi_frac,
                    );
                    // Live QSO phase gates the selection. While armed/working — or
                    // still keyed at the tail of an over (`tx_hold`) — the selection
                    // is locked: the operator can't change the audio offset or pick
                    // another station mid-QSO. It's only mutable when disarmed.
                    let phase = ctx
                        .bus
                        .qso_state()
                        .map(|s| s.phase)
                        .unwrap_or(QsoPhase::Idle);
                    let armed = !matches!(phase, QsoPhase::Idle) || self.tx_hold.is_some();

                    // (The completed-contact deselect runs once on the `Complete` edge
                    // at the top of `ui`, against the published selection.)

                    // Click-to-select on the live waterslide (mock mode selects via
                    // the sim's own `ui()`; the real waterslide is draw-only). We
                    // hit-test via the body interaction above and let `draw_waterslide`
                    // resolve the click to a station (decoded line) or a bare TX offset
                    // (empty spectrum). Only act on clicks (and offer the pointing-hand
                    // cursor) when disarmed, so the locked selection can't be
                    // overridden; pan/zoom stay live regardless. A double-click is a
                    // view reset (handled in `apply_view_gestures`), not a select.
                    let resp = if armed {
                        resp
                    } else {
                        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
                    };
                    let click = if armed || resp.double_clicked() {
                        None
                    } else {
                        resp.clicked().then(|| resp.interact_pointer_pos()).flatten()
                    };

                    // Selection feedback: highlight the selected station's lane and
                    // tag it with the live QSO phase (ARMED while waiting for its CQ,
                    // WORKING once the exchange is under way). The selected call is the
                    // single owner's — the published selection, the same value the map
                    // and Call Sign panels highlight.
                    let sel_call = ctx
                        .bus
                        .selection()
                        .and_then(|s| s.target)
                        .and_then(|t| t.call)
                        .map(|c| c.0);
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
                    // Stations already logged on the band we're currently tuned to
                    // (the dial freq → band). "Worked" is per-band, so a contact on
                    // another band doesn't dim a caller here. Empty when off-band.
                    let worked = ctx
                        .bus
                        .rig_state()
                        .map(|r| r.vfo.0)
                        .and_then(|hz| Band::from_hz(AbsHz(hz)))
                        .map(|b| ctx.bus.worked_calls_on_band(b))
                        .unwrap_or_default();
                    // The engine-owned `tx_off` (read at the top of `ui`) is the single
                    // source of truth for the TX lane — auto-QSY hops are reflected for
                    // free, since the lane reads the engine directly.

                    // Slot-locked CQ shortcuts. Assignments are updated once per
                    // slot boundary (drop aged/worked, fill freed slots with new
                    // top-SNR candidates); within a slot they are frozen so number
                    // badges don't shuffle on every frame.
                    let slot_ms = (app_core::slot_period(protocol) * 1_000.0) as i64;
                    let current_slot_ms =
                        if slot_ms > 0 { (now_ms / slot_ms) * slot_ms } else { now_ms };
                    let recent_decodes = ctx.bus.recent_decodes();
                    if current_slot_ms != self.last_assigned_slot_ms {
                        self.last_assigned_slot_ms = current_slot_ms;
                        update_cq_assignments(
                            &mut self.cq_assignments,
                            &recent_decodes,
                            &worked,
                            current_slot_ms,
                            slot_ms,
                        );
                    }
                    // Rebuild the per-frame shortcuts vec from the stable assignment
                    // map, picking the highest-SNR decode for each assigned callsign.
                    self.cq_shortcuts = {
                        let mut slots: Vec<Option<Decode>> = vec![None; 10];
                        for d in &recent_decodes {
                            if !is_cq(d) {
                                continue;
                            }
                            let Some((call, _)) = decode_station(d) else {
                                continue;
                            };
                            let Some((idx, _)) =
                                self.cq_assignments.get(&call.to_ascii_uppercase()).copied()
                            else {
                                continue;
                            };
                            let better = slots[idx]
                                .as_ref()
                                .map(|s| d.snr_db > s.snr_db)
                                .unwrap_or(true);
                            if better {
                                slots[idx] = Some(d.clone());
                            }
                        }
                        slots
                    };

                    // LAN peers' working offsets for the deconfliction overlay.
                    // Filtered to the band we're tuned to — a peer's 40 m offset on
                    // our 20 m waterslide would be meaningless — and to fresh beacons
                    // (stale ones are dropped, not parked; see `PEER_STALE_SECS`).
                    // `local_band` is derived the same way `worked` is (dial freq →
                    // band); off-band (`None`) draws no peers. Sorted high→low offset
                    // so the renderer's label stagger cascades downward. Display-only:
                    // these offsets never drive a retune.
                    let local_band = ctx
                        .bus
                        .rig_state()
                        .map(|r| r.vfo.0)
                        .and_then(|hz| Band::from_hz(AbsHz(hz)));
                    let peer_ticks: Vec<PeerTick> = {
                        let stale = std::time::Duration::from_secs(PEER_STALE_SECS);
                        let mut ticks: Vec<PeerTick> = ctx
                            .bus
                            .peers()
                            .into_iter()
                            .filter(|p| Some(p.band) == local_band)
                            .filter(|p| p.last_seen.elapsed() <= stale)
                            .map(|p| {
                                let label = match &p.call {
                                    Some(call) => format!("{} \u{00B7} {}", p.station, call),
                                    None => p.station.clone(),
                                };
                                PeerTick { offset: p.offset, label }
                            })
                            .collect();
                        ticks.sort_by(|a, b| b.offset.total_cmp(&a.offset));
                        ticks
                    };

                    if let Some(click) = draw_waterslide(
                        painter,
                        body,
                        pal,
                        &recent_decodes,
                        now_ms,
                        click,
                        tx_off,
                        sel_call.as_deref(),
                        (!ctx.call.trim().is_empty()).then(|| ctx.call.trim()),
                        &worked,
                        &self.cq_assignments,
                        tag.as_deref(),
                        bandwidth_hz,
                        ws_secs,
                        app_core::slot_period(protocol) as f32,
                        op_accent,
                        op_armed,
                        op_transmitting,
                        locked,
                        now_frac,
                        self.view_lo_hz,
                        self.view_span_hz,
                        &peer_ticks,
                    ) {
                        // Publish the click as a selection (the single owner) — who (a
                        // decoded station, or none for bare spectrum) + where (the
                        // clicked lane, in-passband). The handler at the top of `ui`
                        // places the TX offset next frame (engine-gated by the lock), so
                        // a locked click still selects without moving the frequency. The
                        // resume hint can't ride in `Selection`, so keep it panel-local,
                        // tagged with its target so a later selection can't misapply it.
                        let target = click.sel.target.map(|(call, slot)| DecodeRef {
                            radio: app_core::radio_id(),
                            slot,
                            call: Some(Callsign(call)),
                        });
                        self.resume = match (&target, click.sel.resume) {
                            (Some(t), Some((msg, snr))) => Some((t.clone(), msg, snr)),
                            _ => None,
                        };
                        ctx.bus.select(
                            target,
                            Some(SelectionContext::Passband(OffsetHz(click.offset))),
                        );
                    }

                    // When locked, show a "LOCKED" key button at the right edge of
                    // the TX band. Clicking it unlocks. Nothing is shown when unlocked.
                    if locked
                        && tx_off < self.view_lo_hz + self.view_span_hz
                        && tx_off + bandwidth_hz > self.view_lo_hz
                    {
                        let y_bot = (body.bottom()
                            - ((tx_off - self.view_lo_hz) / self.view_span_hz)
                                * body.height())
                        .min(body.bottom());
                        let y_top = (body.bottom()
                            - ((tx_off + bandwidth_hz - self.view_lo_hz)
                                / self.view_span_hz)
                                * body.height())
                        .max(body.top())
                        .min(y_bot - 3.0);
                        let band_cy = (y_top + y_bot) * 0.5;
                        let cell_w =
                            measure(painter, &tracked("LOCKED"), heading_bold(9.0)) + 14.0;
                        let track_w = cell_w + 4.0;
                        let track = Rect::from_center_size(
                            Pos2::new(body.right() - track_w * 0.5 - 4.0, band_cy),
                            egui::Vec2::new(track_w, 16.0),
                        );
                        lcd_panel(painter, track, pal, 3);
                        let cell = Rect::from_min_max(
                            Pos2::new(track.left() + 2.0, track.top() + 2.0),
                            Pos2::new(track.right() - 2.0, track.bottom() - 2.0),
                        );
                        let locked_btn = key_cell_accent(
                            ctx.ui,
                            painter,
                            pal,
                            cell,
                            "LOCKED",
                            true,
                            op_accent,
                            ctx.ui.id().with("offset_lock_btn"),
                        );
                        if locked_btn.clicked() {
                            ctx.bus.set_offset_lock(false);
                        }
                    }

                    ctx.ui
                        .ctx()
                        .request_repaint_after(std::time::Duration::from_millis(33));
                }
            }
        }

        // The send row is the operating control, shown only when locked. When
        // unlocked the bottom strip is the settings/edit surface, not the radio.
        // The transmit lifecycle now lives in the QSO engine, which the row reads
        // and commands (no local arm cadence to step).
        if !ctx.unlocked {
            self.draw_send_row(ctx, send_row);
        }
        // The selected-station highlight string is derived centrally (in `App::ui`)
        // from the published selection — the single owner — so no panel writes it.
    }
}

impl Waterfall {
    /// The view window as fractions of the full `[0, WS_MAX_HZ]` range
    /// (`lo_frac`, `hi_frac`) — fed to the spectrogram so its texture crops to
    /// the same band the decode side shows.
    fn view_fracs(&self) -> (f32, f32) {
        (
            self.view_lo_hz / WS_MAX_HZ,
            (self.view_lo_hz + self.view_span_hz) / WS_MAX_HZ,
        )
    }

    /// Apply scroll-wheel zoom (anchored at the cursor), drag-to-pan, and
    /// double-click-to-reset to the frequency view window. View-only: it moves
    /// `view_lo_hz`/`view_span_hz`, never any offset *state*. Always live — zoom
    /// and pan work whether or not a QSO is armed.
    fn apply_view_gestures(&mut self, ui: &egui::Ui, body: Rect, resp: &egui::Response) {
        // Double-click anywhere resets to the full-band view.
        if resp.double_clicked() {
            self.view_lo_hz = 0.0;
            self.view_span_hz = WS_MAX_HZ;
            return;
        }

        // Zoom, anchored so the Hz under the cursor stays put. Bottom of the pane
        // is `view_lo`, top is `view_lo + span`, so `frac` runs 0 (bottom) → 1
        // (top). Two input channels feed it: scroll-wheel / two-finger scroll
        // (`smooth_scroll_delta.y`, additive — scrolling up shrinks the span) and
        // touchpad pinch / ctrl+scroll (`zoom_delta`, a multiplicative factor where
        // >1 is pinch-out = zoom in). Reading both makes the gesture work on a
        // trackpad as well as a mouse.
        let (scroll, zoom) = if resp.hovered() {
            ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()))
        } else {
            (0.0, 1.0)
        };
        if (scroll != 0.0 || zoom != 1.0)
            && let Some(p) = resp.hover_pos()
        {
            let frac = ((body.bottom() - p.y) / body.height().max(1.0)).clamp(0.0, 1.0);
            let cursor_hz = self.view_lo_hz + frac * self.view_span_hz;
            // Combine: scroll trims the span linearly, pinch scales it (divide by
            // `zoom` so pinch-out shrinks the span = zooms in).
            let factor = ((1.0 - scroll / WS_ZOOM_DIV) / zoom).clamp(0.2, 5.0);
            self.view_span_hz = (self.view_span_hz * factor).clamp(WS_MIN_SPAN_HZ, WS_MAX_HZ);
            self.view_lo_hz = cursor_hz - frac * self.view_span_hz;
        }

        // Drag to pan: keep the grabbed Hz under the pointer (drag down → window
        // rises, revealing higher frequencies from above).
        if resp.dragged() {
            let hz_per_px = self.view_span_hz / body.height().max(1.0);
            self.view_lo_hz += resp.drag_delta().y * hz_per_px;
        }

        // Keep the window inside the full band.
        self.view_span_hz = self.view_span_hz.clamp(WS_MIN_SPAN_HZ, WS_MAX_HZ);
        self.view_lo_hz = self.view_lo_hz.clamp(0.0, WS_MAX_HZ - self.view_span_hz);
    }
}

/// Audio-offset axis span (Hz): FT8/FT4 decodes land in roughly 0..3000 Hz.
const WS_MAX_HZ: f32 = 3000.0;

/// How long a LAN peer's beacon stays drawable on the deconfliction overlay, in
/// seconds. Mirrors the transport's peer TTL (~30 s): a peer we haven't heard from
/// within this window is dropped from the waterslide rather than left lingering —
/// a stale "someone is here" tick would mislead the very view it exists to inform.
const PEER_STALE_SECS: u64 = 30;

/// Tightest frequency span (Hz) the view may zoom to — keeps a few signal lanes
/// on screen so zoom-in stays useful rather than collapsing to a single trace.
const WS_MIN_SPAN_HZ: f32 = 200.0;

/// Scroll-delta divisor for wheel zoom: larger = gentler. One notch (~50 px of
/// smoothed scroll) is roughly a ±12% span change.
const WS_ZOOM_DIV: f32 = 400.0;

/// NOW-line position (fraction of panel width from the left) in the "wide decode"
/// split: the decode/text side gets 2/3 of the panel and the spectrogram 1/3. The
/// 1:1 split parks NOW at 0.5. Both sides span the same amount of *time* either
/// way — only the pixels-per-second differ (see `draw_waterslide`).
const WS_DECODE_WIDE_FRAC: f32 = 2.0 / 3.0;

/// Gap (px) between the outgoing-lane's shaded band (sized to the signal's
/// nominal bandwidth) and its bracketing rules. Real signals smear a little
/// taller than the nominal width on the spectrogram, so the rules sit just
/// outside the shading rather than clipping the trace.
const WS_RULE_GAP: f32 = 5.0;
const WS_HATCH_GAP: f32 = 7.0;
const WS_HATCH_STRIP_H: f32 = 6.0;

/// Gap (px) between a decode row's status icon and its signal-report field.
const WS_ICON_GAP: f32 = 4.0;

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

/// A scrolling spectrogram texture for the right side. Newest column sits at the
/// NOW line; older columns flow right, brightness = signal intensity. It always
/// spans `history_secs` across its rect — equal to the decode side's time window —
/// so at the 1:1 split the on-screen pixels-per-second match the decode text; at
/// the 2:1 split it shows the same time compressed into its narrower 1/3.
struct Spectrogram {
    w: usize,
    h: usize,
    /// `h*w` row-major intensities; row 0 = top (high freq), col 0 = newest.
    intensity: Vec<u8>,
    image: ColorImage,
    tex: Option<TextureHandle>,
    /// Sub-column scroll remainder carried across frames (the fractional column the
    /// last frame's advance didn't reach).
    dx_frac: f64,
    /// Wall-clock (ms since epoch) at the previous `update_and_paint`. The scroll
    /// advances by the true `now_ms` delta between frames, so it telescopes to the
    /// decode text's absolute `now − decode.t` placement and the two can't drift.
    last_now_ms: Option<i64>,
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
            last_now_ms: None,
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

    /// Advance the scroll by the wall-clock time elapsed since the last frame, fill
    /// the newly-exposed columns with `latest`, recolour through `cmap`, and blit
    /// into `rect` (the right half).
    ///
    /// The scroll is driven by absolute wall-clock (`now_ms`), not egui's frame
    /// `dt`: accumulating the true `(now_ms − last_now_ms)` delta telescopes exactly
    /// to the decode text's absolute `now − decode.t` placement, so the spectrogram
    /// and the text share one clock and can't drift apart across dropped, capped, or
    /// backgrounded frames.
    #[allow(clippy::too_many_arguments)]
    fn update_and_paint(
        &mut self,
        ui: &egui::Ui,
        rect: Rect,
        // Wall-clock NOW (ms since epoch) — the same value `draw_waterslide` places
        // the decode text against, so the scroll stays locked to it.
        now_ms: i64,
        history_secs: f32,
        latest: Option<&SpectrumRow>,
        cmap: &[Color32; 256],
        // Frequency-view window as fractions of the full band: `lo_frac`/`hi_frac`
        // are `view_lo`/`view_hi ÷ WS_MAX_HZ`. The texture is cropped vertically to
        // this band so the spectrogram zooms/pans in lock-step with the decode side.
        lo_frac: f32,
        hi_frac: f32,
    ) {
        if let Some(row) = latest {
            self.ensure_size(row.mags.len().clamp(1, SPECTRO_MAX_H));
        }
        if self.w == 0 || self.h == 0 {
            return;
        }

        // Scroll right by whole columns; carry the fraction across frames. The
        // advance is the wall-clock delta since the last frame (not egui's frame
        // `dt`), so it accumulates to the same absolute-time axis the decode text
        // uses and the two can't drift.
        let dt_s = self
            .last_now_ms
            .map_or(0.0, |prev| (now_ms - prev).max(0) as f64 / 1000.0);
        self.last_now_ms = Some(now_ms);
        self.dx_frac += dt_s * (self.w as f64 / history_secs as f64);
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
        // Crop the texture to the frequency window. Row 0 (texture top, v=0) is the
        // highest bin and the bottom row (v=1) the lowest, so the top of the rect
        // (high freq) maps to `v = 1 - hi_frac` and the bottom (low freq) to
        // `v = 1 - lo_frac` — the full band (0..1) when not zoomed.
        let uv = Rect::from_min_max(
            Pos2::new(0.0, 1.0 - hi_frac),
            Pos2::new(1.0, 1.0 - lo_frac),
        );
        ui.painter_at(rect)
            .image(self.tex.as_ref().unwrap().id(), rect, uv, Color32::WHITE);
    }
}
/// A representative decode line (SNR + a typical exchange) used to gauge how wide
/// one message renders in the current font. The font is monospaced, so this is an
/// exact stand-in for any same-length line; longer/shorter real messages vary a
/// little, which is fine — the scroll speed only needs to be approximately right.
const WS_REF_MSG: &str = "−15 KX1ABC W4XYZ R−12";

/// Extra monospace cells added to the reference line when sizing a slot column. The
/// message text rides *right* of the slot rule, but each line also draws a status
/// icon and a half-space gap to the *left* of the rule that `WS_REF_MSG` doesn't
/// span — so a column sized to the bare string runs a touch tight. A few cells of
/// headroom keep one line clear of the next slot's icon/report as it scrolls.
const WS_LINE_PAD_CELLS: f32 = 4.0;

/// Seconds of decode history the left (text) side spans (NOW at `now_frac` → oldest
/// at left), chosen so one line clears as the next renders into place. We render
/// `WS_REF_MSG` in the current font to get its pixel width, add `WS_LINE_PAD_CELLS`
/// for the icon/report/gap that sit left of the slot rule, then scroll at
/// `line_w / slot_period` px/s — i.e. a line travels its own footprint in one slot,
/// so the previous slot's line has moved off NOW before the next lands on top of
/// it. That makes `history_secs = (left_w / line_w) * slot_period`: faster for FT4
/// (7.5 s slots), slower for FT8 (15 s), and it tracks font/window size so roughly
/// the same number of lines always fit across the side. The decode side sizes
/// the window; the spectrogram shares it, so the wider 2:1 split (`now_frac` ≈ 2/3)
/// shows *more* time on both sides than the centered 1:1.
fn ws_history_secs(painter: &egui::Painter, body: Rect, protocol: Protocol, now_frac: f32) -> f32 {
    let msg_pt = (WS_MSG_FONT_MAX * body.height() / WS_REF_H).clamp(MIN_FONT_PT, WS_MSG_FONT_MAX);
    let font = mono(msg_pt);
    let cell_w = measure(painter, "0", font.clone()).max(1.0);
    let line_w = (measure(painter, WS_REF_MSG, font) + WS_LINE_PAD_CELLS * cell_w).max(1.0);
    let left_w = (body.width() * now_frac).max(1.0);
    (left_w / line_w) * app_core::slot_period(protocol) as f32
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

/// The parsed message + SNR to resume a contact from, when `d` is a slotted line
/// directed *to us* (`<my call> <their call> …`). `None` for CQs, free text, or
/// lines to someone else — those start no contact we can pick up. The engine does
/// the final role/exchange inference; this only filters to lines addressed to us.
fn resume_intent(d: &Decode, my_call: Option<&str>) -> Option<(ParsedMessage, i8)> {
    let me = my_call?;
    let DecodeContent::Slotted { message, .. } = &d.content else {
        return None;
    };
    let to = match message {
        ParsedMessage::Exchange { to, .. } | ParsedMessage::Signoff { to, .. } => to,
        _ => return None,
    };
    if !to.0.eq_ignore_ascii_case(me) {
        return None;
    }
    Some((message.clone(), d.snr_db.unwrap_or(0)))
}

/// Update CQ shortcut assignments at a slot boundary.
///
/// 1. Drop any callsign that is now worked or was assigned more than 2 slot
///    periods ago. Age is checked against the stored assignment timestamp, so
///    this is independent of whether the decode is still in `recent_decodes()`.
/// 2. Fill freed digit slots (lowest index first) with the top-SNR unassigned
///    callers from the most recent complete slot in `decodes`.
fn update_cq_assignments(
    assignments: &mut HashMap<String, (usize, i64)>,
    decodes: &[Decode],
    worked: &HashSet<String>,
    current_slot_ms: i64,
    slot_ms: i64,
) {
    // Drop worked stations or those assigned more than 2 slot periods ago.
    // Using current_slot_ms (boundary-aligned) rather than wall-clock time
    // ensures exact integer arithmetic against the boundary-aligned d.t.0 values.
    let age_limit = current_slot_ms - slot_ms * 2;
    assignments.retain(|call, &mut (_, assigned_at)| {
        !worked.contains(call) && assigned_at >= age_limit
    });

    // Collect free digit slots (ascending order = lowest index wins).
    let used: HashSet<usize> = assignments.values().map(|&(idx, _)| idx).collect();
    let free: Vec<usize> = (0..10).filter(|i| !used.contains(i)).collect();
    if free.is_empty() {
        return;
    }

    // Candidates: unworked, unassigned CQ callers from the most recent
    // complete slot, ranked by SNR, one entry per callsign (best decode).
    let fill_cutoff = current_slot_ms - slot_ms;
    let mut all: Vec<&Decode> = decodes
        .iter()
        .filter(|d| is_cq(d) && d.t.0 >= fill_cutoff)
        .collect();
    all.sort_by_key(|d| std::cmp::Reverse(d.snr_db));
    let mut seen: HashSet<String> = HashSet::new();
    let mut candidates: Vec<String> = Vec::new();
    for d in all {
        let Some((call, _)) = decode_station(d) else {
            continue;
        };
        let upper = call.to_ascii_uppercase();
        if worked.contains(&upper) || assignments.contains_key(&upper) {
            continue;
        }
        if seen.insert(upper.clone()) {
            candidates.push(upper);
        }
    }

    for (call, &idx) in candidates.iter().zip(free.iter()) {
        assignments.insert(call.clone(), (idx, current_slot_ms));
    }
}

/// Map a top-row digit key to a 0-based shortcut index: '1'→0 … '9'→8, '0'→9.
fn digit_key_index(key: egui::Key) -> Option<usize> {
    match key {
        egui::Key::Num1 => Some(0),
        egui::Key::Num2 => Some(1),
        egui::Key::Num3 => Some(2),
        egui::Key::Num4 => Some(3),
        egui::Key::Num5 => Some(4),
        egui::Key::Num6 => Some(5),
        egui::Key::Num7 => Some(6),
        egui::Key::Num8 => Some(7),
        egui::Key::Num9 => Some(8),
        egui::Key::Num0 => Some(9),
        _ => None,
    }
}

/// Whether a decode is a CQ call (the message type the waterslide bolds when the
/// caller is still unworked).
fn is_cq(d: &Decode) -> bool {
    matches!(
        &d.content,
        DecodeContent::Slotted {
            message: ParsedMessage::Cq { .. },
            ..
        }
    )
}

/// The "waterslide" decode view: each decode placed by audio offset (vertical)
/// and age (horizontal), newest at the centre NOW line and sliding left as it
/// ages. The right (FFT) half is left blank until a spectrum producer is wired.
/// Fed by `BusView::recent_decodes`, i.e. the real decoder's bus stream.
///
/// `click` is the pointer position of a click this frame (if any); a click on a
/// decoded line selects that station (snapping to its *true* audio offset, never
fn draw_hatch(painter: &egui::Painter, strip: Rect, color: Color32) {
    let p = painter.with_clip_rect(strip);
    let h = strip.height().max(1.0);
    let stroke = egui::Stroke::new(1.5, color);
    let mut x = strip.left() - h;
    while x < strip.right() {
        p.line_segment(
            [Pos2::new(x, strip.bottom()), Pos2::new(x + h, strip.top())],
            stroke,
        );
        x += 7.0;
    }
}

fn draw_tx_hatch_row(
    painter: &egui::Painter,
    x_left: f32,
    x_right: f32,
    y_center: f32,
    label: &str,
    font: egui::FontId,
    color: Color32,
) {
    let half_h = WS_HATCH_STRIP_H * 0.5;
    let label_gap = 6.0;
    let label_w = measure(painter, label, font.clone());
    let cx = (x_left + x_right) * 0.5;
    painter.text(Pos2::new(cx, y_center), Align2::CENTER_CENTER, label, font, color);
    let hatch_right = cx - label_w * 0.5 - label_gap;
    let hatch_left = cx + label_w * 0.5 + label_gap;
    if hatch_right > x_left + 2.0 {
        draw_hatch(
            painter,
            Rect::from_min_max(
                Pos2::new(x_left, y_center - half_h),
                Pos2::new(hatch_right, y_center + half_h),
            ),
            color,
        );
    }
    if hatch_left < x_right - 2.0 {
        draw_hatch(
            painter,
            Rect::from_min_max(
                Pos2::new(hatch_left, y_center - half_h),
                Pos2::new(x_right, y_center + half_h),
            ),
            color,
        );
    }
}

/// One LAN peer's working offset, resolved for the deconfliction overlay: the
/// audio offset to place on the vertical axis and the label to draw beside it
/// (`station` plus the worked call, if known). Built by the panel from
/// [`BusView::peers`](crate::bus_view::BusView::peers) — already filtered to the
/// local band and freshness, and sorted high→low offset for the label stagger.
struct PeerTick {
    offset: f32,
    label: String,
}

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
    // Our own station call (upper-cased), so decodes that echo our own over can be
    // drawn in the transmit accent. `None`/empty when no callsign is configured.
    my_call: Option<&str>,
    // Calls already logged on the current band (upper-cased). Decodes from these
    // stations get a "+" status icon; an unworked CQ caller gets a filled circle.
    worked: &HashSet<String>,
    // Stable digit assignments: callsign (upper) → (digit index 0..9, assigned
    // slot ms). A CQ station present here gets a reverse-video digit badge.
    cq_assignments: &HashMap<String, (usize, i64)>,
    tag: Option<&str>,
    bandwidth_hz: f32,
    history_secs: f32,
    slot_secs: f32,
    // The operating-state accent (amber idle / accent2 armed / accent3 keyed),
    // resolved by the caller. Tints the NOW divider and the TX lane.
    accent: Color32,
    // Whether the QSO engine is armed (TX queued) or actively transmitting.
    // Controls the TX lane indicator style (rules vs. hatch rows).
    tx_armed: bool,
    tx_transmitting: bool,
    // When true, the TX audio offset is locked; the idle rules are drawn thicker
    // to make the locked state more prominent.
    offset_locked: bool,
    // NOW-line position as a fraction of the panel width (0.5 = centered 1:1,
    // ~0.667 = the wide-decode 2:1 split). Both sides span `history_secs`, so the
    // right (spectrogram) side gets its own pixels-per-second when it's narrower.
    now_frac: f32,
    // Frequency-view window (Hz): the lowest visible offset and the span shown
    // bottom-to-top. `(0, WS_MAX_HZ)` is the un-zoomed full band. Offsets outside
    // the window are culled; the click→offset inverse reads against it too.
    view_lo: f32,
    view_span: f32,
    // LAN peers' working offsets (deconfliction): already filtered to the local
    // band + freshness and sorted high→low offset by the caller. Drawn as thin
    // dashed lanes distinct from our own solid TX band — display-only, never
    // commands the rig.
    peers: &[PeerTick],
) -> Option<WsClick> {
    let painter = painter.with_clip_rect(rect);
    let now_x = rect.left() + rect.width() * now_frac; // the NOW line
    let left_w = (now_x - rect.left()).max(1.0);
    let pps = left_w / history_secs; // pixels per second on the decode (left) side
    // The spectrogram (right) side spans the same time in its own width, so its
    // pixels-per-second differ once the split isn't 1:1. Used for the slot-rule
    // mirror so the rules still land on the spectrogram's slot columns.
    let right_w = (rect.right() - now_x).max(1.0);
    let pps_right = right_w / history_secs;
    let mut hit: Option<WsClick> = None;

    // Audio offset → vertical position (low Hz at bottom, high Hz at top), mapped
    // through the current frequency-view window. Unclamped: callers that need edge
    // behaviour (the TX lane) clamp to `rect`; decode placement culls out-of-window.
    let view_hi = view_lo + view_span;
    let y_of = |off: f32| rect.bottom() - ((off - view_lo) / view_span) * rect.height();

    // Faint frequency graticule with Hz labels (parked at the right edge, in the
    // otherwise-blank FFT half). Only the gridlines inside the view window are drawn.
    for hz in [500.0f32, 1000.0, 1500.0, 2000.0, 2500.0] {
        if hz < view_lo || hz > view_hi {
            continue;
        }
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
    // (right side) carries the same boundaries, so each draws twice: left over the
    // text at `now_x - age·pps`, and its counterpart over the spectrogram at
    // `now_x + age·pps_right` (the same age, but the spectrogram's own scale).
    let slot_ms = (slot_secs as f64 * 1000.0) as i64;
    if slot_ms > 0 {
        let stroke = egui::Stroke::new(1.0, pal.accent.gamma_multiply(0.5));
        let mut t = (now_ms / slot_ms) * slot_ms; // most recent boundary ≤ now
        loop {
            let age_s = (now_ms - t) as f32 / 1000.0; // seconds before NOW
            // Each side scrolls at its own rate, so the mirror isn't symmetric once
            // the split is 2:1: left uses `pps`, the spectrogram mirror `pps_right`.
            let (xl, xr) = (now_x - age_s * pps, now_x + age_s * pps_right);
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

    // Message font + the half-cell gap, computed up here (not just in the draw
    // pass) so the cull below can measure each line and keep it on screen until
    // its *text* has fully scrolled off — a decode reads rightward from `x`, so
    // it stays legible long after its slot anchor passes the left edge.
    let msg_font = mono(msg_pt);
    // Half a monospace cell — the breathing room either side of the slot rule.
    let half_space = 0.5 * measure(&painter, "0", msg_font.clone());

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
        // Keep it until the message itself (left-aligned at `x + half_space`,
        // reading rightward) is wholly past the left edge — not the moment its
        // slot anchor `x` is, which would blank still-readable text early. The
        // painter clips the partial line at `rect.left()` as it slides off.
        let msg_w = measure(&painter, &decode_text(d), msg_font.clone());
        if x + half_space + msg_w < rect.left() {
            continue; // fully scrolled off the left edge
        }
        // Cull decodes whose signal centre falls outside the frequency window —
        // when zoomed in, off-window traffic drops out rather than piling onto the
        // top/bottom edge (the spatial half of the de-clutter).
        let center_hz = d.offset.0 + half_bw;
        if center_hz < view_lo || center_hz > view_hi {
            continue;
        }
        let ty = y_of(center_hz);
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

    // Second pass: draw. Each message is left-aligned and bumped half a space
    // right of its slot rule so it doesn't crowd it, reading rightward; the
    // signal report sits half a space to the *left* of the rule (its status icon
    // further left still). Both ride `final_y`. `msg_font`/`half_space` are
    // computed above (the cull needs them).
    let snr_font = mono(snr_pt);
    // Reserved width of the (always 3-char) signal report, so the status icon
    // lands at a fixed column whether or not the decode carries an SNR.
    let snr_field_w = 3.0 * measure(&painter, "0", snr_font.clone());
    for p in &placed {
        let d = &decodes[p.idx];
        let station = decode_station(d);
        // A decode from the selected station reads in the secondary accent so the
        // whole lane is easy to follow.
        let is_sel = match (&station, sel_call) {
            (Some((c, _)), Some(s)) => c.as_str() == s,
            _ => false,
        };
        // A decode whose sender is our own station is the echo of what we put on the
        // air (the decoder hears our own over). Draw it in the transmit accent
        // (accent3) so the lane shows our outgoing text in our own colour. This
        // outranks the selected-station tint — it's our message, not one we're working.
        let is_own_tx = match (&station, my_call) {
            (Some((c, _)), Some(mine)) => c.eq_ignore_ascii_case(mine),
            _ => false,
        };
        // A decode addressed *to* our callsign — someone answering our CQ or sending
        // us an exchange/signoff — also reads in accent3 so it catches the eye.
        let is_addressed_to_me = match (&d.content, my_call) {
            (
                DecodeContent::Slotted {
                    message:
                        ParsedMessage::Exchange { to, .. } | ParsedMessage::Signoff { to, .. },
                    ..
                },
                Some(mine),
            ) => to.0.eq_ignore_ascii_case(mine),
            _ => false,
        };
        let msg_col = if is_own_tx || is_addressed_to_me {
            pal.accent3
        } else if is_sel {
            pal.accent2
        } else {
            pal.body
        };
        let snr_col = if is_sel { pal.accent2 } else { pal.accent };
        let text_x = p.x + half_space;
        let msg_rect = painter.text(
            Pos2::new(text_x, p.final_y),
            Align2::LEFT_CENTER,
            decode_text(d),
            msg_font.clone(),
            msg_col,
        );
        // A click on this line selects the station to work (and snaps the TX offset
        // to it); resolved after the loop. Unparsed lines can't be worked. If the
        // line is directed *to us*, also capture it so SEND can pick the contact up
        // mid-stream (resume) rather than arm-and-wait-for-CQ.
        if let Some(cp) = click
            && msg_rect.expand(4.0).contains(cp)
            && let Some((call, slot)) = &station
        {
            hit = Some(WsClick {
                offset: d.offset.0,
                sel: RealSel {
                    target: Some((call.clone(), *slot)),
                    resume: resume_intent(d, my_call),
                },
            });
        }
        // Signal report: right-aligned, half a space to the left of the slot rule.
        let snr = d.snr_db.map(fmt_snr).unwrap_or_else(|| "   ".into());
        let snr_right = p.x - half_space;
        painter.text(
            Pos2::new(snr_right, p.final_y),
            Align2::RIGHT_CENTER,
            snr,
            snr_font.clone(),
            snr_col,
        );
        // Status icon, left of the report and sized to read in sunlight: a
        // right-facing triangle (accent2) marks the selected station; a plus
        // (accent) flags a station already worked on this band; a filled circle
        // (accent) flags an unworked CQ caller; a reverse-video digit badge
        // replaces the circle for stations assigned to a number-key shortcut.
        // Worked outranks CQ so a logged station never reads as "answer me".
        let worked_here = station
            .as_ref()
            .is_some_and(|(c, _)| worked.contains(&c.to_ascii_uppercase()));
        let icon_r = (snr_pt * 0.32).clamp(2.5, 5.0);
        let icon_cx = snr_right - snr_field_w - WS_ICON_GAP - icon_r;
        if is_sel {
            painter.add(egui::Shape::convex_polygon(
                vec![
                    Pos2::new(icon_cx - icon_r, p.final_y - icon_r),
                    Pos2::new(icon_cx - icon_r, p.final_y + icon_r),
                    Pos2::new(icon_cx + icon_r, p.final_y),
                ],
                pal.accent2,
                egui::Stroke::NONE,
            ));
        } else if worked_here {
            let stroke = egui::Stroke::new((icon_r * 0.42).clamp(1.3, 2.0), pal.accent);
            let c = Pos2::new(icon_cx, p.final_y);
            painter.line_segment([Pos2::new(c.x - icon_r, c.y), Pos2::new(c.x + icon_r, c.y)], stroke);
            painter.line_segment([Pos2::new(c.x, c.y - icon_r), Pos2::new(c.x, c.y + icon_r)], stroke);
        } else if is_cq(d) {
            // Check if this CQ caller has a number-key shortcut assigned.
            let rank = decode_station(d)
                .and_then(|(c, _)| cq_assignments.get(&c.to_ascii_uppercase()))
                .map(|&(idx, _)| idx);
            if let Some(rank) = rank
                && !tx_armed
                && !tx_transmitting
            {
                let digit = if rank == 9 { '0' } else { (b'1' + rank as u8) as char };
                let half = (snr_pt * 0.65).max(6.0);
                let badge = Rect::from_center_size(
                    Pos2::new(icon_cx, p.final_y),
                    egui::vec2(half * 2.0, half * 2.0),
                );
                painter.rect_filled(badge, corner_radius(1), pal.accent);
                painter.text(
                    Pos2::new(icon_cx, p.final_y),
                    Align2::CENTER_CENTER,
                    digit.to_string(),
                    heading_bold((snr_pt * 0.9).max(MIN_FONT_PT + 1.0)),
                    pal.screen_bg,
                );
            } else {
                painter.circle_filled(Pos2::new(icon_cx, p.final_y), icon_r, pal.accent);
            }
        }
        // Bumped off its lane: draw a faint leader just left of the text from the
        // true audio centre to the shifted text so the eye still maps the row to
        // its real frequency.
        if (p.final_y - p.true_y).abs() > 1.0 {
            let leader = snr_col.gamma_multiply(0.5);
            painter.line_segment(
                [Pos2::new(text_x - 1.0, p.true_y), Pos2::new(text_x - 1.0, p.final_y)],
                egui::Stroke::new(1.0, leader),
            );
            painter.line_segment(
                [Pos2::new(text_x - 3.0, p.true_y), Pos2::new(text_x + 1.0, p.true_y)],
                egui::Stroke::new(1.0, leader),
            );
        }
    }

    // NOW line at the centre. Tinted by operating state (amber idle / accent2
    // armed / accent3 keyed) so the dividing line echoes the panel frame.
    painter.line_segment(
        [
            Pos2::new(now_x, rect.top()),
            Pos2::new(now_x, rect.bottom()),
        ],
        egui::Stroke::new(2.0, accent),
    );

    // Deconfliction overlay: other operators' working offsets, beaconed over the
    // LAN. Each is a thin DASHED full-width rule with a hollow caret + small label
    // in the secondary accent (`accent2`) — deliberately unlike our own solid,
    // hatched TX band so "theirs" never reads as "mine" (heard ≠ worked, peers ≠
    // me). Drawn before the TX lane so our own band sits on top. The caller has
    // already culled stale and off-band peers and sorted them high→low offset;
    // here we only cull out-of-window offsets and stagger colliding labels. This is
    // display-only — nothing here retunes the rig from peer data.
    if !peers.is_empty() {
        let peer_col = pal.accent2;
        let peer_line = egui::Stroke::new(1.0, peer_col.gamma_multiply(0.65));
        let label_h = snr_pt + 3.0; // min vertical gap between staggered labels
        let mut last_label_y = f32::NEG_INFINITY;
        for tick in peers {
            // Cull peers outside the (possibly zoomed) frequency window — unlike the
            // own-TX lane, an off-band-window peer isn't parked at an edge (that would
            // misrepresent where they are; staleness/edge tricks must not mislead).
            if tick.offset < view_lo || tick.offset > view_hi {
                continue;
            }
            let y = y_of(tick.offset);
            // Dashed horizontal rule across the full pane.
            let mut x = rect.left();
            while x < rect.right() {
                let x2 = (x + 6.0).min(rect.right());
                painter.line_segment([Pos2::new(x, y), Pos2::new(x2, y)], peer_line);
                x += 11.0; // 6 px dash + 5 px gap
            }
            // Small label at the left edge, nudged down if it would collide with the
            // previous one (peers are sorted high→low offset = top→bottom, so the
            // stagger only ever cascades downward).
            let label_y = if y < last_label_y + label_h {
                last_label_y + label_h
            } else {
                y
            };
            last_label_y = label_y;
            painter.text(
                Pos2::new(rect.left() + 5.0, label_y),
                Align2::LEFT_CENTER,
                format!("\u{25C1} {}", tick.label), // ◁ hollow caret = a peer, not us
                snr_font.clone(),
                peer_col,
            );
        }
    }

    // Outgoing-frequency lane (matches the mock-mode indicator): a translucent
    // full-width band shaded to the signal's bandwidth, with bright rules a hair
    // above and below it. The shading spans [offset, offset + bandwidth] — the
    // decode/TX offset is the signal's *lowest* tone, so the energy sits above it
    // (FT8 ≈ 50 Hz, FT4 ≈ 83 Hz); basing the band there lines it up with the
    // traffic on the spectrogram. The rules sit `WS_RULE_GAP` outside the shading
    // so they frame the (slightly taller-looking) real trace rather than cut it.
    // Tinted by operating state — amber idle, accent2 while armed, accent3 while
    // keyed — the same convention as SEND/STOP and the panel frame. Labelled with
    // the selected station's QSO phase (`tag`) or a bare offset.
    let lane = accent;
    let tx_label = match tag {
        Some(tag) => format!("{tag}  {} Hz", tx_off as i32),
        None => format!("\u{25B6} TX {} Hz", tx_off as i32),
    };
    if tx_off + bandwidth_hz < view_lo {
        // Entirely below the view: a chevron + label parked at the bottom edge so
        // the operator never loses track of where they're transmitting.
        painter.text(
            Pos2::new(rect.left() + 4.0, rect.bottom() - 2.0),
            Align2::LEFT_BOTTOM,
            format!("\u{25BC} {tx_label}"),
            snr_font,
            lane,
        );
    } else if tx_off > view_hi {
        // Entirely above the view: chevron + label at the top edge.
        painter.text(
            Pos2::new(rect.left() + 4.0, rect.top() + 2.0),
            Align2::LEFT_TOP,
            format!("\u{25B2} {tx_label}"),
            snr_font,
            lane,
        );
    } else {
        // Visible (possibly partially): clamp the shaded band to the pane so it
        // doesn't invert or draw off-rect when the offset sits near a view edge.
        let bottom = y_of(tx_off).min(rect.bottom());
        let top = y_of(tx_off + bandwidth_hz)
            .max(rect.top())
            .min(bottom - 3.0); // floor at 3px so it stays visible
        let band = Rect::from_min_max(Pos2::new(rect.left(), top), Pos2::new(rect.right(), bottom));
        painter.rect_filled(band, 0.0, lane.gamma_multiply(0.22));
        painter.rect_stroke(
            band,
            0.0,
            egui::Stroke::new(1.0, lane.gamma_multiply(0.50)),
            egui::StrokeKind::Inside,
        );
        if tx_armed || tx_transmitting {
            let state_label = if tx_transmitting {
                tracked("TRANSMITTING")
            } else {
                tracked("ARMED")
            };
            let top_row_y = band.top() - WS_HATCH_GAP;
            let bot_row_y = band.bottom() + WS_HATCH_GAP;
            let hatch_font = heading_bold(snr_pt);
            draw_tx_hatch_row(
                &painter,
                rect.left(),
                rect.right(),
                top_row_y,
                &state_label,
                hatch_font.clone(),
                lane,
            );
            draw_tx_hatch_row(
                &painter,
                rect.left(),
                rect.right(),
                bot_row_y,
                &state_label,
                hatch_font,
                lane,
            );
            painter.text(
                Pos2::new(rect.left() + 4.0, top_row_y - WS_HATCH_STRIP_H * 0.5 - 1.0),
                Align2::LEFT_BOTTOM,
                tx_label,
                snr_font,
                lane,
            );
        } else {
            let rule_top = band.top() - WS_RULE_GAP;
            let rule_bottom = band.bottom() + WS_RULE_GAP;
            let rule_w = if offset_locked { 3.0 } else { 1.5 };
            painter.line_segment(
                [Pos2::new(band.left(), rule_top), Pos2::new(band.right(), rule_top)],
                egui::Stroke::new(rule_w, lane),
            );
            painter.line_segment(
                [Pos2::new(band.left(), rule_bottom), Pos2::new(band.right(), rule_bottom)],
                egui::Stroke::new(rule_w, lane),
            );
            painter.text(
                Pos2::new(rect.left() + 4.0, rule_top - 1.0),
                Align2::LEFT_BOTTOM,
                tx_label,
                snr_font,
                lane,
            );
        }
    }

    // Resolve a click into a new selection: a decoded station (captured in the
    // loop) wins; otherwise it's a bare TX offset read off the vertical position.
    if let Some(cp) = click {
        return Some(hit.unwrap_or_else(|| {
            // Inverse of the windowed `y_of`: read the offset off the vertical click
            // position against the current view. The offset is the signal's *lowest*
            // tone, so its energy center sits `bandwidth/2` above it — subtract half
            // to land the band center on the click point, then clamp to the full band.
            let off = (view_lo + (rect.bottom() - cp.y) / rect.height() * view_span
                - bandwidth_hz / 2.0)
                .clamp(0.0, WS_MAX_HZ);
            WsClick {
                offset: off,
                sel: RealSel::default(),
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

    fn ui(&mut self, ui: &mut egui::Ui, bus: &BusView, pal: &Palette, wide: &mut bool, auto_hop: &mut bool) {
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

                // Display preferences apply immediately and persist on their own (not
                // tied to the lock-to-apply hardware flow), so the change is in effect
                // the moment you re-lock and see the waterslide again.
                ui.add_space(10.0);
                ui.separator();
                ui.label(egui::RichText::new("DISPLAY").color(pal.legend).strong());
                ui.add_space(4.0);
                ui.label("Waterslide split");
                ui.horizontal(|ui| {
                    if ui.radio(!*wide, "1:1  (centered)").clicked() && *wide {
                        *wide = false;
                        crate::settings::save_waterslide_wide(false);
                    }
                    if ui.radio(*wide, "2:1  (wider decode)").clicked() && !*wide {
                        *wide = true;
                        crate::settings::save_waterslide_wide(true);
                    }
                });
                ui.label(
                    egui::RichText::new(
                        "2:1 gives decoded text 2/3 of the panel; both sides span the same time.",
                    )
                    .color(pal.sub)
                    .italics(),
                );

                ui.add_space(8.0);
                ui.separator();
                ui.label(egui::RichText::new("OPERATING").color(pal.legend).strong());
                ui.add_space(4.0);
                let prev_hop = *auto_hop;
                ui.checkbox(auto_hop, "Auto QSY — hop to clearest lane after 3 unanswered CQs");
                if *auto_hop != prev_hop {
                    bus.set_auto_hop(*auto_hop);
                }
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
