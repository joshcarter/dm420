//! Waterslide — shared view vocabulary used by the live Digital panel.
//!
//! Holds the palette-derived [`WaterslideTheme`] (colours + colormap), the
//! amber/spectral colormap builders ([`martian_cmap`] / [`martian_cmap_light`]),
//! and the [`Target`] enum naming what the next transmission is aimed at. These
//! are consumed by `panels/waterfall.rs` and the send-row builder (`send.rs`);
//! the panel's rendering lives there, not here.

use eframe::egui::Color32;

use crate::theme::Palette;

/// Palette the panel reads. Built from the app's active `Palette` so it flips on
/// the existing light/dark toggle; the colormap is the only new token.
//
// Retained as part of the live waterslide view vocabulary (alongside the colormap
// builders below) though not yet wired into the panel's render path, which calls
// `martian_cmap[_light]` directly; see ARCHITECTURE_REVIEW Split #1.
#[allow(dead_code)]
pub struct WaterslideTheme {
    pub accent: Color32,
    pub text: Color32,
    pub dim: Color32,
    pub legend: Color32,
    pub screen_bg: Color32,
    pub grid: Color32,
    pub grid_mid: Color32,
    pub cmap: [Color32; 256], // intensity 0..1 → colour
}

#[allow(dead_code)]
impl WaterslideTheme {
    /// Derive a Waterslide theme from the spike's active palette. The amber
    /// "Martian" colormap is used in both light and dark — a spectrogram is an
    /// inherently dark scientific display, so it reads correctly on either face.
    pub fn from_palette(pal: &Palette) -> Self {
        Self {
            accent: pal.accent,
            text: pal.body,
            dim: pal.dim,
            legend: pal.legend,
            screen_bg: pal.screen_bg,
            grid: pal.dim.gamma_multiply(0.35),
            grid_mid: pal.accent.gamma_multiply(0.55),
            // Dark face: a dark-background amber map (signal = bright). Light face:
            // the "Martian Ink" spectral map — a paper floor ramping up through
            // gold/orange/magenta/violet to deep indigo at the peaks.
            cmap: if pal.is_dark {
                martian_cmap()
            } else {
                martian_cmap_light()
            },
        }
    }
}

/// 6-stop colormap builder. Stops are (position 0..1, [r,g,b]).
pub fn build_cmap(stops: &[(f32, [u8; 3])]) -> [Color32; 256] {
    let mut lut = [Color32::BLACK; 256];
    for (i, slot) in lut.iter_mut().enumerate() {
        let v = i as f32 / 255.0;
        let mut a = stops[0];
        let mut b = stops[stops.len() - 1];
        for w in stops.windows(2) {
            if v >= w[0].0 && v <= w[1].0 {
                a = w[0];
                b = w[1];
                break;
            }
        }
        let tt = if (b.0 - a.0).abs() < 1e-6 {
            0.0
        } else {
            (v - a.0) / (b.0 - a.0)
        };
        let lerp = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * tt) as u8;
        *slot = Color32::from_rgb(
            lerp(a.1[0], b.1[0]),
            lerp(a.1[1], b.1[1]),
            lerp(a.1[2], b.1[2]),
        );
    }
    lut
}

/// The "Martian / graphite" colormap (dark amber): dark background, bright signal.
/// Runs from the screen background, up through the orange accent (#F7920F), and
/// on to white so the strongest signals read at full brightness.
pub fn martian_cmap() -> [Color32; 256] {
    build_cmap(&[
        (0.00, [8, 6, 4]),       // GRAPHITE screen_bg — noise floor blends in
        (0.18, [33, 18, 8]),     // noise floor lifts just off the background
        (0.42, [150, 70, 12]),   // ramping up into the accent
        (0.62, [247, 146, 15]),  // GRAPHITE accent #F7920F at full strength
        (0.82, [251, 201, 120]), // hot amber on the way to white
        (1.00, [255, 255, 255]), // strongest signal = white
    ])
}

/// The light-mode spectrogram colormap: the Daylight "Martian Ink" spectral map.
/// Low signal stays clean paper; peaks ramp gold → orange → coral → magenta →
/// violet → deep indigo, so traces carry color depth *and* contrast on the light
/// display (instead of faint orange-on-white). Per the handoff, raw intensity is
/// normalized (noise-floor clamp + gamma lift) before the color lookup; that curve
/// is baked into the LUT here, so the renderer keeps indexing it by raw intensity
/// exactly as it does the dark map. See `design_handoff_daylight_color/README.md`.
pub fn martian_cmap_light() -> [Color32; 256] {
    let spectral = build_cmap(&[
        (0.00, [240, 236, 226]), // paper (no signal)
        (0.10, [242, 196, 112]), // gold
        (0.26, [230, 144, 52]),  // orange
        (0.44, [210, 76, 76]),   // coral/red
        (0.62, [168, 46, 114]),  // magenta
        (0.80, [96, 42, 136]),   // violet
        (1.00, [32, 26, 84]),    // deep indigo (peaks)
    ]);
    // Normalize raw intensity before the lookup. Intensity is dsp's fixed dB
    // brightness (COL_DB_FLOOR..COL_DB_CEIL); with real traffic the noise floor
    // sits well up that range, so the handoff's 0.105 floor pinned the whole
    // background to the indigo peak (solid blue). FLOOR clamps everything below it
    // to clean paper; signals ramp over FLOOR..FLOOR+SPAN up to the indigo peak,
    // with a gamma lift biasing toward the low end. Raise FLOOR if the noise floor
    // still carries colour; lower it if weak signals disappear.
    const FLOOR: f32 = 0.40;
    const SPAN: f32 = 0.45;
    let mut lut = [Color32::BLACK; 256];
    for (i, slot) in lut.iter_mut().enumerate() {
        let l = i as f32 / 255.0;
        let t = (((l - FLOOR) / SPAN).clamp(0.0, 1.0)).powf(0.72);
        *slot = spectral[(t * 255.0) as usize];
    }
    lut
}

/// What the next transmission is aimed at. A click on empty spectrum is a bare
/// retune (`Offset`); a click on a decoded line is intent to work that station
/// (`Station`, carrying its call so the send row can address it). This is the
/// GUI-local stand-in for the bus `Selection.target: Option<DecodeRef>` — named
/// so the later bus wiring is a mechanical swap.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Offset(i32),
    Station { call: String, off: i32 },
}

impl Target {
    /// The audio offset (Hz) the next TX lands on, regardless of variant.
    #[allow(dead_code)] // Target's offset accessor; kept with the type for the live path.
    pub fn off(&self) -> i32 {
        match self {
            Target::Offset(o) => *o,
            Target::Station { off, .. } => *off,
        }
    }

    /// The targeted station's callsign, if a station (not bare spectrum) is selected.
    pub fn station(&self) -> Option<&str> {
        match self {
            Target::Station { call, .. } => Some(call),
            Target::Offset(_) => None,
        }
    }
}
