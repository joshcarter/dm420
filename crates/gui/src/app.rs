//! Application state. The per-frame loop (`impl eframe::App`) lives in `main.rs`
//! alongside the top bar and tile wiring it drives; this module holds the `App`
//! struct, its construction, palette selection, and the headless screenshot
//! driver. Panel view state now lives inside the panels themselves.

use eframe::egui;
use egui::TextureHandle;
use egui_tiles::Tree;

use crate::bus_view::BusView;
use crate::panels::Panel;
use crate::settings::Station;
use crate::theme::{GRAPHITE, Palette, SILVER};
use crate::{TreeIds, build_tree};

pub struct App {
    pub dark: bool,
    /// Seed `dark` from the OS appearance on the first frame. False when
    /// `MARTIAN_LIGHT` pins light (screenshots), so the env wins. The OS theme is
    /// only readable once a pass has begun, so the seed happens in `ui`, not
    /// `new`; see [`App::seed_theme_from_system`].
    pub follow_system_at_startup: bool,
    /// One-shot guard for the startup OS-theme seed. Set once the seed is applied
    /// or the operator touches the DARK/LIGHT toggle — after that the manual
    /// toggle owns the theme and live OS changes are ignored.
    pub system_seeded: bool,
    pub edit_mode: bool, // GUI LOCK/EDIT
    /// The operator's station identity (call sign + grid). Shown in the top bar
    /// and editable there when unlocked; read by the panels via `PanelCtx`.
    pub station: Station,
    pub tree: Tree<Box<dyn Panel>>,
    pub tree_ids: TreeIds,
    /// The pane that currently has keyboard focus (click or Cmd/Ctrl-1..4).
    pub focused: egui_tiles::TileId,
    pub brushed: Option<TextureHandle>,
    pub brushed_is_dark: bool,
    pub relief: Option<TextureHandle>,
    pub visuals_set_for: Option<bool>,
    /// If set (via MARTIAN_SHOT=path), render a few frames, save a PNG, exit.
    pub shot_path: Option<String>,
    pub frame: u64,
    /// Live bus state the panels render from (mock-fed for now).
    pub view: BusView,
    /// The callsign selected in the waterslide, shared between panels each frame.
    /// The Waterfall panel publishes its current selection here; the Contacts map
    /// reads it to crosshair that station's location. `None` when nothing is selected.
    pub selected_station: Option<String>,
    /// The most recent *windowed* geometry (size + position), refreshed each frame
    /// the window isn't fullscreen. Persisted on a fullscreen close so the saved
    /// fallback size is the real window, not the whole screen. `None` until the
    /// first windowed frame. (`fullscreen` on the stored value is always `false`.)
    pub last_windowed: Option<crate::settings::WindowSize>,
    /// Skip writing geometry/theme back to the config — set for the deterministic
    /// screenshot paths (`MARTIAN_SHOT`/`MARTIAN_LIGHT`) so a capture run never
    /// clobbers the operator's saved window.
    pub deterministic: bool,
    /// The window geometry last written to the config, so the reactive save only
    /// writes on a real change. Seeded from the file at startup; `None` if none was
    /// saved yet (so the first stable frame creates the `[window]` table).
    pub persisted_window: Option<crate::settings::WindowSize>,
    /// Debounce state for the reactive save: the geometry seen and the time (eframe
    /// seconds) it was first seen. Flushed to disk once it holds still; reset on
    /// every change so a drag/resize writes once, when it settles.
    pub window_pending: Option<(crate::settings::WindowSize, f64)>,
}

impl App {
    /// Build the app. `egui_ctx` is handed to the bus bridge so background data
    /// arriving off-frame can wake the UI.
    pub fn new(egui_ctx: &egui::Context) -> Self {
        // Theme precedence: `MARTIAN_LIGHT` (the headless screenshot path) pins
        // light → a saved `[display] dark` choice → the OS appearance, seeded on
        // the first frame (`seed_theme_from_system`). The env always wins; a saved
        // choice wins over the OS seed and turns the startup follow off.
        let forced_light = std::env::var("MARTIAN_LIGHT").is_ok();
        // The screenshot paths keep a fixed canvas and must not write geometry back.
        let deterministic = forced_light || std::env::var("MARTIAN_SHOT").is_ok();
        let saved_dark = (!forced_light).then(crate::settings::read_theme_dark).flatten();
        let dark = if forced_light {
            false
        } else {
            saved_dark.unwrap_or(true)
        };
        let (tree, tree_ids) = build_tree();
        let focused = tree_ids.waterfall; // FT8 panel holds focus at startup
        let station = Station::load();
        let view = BusView::start(egui_ctx.clone(), station.to_qso_config());
        Self {
            dark,
            // Only seed from the OS when light isn't pinned and nothing was saved.
            follow_system_at_startup: !forced_light && saved_dark.is_none(),
            system_seeded: false,
            // No default callsign: when the station identity isn't set yet, boot
            // straight into config (unlocked) so the operator is prompted for it.
            edit_mode: !station.is_set(),
            station,
            tree,
            tree_ids,
            focused,
            brushed: None,
            brushed_is_dark: !dark,
            relief: None,
            visuals_set_for: None,
            shot_path: std::env::var("MARTIAN_SHOT").ok(),
            frame: 0,
            view,
            selected_station: None,
            last_windowed: None,
            deterministic,
            // Seed from the file so we don't re-write an unchanged geometry on boot;
            // `None` (no saved `[window]` yet) means the first stable frame creates it.
            persisted_window: (!deterministic).then(crate::settings::read_window_size).flatten(),
            window_pending: None,
        }
    }

    pub fn palette(&self) -> Palette {
        if self.dark { GRAPHITE } else { SILVER }
    }

    /// One-shot: on the first frame the OS appearance is known, seed `dark` from
    /// it so the app boots matching the host's light/dark setting. `MARTIAN_LIGHT`
    /// opts out (the env pins light). After seeding, the manual DARK/LIGHT toggle
    /// owns the theme — we don't keep following live OS changes (the chosen
    /// "startup default only" behavior). Cheap to call every frame; it no-ops once
    /// seeded or while the OS theme is still unknown.
    pub fn seed_theme_from_system(&mut self, ctx: &egui::Context) {
        if self.system_seeded || !self.follow_system_at_startup {
            return;
        }
        if let Some(theme) = ctx.system_theme() {
            self.dark = theme == egui::Theme::Dark;
            self.system_seeded = true;
        }
    }

    // -----------------------------------------------------------------
    // Headless screenshot driver (MARTIAN_SHOT=path)
    // -----------------------------------------------------------------

    pub fn run_screenshot(&mut self, ctx: &egui::Context) {
        let Some(path) = self.shot_path.clone() else {
            return;
        };
        self.frame += 1;
        ctx.request_repaint();
        if self.frame == 4 {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
        let shot = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = shot {
            let [w, h] = image.size;
            if let Some(buf) =
                image::RgbaImage::from_raw(w as u32, h as u32, image.as_raw().to_vec())
            {
                let _ = buf.save(&path);
                tracing::info!("saved screenshot {path} ({w}x{h})");
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}
