# Handoff: Martian FT8 Radio — "Daylight Color" Light Theme

## Overview
"Daylight Color" is the high-contrast light/daytime theme for the Martian FT8 radio
interface — a single 960×600 control surface for an FT8 amateur-radio station
(waterfall spectrogram, decode list, log book, band scan, and a North-America
contact map). It is the daylight counterpart to the existing dark theme, designed
to stay legible in direct sunlight where the original pale orange-on-cream light
theme washed out.

Two ideas define it:
1. **A brushed-silver chassis with near-black legends** for sunlight contrast.
2. **Paper-white instrument displays** with a **spectral (multi-hue) waterfall**
   that ramps gold → orange → magenta → violet → deep indigo, so signal traces
   carry real color depth *and* high contrast on a light background — instead of
   faint orange-on-white.

## About the Design Files
The files in this bundle are **design references created in HTML** — a working
prototype showing the intended look and behavior, **not production code to copy
verbatim**. The component is authored as a "Design Component" (a lightweight
template + logic-class format used by our design tool); treat it as a precise
visual/behavioral spec.

The task is to **recreate this design in the target codebase's existing
environment** (React, Vue, SwiftUI, native, etc.) using its established patterns,
component library, and conventions. If no front-end environment exists yet, choose
the most appropriate framework for the project and implement it there. Do not ship
the HTML/Design-Component files directly.

## Fidelity
**High-fidelity (hifi).** All colors, typography, spacing, and interactions are
final. Recreate the UI pixel-accurately. Exact hex values, gradients, shadows, and
measurements are given below.

---

## Theme Token Set (authoritative)

The whole UI is driven by a single theme object. These are the exact values for
the Daylight Color theme. Where a value is a CSS gradient/shadow string, reproduce
it literally.

| Token | Value | Used for |
|---|---|---|
| `face` | `linear-gradient(180deg,#f3f4f1,#d9dbd5)` | Chassis background (brushed silver) |
| `brush` | `repeating-linear-gradient(90deg, rgba(255,255,255,0.6) 0px, rgba(255,255,255,0.6) 1px, rgba(0,0,0,0.045) 1px, rgba(0,0,0,0.045) 2px)` | Brushed-metal overlay (1px stripes, ~1.0 alpha pairs) over the whole face |
| `edge` | `#9a9c97` | 1px outer border of the chassis |
| `bevel` | `0 3px 12px rgba(0,0,0,0.3), inset 0 1px 0 rgba(255,255,255,0.95), inset 0 -2px 6px rgba(0,0,0,0.16)` | Chassis drop shadow + top highlight + bottom shade |
| `grooveH` | `linear-gradient(180deg, rgba(40,44,50,0.4), rgba(255,255,255,0.85))` | 2px horizontal groove under the top bar |
| `grooveV` | `linear-gradient(90deg, rgba(40,44,50,0.4), rgba(255,255,255,0.85))` | 2px vertical groove between columns |
| `legendColor` | `#191c20` | All chassis labels/headings (near-black) |
| `legendShadow` | `0 1px 0 rgba(255,255,255,0.85)` | Engraved-text highlight under legends |
| `subColor` | `rgba(64,70,78,0.9)` | Secondary chassis labels / sublabels |
| `screenBg` | `#f0ece2` | All recessed display backgrounds (paper) + the Send input |
| `screenInset` | `inset 0 2px 7px rgba(0,0,0,0.18), inset 0 0 0 1px rgba(120,90,40,0.4)` | Recessed-screen inner shadow + hairline frame |
| `sheen` | `linear-gradient(135deg, rgba(255,255,255,0.42), rgba(255,255,255,0) 40%)` | Glass sheen overlay on screens |
| `text` | `#20242a` | In-screen content text (dark ink) |
| `dim` | `rgba(92,100,110,0.72)` | In-screen dim/secondary text, grid lines, México map dots |
| `accent` | `#b8530a` | Brand accent (deep burnt orange): corner brackets, identity bar, active toggles, decode ticks, received-report column, US map dots, NOW line |
| `led` | `radial-gradient(circle at 40% 35%, #f0b070, #b8530a 55%, #6e3204)` | Indicator LED dots |
| `ledGlow` | `0 0 5px 1px rgba(184,83,10,0.5), inset 0 0 1px rgba(255,255,255,0.8)` | LED glow |
| `landFill` | `rgba(60,52,32,0.10)` | Map landmass fill |
| `landStroke` | `rgba(120,72,16,0.5)` | Map coastline stroke |
| `lcdBg` | `linear-gradient(180deg,#e9e2d2,#d2c9b2)` | Small LCD readouts (clocks, segmented toggles) |
| `lcdText` | `#2a2010` | LCD text |
| `lcdGlow` | `none` | LCD text glow (off in daylight) |
| `accent (armed state)` | `#2fe3d8` (cyan) | Transmit "armed" highlight on the Send field + button (shared across themes) |

### Typography
- **Display / numerals / labels:** `Chakra Petch` (Google Font), weights 600–700,
  uppercase, letter-spacing 0.08–0.2em. Used for callsign, section titles, band
  numbers, clock digits, button text.
- **Body / mono / data:** `IBM Plex Mono` (Google Font), weights 400–600. Used for
  log rows, decode calls, SNR values, axis labels, map text.

### Spacing / sizing constants
- Panel: **960 × 600**, `border-radius: 4px`, `overflow: hidden`, `position: relative`.
- Top bar height **46px**, padding `0 24px 0 14px`.
- Grooves: **2px** thick.
- Main row height **552px**.
- Left waterfall column width **470px**, padding `8px 10px 8px 14px`.
- Right column `flex: 1`, padding `8px 14px 8px 12px`.
- Right column stack: Log **142px**, Band Scan **112px**, Map `flex: 1`, footer **30px**; `8px` gaps.
- Every section header row is **24px** tall with an 8px gap; its content screen starts 6px below.
- Recessed screens have **no border-radius** (square corners) and 1.5px accent
  **L-bracket corner marks** (9×9px) in all four corners (z-index above content).

---

## Screens / Views

It is one screen composed of five instrument panels.

### 1. Top bar
- **Left:** 3px × 16px accent identity bar, then callsign `N0JDC` (Chakra Petch
  700, 18px, color `legendColor`) + grid `DN70KA` (9px, `subColor`, uppercase,
  letter-spacing 0.18em).
- **Center-right (pushed with `margin-left:auto`):** two LCD readouts — **LOCAL**
  and **UTC** clocks. Each: `lcdBg` background, `screenInset` shadow, 3px×12px
  padding, an 8px label (`lcdText` @ 0.6 opacity) + a monospaced-width digit field
  (Chakra Petch 700, 16px, `lcdText`, fixed width 79px, centered), with a `sheen`
  overlay. Clocks tick every second; format `HH:MM:SS`, 24-hour.
- **Right:** two segmented toggles — **Display** (DARK | LIGHT) and **GUI**
  (LOCK | EDIT). Each cell 5px×11px padding, Chakra Petch 9px, letter-spacing
  0.1em. Active cell: weight 700, text = `onAccent` (`#fdf6ec` in light contexts —
  see note), background = `accent`, shadow `inset 0 1px 0 rgba(255,255,255,0.28),
  0 1px 2px rgba(0,0,0,0.45)`. Inactive cell: weight 600, text `subColor`,
  transparent, no shadow. LOCK/EDIT are clickable and switch edit mode.

> **onAccent note:** text drawn on top of the `accent` fill uses `#fdf6ec` in light
> mode (a near-white) so burnt-orange chips read clearly.

### 2. Waterfall (left column)
- **Header (clickable, toggles focus):** a 9×9px accent-outlined box (fills with
  `accent` + glow `0 0 7px rgba(247,146,15,0.6)` when focused), title "WATERFALL"
  (Chakra Petch 600, 11px, 0.18em, uppercase, `legendColor`), subtitle
  "0–3000 Hz · time → left" (8.5px `subColor`). Right side: a SPLIT lamp (LED dot)
  and "AGC" label (8px `subColor`).
- **Recessed screen:** CSS grid `28px 1fr 124px`:
  - **Col 1 — frequency axis (28px):** right-aligned labels `3k / 2k / 1k / Hz`
    at top / 33% / 66% / bottom (8px `dim`). 1px right divider `rgba(128,128,128,0.18)`.
  - **Col 2 — spectrogram:** the waterfall image fills the cell
    (`object-fit: fill`). A 2px **accent NOW line** down the right edge (opacity
    0.85) labeled "NOW" (8px accent, Chakra Petch) at top-right; "−60s" bottom-left
    (8px `dim`). `sheen` overlay on top.
  - **Col 3 — decode rail (124px):** absolutely-positioned decode rows over the
    rail height (**RAILH = 438px** usable). Each decode: a small accent tick
    (7px×1px @ 0.6 opacity) + LED dot, the callsign (Chakra Petch 600, 10px,
    `text`), and right-aligned SNR (8.5px `dim`). Rows are placed by frequency:
    `topPx = clamp((1 - freqHz/3000) * 438 - 7, 2, 422)`.
- **Footer — "Send":** label "Send:" (Chakra Petch 600, 9px, uppercase,
  `legendColor`) + a text input + an arm/send toggle.
  - **Input:** `flex:1`, height 22px, `background: screenBg`, `box-shadow:
    screenInset`, 1px border (`rgba(128,128,128,0.35)` idle), border-radius 2px,
    padding `0 9px`, IBM Plex Mono 600 11px. Text color = `accent` when idle,
    cyan `#2fe3d8` when armed. Default value `CQ N0JDC DN70`.
  - **Send button:** `lcdBg`/`screenInset` housing; inner chip 4px×15px, Chakra
    Petch 700, 9px, uppercase. Idle: label "Send", bg `accent`, text `onAccent`.
    Armed: label "Armed", bg cyan `#2fe3d8`, text `#04211f`, plus a cyan glow
    `0 0 9px rgba(47,227,216,0.65)`. Clicking the housing toggles armed state
    (also recolors the input border/text/glow to cyan).

### 3. Log Book (right column, top, 142px)
- Header: accent-outlined 9×9 box, "LOG BOOK" title, "last 4 · FT8" subtitle,
  right-aligned "312 QSO" count (Chakra Petch 9px, `legendColor`).
- Screen: a 5-column grid `50px 1fr 60px 48px 48px` = **UTC · CALL · GRID · SNT ·
  RCV**. Header row 8px uppercase `dim`, 1px bottom divider. Data rows 22px tall,
  10px text, hairline dividers `rgba(128,128,128,0.10)`. CALL is Chakra Petch 600
  `text`; RCV column is `accent`; UTC/GRID are `dim`.
- Seed rows: `2358 W7GH CN94 −11 −09`, `2355 JA1NUT PM95 −15 −13`,
  `2351 G4ABC IO91 −13 −07`, `2347 VE3EN FN25 −09 −02`.

### 4. Band Scan (right column, middle, 112px)
- Header: "BAND SCAN" title, status subtitle (e.g. "Last scan: 4 min ago" / while
  scanning "Scanning 20m …"), and a right-aligned **Scan / Cancel** button
  (accent-filled chip, Chakra Petch 700, 9px, uppercase).
- Screen: two equal halves split by a 1px divider, each holding two band rows.
  Each band row: a 2px left bar, a large band number (Chakra Petch 700, **22px**),
  and two stat lines ("N heard" with N in `legendColor`; "N unworked" with N in
  `accent`); labels in `dim`, 11px.
- Bands (left → right halves): **40m** 23/7, **20m** 41/12 | **15m** 18/9,
  **10m** 6/4.
- **Scan behavior:** pressing Scan steps the highlight through bands every
  **2500 ms**; the active band's left bar + number turn `accent`. Reaching the end
  resets to idle and sets status "Last scan: just now". Cancel stops immediately.

### 5. Contacts Map (right column, fills remaining height)
- Header: "CONTACTS" title, "N. America · DN70KA" subtitle, right-aligned
  "16 spots" count.
- Screen: an inline **SVG** (`viewBox="0 0 393 190"`, equirectangular projection
  centered near Lafayette, CO). Contents:
  - Land polygon (`landFill` / `landStroke`, 0.6 stroke).
  - Meridians/parallels as faint `dim` lines (0.4 width, 0.25 opacity); parallels
    labeled `20° 30° 40° 50°`.
  - A dashed accent **US–Canada border** line and two dashed accent **range rings**
    (~750 / 1500 units) around the QTH.
  - Country labels CANADA / MÉXICO (`subColor`).
  - **Contact dots:** US = filled `accent` (r 2.4); Canada = `screenBg` fill +
    accent stroke 1.1 (r 2.4); México = `dim` fill (r 2.2). Each dot has a 4.8px
    monospace call label, anchored end/start depending on x.
  - **QTH crosshair:** accent ring (r 4.6), crosshair lines, center dot, "QTH"
    label — at Lafayette, CO (40.00, −105.10).

### 6. Footer (right column, 30px)
- Flat status legend: **DX ONLY** (accent square), **CQ** (outline square),
  **ALERT** (outline square), **LOG** (accent square) — each 10px chip + 8.5px
  label. Right side: a small accent **SNR bar meter** (6 bars, ascending heights,
  last two dimmed) + "SNR" label.

---

## Interactions & Behavior
- **Clocks:** update every 1000ms; LOCAL = device local time, UTC = UTC, both
  `HH:MM:SS`.
- **Send arm toggle:** click toggles `armed`; recolors input border/text/glow and
  the Send button to cyan `#2fe3d8` ("Armed"), reverts to accent ("Send").
- **Band scan:** click Scan → highlight advances per band every 2.5s, button reads
  "Cancel" while running; auto-stops at end; Cancel stops mid-run.
- **Waterfall focus:** clicking the header toggles a focused state (corner box
  fills accent + glow). Cosmetic only.
- **Display / GUI toggles:** DARK/LIGHT select the theme; LOCK/EDIT switch an edit
  mode flag. Wire LOCK/EDIT to your app's lock/edit state.
- No page-level animations/transitions beyond the focus color change
  (`transition: background-color 0.15s` on the waterfall corner box). The waterfall
  itself is a static image in this prototype — see below for live implementation.

## State Management
- `armed: boolean` — transmit arm state (Send field/button).
- `scanning: boolean`, `activeBand: number`, `lastScan: number(min)` — band scan.
- `editMode: boolean` — GUI lock/edit.
- `focus: 'waterfall' | null` — waterfall focus.
- A 1s interval for the clocks; a 2.5s interval (only while scanning) for band scan.
- Data that would come from the radio backend: decode list (freq, call, SNR),
  log rows, band stats, contact list (call, lat, lon, region), QTH.

---

## The Spectral Waterfall (most important detail)

The waterfall is the centerpiece of this theme. In the prototype it is a
pre-rendered PNG (`wf-martian-ink-color.png`). In production it should be a **live
spectrogram** drawn to a `<canvas>`, using this exact intensity→color mapping so it
matches the design.

**Intensity normalization** (per pixel/bin, from a 0–1 signal magnitude `L`):
```
t = clamp((L - 0.105) / 0.34, 0, 1)
t = t ^ 0.72            // gamma lift
```
This keeps the noise floor as clean paper and pushes mid-strength signals into the
colored range.

**Colormap** (linear interpolation between stops; `t` 0→1):
| t | hex | rgb | meaning |
|---|---|---|---|
| 0.00 | `#f0ece2` | 240,236,226 | paper (no signal) |
| 0.10 | `#f2c470` | 242,196,112 | gold |
| 0.26 | `#e69034` | 230,144,52 | orange |
| 0.44 | `#d24c4c` | 210,76,76 | coral/red |
| 0.62 | `#a82e72` | 168,46,114 | magenta |
| 0.80 | `#602a88` | 96,42,136 | violet |
| 1.00 | `#201a54` | 32,26,84 | deep indigo (peaks) |

Low signal → paper; peaks → dark saturated indigo, giving color depth *and* strong
contrast on a light display. Y axis = frequency (0 at bottom → 3000 Hz at top),
X axis = time (now at right → −60s at left).

The same pipeline was used to generate the dark theme's waterfall from raw
intensity, so a single canvas renderer can serve every theme by swapping the
colormap + background.

---

## Design Tokens (quick reference)
**Colors:** chassis `#f3f4f1`→`#d9dbd5`, edge `#9a9c97`, legend `#191c20`,
sub `rgba(64,70,78,0.9)`, screen `#f0ece2`, in-screen text `#20242a`,
dim `rgba(92,100,110,0.72)`, accent `#b8530a`, armed-cyan `#2fe3d8`,
lcd `#e9e2d2`→`#d2c9b2` / text `#2a2010`.
**Radii:** chassis 4px, screens 0px (square), chips/inputs 2px, toggle housings 4px.
**Type scale:** 7–9px micro labels, 10–11px data, 16px clocks, 18px callsign,
22px band numbers, 32px (wrapper title only).
**Spacing:** 6/7/8px small gaps, 14/24px paddings; section headers 24px.
**Shadows:** see `bevel`, `screenInset`, `ledGlow` tokens above.

## Assets
- `wf-martian-ink-color.png` — the spectral waterfall image (480×620 source),
  generated from raw intensity via the colormap above. In production, render this
  live on a canvas instead of using the static image.
- Fonts: **Chakra Petch** and **IBM Plex Mono** from Google Fonts.
- No icon library — all glyphs (squares, LED dots, brackets, bar meters, map) are
  CSS/SVG primitives.

## Files
- `MartianHybrid.dc.html` — the full radio component (template + logic). The theme
  is passed in as the `theme` prop; the Daylight Color values are tabulated above.
- `Martian Hybrid.dc.html` — comparison board that mounts the component under the
  Dark, Daylight Color, and Viridis themes side by side. Useful for seeing the
  Daylight Color theme object defined in code (the `ink` constant in its logic).
- `wf-martian-ink-color.png` — waterfall asset for Daylight Color.

> The `.dc.html` files are Design Components (template + a `class Component`
> logic block). Read them as a spec; reimplement in your stack.
