//! The waterslide renderer: the scrolling spectrogram, the decode lanes (the
//! "waterslide"), the TX-lane hatching, peer-deconfliction ticks, and the fault
//! placeholder — everything that paints the Digital panel's screen body. Pure
//! drawing plus a couple of decode-interpretation helpers; the panel
//! orchestration, send row, and view gestures stay in the parent module.

use eframe::egui;
use egui::{Align2, Color32, ColorImage, Pos2, Rect, TextureHandle, TextureOptions};
use std::collections::{HashMap, HashSet};

use types::{Decode, DecodeContent, HealthState, ParsedMessage, SlotId, SpectrumRow, SubsystemHealth};

use app_core::Protocol;

use crate::chrome::measure;
use crate::format::{decode_text, fmt_snr};
use crate::theme::*;

use super::{RealSel, WsClick};

/// Audio-offset axis span (Hz): FT8/FT4 decodes land in roughly 0..3000 Hz.
pub(super) const WS_MAX_HZ: f32 = 3000.0;

/// How long a LAN peer's beacon stays drawable on the deconfliction overlay, in
/// seconds. Mirrors the transport's peer TTL (~30 s): a peer we haven't heard from
/// within this window is dropped from the waterslide rather than left lingering —
/// a stale "someone is here" tick would mislead the very view it exists to inform.
pub(super) const PEER_STALE_SECS: u64 = 30;

/// Tightest frequency span (Hz) the view may zoom to — keeps a few signal lanes
/// on screen so zoom-in stays useful rather than collapsing to a single trace.
pub(super) const WS_MIN_SPAN_HZ: f32 = 200.0;

/// Scroll-delta divisor for wheel zoom: larger = gentler. One notch (~50 px of
/// smoothed scroll) is roughly a ±12% span change.
pub(super) const WS_ZOOM_DIV: f32 = 400.0;

/// NOW-line position (fraction of panel width from the left) in the "wide decode"
/// split: the decode/text side gets 2/3 of the panel and the spectrogram 1/3. The
/// 1:1 split parks NOW at 0.5. Both sides span the same amount of *time* either
/// way — only the pixels-per-second differ (see `draw_waterslide`).
pub(super) const WS_DECODE_WIDE_FRAC: f32 = 2.0 / 3.0;

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
pub(super) fn signal_bandwidth_hz(protocol: Protocol) -> f32 {
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
pub(super) struct Spectrogram {
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
    pub(super) fn new() -> Self {
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
    pub(super) fn update_and_paint(
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
pub(super) fn ws_history_secs(painter: &egui::Painter, body: Rect, protocol: Protocol, now_frac: f32) -> f32 {
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
pub(super) fn decode_station(d: &Decode) -> Option<(String, SlotId)> {
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

/// Whether a decode is a CQ call (the message type the waterslide bolds when the
/// caller is still unworked).
pub(super) fn is_cq(d: &Decode) -> bool {
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
pub(super) fn draw_hatch(painter: &egui::Painter, strip: Rect, color: Color32) {
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

/// Vertical sibling of [`draw_hatch`]: the same 45° diagonals filling a *tall, thin*
/// strip (the left/right edges of the scanning border), stepping down in `y` so each
/// line spans the strip's narrow width rather than its full height.
pub(super) fn draw_hatch_v(painter: &egui::Painter, strip: Rect, color: Color32) {
    let p = painter.with_clip_rect(strip);
    let w = strip.width().max(1.0);
    let stroke = egui::Stroke::new(1.5, color);
    let mut y = strip.top() - w;
    while y < strip.bottom() {
        p.line_segment(
            [Pos2::new(strip.left(), y + w), Pos2::new(strip.right(), y)],
            stroke,
        );
        y += 7.0;
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn draw_tx_hatch_row(
    painter: &egui::Painter,
    x_left: f32,
    x_right: f32,
    y_center: f32,
    label: &str,
    font: egui::FontId,
    color: Color32,
    strip_h: f32,
) {
    let half_h = strip_h * 0.5;
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
pub(super) struct PeerTick {
    pub(super) offset: f32,
    pub(super) label: String,
}

/// the de-collided text position), anything else a bare TX offset — returned as
/// the new [`RealSel`]. `tx_off` is the current outgoing offset (marked as the TX
/// lane), `sel_call`/`tag` highlight + label the selected station's lane, and
/// `bandwidth_hz`/`armed` size and tint that lane to the on-air signal.
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_waterslide(
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
                WS_HATCH_STRIP_H,
            );
            draw_tx_hatch_row(
                &painter,
                rect.left(),
                rect.right(),
                bot_row_y,
                &state_label,
                hatch_font,
                lane,
                WS_HATCH_STRIP_H,
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

/// Fault placeholder for the screen body: a centred status line plus the
/// producer's reason, shown when the audio subsystem is down/degraded so the
/// panel reads as "no signal because the device is gone", not "band is quiet".
pub(super) fn draw_fault_body(painter: &egui::Painter, screen: Rect, pal: &Palette, health: &SubsystemHealth) {
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
