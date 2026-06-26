//! Waterfall panel: header (Digital mode + freq readout + FT8/FT4 toggle) + the live Waterslide
//! FFT sim as the screen body + a decode ticker along the bottom.

use eframe::egui;
use egui::{Pos2, Rect};
use std::collections::HashMap;

use types::{
    AbsHz, Band, Callsign, Decode, DecodeRef, OffsetHz, OverAirMode, ParsedMessage, QsoPhase,
    ScanStatus, Selection, SelectionContext, SlotId, SubsystemHealth, SubsystemId,
};

use app_core::Protocol;

use super::{Panel, PanelCtx};
use crate::chrome::{key_cell_accent, lcd_panel, measure, panel_header};
use crate::panel_data as pd;
use crate::send::SendState;
use crate::theme::*;

mod config_form;
mod render;
mod send_row;
use config_form::ConfigForm;
use render::*;
use send_row::update_cq_assignments;

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

        // SCAN button — just right of the FREQ readout. Toggles the band scanner; the
        // label tracks the run state (read from `ScannerState`, the single owner),
        // showing STOP while a sweep runs. Mouse path, so it works regardless of focus.
        let scanning = ctx
            .bus
            .scanner()
            .map(|s| s.status == ScanStatus::Scanning)
            .unwrap_or(false);
        let scan_label = if scanning { "STOP" } else { "SCAN" };
        let scan_cell_w = measure(painter, &tracked(scan_label), heading_bold(9.0)) + 14.0;
        let scan_track_w = scan_cell_w + 4.0;
        let scan_track = Rect::from_center_size(
            Pos2::new(header.center().x + 48.0 + scan_track_w * 0.5, cy),
            egui::Vec2::new(scan_track_w, 20.0),
        );
        lcd_panel(painter, scan_track, pal, 4);
        let scan_cell = Rect::from_min_max(
            Pos2::new(scan_track.left() + 2.0, scan_track.top() + 2.0),
            Pos2::new(scan_track.right() - 2.0, scan_track.bottom() - 2.0),
        );
        if key_cell_accent(
            ctx.ui,
            painter,
            pal,
            scan_cell,
            scan_label,
            true,
            pal.accent,
            ctx.ui.id().with("header_scan"),
        )
        .clicked()
        {
            Self::toggle_scan(ctx);
        }

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

