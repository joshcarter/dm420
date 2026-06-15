//! Waterslide — deterministic "fake data" engine (FT8 transmissions + decodes).
//!
//! This is a 1:1 port of the generator in the HTML prototype. It has **no
//! dependencies, no I/O and no runtime randomness** — every transmission streak
//! and every decoded line is a pure function of the simulated clock `t`
//! (seconds). That means the Rust panel looks bit-for-bit like the mockup, and
//! it's trivially reproducible / testable.
//!
//! Two things come out of here:
//!   1. `column_values(ts, …)`  → one vertical column of FFT intensities for a
//!      given instant. The panel maps these through a colormap into pixels.
//!   2. `Sim` (advance + records) → the list of decoded text lines currently on
//!      screen, each carrying the time it was decoded (`td`) and the vertical
//!      position (`final_y`) it should sit at.
//!
//! When you later wire in the *real* radio:
//!   • replace the signal-synthesis loop in `column_values` with your real FFT
//!     magnitude bins, and
//!   • stop calling `decodes_for_slot`; instead push a `DecodeRec` whenever your
//!     decoder emits a line (set `td = now`, `final_y = y_of(audio_offset_hz)`).
//!   The panel/rendering code does not change.

use std::collections::HashMap;

// ---- constants (match the prototype) -------------------------------------
pub const SLOT: f64 = 15.0; // FT8 transmit period (seconds)
pub const TX: f64 = 12.6; // active transmit length inside a slot (seconds)
pub const WINDOW: f64 = 30.0; // seconds shown across each half of the panel
pub const FSPAN: f32 = 3000.0; // vertical span in Hz (±1500 around the dial)
pub const REF_H: f32 = 500.0; // reference pixel height the model positions against
pub const COL_H: usize = 500; // a column is REF_H pixels tall

#[derive(Clone)]
pub struct Station {
    pub call: &'static str,
    pub grid: &'static str,
    pub off: i32, // audio offset in Hz, relative to dial centre (−1500..+1500)
    pub snr: i32, // nominal signal strength (drives brightness)
    pub p: f64,   // probability this station transmits in any given slot
}

/// The fake population on 14.074 MHz. Offsets are spread across the passband;
/// a couple sit close together on purpose to exercise the de-collision packer.
pub const POOL: &[Station] = &[
    Station { call: "SM5KQ",  grid: "JO89", off:  1420, snr: -7,  p: 0.50 },
    Station { call: "K1ABC",  grid: "FN42", off:  1180, snr: -3,  p: 0.60 },
    Station { call: "W9XYZ",  grid: "EM48", off:  1080, snr: -12, p: 0.55 },
    Station { call: "OH8X",   grid: "KP24", off:   880, snr: -8,  p: 0.52 },
    Station { call: "JA1NUT", grid: "PM95", off:   600, snr: -15, p: 0.48 },
    Station { call: "DL3ABC", grid: "JO31", off:   360, snr: -6,  p: 0.55 },
    Station { call: "EA7KW",  grid: "IM67", off:   120, snr: -17, p: 0.46 },
    Station { call: "VK3WE",  grid: "QF22", off:   -80, snr: -21, p: 0.42 },
    Station { call: "N5JR",   grid: "EM12", off:  -300, snr: -5,  p: 0.58 },
    Station { call: "PY2OG",  grid: "GG66", off:  -540, snr: -19, p: 0.46 },
    Station { call: "G4XYZ",  grid: "IO91", off:  -760, snr: -11, p: 0.52 },
    Station { call: "VE3EN",  grid: "FN25", off:  -980, snr: -9,  p: 0.50 },
    Station { call: "ZL2AB",  grid: "RE78", off: -1180, snr: -23, p: 0.40 },
    Station { call: "UA9CDC", grid: "MO06", off: -1360, snr: -14, p: 0.48 },
];

// ---- deterministic hash (identical bits to the JS `mulberry32`) ----------
/// Stateful generator. `Rng::new(seed).next_f64()` reproduces JS
/// `mulberry32(seed)()` exactly; call repeatedly for a sequence.
pub struct Rng(u32);
impl Rng {
    #[inline]
    pub fn new(seed: u32) -> Self { Rng(seed) }
    #[inline]
    pub fn next_f64(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x6D2B_79F5);
        let mut t = self.0;
        t = (t ^ (t >> 15)).wrapping_mul(1 | t);
        t = t.wrapping_add((t ^ (t >> 7)).wrapping_mul(61 | t)) ^ t;
        ((t ^ (t >> 14)) as f64) / 4_294_967_296.0
    }
}
#[inline]
fn hash01(seed: u32) -> f64 { Rng::new(seed).next_f64() }

// ---- pure helpers --------------------------------------------------------
/// Map an audio offset (Hz) to a pixel row in the 0..REF_H reference space.
#[inline]
pub fn y_of(off: i32) -> f32 { 250.0 - (off as f32) / FSPAN * REF_H }

/// True if station `i` transmits during slot `s`.
#[inline]
pub fn active(s: i64, i: usize) -> bool {
    let seed = (s.wrapping_mul(9973).wrapping_add(i as i64 * 131).wrapping_add(7)) as u32;
    hash01(seed) < POOL[i].p
}

/// Brightness amplitude (0.4..1.0) for station `i` in slot `s`.
#[inline]
pub fn amp_of(i: usize, s: i64) -> f32 {
    let base = 0.46 + ((POOL[i].snr + 25) as f64 / 30.0) * 0.5;
    let seed = (s.wrapping_mul(17).wrapping_add(i as i64 * 101).wrapping_add(3)) as u32;
    let j = hash01(seed) * 0.1 - 0.05;
    (base + j).clamp(0.4, 1.0) as f32
}

/// Which of the 8 FSK tones is lit at a given step within the transmission.
#[inline]
pub fn tone_of(s: i64, i: usize, step: i32) -> i32 {
    let seed = (s.wrapping_mul(131).wrapping_add(i as i64 * 17).wrapping_add(step as i64 * 7)) as u32;
    (hash01(seed) * 8.0).floor() as i32
}

fn sgn(n: i32) -> String { format!("{}{:02}", if n <= 0 { "\u{2212}" } else { "+" }, n.abs()) }

/// Build the (message text, reception SNR) for station `i` in slot `s`.
fn message_for(s: i64, i: usize) -> (String, i32) {
    let mut r = Rng::new((s.wrapping_mul(761).wrapping_add(i as i64 * 53).wrapping_add(11)) as u32);
    let st = &POOL[i];
    let mut pj = (r.next_f64() * POOL.len() as f64).floor() as usize;
    if pj == i { pj = (pj + 1) % POOL.len(); }
    let p = POOL[pj].call;
    let rep = -(1 + (r.next_f64() * 23.0).floor() as i32);
    let rsnr = -(1 + (r.next_f64() * 24.0).floor() as i32);
    let rr = r.next_f64();
    let msg = if rr < 0.26 {
        format!("CQ {} {}", st.call, st.grid)
    } else if rr < 0.46 {
        format!("{} {} {}", p, st.call, st.grid)
    } else if rr < 0.66 {
        format!("{} {} {}", p, st.call, sgn(rep))
    } else if rr < 0.82 {
        format!("{} {} R{}", p, st.call, sgn(rep))
    } else if rr < 0.93 {
        format!("{} {} RR73", p, st.call)
    } else {
        format!("{} {} 73", p, st.call)
    };
    (msg, rsnr)
}

/// One spectrogram column: fills `out[0..COL_H]` with intensities in ~0..1.2.
/// `col_id` makes the (low-level) noise floor deterministic per column so that,
/// with the scroll-blit approach, frozen history stays stable.
pub fn column_values(ts: f64, col_id: i64, out: &mut [f32; COL_H]) {
    // noise floor
    for y in 0..COL_H {
        let n = hash01((col_id.wrapping_mul(374_761_393).wrapping_add(y as i64 * 668_265_263).wrapping_add(1)) as u32) as f32;
        out[y] = 0.045 + n * 0.05;
    }
    let sidx = (ts / SLOT).floor() as i64;
    let slot_start = sidx as f64 * SLOT;
    let tx_start = slot_start + 0.4;
    if ts < tx_start || ts > tx_start + TX {
        return; // dead air between transmissions
    }
    for i in 0..POOL.len() {
        if !active(sidx, i) { continue; }
        let amp = amp_of(i, sidx);
        let step = ((ts - tx_start) / 0.16).floor() as i32;
        let tone = tone_of(sidx, i, step);
        let cy = y_of(POOL[i].off);
        let ty = cy - (tone as f32 - 3.5) * 1.7; // lit tone, jitters within the band
        let lo = (cy - 10.0).max(0.0) as usize;
        let hi = ((cy + 10.0).min((COL_H - 1) as f32)) as usize;
        for y in lo..=hi {
            let d1 = y as f32 - ty;
            let d2 = y as f32 - cy;
            // bright FSK core + faint full-band haze
            out[y] += amp * ((-d1 * d1 / 1.62).exp() + 0.22 * (-d2 * d2 / 20.5).exp());
        }
    }
}

// ---- decode bookkeeping --------------------------------------------------
#[derive(Clone)]
pub struct Decode {
    pub true_y: f32,
    pub final_y: f32,
    pub msg: String,
    pub rsnr: i32,
    pub off: i32,
}

/// A decode placed on the scrolling text lane.
#[derive(Clone)]
pub struct DecodeRec {
    pub td: f64,     // time it was decoded (slot end). Drives horizontal scroll.
    pub final_y: f32, // vertical position in 0..REF_H (frequency lane, de-collided)
    pub msg: String,
    pub rsnr: i32,
    pub off: i32,    // station's true audio offset (Hz); clicking snaps TX here
}

pub struct Sim {
    t: f64,
    max_emit: i64,
    records: Vec<DecodeRec>,
    cache: HashMap<i64, Vec<Decode>>,
}

impl Sim {
    pub fn new(start_t: f64) -> Self {
        let max_emit = (start_t / SLOT).floor() as i64 - 1 - 3; // backfill ~3 slots
        Sim { t: start_t, max_emit, records: Vec::new(), cache: HashMap::new() }
    }

    #[inline]
    pub fn t(&self) -> f64 { self.t }

    /// Decodes currently alive (newest first by td); the panel scrolls these.
    #[inline]
    pub fn records(&self) -> &[DecodeRec] { &self.records }

    /// Advance the clock and emit decodes for any slots that just completed.
    pub fn advance(&mut self, dt: f64) {
        let dt = dt.clamp(0.0, 0.1);
        self.t += dt;
        let completed = (self.t / SLOT).floor() as i64 - 1;
        let mut s = self.max_emit + 1;
        while s <= completed {
            let td = (s + 1) as f64 * SLOT;
            for d in self.decodes_for_slot(s) {
                self.records.push(DecodeRec { td, final_y: d.final_y, msg: d.msg, rsnr: d.rsnr, off: d.off });
            }
            s += 1;
        }
        if completed > self.max_emit { self.max_emit = completed; }
        let t = self.t;
        self.records.retain(|r| t - r.td < WINDOW + 20.0); // cull once well off-screen
    }

    /// The set of decodes for one completed slot, de-collided vertically.
    /// (Deterministic + cached. This is what you'd replace with the real decoder.)
    pub fn decodes_for_slot(&mut self, s: i64) -> Vec<Decode> {
        if let Some(v) = self.cache.get(&s) { return v.clone(); }
        let mut list: Vec<Decode> = Vec::new();
        for i in 0..POOL.len() {
            if active(s, i) {
                let (msg, rsnr) = message_for(s, i);
                let ty = y_of(POOL[i].off);
                list.push(Decode { true_y: ty, final_y: ty, msg, rsnr, off: POOL[i].off });
            }
        }
        list.sort_by(|a, b| a.true_y.partial_cmp(&b.true_y).unwrap());
        const GAP: f32 = 16.0;
        for k in 0..list.len() { list[k].final_y = list[k].true_y; }
        for k in 1..list.len() {
            if list[k].final_y < list[k - 1].final_y + GAP {
                list[k].final_y = list[k - 1].final_y + GAP;
            }
        }
        let overflow = list.last().map(|d| d.final_y - 492.0).unwrap_or(0.0);
        if overflow > 0.0 {
            for d in &mut list { d.final_y = (d.final_y - overflow).max(8.0); }
        }
        self.cache.insert(s, list.clone());
        list
    }
}
