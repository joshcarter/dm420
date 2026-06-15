//! Panels are self-contained instruments laid out by the tile tree. Each owns
//! its own *view* state (scan-running, footer toggles, scrolled FFT history) and
//! draws itself from a `PanelCtx` plus its assigned block rect. Domain data
//! (logs, contacts) is read from `panel_data` today; a shared store will feed it
//! through `PanelCtx` in a later effort — panel signatures won't change shape.

use eframe::egui;
use egui::{Rect, TextureHandle};

use crate::theme::Palette;

mod band_scan;
mod contacts;
mod log_book;
mod waterfall;

pub use band_scan::BandScan;
pub use contacts::Contacts;
pub use log_book::LogBook;
pub use waterfall::Waterfall;

/// Per-frame inputs handed to a panel: the egui `Ui` + a cloned `Painter` for
/// hand-laid chrome, the active palette, the shared relief texture, and the
/// frame delta. Panels use the subset they need.
pub struct PanelCtx<'a> {
    pub ui: &'a mut egui::Ui,
    pub painter: &'a egui::Painter,
    pub pal: &'a Palette,
    pub relief: &'a TextureHandle,
    pub dt: f64,
}

/// A drawable instrument. Implementors own their view state and render into the
/// `block` rect (already inset from the chassis groove by the tile behavior).
pub trait Panel {
    fn title(&self) -> &str;
    fn ui(&mut self, ctx: &mut PanelCtx, block: Rect);
}
