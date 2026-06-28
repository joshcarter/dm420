//! "Martian Hybrid" — FT8 console panel in egui.
//!
//! A single instrument-style panel in the "Martian" theme: brushed-metal chassis,
//! recessed glass screens, amber accent, flat tactical hardware. A fixed-height
//! top bar (identity · clocks · DISPLAY/GUI switches) sits over a resizable body
//! laid out by `egui_tiles` — Waterfall (left) and a right stack of Log Book,
//! Band Scan, and Contacts map. The window and every split are draggable.
//!
//! This file is the harness: app boot (fonts, visuals), the per-frame loop, the
//! top bar, and the tile tree/behavior. The panels themselves live in `panels/`,
//! each owning its own view state behind the `Panel` trait; shared drawing
//! helpers live in `chrome`; all colour/chrome flows through a `theme::Palette`.

mod app;
mod app_nap;
mod bus_view;
mod chrome;
mod config_toml;
mod flag;
mod format;
mod geo;
mod geo_data;
mod lane_finder;
mod logging;
mod panel_data;
mod panels;
mod send;
mod settings;
mod theme;
mod waterslide_panel;

use std::time::Duration;

use eframe::egui;
use egui::{
    Align2, CornerRadius, FontData, FontDefinitions, FontFamily, Pos2, Rect, Stroke, Vec2,
};
use egui_tiles::{Behavior, Container, Tile, TileId, Tiles, Tree, UiResponse};

use app::App;
use bus_view::BusView;
use chrome::{lcd_panel, make_brushed, make_relief, measure, paint_chassis, shadow};
use panel_data as pd;
use panels::{BandStatusPanel, CallSign, Contacts, LogBook, Panel, PanelCtx, Waterfall};
use theme::*;

fn main() -> eframe::Result<()> {
    // Install file logging first so everything after it is captured. The guard
    // must live for the whole run (it flushes the writer on drop).
    let _log_guard = logging::init();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "DM420 starting");

    // Keep macOS from napping the process when the window is backgrounded. The
    // key-down and PTT-watchdog paths run on background threads/timers that App Nap
    // would otherwise throttle, which can leave the rig keyed after you tab away
    // mid-over. Held for the whole run (no-op off macOS); dropped at exit.
    let _nap_guard = app_nap::prevent_app_nap();

    // Reopen at the last session's window size & position. The screenshot path
    // keeps a fixed canvas (deterministic shots), so it ignores the saved geometry.
    let deterministic = std::env::var("MARTIAN_SHOT").is_ok() || std::env::var("MARTIAN_LIGHT").is_ok();
    let saved = (!deterministic).then(settings::read_window_size).flatten();
    let size = saved.map_or([pd::PANEL_W, pd::PANEL_H], |w| [w.width, w.height]);
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size(size)
        .with_min_inner_size([900.0, 650.0])
        .with_title("Dingus Mangler 420");
    if let Some((x, y)) = saved.and_then(|w| w.pos) {
        viewport = viewport.with_position([x, y]);
    }
    // Reopen in fullscreen if that's how it was left; the inner size above is the
    // last windowed geometry, so exiting fullscreen returns to a sane window.
    if saved.is_some_and(|w| w.fullscreen) {
        viewport = viewport.with_fullscreen(true);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "martian_hybrid",
        options,
        Box::new(|cc| {
            install_fonts(&cc.egui_ctx);
            Ok(Box::new(App::new(&cc.egui_ctx)))
        }),
    )
}

// ---------------------------------------------------------------------------
// Fonts
// ---------------------------------------------------------------------------

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();

    fonts.font_data.insert(
        "chakra".into(),
        FontData::from_static(include_bytes!("../assets/fonts/ChakraPetch-SemiBold.ttf")).into(),
    );
    fonts.font_data.insert(
        "chakra_bold".into(),
        FontData::from_static(include_bytes!("../assets/fonts/ChakraPetch-Bold.ttf")).into(),
    );
    fonts.font_data.insert(
        "plex".into(),
        FontData::from_static(include_bytes!("../assets/fonts/IBMPlexMono-Medium.ttf")).into(),
    );

    // Two heading families so the design's 600 vs 700 weights stay distinct:
    // legends/headers use Chakra SemiBold; callsigns/numerals/clocks use Bold.
    fonts
        .families
        .insert(FontFamily::Name("heading".into()), vec!["chakra".into()]);
    fonts.families.insert(
        FontFamily::Name("heading_bold".into()),
        vec!["chakra_bold".into()],
    );
    // All data/body text -> Monospace remapped to IBM Plex Mono.
    fonts
        .families
        .insert(FontFamily::Monospace, vec!["plex".into()]);
    // Keep Chakra as the proportional default too, so stray egui widgets match.
    fonts.families.insert(
        FontFamily::Proportional,
        vec!["chakra".into(), "plex".into()],
    );

    ctx.set_fonts(fonts);
}

// ---------------------------------------------------------------------------
// egui widget visuals derived from the active palette
// ---------------------------------------------------------------------------

fn apply_visuals(ctx: &egui::Context, pal: &Palette) {
    let mut v = if pal.is_dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    v.panel_fill = pal.face_bottom;
    v.window_fill = pal.face_bottom;
    v.extreme_bg_color = pal.screen_bg;
    v.override_text_color = Some(pal.body);
    ctx.set_visuals(v);
}

// =====================================================================
// Tile tree + Behavior
// =====================================================================

/// Per-frame view handed to the tile behavior: active palette plus the shared
/// resources panels read. Panels own their own view state, so the behavior no
/// longer threads it through.
struct Tactical<'a> {
    pal: &'a Palette,
    relief: &'a egui::TextureHandle,
    dt: f64,
    bus: &'a BusView,
    /// The operator's configured station identity, threaded to the panels.
    call: &'a str,
    grid: &'a str,
    /// The contest exchange profile + Field Day exchange, threaded mutably so the
    /// unlocked Digital panel's CONTEST selector can edit them in place (on the
    /// `App`'s `Station`, the single owner). Disjoint `Station` fields from
    /// `call`/`grid` above, so the shared + exclusive borrows coexist.
    contest: &'a mut types::ContestProfile,
    fd_class: &'a mut String,
    fd_section: &'a mut String,
    unlocked: bool,
    /// The pane that currently receives keyboard input. Panels compare their own
    /// tile id against this to decide whether Enter/typing is theirs to handle.
    focused: TileId,
    /// Set to a pane id when that pane is clicked this frame, so the app can move
    /// focus after the tree finishes laying out.
    clicked: &'a mut Option<TileId>,
    /// Shared selected-station highlight string. Both the Digital and Contacts panels
    /// select via the `selection/{id}/active` bus topic (the single owner); this is
    /// the callsign read back from it each frame, threaded to every panel so they
    /// highlight/crosshair the same station. Held on the `App` so it survives frames.
    selected_station: &'a mut Option<String>,
}

impl<'a> Behavior<Box<dyn Panel>> for Tactical<'a> {
    fn pane_ui(&mut self, ui: &mut egui::Ui, id: TileId, pane: &mut Box<dyn Panel>) -> UiResponse {
        // The chassis is already painted behind the whole tree. Inset the pane
        // so the recessed screen has chassis breathing room around it (and the
        // grooves between panes read as metal).
        let block = ui.max_rect().shrink2(Vec2::new(8.0, 6.0));
        // Focus-on-click: a press anywhere in the pane focuses it. We test the
        // press position rather than adding a click-sensing widget, so panels'
        // own interactions (waterslide tuning, send button) keep their clicks.
        let press = ui.input(|i| {
            i.pointer
                .any_pressed()
                .then(|| i.pointer.interact_pos())
                .flatten()
        });
        if let Some(pos) = press
            && ui.max_rect().contains(pos)
            && pane.takes_keyboard_focus()
        {
            *self.clicked = Some(id);
        }
        let painter = ui.painter().clone();
        let mut ctx = PanelCtx {
            ui,
            painter: &painter,
            pal: self.pal,
            relief: self.relief,
            dt: self.dt,
            bus: self.bus,
            call: self.call,
            grid: self.grid,
            contest: &mut *self.contest,
            fd_class: &mut *self.fd_class,
            fd_section: &mut *self.fd_section,
            unlocked: self.unlocked,
            active: id == self.focused,
            selected_station: &mut *self.selected_station,
        };
        pane.ui(&mut ctx, block);
        UiResponse::None
    }

    fn tab_title_for_pane(&mut self, pane: &Box<dyn Panel>) -> egui::WidgetText {
        pane.title().into()
    }

    // ---- chrome suppression: flatten everything egui_tiles would draw ----

    fn gap_width(&self, _style: &egui::Style) -> f32 {
        pd::VGROOVE_W // grooves: chassis shows through between panes
    }

    fn min_size(&self) -> f32 {
        // No pane may be dragged below this — enough for a panel header plus a single
        // band row. Sized to the smallest the (pinned) Band Status pane can take so
        // that pin, when the operator has few active bands, isn't clamped back up.
        panels::band_status_pane_height(1)
    }

    fn simplification_options(&self) -> egui_tiles::SimplificationOptions {
        egui_tiles::SimplificationOptions {
            all_panes_must_have_tabs: false,
            ..Default::default()
        }
    }

    fn resize_stroke(&self, _style: &egui::Style, state: egui_tiles::ResizeState) -> Stroke {
        match state {
            egui_tiles::ResizeState::Idle => Stroke::NONE,
            egui_tiles::ResizeState::Hovering => {
                Stroke::new(1.0, self.pal.accent.gamma_multiply(0.5))
            }
            egui_tiles::ResizeState::Dragging => Stroke::new(2.0, self.pal.accent),
        }
    }
}

/// IDs needed after construction to keep Band Scan pinned to a fixed height and
/// to clamp the column widths.
pub struct TreeIds {
    pub root: TileId,
    pub right: TileId,
    pub band: TileId,
    /// The five panes, in keyboard-shortcut order (Cmd/Ctrl-1..5).
    pub waterfall: TileId,
    pub log: TileId,
    pub callsign: TileId,
    pub contacts: TileId,
}

impl TreeIds {
    /// The pane bound to Cmd/Ctrl-`n` (1-based): 1 FT8, 2 Log, 3 Band,
    /// 4 Call Sign, 5 Map.
    pub fn by_number(&self, n: usize) -> Option<TileId> {
        match n {
            1 => Some(self.waterfall),
            2 => Some(self.log),
            3 => Some(self.band),
            4 => Some(self.callsign),
            5 => Some(self.contacts),
            _ => None,
        }
    }
}

/// How long the window geometry must hold still before the reactive save writes
/// it — long enough that a drag/resize coalesces into a single file write, short
/// enough that a quick resize-then-quit still lands before exit.
const WINDOW_SAVE_DEBOUNCE: Duration = Duration::from_millis(700);

impl App {
    /// The current window geometry to persist. When fullscreen, the live rect is
    /// the whole screen, so report the last *windowed* geometry instead (so leaving
    /// fullscreen next launch returns to a sane window) with the flag flipped on;
    /// fall back to the live rect if no windowed frame was seen this session.
    ///
    /// `pos` is the window's top-left in OS points — on macOS that's a global,
    /// multi-monitor coordinate, so restoring it reopens on the same display
    /// without a separate monitor id. `None` if the platform doesn't report it.
    fn current_window(&self, ctx: &egui::Context) -> settings::WindowSize {
        let (sz, outer, fullscreen) = ctx.input(|i| {
            let v = i.viewport();
            (i.content_rect().size(), v.outer_rect, v.fullscreen.unwrap_or(false))
        });
        let live = settings::WindowSize {
            width: sz.x,
            height: sz.y,
            pos: outer.map(|r| (r.min.x, r.min.y)),
            fullscreen: false,
        };
        if fullscreen {
            // Prefer this session's last windowed frame; else the geometry already
            // saved (so a launch straight into restored fullscreen doesn't clobber
            // the good windowed size with the screen rect); else the live rect.
            settings::WindowSize {
                fullscreen: true,
                ..self.last_windowed.or(self.persisted_window).unwrap_or(live)
            }
        } else {
            live
        }
    }

    /// The current tile-split proportions to persist.
    fn current_layout(&self) -> settings::LayoutShares {
        settings::LayoutShares {
            waterfall: self.share_of(self.tree_ids.waterfall),
            right: self.share_of(self.tree_ids.right),
            log: self.share_of(self.tree_ids.log),
            band: self.share_of(self.tree_ids.band),
            callsign: self.share_of(self.tree_ids.callsign),
            contacts: self.share_of(self.tree_ids.contacts),
        }
    }

    /// Save the window geometry + tile layout to the config so the next launch
    /// reopens the same way. Used as a final flush on the close-request path; the
    /// reactive [`Self::maybe_persist_window`] is what catches the common case
    /// (Cmd+Q and other exits never deliver a `close_requested` frame).
    fn persist_window_layout(&self, ctx: &egui::Context) {
        if self.deterministic {
            return;
        }
        settings::save_window_layout(self.current_window(ctx), self.current_layout());
    }

    /// Persist the window geometry whenever it settles after a change. Called every
    /// frame: it debounces so a drag/resize/move (or entering fullscreen) writes
    /// once, when it holds still — independent of the close path, which macOS skips
    /// on Cmd+Q. `request_repaint_after` guarantees a frame fires to flush even if
    /// the app would otherwise idle.
    fn maybe_persist_window(&mut self, ctx: &egui::Context) {
        if self.deterministic {
            return;
        }
        let current = self.current_window(ctx);
        // Never write a degenerate size (frame 0 before layout, etc.).
        if !(current.width.is_finite() && current.height.is_finite())
            || current.width <= 0.0
            || current.height <= 0.0
        {
            return;
        }
        if self.persisted_window == Some(current) {
            self.window_pending = None;
            return;
        }
        let now = ctx.input(|i| i.time);
        // Restart the debounce clock whenever the pending value changes.
        if self.window_pending.map(|(w, _)| w) != Some(current) {
            self.window_pending = Some((current, now));
        }
        let (pending, since) = self.window_pending.unwrap();
        if now - since >= WINDOW_SAVE_DEBOUNCE.as_secs_f64() {
            settings::save_window_layout(pending, self.current_layout());
            self.persisted_window = Some(pending);
            self.window_pending = None;
        } else {
            ctx.request_repaint_after(WINDOW_SAVE_DEBOUNCE);
        }
    }

    /// The linear-container share currently assigned to `id` (its parent split's
    /// weight). Falls back to `1.0` if the tile isn't in a linear container — every
    /// pane here is, so that's a defensive default, not an expected path.
    fn share_of(&self, id: TileId) -> f32 {
        for parent in [self.tree_ids.root, self.tree_ids.right] {
            if let Some(Tile::Container(Container::Linear(lin))) = self.tree.tiles.get(parent)
                && lin.children.contains(&id)
            {
                return lin.shares[id];
            }
        }
        1.0
    }
}

fn build_tree() -> (Tree<Box<dyn Panel>>, TreeIds) {
    let mut tiles = Tiles::default();
    let waterfall = tiles.insert_pane(Box::new(Waterfall::new()) as Box<dyn Panel>);
    let log = tiles.insert_pane(Box::new(LogBook::new()) as Box<dyn Panel>);
    let band = tiles.insert_pane(Box::new(BandStatusPanel::new()) as Box<dyn Panel>);
    let callsign = tiles.insert_pane(Box::new(CallSign::new()) as Box<dyn Panel>);
    let contacts = tiles.insert_pane(Box::new(Contacts::new()) as Box<dyn Panel>);

    // Saved tile proportions from a previous session, if any; otherwise the
    // design defaults (Log 142, Band 128, Call Sign 150, Contacts fills the rest;
    // Waterfall on the left). Resizable from here either way.
    let saved = settings::read_layout_shares();

    let right = tiles.insert_vertical_tile(vec![log, band, callsign, contacts]);
    if let Some(Tile::Container(Container::Linear(lin))) = tiles.get_mut(right) {
        let (log_s, band_s, call_s, contacts_s) = match saved {
            Some(s) => (s.log, s.band, s.callsign, s.contacts),
            None => (
                pd::LOG_H,
                pd::BANDSCAN_H,
                pd::CALLSIGN_H,
                pd::PANEL_H - pd::LOG_H - pd::BANDSCAN_H - pd::CALLSIGN_H,
            ),
        };
        lin.shares.set_share(log, log_s);
        lin.shares.set_share(band, band_s);
        lin.shares.set_share(callsign, call_s);
        lin.shares.set_share(contacts, contacts_s);
    }

    let root = tiles.insert_horizontal_tile(vec![waterfall, right]);
    if let Some(Tile::Container(Container::Linear(lin))) = tiles.get_mut(root) {
        let (wf_s, right_s) = match saved {
            Some(s) => (s.waterfall, s.right),
            None => (pd::LEFT_COL_W, pd::PANEL_W - pd::LEFT_COL_W),
        };
        lin.shares.set_share(waterfall, wf_s);
        lin.shares.set_share(right, right_s);
    }
    (
        Tree::new("martian_tree", root, tiles),
        TreeIds {
            root,
            right,
            band,
            waterfall,
            log,
            callsign,
            contacts,
        },
    )
}

/// Clamp the two-column root split so neither the Waterfall column nor the
/// right-hand stack can be dragged narrower than `pd::MIN_PANEL_W`. egui_tiles'
/// `min_size()` is a single scalar shared by width and height (we use it for the
/// 128px height floor), so the wider width minimum is enforced here each frame
/// by rewriting the horizontal shares — same approach as `pin_band_height`.
fn enforce_min_width(tree: &mut Tree<Box<dyn Panel>>, root: TileId, min_px: f32, gap: f32) {
    let Some(rect) = tree.tiles.rect(root) else {
        return;
    };
    if let Some(Tile::Container(Container::Linear(lin))) = tree.tiles.get_mut(root) {
        if lin.children.len() != 2 {
            return;
        }
        let avail = (rect.width() - gap).max(1.0);
        // Keep feasible if the window is narrower than two minimums.
        let min_px = min_px.min(avail / 2.0);
        let (left, right) = (lin.children[0], lin.children[1]);
        let total = (lin.shares[left] + lin.shares[right]).max(f32::EPSILON);
        let left_px = avail * lin.shares[left] / total;
        if left_px < min_px {
            lin.shares.set_share(left, min_px);
            lin.shares.set_share(right, avail - min_px);
        } else if avail - left_px < min_px {
            lin.shares.set_share(left, avail - min_px);
            lin.shares.set_share(right, min_px);
        }
    }
}

/// Force the Band Scan pane to `target_h` pixels while letting Log Book and Contacts
/// keep sharing the remaining height. egui_tiles lays out a Linear container purely
/// by *shares*, so each frame we solve for the band share that yields the target
/// height given the container's current size, leaving the other two children's shares
/// (and thus their ratio) intact. `target_h` tracks the active-band count
/// ([`panels::band_status_pane_height`]), so the pane grows/shrinks with its rows.
fn pin_band_height(tree: &mut Tree<Box<dyn Panel>>, ids: &TreeIds, gap: f32, target_h: f32) {
    // The container rect from the previous layout (None on the very first frame).
    let Some(rect) = tree.tiles.rect(ids.right) else {
        return;
    };
    if let Some(Tile::Container(Container::Linear(lin))) = tree.tiles.get_mut(ids.right) {
        let num_gaps = lin.children.len().saturating_sub(1) as f32;
        let avail = (rect.height() - gap * num_gaps).max(1.0);
        // Desired fraction of the available height for the band pane.
        let frac = (target_h / avail).clamp(0.05, 0.9);
        // Sum of the other children's shares; band's share is solved so that
        // band / (band + rest) == frac.
        let rest: f32 = lin
            .children
            .iter()
            .filter(|&&c| c != ids.band)
            .map(|&c| lin.shares[c])
            .sum();
        lin.shares.set_share(ids.band, rest * frac / (1.0 - frac));
    }
}

// =====================================================================
// Per-frame loop
// =====================================================================

impl eframe::App for App {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = root.ctx().clone();

        // Work around an upstream macOS/AppKit teardown crash: exit immediately
        // when a close is requested, skipping winit's responder-chain teardown.
        // Because this bypasses eframe's own on-exit `save`, persist the window
        // size + tile layout here, the last reliable point before we exit.
        if ctx.input(|i| i.viewport().close_requested()) {
            // Safety first: never leave the rig keyed on exit. Drop PTT before we
            // bypass normal teardown with process::exit — a mid-over quit would
            // otherwise hold the carrier until the rig's PTT watchdog trips (~15 s).
            self.view.unkey_for_shutdown();
            self.persist_window_layout(&ctx);
            std::process::exit(0);
        }

        // Remember the latest windowed geometry. In fullscreen the live rect is the
        // whole screen, so a fullscreen close persists this instead — keeping the
        // saved fallback size the real window. (Runs after the close check, so it
        // reflects the last drawn-windowed frame.)
        let (live_sz, live_outer, live_fs) = ctx.input(|i| {
            let v = i.viewport();
            (i.content_rect().size(), v.outer_rect, v.fullscreen.unwrap_or(false))
        });
        if !live_fs {
            self.last_windowed = Some(settings::WindowSize {
                width: live_sz.x,
                height: live_sz.y,
                pos: live_outer.map(|r| (r.min.x, r.min.y)),
                fullscreen: false,
            });
        }
        // Persist geometry as it settles, so size/position/fullscreen survive every
        // exit path (the close-request flush below only fires on the red button).
        self.maybe_persist_window(&ctx);

        // Seed the theme from the OS appearance on the first frame it's known
        // (can't be done in `App::new` — the system theme isn't populated until a
        // pass has begun). One-shot; the manual toggle owns the theme afterward.
        self.seed_theme_from_system(&ctx);

        let pal = self.palette();
        if self.visuals_set_for != Some(self.dark) {
            apply_visuals(&ctx, &pal);
            self.visuals_set_for = Some(self.dark);
        }
        if self.brushed.is_none() || self.brushed_is_dark != self.dark {
            self.brushed = Some(make_brushed(&ctx, &pal));
            self.brushed_is_dark = self.dark;
        }
        let brushed = self.brushed.clone().unwrap();
        // Relief is theme-independent (unlike `brushed`), so a one-time lazy build
        // suffices — no dark-mode guard needed. load_texture is synchronous, so the
        // Some(..) set below is always visible to the unwrap on the next line.
        if self.relief.is_none() {
            self.relief = Some(make_relief(&ctx));
        }
        let relief = self.relief.clone().unwrap();

        let dt = ctx.input(|i| i.stable_dt);

        // -------- keyboard ownership: Tab + Enter are DM420's, never egui's --------
        // Tab is exclusively the "lock frequency" hotkey and Enter exclusively
        // "arm/disarm" (handled in the Digital panel's send row). Neither may drive
        // egui's widget-focus system, which otherwise (a) consumes Tab/Shift-Tab/arrows
        // to traverse focus and (b) treats Enter/Space as a click on the focused
        // widget. Both are neutralized here, before any widget is drawn this frame:
        //
        //  (a) egui latches a focus-traversal direction from the raw Tab/arrow events
        //      in `Context::begin_pass` — already run by the time `ui` is called — then
        //      applies it lazily as focusable widgets register during the pass. So an
        //      in-frame `events.retain(Tab)` can't stop it (the direction is already
        //      latched); clearing the direction here, ahead of the top bar and panels,
        //      can. Tab then never moves focus (e.g. onto the LOCK/EDIT posture switch),
        //      deterministically — independent of what egui had focused, including a
        //      station just clicked in the waterslide.
        //
        //  (b) In operate (locked) posture nothing on screen needs the keyboard caret
        //      (the only TextEdits — top-bar call/grid and the contest form — are
        //      unlocked-only), so release any focus a prior click parked: a waterslide
        //      station target (`ws_select`), the LOCK/EDIT switch, the SEND key. A
        //      focused clickable widget fires `.clicked()` on Enter, which would steal
        //      the arm/disarm Enter or re-fire a station select. Dropping it each frame
        //      makes the real chase workflow — click station, Tab to lock, Enter to
        //      arm — resolve to select -> lock -> arm every time. Unlocked posture keeps
        //      focus so the call/grid/contest fields stay typable (click-to-focus; Tab
        //      still won't hop between them, per (a)).
        let operate = !self.edit_mode;
        ctx.memory_mut(|m| {
            m.move_focus(egui::FocusDirection::None);
            if operate && let Some(focused) = m.focused() {
                m.surrender_focus(focused);
            }
        });

        // -------- top bar (fixed height) --------
        egui::Panel::top("topbar")
            .exact_size(pd::TOPBAR_H + pd::GROOVE_H)
            .frame(egui::Frame::NONE)
            .show_inside(root, |ui| {
                let painter = ui.painter().clone();
                let rect = ui.max_rect();
                paint_chassis(&painter, rect, &pal, &brushed);
                let bar = Rect::from_min_max(
                    rect.min,
                    Pos2::new(rect.right(), rect.top() + pd::TOPBAR_H),
                );
                self.top_bar(ui, &painter, bar, &pal);
                // groove under the bar
                painter.rect_filled(
                    Rect::from_min_max(
                        Pos2::new(rect.left(), bar.bottom()),
                        Pos2::new(rect.right(), bar.bottom() + pd::GROOVE_H),
                    ),
                    CornerRadius::ZERO,
                    pal.edge,
                );
            });

        // Keyboard focus: Cmd/Ctrl-1..5 selects a panel (1 FT8, 2 Log, 3 Band,
        // 4 Call Sign, 5 Map). `modifiers.command` is Cmd on macOS, Ctrl elsewhere.
        let focus_num = ctx.input(|i| {
            if !i.modifiers.command {
                return None;
            }
            [
                egui::Key::Num1,
                egui::Key::Num2,
                egui::Key::Num3,
                egui::Key::Num4,
                egui::Key::Num5,
            ]
            .iter()
            .position(|k| i.key_pressed(*k))
            .map(|idx| idx + 1)
        });
        if let Some(id) = focus_num.and_then(|n| self.tree_ids.by_number(n)) {
            self.focused = id;
        }

        // The selection's callsign is the shared highlight string. Both the Digital
        // and Contacts panels select by writing the `selection/{id}/active` bus topic
        // (the single owner); derive the callsign back from it once here, so every
        // panel (Digital lane, map crosshair, Call Sign) reads one consistent value
        // instead of two panels racing to write it.
        self.selected_station = self
            .view
            .selection()
            .and_then(|s| s.target)
            .and_then(|t| t.call)
            .map(|c| c.0);

        // -------- body: chassis + resizable tile tree --------
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(pal.face_bottom))
            .show_inside(root, |ui| {
                let painter = ui.painter().clone();
                paint_chassis(&painter, ui.max_rect(), &pal, &brushed);
                let mut clicked: Option<TileId> = None;
                let mut behavior = Tactical {
                    pal: &pal,
                    relief: &relief,
                    dt: dt as f64,
                    bus: &self.view,
                    call: &self.station.call,
                    grid: &self.station.grid,
                    contest: &mut self.station.contest,
                    fd_class: &mut self.station.fd_class,
                    fd_section: &mut self.station.fd_section,
                    unlocked: self.edit_mode,
                    focused: self.focused,
                    clicked: &mut clicked,
                    selected_station: &mut self.selected_station,
                };
                enforce_min_width(
                    &mut self.tree,
                    self.tree_ids.root,
                    pd::MIN_PANEL_W,
                    pd::VGROOVE_W,
                );
                // Size the Band Status pane to its active-band row count, not a fixed
                // height, so it doesn't reserve space for bands the operator dropped.
                let band_h = panels::band_status_pane_height(self.view.active_bands().len());
                pin_band_height(&mut self.tree, &self.tree_ids, pd::VGROOVE_W, band_h);
                self.tree.ui(&mut behavior, ui);
                // Apply a click-to-focus once the tree has been walked.
                if let Some(id) = clicked {
                    self.focused = id;
                }
            });

        self.run_screenshot(&ctx);
    }

    /// The ⌘Q / normal-termination path. macOS does **not** deliver a
    /// `close_requested` frame here — only the red close button does (handled in
    /// `ui`) — so this `on_exit` is the one hook that runs on a ⌘Q quit. Drop the
    /// rig's PTT so quitting mid-over can't leave the transmitter keyed, then exit
    /// hard to skip winit's crashy AppKit teardown (the same workaround as the
    /// close-request path). Window layout is already persisted by the reactive
    /// `maybe_persist_window` debounce, so there's nothing else to flush here.
    fn on_exit(&mut self) {
        self.view.unkey_for_shutdown();
        std::process::exit(0);
    }
}

// =====================================================================
// Top bar
// =====================================================================

impl App {
    fn top_bar(&mut self, ui: &mut egui::Ui, painter: &egui::Painter, bar: Rect, pal: &Palette) {
        let cy = bar.center().y;

        // ---- identity (far left): a 9px accent marker that matches the FT8
        // panel's focus box (same left edge + width), with the callsign indented
        // to line up with the FT8 panel's label. ----
        // The FT8 pane rect comes from the tile layout (previous frame's, which
        // is fine for a stable layout); fall back to the design inset.
        let panel_left = match self.tree.tiles.rect(self.tree_ids.waterfall) {
            // Match the inset block's left edge (the Tactical shrink2(8, _) inset),
            // where the FT8 header's focus box is drawn.
            Some(r) => r.left() + 8.0,
            None => bar.left() + 8.0,
        };
        let marker = Rect::from_min_max(
            Pos2::new(panel_left, cy - 8.0),
            Pos2::new(panel_left + FOCUS_BOX_SZ, cy + 8.0),
        );
        painter.rect_filled(marker, CornerRadius::ZERO, pal.accent);
        // Callsign left edge == FT8 header label left: focus box (9) + 8px gap.
        let call_x = panel_left + FOCUS_BOX_SZ + 8.0;
        if self.edit_mode {
            // Unlocked: the identity becomes two text fields so the operator can
            // retype the station call sign and grid, then re-lock to commit. Both
            // are kept upper-case to FT8/Maidenhead convention as they're typed.
            let box_h = 22.0;
            let call_rect =
                Rect::from_min_size(Pos2::new(call_x, cy - box_h / 2.0), Vec2::new(118.0, box_h));
            let call_resp = ui.put(
                call_rect,
                egui::TextEdit::singleline(&mut self.station.call)
                    .font(heading_bold(16.0))
                    .char_limit(11)
                    .hint_text("CALL"),
            );
            if call_resp.changed() {
                self.station.call = self.station.call.to_uppercase();
            }
            let grid_rect = Rect::from_min_size(
                Pos2::new(call_rect.right() + 8.0, cy - box_h / 2.0),
                Vec2::new(84.0, box_h),
            );
            let grid_resp = ui.put(
                grid_rect,
                egui::TextEdit::singleline(&mut self.station.grid)
                    .font(mono(13.0))
                    .char_limit(6)
                    .hint_text("GRID"),
            );
            if grid_resp.changed() {
                self.station.grid = self.station.grid.to_uppercase();
            }
        } else {
            let call = tracked(&self.station.call);
            engraved_text(
                painter,
                Pos2::new(call_x, cy),
                &call,
                heading_bold(18.0),
                pal.legend,
                shadow(pal),
                Align2::LEFT_CENTER,
            );
            let grid_x = call_x + measure(painter, &call, heading_bold(18.0)) + 9.0;
            painter.text(
                Pos2::new(grid_x, cy + 1.0),
                Align2::LEFT_CENTER,
                tracked(&self.station.grid),
                mono(9.0),
                pal.sub,
            );
        }

        // ---- right cluster, laid out right-to-left ----
        let right_edge = bar.right() - 24.0;

        let (gui_left, gui_clicks) = self.segmented(
            ui,
            painter,
            pal,
            right_edge,
            cy,
            "GUI",
            &[("LOCK", !self.edit_mode), ("EDIT", self.edit_mode)],
            "sw_gui",
        );
        if gui_clicks[0] {
            // Re-lock commits configuration: push any edited call/grid to the QSO
            // engine, and persist the identity to the config file so it survives a restart
            // (comment-preserving; only once a callsign is actually set).
            self.edit_mode = false;
            self.view.set_qso_station(self.station.to_qso_config());
            if self.station.is_set() {
                self.station.save();
            }
        }
        if gui_clicks[1] {
            self.edit_mode = true;
        }

        let (disp_left, disp_clicks) = self.segmented(
            ui,
            painter,
            pal,
            gui_left - 14.0,
            cy,
            "DISPLAY",
            &[("DARK", self.dark), ("LIGHT", !self.dark)],
            "sw_disp",
        );
        if disp_clicks[0] || disp_clicks[1] {
            self.dark = disp_clicks[0];
            // A manual choice wins from here on — stop the startup OS seed in case
            // the OS theme hadn't been reported yet on this first frame, and
            // persist it so the next launch reopens on the chosen palette.
            self.system_seeded = true;
            settings::save_theme_dark(self.dark);
        }

        // ---- clocks (two LCD chips), centered in the header ----
        // The pair (LOCAL | UTC) is centered on the bar, independent of the
        // right-hand switch cluster. `disp_left` is left unused for placement so
        // the clocks stay put regardless of switch-label width.
        let _ = disp_left;
        let utc = format!("{}", chrono::Utc::now().format("%H:%M:%S"));
        let local = format!("{}", chrono::Local::now().format("%H:%M:%S"));
        const CLOCK_GAP: f32 = 10.0;
        let pair_w =
            lcd_clock_width(painter, "UTC") + CLOCK_GAP + lcd_clock_width(painter, "LOCAL");
        let pair_right = bar.center().x + pair_w / 2.0;
        let utc_left = lcd_clock(painter, pal, pair_right, cy, "UTC", &utc);
        let _ = lcd_clock(painter, pal, utc_left - CLOCK_GAP, cy, "LOCAL", &local);

        // Tick the clocks at least once a second even if nothing animates.
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs(1));
    }

    /// A segmented switch (micro-label above a recessed track of key cells),
    /// flush to `right_x`. Returns its left edge and a per-cell click flag.
    #[allow(clippy::too_many_arguments)]
    fn segmented(
        &self,
        ui: &mut egui::Ui,
        painter: &egui::Painter,
        pal: &Palette,
        right_x: f32,
        cy: f32,
        micro: &str,
        cells: &[(&str, bool)],
        id_src: &str,
    ) -> (f32, Vec<bool>) {
        // 22px track dropped 5px below center so the micro-label clears it above —
        // tuned for the 46px top bar. (Panel headers call `chrome::segmented`
        // directly with a compact, label-less geometry.)
        chrome::segmented(ui, painter, pal, right_x, cy + 5.0, 22.0, micro, cells, id_src)
    }
}

/// LCD clock chip geometry (shared by `lcd_clock` and `lcd_clock_width`).
const CLOCK_READOUT_W: f32 = 79.0;
const CLOCK_PAD_X: f32 = 12.0;
const CLOCK_GAP_X: f32 = 8.0;

/// Total chip width an `lcd_clock` would occupy for `label` — used to center the
/// pair in the header before drawing.
fn lcd_clock_width(painter: &egui::Painter, label: &str) -> f32 {
    let label_w = measure(painter, &tracked(label), mono(8.0));
    CLOCK_PAD_X + label_w + CLOCK_GAP_X + CLOCK_READOUT_W + CLOCK_PAD_X
}

/// One recessed LCD clock chip flush to `right_x`; returns its left edge.
fn lcd_clock(
    painter: &egui::Painter,
    pal: &Palette,
    right_x: f32,
    cy: f32,
    label: &str,
    value: &str,
) -> f32 {
    const READOUT_W: f32 = CLOCK_READOUT_W;
    const PAD_X: f32 = CLOCK_PAD_X;
    const GAP: f32 = CLOCK_GAP_X;
    const H: f32 = 26.0;

    let label_t = tracked(label);
    let label_w = measure(painter, &label_t, mono(8.0));
    let chip_w = lcd_clock_width(painter, label);
    let chip = Rect::from_min_max(
        Pos2::new(right_x - chip_w, cy - H / 2.0),
        Pos2::new(right_x, cy + H / 2.0),
    );
    lcd_panel(painter, chip, pal, 3);

    let lx = chip.left() + PAD_X;
    painter.text(
        Pos2::new(lx, cy),
        Align2::LEFT_CENTER,
        &label_t,
        mono(8.0),
        pal.lcd_text.gamma_multiply(0.6),
    );
    let cell = Rect::from_min_max(
        Pos2::new(lx + label_w + GAP, chip.top()),
        Pos2::new(lx + label_w + GAP + READOUT_W, chip.bottom()),
    );
    // faint glow under the readout
    painter.text(
        cell.center(),
        Align2::CENTER_CENTER,
        value,
        heading_bold(16.0),
        pal.accent.gamma_multiply(0.18),
    );
    painter.text(
        cell.center(),
        Align2::CENTER_CENTER,
        value,
        heading_bold(16.0),
        pal.lcd_text,
    );
    chip.left()
}
