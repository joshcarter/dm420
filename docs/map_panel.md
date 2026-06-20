# Map Panel

The map panel shows contacts in the log book. It should show either
the positions of recent entries (last 24hr) or all log book entries.

In addition, transient points will come from other sources. The most
common source of these points will be other stations heard on FT8 but
not worked yet. The transient points should only show up for a maximum
of 1 hour, and their indicator on the map should dim if they haven't
been heard recently.

The exact location of stations will not be known. These will need to
be inferred from grid locators in normal FT8 traffic or the ARRL
section identifiers during ARRL Field Day. If traffic doesn't have
either of those, it shouldn't be displayed on the map.

Since the location of each point will be approximate, the point should
use the bounds of the grid locator and, if over a body of water, be
relocated to land within the grid locator. Once a location has been
chosen for a given station ID, it should remain at that position.

Points should additionally be categorized by marker shape (all drawn in
the theme accent color):

- Worked station (in log book): filled circle.

- Worked station (in log book): plus sign.

- Heard but unworked station: unfilled circle. The intensity dims based
  on last-heard time, disappearing at one hour.

- Unworked station calling CQ: filled circle (an answerable caller).
  Dims with last-heard time like the hollow circle.

- Currently selected station (the call selected in the waterslide): a
  full-screen accent crosshair plus a highlight ring on its marker.

section identifiers during ARRL Field Day. If traffic doesn't have
either of those, it shouldn't be displayed on the map.

Since the location of each point will be approximate, the point should
use the bounds of the grid locator and, if over a body of water, be
relocated to land within the grid locator. Once a location has been
chosen for a given station ID, it should remain at that position.

Points should additionally be categorized by:

- Worked station (in log book): filled circle.

- Unworked stations: unfilled circle. The intensity of the circle
  should dim based on last-heard time, disappearing at one hour.

The map's scale and bounds should dynamically adjust to show all
plotted points. The position of our own station should also be plotted
with a stronger indicator. The map will not be centered on our
position, however. (For example, if the user is mostly working
stations to their West, their own station's location will be biased
towards the right of the map.)

The map shows one band at a time. A band switcher in the panel header
(a multi-state segmented control: 40 / 20 / 15 / 10) selects which
band's spots are plotted. "Worked" is per band — the Field Day rule —
so a call worked on another band still reads as unworked here, exactly
as the waterslide partitions its worked-station list. Worked spots take
their band from the log entry; heard spots take the band the rig was
parked on when the station was decoded.

plotted points. The position of our own station should also be plotted
with a stronger indicator. The map will not be centered on our
position, however. (For example, if the user is mostly working
stations to their West, their own station's location will be biased
towards the right of the map.)

The following controls should show up in the panel's footer bar:

- Recent/all logged entries

- Include unworked stations

The background of the map should show distinct backgrounds for land
masses and bodies of water, plus land mass boundaries in the theme's
accent color.

If possible, it would be great to see some amount of texture on land
masses, e.g. Rocky Mountains in the United States, but land texture
should not be prominent. Land mass texture is optional, however.



 
