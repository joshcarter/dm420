//! The Digital panel's auto-sequenced send row + slash-command handler, plus the
//! CQ digit-shortcut bookkeeping. Split out of the panel module so the
//! operating/keyboard surface sits apart from the orchestration and the renderer.
//! The two methods extend `Waterfall` (a descendant `impl`), so they reach the
//! panel's private state directly.

use eframe::egui;
use egui::{Align2, Pos2, Rect};
use std::collections::{HashMap, HashSet};

use types::{
    Callsign, Decode, DecodeRef, OffsetHz, OverAirMode, QsoPhase, ScanStatus, SelectionContext,
};

use crate::chrome::{key_cell_accent, lcd_panel, measure};
use crate::send::{Activation, Command};
use crate::theme::*;
use crate::waterslide_panel::Target;

use super::render::{decode_station, is_cq};
use super::{PanelCtx, Waterfall};

impl Waterfall {
    /// The bottom "Send" row: `Send:` label, recessed message box (mirrors the
    /// next outgoing message, themed to the screen background), and a right-aligned
    /// Scan-style lit key — orange `SEND` when idle/armed, cyan `CANCEL` while
    /// transmitting (cyan also signals the armed state). The box is not a free text
    /// field: only `/`/`:` (start a slash command) and Enter (activate the button)
    /// are accepted; see `send.rs`.
    pub(super) fn draw_send_row(&mut self, ctx: &mut PanelCtx, row: Rect) {
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
        // The scanner sweeping makes the panel non-interactive: suppress the digit /
        // Tab / Q shortcuts (the `s` and ESC scan toggles stay live, gated separately).
        let scanning = ctx
            .bus
            .scanner()
            .map(|s| s.status == ScanStatus::Scanning)
            .unwrap_or(false);
        let shortcuts_active = !scanning
            && self.tx_hold.is_none()
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
                // S — toggle the band scanner: cancel a running sweep (anytime), or
                // start one over the configured stops (only when not mid-QSO, so a scan
                // can't hijack a live contact). No modifiers, NOT mid-command.
                if let egui::Event::Key { key: egui::Key::S, pressed: true, modifiers, .. } = ev
                    && !modifiers.any()
                    && !self.send.entering
                {
                    let scanning = ctx
                        .bus
                        .scanner()
                        .map(|s| s.status == ScanStatus::Scanning)
                        .unwrap_or(false);
                    if scanning || shortcuts_active {
                        Self::toggle_scan(ctx);
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
                    } => {
                        // Abandoning a slash-command entry wins; otherwise ESC cancels a
                        // running scan.
                        if self.send.entering {
                            self.send.escape();
                        } else if ctx
                            .bus
                            .scanner()
                            .map(|s| s.status == ScanStatus::Scanning)
                            .unwrap_or(false)
                        {
                            ctx.bus.cancel_scan();
                        }
                    }
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
        // Blocked while scanning — the panel is non-interactive; cancel a sweep with
        // the SCAN button, s, or ESC instead.
        if (activate || btn.clicked()) && !scanning {
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
            Command::Scan => Self::toggle_scan(ctx),
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
                    Command::ClearQsy | Command::Scan => unreachable!(),
                };
                if let Some(hz) = hz {
                    ctx.bus.set_freq(hz);
                }
            }
        }
    }

    /// Start a survey of the configured stops, or cancel the one in progress —
    /// shared by the SCAN button, the `s` key, and `/scan`. Which way to toggle is
    /// read from the authoritative `ScannerState`, never a panel-local flag.
    pub(super) fn toggle_scan(ctx: &PanelCtx) {
        let scanning = ctx
            .bus
            .scanner()
            .map(|s| s.status == ScanStatus::Scanning)
            .unwrap_or(false);
        if scanning {
            ctx.bus.cancel_scan();
        } else {
            ctx.bus.start_scan(ctx.bus.scan_stops(), 2);
        }
    }
}

/// Update CQ shortcut assignments at a slot boundary.
///
/// 1. Drop any callsign that is now worked or was assigned more than 2 slot
///    periods ago. Age is checked against the stored assignment timestamp, so
///    this is independent of whether the decode is still in `recent_decodes()`.
/// 2. Fill freed digit slots (lowest index first) with the top-SNR unassigned
///    callers from the most recent complete slot in `decodes`.
pub(super) fn update_cq_assignments(
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
pub(super) fn digit_key_index(key: egui::Key) -> Option<usize> {
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

