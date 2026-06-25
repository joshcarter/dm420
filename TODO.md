# Next

Once QSO finishes, unhighlight that station's traffic and set our "send" box back to CQ

When someone hops on the RR73 with a call, answer instead of calling CQ

I'm pretty sure the map is displaying grid squares in wrong places


# Soon

Map Panel: Once QSO is cleared, turn off crosshairs

Map Panel: When a station answers my CQ, highlight them on the map

Remember FT4/FT8 last operating mode.

Audio option: RX clipping indicator

# Field Day Specific

Map panel: Field Day station positions — DONE (heard + worked). A Field Day
responder sends only its ARRL *section* (e.g. `WI`), not a grid. The map now
places those from a section → regional-centroid table
(`gui::panel_data::section_to_lonlat`), spreading co-section stations across the
section's extent via the same per-callsign jitter as grid cells.
`bus_view::station_locator` yields a `Locator::{Grid,Section}`, and
`LogEntry`/`CompletedQso` carry the `Section` so worked FD contacts plot too.
Land-snapping is implemented (`panel_data::snap_to_land`): a position over water
is relocated to the nearest land within its region, tested against the same
land/lake mesh the map draws. Applies to grids and sections alike; resolved
positions are memoized so a spot stays put across redraws.

Log Book should show station class/section instead of signal report.

Note when tracking unworked stations: when Field Day starts we need a
way to quickly reset the log book and mark all stations as unworked
again. Also, "unworked" is specific to each band. Same station on a
different band counts as a different contact. Maybe when UI is
unlocked, have a "reset" button on the log book which moves the old
log book to a new file name.

# Future

CQ has a distinctive pattern on the spectrogram. Could we queue a "respond to that station" before we have the message fully decoded?

I'm not sure what to do when a station I'm trying to work goes and chases other stations. Cancel? Or just wait? Wait on same offset?

Unlocked view should show the keyboard shortcut for each panel.

Summary of slash commands on unlocked panel.

Auto-arm when selecting a channel's traffic?

Shared multi-operator notebook panel. Freeform text box.

Consider: as audio is being received, use the signal intensity to guess
where a FT4/FT8 decode will occur, and do a Matrix-style "scrolling
letter" effect to indicate that signal is being decoded there. Also
vary the letter color intensity from dark orange to the accent color.
Then as the decode runs, replace those random letters with the actual
message.

