# Project Overview & Design Objectives

**Audience:** the implementing agent (Claude Code) and any human contributor.
**Status:** design handoff. This document is the orientation layer; the per-feature
specs in `docs/` are authoritative for the details of each panel.

> The seven specs that accompany this file live in `docs/`:
> `FEASIBILITY.md`, `waterslide_panel.md`, `radio_control.md`, `log_book.md`,
> `map_panel.md`, `band_scanner.md`, `keyboard_control.md`.
> This document does **not** repeat them. Where a behavior is specified in one of
> those files, that file wins; read it before implementing the corresponding module.

---

## 1. What this is

A desktop application for operating **digital-mode amateur radio** — initially
**FT8/FT4** at QRP power levels (~5 W) — with a strong emphasis on *multi-band
situational awareness* and *small-network collaboration between operators*.

It is **not** a clone of WSJT-X. The differentiators are:

- A rotated-waterfall ("waterslide") display that places decoded text directly
  beside each signal's frequency lane, so several simultaneous senders are legible
  at a glance.
- First-class **multi-band monitoring**: a band scanner that time-slices a single
  receiver across bands, plus a map and log designed to aggregate what's being
  heard, not just what's been worked.
- **Local-network sharing** of contacts and heard stations between operators (e.g.
  a Field Day or club setup) with no central database.
- A deliberate, instrument-panel visual identity (see §4).

### Initial hardware target vs. the longer arc

The first supported radios are the **Kenwood TS-480SAT** and **TS-590S** — classic
superheterodyne rigs controlled over a serial CAT link, presenting a *single* ~3 kHz
audio passband at a time. Everything in v1 must work against that constraint.

However, the feature set is shaped by a longer-term goal: **true simultaneous
multi-band reception** via SDR back-ends (HPSDR/Hermes-Lite 2, RX-888 + ka9q-radio,
FlexRadio/ANAN). The band scanner exists precisely *because* a single conventional
receiver cannot watch several bands at once; on a multi-receiver SDR back-end the
same "what's open right now" question is answered by parallel receivers instead of
time-slicing. **Design the radio abstraction so a back-end can advertise how many
simultaneous receivers it has, and let the band scanner and waterslide adapt.** Don't
hard-code the one-receiver assumption into the domain model even though v1 ships it.

---

## 2. Design objectives (the principles to optimize for)

1. **Radio capability lives in panels, not in app chrome.** The app is meant to drive
   more than one radio, in setups the operator composes themselves. So per-radio
   controls (frequency, band, mode, send) belong to a panel — specifically the
   waterslide panel — not the top-level header. See `radio_control.md`. (Note the
   reconciliation in §4 about the spike's placeholder header readout.)

2. **Two operating postures: unlocked (configure) and locked (operate).** Unlocked
   exposes setup affordances; locked hides them to reduce clutter and prevent
   fat-finger config changes mid-QSO. This is a global mode that every panel reads.

3. **Keyboard-first operation.** A user-selectable shortcut (default `Cmd/Ctrl+1..N`)
   assigns keyboard focus to a panel; the focused panel interprets keys by its role.
   The waterslide panel additionally routes free text either to the outgoing message
   or, when prefixed, to **slash-command** rig control (`/f 14.074`, `/b 20m`, …),
   with tolerant parsing. See `keyboard_control.md` and `radio_control.md`.

4. **Heard ≠ worked, and both matter.** A large part of the value is surfacing
   stations *heard but not yet worked* (transient map pips, per-band counts,
   waterslide text) so the operator can judge band openings and pick targets.

5. **Collaborative but eventually-consistent.** Operators on the LAN broadcast their
   own logged contacts and (optionally) heard stations. There is **no shared
   database and no guaranteed sync** — peers learn what they happen to receive. The
   UI must clearly distinguish *my* data from *peer* data.

6. **Hardware-agnostic core.** The decode/contact/log/map domain knows nothing about
   a specific rig. Radios are plugins behind a trait (§3).

7. **Faithful, runtime-switchable theme.** The "Martian Hybrid" instrument look is
   already de-risked (see `FEASIBILITY.md`: verdict GO). Dark/light palettes switch
   at runtime; all painters are palette-driven.

---

## 3. Recommended architecture

This is a recommendation, not a spec — adjust if a better structure emerges, but keep
the layering boundaries.

### 3.1 Stack (settled)

- Language: **Rust**.
- GUI: **eframe/egui 0.34.3**, **egui_tiles 0.15.0**, **egui_extras 0.34.3**
  (pinned; the feasibility spike validated exactly these versions).
- Layout: egui_tiles **linear splits, not tabs** — this is a deliberate choice that
  sidesteps most of egui_tiles' chrome (no tab bars to suppress). See
  `FEASIBILITY.md` §3 for the specifics (`gap_width`, `resize_stroke`,
  `SimplificationOptions { all_panes_must_have_tabs: false }`).
- Fonts: Chakra Petch (headings), IBM Plex Mono (data) — vendored TTFs.

### 3.2 Concurrency model

egui is immediate-mode: a single `App` struct holds shared state and re-renders every
frame. I/O must not block the UI thread. Use background workers that communicate over
channels (`std::sync::mpsc` or `crossbeam-channel`); the UI thread drains channels at
the top of each frame, updates state, then paints.

Suggested workers:

- **Radio back-end worker** — owns the CAT serial link and the audio device(s);
  applies frequency/band/mode/PTT commands; emits captured audio (or IQ) buffers.
- **Decoder worker** — receives interval-aligned audio, runs the FT8/FT4 decode,
  emits `DecodeEvent`s. This is CPU-heavy and must be off the UI thread.
- **Scheduler / clock** — the heartbeat. FT8 is **UTC-synchronized on 15 s
  boundaries** (FT4 on 7.5 s). This worker fires the windows that drive decode
  capture, TX timing, and band-scanner band steps. The whole app's correctness leans
  on the system clock being within ~1 s of UTC — surface clock status in the UI and
  document the NTP dependency.
- **Network worker** — broadcast/receive of logged contacts and heard-station
  beacons; peer discovery.

### 3.3 Layers

```
┌──────────────────────────────────────────────────────────┐
│  UI layer  — egui_tiles tree, panels, theme, key routing   │
│   panels: Waterslide · LogBook · Map · BandScanner         │
├──────────────────────────────────────────────────────────┤
│  App state — single source of truth, read by all panels    │
│   timeline ("now"), outgoing freq, band/mode, QSO state,    │
│   log, heard-station registry, lock state, palette          │
├──────────────────────────────────────────────────────────┤
│  Domain — radio-agnostic                                    │
│   FT8/FT4 message parse+encode, contact state machine,      │
│   grid-locator math, contact & heard-station models         │
├──────────────────────────────────────────────────────────┤
│  Back-ends (trait objects)                                  │
│   RadioBackend (TS-480/TS-590 v1; SDR later)                │
│   NetworkPeer (LAN share)                                   │
│   Decoder (FT8/FT4; later PSK31/RTTY)                       │
└──────────────────────────────────────────────────────────┘
```

### 3.4 Key domain types (sketch — names illustrative)

- `Band`, `Mode { Ft8, Ft4, Psk31, … }`, and a **frequency model that separates the
  dial/center frequency from the outgoing audio (TX) offset**. The waterslide spec is
  explicit that clicking sets *outgoing* frequency without retuning the radio — keep
  these distinct from day one. (`waterslide_panel.md`.)
- `DecodeEvent { utc, audio_hz, dial_hz, snr, raw_text, parsed: Option<Ft8Message> }`.
- `Ft8Message` — typed parse of the standard message forms (CQ / call+grid /
  report / R+report / RRR / RR73 / 73), plus contest variants (Field Day:
  class+section). The map and contact machine both depend on this.
- `Qso` / contact state machine — given the last message addressed to us, computes the
  next message to send and advances on each interval. CQ is the default outgoing
  message; clicking a decoded station switches the target. (`radio_control.md`.)
- `Contact` (log entry) — call, grid, band, mode, time, reports, **`origin: Mine |
  Peer(peer_id)`** so the log and map can render the distinction
  (`log_book.md`, `map_panel.md`).
- `HeardStation` — call, inferred grid/location, **sticky** assigned map coordinate,
  last-heard, band, worked flag. Transients expire at 1 h and dim with age
  (`map_panel.md`).

### 3.5 The radio back-end trait (the hardware/SDR bridge)

Sketch the trait so v1's single-receiver Kenwood path and a future multi-receiver SDR
path are both expressible:

```rust
trait RadioBackend {
    fn capabilities(&self) -> RadioCaps;        // e.g. simultaneous_receivers: usize
    fn set_dial_frequency(&mut self, hz: u64) -> Result<()>;
    fn set_mode(&mut self, mode: Mode) -> Result<()>;
    fn ptt(&mut self, on: bool) -> Result<()>;
    // audio/IQ delivered to the decoder via a channel handed in at construction
}
```

`capabilities().simultaneous_receivers == 1` ⇒ band scanner time-slices.
`> 1` ⇒ the same "all-band view" can be served by parallel receivers, and the band
scanner can be skipped or repurposed. v1 implements the Kenwood CAT path only.

---

## 4. Cross-cutting concerns (read before building panels)

- **Header vs. panels (reconciliation note).** `FEASIBILITY.md`'s acceptance list has
  the spike header showing a *placeholder* frequency readout. `radio_control.md`
  overrides this for production: **no radio controls in the header.** The header
  should carry only app-global chrome — app title, light/dark toggle, lock/unlock,
  UTC clock + sync status, network/peer status. Live frequency belongs to the
  waterslide panel. Implement the production rule; treat the spike header as scaffold.
- **Lock state** is global and read by every panel to show/hide config affordances.
- **Keyboard focus routing** is global: one panel holds keyboard focus at a time;
  the focused panel's role decides interpretation. Slash-command parsing should be a
  shared utility (used by the waterslide panel now, available to others later).
- **Palette switching** at runtime must update every painter, including the
  regenerated brushed-metal texture (`FEASIBILITY.md`).
- **Time** is a shared concern, not a per-panel one — the scheduler owns it.

---

## 5. Panels at a glance (specs in `docs/`)

| Panel | Role | Hosts radio control? | Spec |
|---|---|---|---|
| **Waterslide** | Primary operating surface: rotated FFT (right, newest at left) + decoded text (left, newest at right), meeting at "now" in the center; outgoing-frequency selection; message send | **Yes** — mode/band/freq/send live here | `waterslide_panel.md` |
| **Log Book** | Mostly read-only recent QSOs from the decoder; shows peers' logged contacts too, visually distinguished from own | No | `log_book.md` |
| **Map** | Plots worked (filled) and heard-but-unworked (dimming) stations from inferred grid locations; own station shown strongly and *off-center*; land/water rendering | No | `map_panel.md` |
| **Band Scanner** | On demand, blocks TX and time-slices the receiver across selected bands (40/20/15/10 m), decoding one interval per band; reports per-band heard/unworked counts | No (controls itself) | `band_scanner.md` |

Cross-panel data flows: the decoder feeds the waterslide (display), the log book
(completed QSOs), the map (heard + worked), and the band scanner (per-band counts).
Build the `DecodeEvent` → app-state fan-out once; all four panels subscribe to it.

---

## 6. Suggested build order

Each phase should compile, run, and be demoable on its own.

0. **Scaffold + theme + layout.** Reproduce the `FEASIBILITY.md` spike: egui_tiles
   split layout, Martian Hybrid theme, fonts, dark/light toggle, four empty panes.
   Lock/unlock toggle and keyboard panel-focus routing as plumbing. *(This is largely
   done in the spike crate — start by lifting it.)*
1. **Radio back-end trait + Kenwood CAT.** Serial control of TS-480SAT/TS-590S
   (frequency/band/mode/PTT) and audio capture via the system audio device. No decode
   yet — verify tune + PTT + audio levels.
2. **Decoder integration + scheduler.** UTC-locked 15 s/7.5 s windows; feed captured
   audio to the FT8/FT4 decoder; emit `DecodeEvent`s. **(See §7 — pick the decoder
   strategy first; this is the largest single risk.)**
3. **Waterslide panel, receive-only.** Render the FFT + lane-aligned decoded text +
   the "now" center; outgoing-frequency indicator and click-to-set behavior.
4. **Contact state machine + send.** Outgoing message logic, send/cancel on interval
   boundaries, own-TX display in the waterslide; write completed QSOs to the log.
5. **Log Book panel** (own contacts first), then **6. Network sharing** (broadcast +
   discovery; peer contacts in the log with origin distinction).
6. **Map panel.** Grid→lat/lon, sticky placement, water→land relocation, dimming
   transients, dynamic off-center bounds, land/water rendering.
7. **Band Scanner panel**, then map pips from scan results (the spec's future
   enhancement).
8. **Mode extensibility:** PSK31 (live-typing TX behavior) and RTTY, exercising the
   waterslide/decoder/mode abstractions.

---

## 7. Open decisions to resolve early

These aren't specified in `docs/` and materially shape the implementation. Flagging
rather than deciding — confirm the operator's preference before committing.

1. **FT8/FT4 decoder strategy — highest-impact decision.** Options, roughly:
   (a) FFI to an existing C decoder such as the `ft8_lib` family (permissive license,
   decode+encode, but FT8/FT4 only); (b) shell out to / link WSJT-X's decoder (most
   battle-tested, but it's GPLv3 — has licensing implications for the whole binary and
   is heavier to integrate); (c) a Rust-native decoder (cleanest integration, most
   work, highest correctness risk). The decoder also produces the SNR and timing the
   waterslide and log need, so its output shape drives `DecodeEvent`. **Decide this
   before phase 2.**
2. **Audio & serial crates.** `cpal` for cross-platform audio and `serialport` for
   CAT are the conventional Rust choices; confirm before phase 1.
3. **Network protocol & discovery.** UDP broadcast/multicast beacons vs. mDNS
   (`mdns-sd`); message schema for shared contacts and heard stations; what (if
   anything) about *heard* stations is shared vs. only *worked* ones.
4. **Log persistence / interop.** Recommend **ADIF** import/export since it's the
   amateur-radio lingua franca; internal store could be SQLite or in-memory + on-disk
   ADIF/JSON. Confirm whether ADIF compatibility is a v1 requirement.
5. **Map base data.** Land/water rendering and the optional terrain texture need
   coastline/relief data (e.g. a simplified Natural Earth GeoJSON rasterized to egui
   meshes). Decide the data source and how it ships with the binary. Grid-locator math
   is trivial; the cartography is the cost.
6. **App name / crate name.** The theme spike crate is `martian_hybrid`; the
   *application* is currently unnamed. Pick a product name distinct from the theme.

---

## 8. Pointers & guardrails for the implementing agent

- Pin the egui stack to the versions in §3.1; APIs shift across egui minor releases.
- Note the **macOS exit crash workaround** documented in `FEASIBILITY.md`'s reader
  notes (winit/AppKit teardown) — carry it forward rather than rediscovering it.
- Honor the production header rule in §4 over the spike's placeholder.
- Keep the dial-frequency / outgoing-audio-frequency distinction explicit everywhere.
- Treat the band scanner as a *strategy for single-receiver hardware*, behind the
  same capability abstraction a multi-receiver SDR back-end would satisfy differently.
- When a panel behavior is underspecified, the `docs/` file for that panel is the first
  reference; if it's silent, surface the question rather than inventing operator
  workflow — this is a domain (real-time QSO timing, contest exchanges) where wrong
  guesses are expensive.
