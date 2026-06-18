# Joel's Notes

Running notes, gotchas, and reminders. Newest at the top.

## 2026-06-18

- **Wrong audio source while decoding:** apparently I was decoding off the
  **MacBook's built-in microphone**, not the rig's **audio input device**. Set
  `DM420_AUDIO_INPUT` (case-insensitive substring, e.g. `USB PnP`) so capture
  binds to the right device — or pick it in the unlocked FT8 panel's Radio Setup.
  - **Fixed:** selected the correct audio input device and the decode looks a
    lot stronger now — way more decodes coming through.
