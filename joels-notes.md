# Joel's Notes

Running notes, gotchas, and reminders. Newest at the top.

## 2026-06-18

- **Station call + grid: no default; TOML config, set via file or UI, persisted — done:**
  no built-in default — a silent one risks transmitting as the wrong station (the old
  `N0JDC` / `DN70KA` fallbacks are gone). Implemented: identity resolves
  `DM420_CALLSIGN` / `DM420_GRID` env → `dm420.toml` (`[station]` table) → unset; with
  nothing set the app **boots unlocked to prompt**; operating (CQ/answer) is blocked
  until a call is set; editing call/grid in the unlocked top bar **writes `dm420.toml`
  on re-lock, preserving comments**. (`dm420.example.toml` is the committed template;
  `dm420.toml` is gitignored; env still overrides the file.) **Still TBD (UX owner):**
  the config format/location may change, a real `toml_edit` swap is the clean upgrade
  once config grows past call/grid, and the broader settings UX (everything beyond
  station identity) is open.

- **Signal strength calibrated to the noise floor — done (522fa46):** replaced the
  `score / 2` POC placeholder with a real noise-relative SNR — per-slot noise floor
  (median of the waterfall magnitudes, since signals are sparse) vs. signal power at
  the Costas sync tones, corrected from the per-bin bandwidth to a 2500 Hz reference.
  It is gain-independent (a power ratio), so a signal at the decode limit reads near
  −21 dB regardless of input level. The waterslide also stopped dimming weak decodes
  (2d31eb0). Target scale (standard FT8 reports), kept for reference:

  | Report | What it means |
  |---|---|
  | −24 or below | Extremely weak; at or near FT8's decoding limit |
  | −15 to −20 | Weak but solid copy; impressive propagation |
  | −10 to −14 | Moderate signal |
  | −5 to 0 | Good signal |
  | +1 and above | Strong signal; well above noise |

- **Wrong audio source while decoding:** apparently I was decoding off the
  **MacBook's built-in microphone**, not the rig's **audio input device**. Set
  `DM420_AUDIO_INPUT` (case-insensitive substring, e.g. `USB PnP`) so capture
  binds to the right device — or pick it in the unlocked FT8 panel's Radio Setup.
  - **Fixed:** selected the correct audio input device and the decode looks a
    lot stronger now — way more decodes coming through.
