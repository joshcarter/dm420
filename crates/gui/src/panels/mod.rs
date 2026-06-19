//! Panels are self-contained instruments laid out by the tile tree. Each owns
//! its own *view* state (scan-running, footer toggles, scrolled FFT history) and
//! draws itself from a `PanelCtx` plus its assigned block rect. Domain data
//! (logs, contacts) is read from `panel_data` today; a shared store will feed it
//! through `PanelCtx` in a later effort — panel signatures won't change shape.

use eframe::egui;
use egui::{Rect, TextureHandle};

use crate::bus_view::BusView;
use crate::theme::Palette;

mod band_scan;
mod contacts;
mod log_book;
pub(crate) mod waterfall;

pub use band_scan::BandScan;
pub use contacts::Contacts;
pub use log_book::LogBook;
pub use waterfall::Waterfall;

/// Per-frame inputs handed to a panel: the egui `Ui` + a cloned `Painter` for
/// hand-laid chrome, the active palette, the shared relief texture, the frame
/// delta, and the live bus view panels read their data from. Panels use the
/// subset they need.
pub struct PanelCtx<'a> {
    pub ui: &'a mut egui::Ui,
    pub painter: &'a egui::Painter,
    pub pal: &'a Palette,
    pub relief: &'a TextureHandle,
    pub dt: f64,
    pub bus: &'a BusView,
    /// The operator's station call sign (the configured identity, upper-cased).
    /// Read by the FT8 send row to build outgoing messages.
    pub call: &'a str,
    /// The operator's Maidenhead grid locator. Used by the send row and to centre
    /// the Contacts map on home.
    pub grid: &'a str,
    /// True when the GUI is unlocked (the top-bar GUI switch is on EDIT). Panels
    /// use this to reveal their edit/settings affordances; the default (locked)
    /// is the normal operating view. Each panel decides what unlocking means for
    /// it — most ignore it.
    pub unlocked: bool,
    /// True when this panel is the active keyboard target. Only the active panel
    /// should act on Enter / typed input; others ignore keyboard events so the
    /// same key means different things per panel. The FT8 panel is active for now.
    pub active: bool,
    /// The callsign currently selected in the waterslide (the station to work),
    /// shared across panels for the frame. The Waterfall panel writes it from its
    /// selection; the Contacts map reads it to crosshair that station's location.
    /// `None` when nothing (or a bare spectrum offset) is selected.
    pub selected_station: &'a mut Option<String>,
}

/// A drawable instrument. Implementers own their view state and render into the
/// `block` rect (already inset from the chassis groove by the tile behavior).
pub trait Panel {
    fn title(&self) -> &str;
    fn ui(&mut self, ctx: &mut PanelCtx, block: Rect);
}
