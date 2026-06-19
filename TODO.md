# Next

Make panel layout show digital at 2/3 width and right panels at 1/3 width. And/or remember panel layout in config file.

FT4/FT8 mode toggle: also switch to calling freq

# Soon

Really need to handle highlighting unworked stations calling CQ

Review if hashed callsigns are properly implemented.

Map screen: highlight with crosshairs the station you're armed to work.

Need to determine actual limits of audio offset for
transmit--currently a hard window, something like 1000-2000Hz.

FT4/FT8 mode switch in the UI. (Per-band calling frequencies already switch
by mode for `/b` — see `send::calling_freq_hz`; the mode itself is still
config-only, read from the live spectrum.)

Waterslide view: indicator for unworked station calling CQ. (Highlight
using cyan secondary accent color?)

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

Toggle switches (e.g. light/dark): hit box for switch should be entire
switch.

# Next

Unlocked view should show the keyboard shortcut for each panel.


# Optional

Shared multi-operator notebook panel. Freeform text box.

Consider: as audio is being received, use the signal intenity to guess
where a FT4/FT8 decode will occur, and do a Matrix-style "scrolling
letter" effect to indicate that signal is being decoded there. Also
vary the letter color intensity from dark orange to the accent color.
Then as the decode runs, replace those random letters with the actual
message.

