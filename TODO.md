# Next

Make band scan panel blank for real mode

Make panel corner accents blue when armed

Better behavior if you select another channel while armed

Auto-arm when selecting a channel's traffic?

Map scroll/zoom/reset

Sent text should be accent2

Sent text not rendering at correct vertical height

# Split audio offset

Double-click to lock audio offset? Click again to unlock.

How can we tell if two stations are communicating at different offsets?

# Soon

Evaluate different color schemes, especially for light mode.

Review if hashed callsigns are properly implemented.

Map screen: highlight with crosshairs the station you're armed to work.

Need to determine actual limits of audio offset for
transmit--currently a hard window, something like 1000-2000Hz.

Note when tracking unworked stations: when Field Day starts we need a
way to quickly reset the log book and mark all stations as unworked
again. Also, "unworked" is specific to each band. Same station on a
different band counts as a different contact. Maybe when UI is
unlocked, have a "reset" button on the log book which moves the old
log book to a new file name.

Map panel: Field Day station positions. The map now places heard stations
from grid squares (CQ grid / standard grid exchange). During ARRL Field Day
the exchange carries an ARRL *section* (e.g. `WI`), not a grid, so those
stations can't be placed by grid and are currently skipped. We need to infer
an approximate position from the section identifier (a section → bounding
region/centroid table, then the same in-grid land-snapping treatment) so
Field Day contacts and heard stations still appear on the map. See the note
in `bus_view::station_grid` and `docs/map_panel.md`.

# Next

Unlocked view should show the keyboard shortcut for each panel.


# Optional

Shared multi-operator notebook panel. Freeform text box.

Consider: as audio is being received, use the signal intensity to guess
where a FT4/FT8 decode will occur, and do a Matrix-style "scrolling
letter" effect to indicate that signal is being decoded there. Also
vary the letter color intensity from dark orange to the accent color.
Then as the decode runs, replace those random letters with the actual
message.

