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
mod chrome;
mod geo_data;
mod panel_data;
mod panels;
mod theme;
mod waterslide_panel;
mod waterslide_sim;

use eframe::egui;
use egui::{
    Align2, CornerRadius, FontData, FontDefinitions, FontFamily, Pos2, Rect, Stroke, Vec2,
};
use egui_tiles::{Behavior, Container, Tile, TileId, Tiles, Tree, UiResponse};

use app::App;
use chrome::{key_cell, lcd_panel, make_brushed, make_relief, measure, paint_chassis, shadow};
use panel_data as pd;
use panels::{BandScan, Contacts, LogBook, Panel, PanelCtx, Waterfall};
use theme::*;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([pd::PANEL_W, pd::PANEL_H])
            .with_min_inner_size([720.0, 460.0])
            .with_title("Martian Hybrid — FT8 Console"),
        ..Default::default()
    };
    eframe::run_native(
        "martian_hybrid",
        options,
        Box::new(|cc| {
            install_fonts(&cc.egui_ctx);
            Ok(Box::new(App::new()))
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
}

impl<'a> Behavior<Box<dyn Panel>> for Tactical<'a> {
    fn pane_ui(&mut self, ui: &mut egui::Ui, _id: TileId, pane: &mut Box<dyn Panel>) -> UiResponse {
        // The chassis is already painted behind the whole tree. Inset the pane
        // so the recessed screen has chassis breathing room around it (and the
        // grooves between panes read as metal).
        let block = ui.max_rect().shrink2(Vec2::new(8.0, 6.0));
        let painter = ui.painter().clone();
        let mut ctx = PanelCtx {
            ui,
            painter: &painter,
            pal: self.pal,
            relief: self.relief,
            dt: self.dt,
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
        // No pane may be dragged below this — enough for the panel header plus a
        // modest slice of inner screen. Matches the pinned Band Scan height.
        pd::BANDSCAN_H
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
            egui_tiles::ResizeState::Hovering => Stroke::new(1.0, self.pal.accent.gamma_multiply(0.5)),
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
}

fn build_tree() -> (Tree<Box<dyn Panel>>, TreeIds) {
    let mut tiles = Tiles::default();
    let waterfall = tiles.insert_pane(Box::new(Waterfall::new()) as Box<dyn Panel>);
    let log = tiles.insert_pane(Box::new(LogBook::new()) as Box<dyn Panel>);
    let band = tiles.insert_pane(Box::new(BandScan::new()) as Box<dyn Panel>);
    let contacts = tiles.insert_pane(Box::new(Contacts::new()) as Box<dyn Panel>);

    let right = tiles.insert_vertical_tile(vec![log, band, contacts]);
    // Initial right-column proportions ≈ the design heights (Log 142, Band 112,
    // Contacts fills the rest). Resizable from here.
    if let Some(Tile::Container(Container::Linear(lin))) = tiles.get_mut(right) {
        lin.shares.set_share(log, pd::LOG_H);
        lin.shares.set_share(band, pd::BANDSCAN_H);
        lin.shares.set_share(contacts, pd::PANEL_H - pd::LOG_H - pd::BANDSCAN_H);
    }

    let root = tiles.insert_horizontal_tile(vec![waterfall, right]);
    if let Some(Tile::Container(Container::Linear(lin))) = tiles.get_mut(root) {
        lin.shares.set_share(waterfall, pd::LEFT_COL_W);
        lin.shares.set_share(right, pd::PANEL_W - pd::LEFT_COL_W);
    }
    (
        Tree::new("martian_tree", root, tiles),
        TreeIds { root, right, band },
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

/// Force the Band Scan pane to a fixed pixel height (`pd::BANDSCAN_H`) while
/// letting Log Book and Contacts keep sharing the remaining height. egui_tiles
/// lays out a Linear container purely by *shares*, so each frame we solve for
/// the band share that yields the target height given the container's current
/// size, leaving the other two children's shares (and thus their ratio) intact.
fn pin_band_height(tree: &mut Tree<Box<dyn Panel>>, ids: &TreeIds, gap: f32) {
    // The container rect from the previous layout (None on the very first frame).
    let Some(rect) = tree.tiles.rect(ids.right) else {
        return;
    };
    if let Some(Tile::Container(Container::Linear(lin))) = tree.tiles.get_mut(ids.right) {
        let num_gaps = lin.children.len().saturating_sub(1) as f32;
        let avail = (rect.height() - gap * num_gaps).max(1.0);
        // Desired fraction of the available height for the band pane.
        let frac = (pd::BANDSCAN_H / avail).clamp(0.05, 0.9);
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
        if ctx.input(|i| i.viewport().close_requested()) {
            std::process::exit(0);
        }

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

        // -------- body: chassis + resizable tile tree --------
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(pal.face_bottom))
            .show_inside(root, |ui| {
                let painter = ui.painter().clone();
                paint_chassis(&painter, ui.max_rect(), &pal, &brushed);
                let mut behavior = Tactical {
                    pal: &pal,
                    relief: &relief,
                    dt: dt as f64,
                };
                enforce_min_width(&mut self.tree, self.tree_ids.root, pd::MIN_PANEL_W, pd::VGROOVE_W);
                pin_band_height(&mut self.tree, &self.tree_ids, pd::VGROOVE_W);
                self.tree.ui(&mut behavior, ui);
            });

        self.run_screenshot(&ctx);
    }
}

// =====================================================================
// Top bar
// =====================================================================

impl App {
    fn top_bar(&mut self, ui: &mut egui::Ui, painter: &egui::Painter, bar: Rect, pal: &Palette) {
        let cy = bar.center().y;

        // ---- identity (far left): spine 3×16 + N0JDC + DN70KA ----
        let x0 = bar.left() + 14.0;
        let spine = Rect::from_min_max(Pos2::new(x0, cy - 8.0), Pos2::new(x0 + 3.0, cy + 8.0));
        painter.rect_filled(spine, CornerRadius::ZERO, pal.accent);
        let call_x = spine.right() + 8.0;
        let call = tracked("N0JDC");
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
            &tracked("DN70KA"),
            mono(9.0),
            pal.sub,
        );

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
            self.edit_mode = false;
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
        if disp_clicks[0] {
            self.dark = true;
        }
        if disp_clicks[1] {
            self.dark = false;
        }

        // ---- clocks (two LCD chips), to the left of the switches ----
        let utc = format!("{}", chrono::Utc::now().format("%H:%M:%S"));
        let local = format!("{}", chrono::Local::now().format("%H:%M:%S"));
        let utc_left = lcd_clock(painter, pal, disp_left - 16.0, cy, "UTC", &utc);
        let _ = lcd_clock(painter, pal, utc_left - 10.0, cy, "LOCAL", &local);

        // Tick the clocks at least once a second even if nothing animates.
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs(1));
    }

    /// A segmented switch (micro-label above a recessed track of key cells),
    /// flush to `right_x`. Returns its left edge and a per-cell click flag.
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
        const TRACK_H: f32 = 22.0;
        const PAD: f32 = 2.0;
        const GAP: f32 = 2.0;
        const CELL_PAD_X: f32 = 11.0;

        let widths: Vec<f32> = cells
            .iter()
            .map(|(t, _)| measure(painter, &tracked(t), heading(9.0)) + CELL_PAD_X * 2.0)
            .collect();
        let track_w: f32 = PAD * 2.0 + widths.iter().sum::<f32>() + GAP * (cells.len() as f32 - 1.0);

        let track_cy = cy + 5.0;
        let track = Rect::from_min_max(
            Pos2::new(right_x - track_w, track_cy - TRACK_H / 2.0),
            Pos2::new(right_x, track_cy + TRACK_H / 2.0),
        );
        lcd_panel(painter, track, pal, 4);

        painter.text(
            Pos2::new(track.left(), track.top() - 3.0),
            Align2::LEFT_BOTTOM,
            &tracked(micro),
            mono(7.0),
            pal.sub,
        );

        let cell_h = TRACK_H - PAD * 2.0;
        let mut x = track.left() + PAD;
        let mut clicks = Vec::with_capacity(cells.len());
        for (i, ((label, active), w)) in cells.iter().zip(widths.iter()).enumerate() {
            let cell = Rect::from_min_size(Pos2::new(x, track.top() + PAD), Vec2::new(*w, cell_h));
            let resp = key_cell(ui, painter, pal, cell, label, *active, ui.id().with((id_src, i)));
            clicks.push(resp.clicked());
            x += w + GAP;
        }
        (track.left(), clicks)
    }
}

/// One recessed LCD clock chip flush to `right_x`; returns its left edge.
fn lcd_clock(painter: &egui::Painter, pal: &Palette, right_x: f32, cy: f32, label: &str, value: &str) -> f32 {
    const READOUT_W: f32 = 79.0;
    const PAD_X: f32 = 12.0;
    const GAP: f32 = 8.0;
    const H: f32 = 26.0;

    let label_t = tracked(label);
    let label_w = measure(painter, &label_t, mono(8.0));
    let chip_w = PAD_X + label_w + GAP + READOUT_W + PAD_X;
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
    painter.text(cell.center(), Align2::CENTER_CENTER, value, heading_bold(16.0), pal.accent.gamma_multiply(0.18));
    painter.text(cell.center(), Align2::CENTER_CENTER, value, heading_bold(16.0), pal.lcd_text);
    chip.left()
}
