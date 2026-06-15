//! Application state. The per-frame loop (`impl eframe::App`) lives in `main.rs`
//! alongside the top bar and tile wiring it drives; this module holds the `App`
//! struct, its construction, palette selection, and the headless screenshot
//! driver. Panel view state now lives inside the panels themselves.

use eframe::egui;
use egui::TextureHandle;
use egui_tiles::Tree;

use crate::panels::Panel;
use crate::theme::{Palette, GRAPHITE, SILVER};
use crate::{build_tree, TreeIds};

pub struct App {
    pub dark: bool,
    pub edit_mode: bool, // GUI LOCK/EDIT
    pub tree: Tree<Box<dyn Panel>>,
    pub tree_ids: TreeIds,
    pub brushed: Option<TextureHandle>,
    pub brushed_is_dark: bool,
    pub relief: Option<TextureHandle>,
    pub visuals_set_for: Option<bool>,
    /// If set (via MARTIAN_SHOT=path), render a few frames, save a PNG, exit.
    pub shot_path: Option<String>,
    pub frame: u64,
}

impl App {
    pub fn new() -> Self {
        let dark = std::env::var("MARTIAN_LIGHT").is_err();
        let (tree, tree_ids) = build_tree();
        Self {
            dark,
            edit_mode: false,
            tree,
            tree_ids,
            brushed: None,
            brushed_is_dark: !dark,
            relief: None,
            visuals_set_for: None,
            shot_path: std::env::var("MARTIAN_SHOT").ok(),
            frame: 0,
        }
    }

    pub fn palette(&self) -> Palette {
        if self.dark { GRAPHITE } else { SILVER }
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
            if let Some(buf) = image::RgbaImage::from_raw(w as u32, h as u32, image.as_raw().to_vec()) {
                let _ = buf.save(&path);
                eprintln!("saved screenshot: {path} ({w}x{h})");
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}
