# Joel's Notes

Running notes, gotchas, and reminders. Newest at the top.

## 2026-06-18

- **Signal strength must be calibrated to the noise floor (near-term commit):**
  the SNR shown on the waterslide is a placeholder — a POC estimate (`score / 2`
  from the decoder, `crates/modes/src/decode.rs:357`, surfaced via
  `crates/core/src/decode.rs:146`), *relative, not calibrated to noise*. A
  near-term commit should report plausible signal strength measured against the
  noise level. Target scale (standard FT8 reports):

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
