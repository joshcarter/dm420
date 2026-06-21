//! Panels are self-contained instruments laid out by the tile tree. Each owns
//! its own *view* state (scan-running, footer toggles, scrolled FFT history) and
//! draws itself from a `PanelCtx` plus its assigned block rect. Domain data
//! (logs, contacts) is read from `panel_data` today; a shared store will feed it
//! through `PanelCtx` in a later effort — panel signatures won't change shape.

use eframe::egui;
use egui::{Rect, TextureHandle};

use crate::bus_view::BusView;
use crate::theme::Palette;

/// A station picked on the Contacts map, handed back to the Waterfall panel so it
/// mirrors the selection (highlight the lane, prime the send row, drive the map
/// crosshair). The map performs the offset move / retune itself via the bus; this
/// only carries the *display* selection so the Digital panel reflects the pick.
pub struct MapPick {
    /// The clicked station's callsign.
    pub call: String,
    /// The slot its last sighting landed in (`SlotId(0)` when unknown), for the
    /// `DecodeRef` the Waterfall builds when the operator arms with Enter.
    pub slot: types::SlotId,
    /// The TX audio offset to snap to, when the map resolved one. `None` leaves the
    /// Waterfall's current offset untouched (a select-only pick with no known freq).
    pub offset: Option<f32>,
}

mod band_scan;
mod call_sign;
mod contacts;
mod log_book;
pub(crate) mod waterfall;

pub use band_scan::BandScan;
pub use call_sign::CallSign;
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
    /// Reverse selection channel: the Contacts map writes a [`MapPick`] when a
    /// station marker is clicked; the Waterfall panel consumes it next frame to
    /// mirror the selection into its own state. `None` most frames.
    pub map_pick: &'a mut Option<MapPick>,
}

/// A drawable instrument. Implementers own their view state and render into the
/// `block` rect (already inset from the chassis groove by the tile behavior).
pub trait Panel {
    fn title(&self) -> &str;
    fn ui(&mut self, ctx: &mut PanelCtx, block: Rect);

    /// Whether clicking this pane should make it the keyboard-focused panel.
    /// Defaults to `true`. Panels that consume only mouse input (e.g. the map,
    /// which pans/zooms on drag) return `false` so they keep receiving pointer
    /// events without stealing keyboard focus from the panel that wants it.
    fn takes_keyboard_focus(&self) -> bool {
        true
    }
}
