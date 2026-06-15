# Band Scanner panel

This document describes a new type of UI panel.

The band scanner will be selectively activated by the user, and when
it runs, it blocks radio transmissions and does the following:

- For each radio band that the user has selected in the panel, it
  switches to that band and listens for a FT4 / FT8 interval and
  decodes all traffic near the calling frequency for that band.
  
- Once that interval has completed, it switches to the next band
  automatically. It will go through all active bands.
  
- Once complete, the band scanner will automatically return the
  application to its normal operating state.
  
- The band scanner will have a "cancel" option for the user to
  immediately cancel the scan if necessary.
  
The panel will display:

Panel headery:

- "Band Scan" label.

- "Last scan: [x] minutes ago" OR "Currently scanning" text.

- "Scan" button (if not scanning) or "Cancel" if scanning.
  
Panel body:
  
- A text block for each band. The bands will be: 40m, 20m, 15m, 10m.
  They should be displayed in two columns, with 40 and 20 on the left
  and 15 and 10 on the right.
  
- Each text block should have the bandwidth as a large number on the
  left (e.g. "20m"). To the right of the bandwidth should be two lines
  which are half the height of the bandwidth. The first line shows the
  total number of stations seen on that band, the second line shows
  the number of unworked stations on that band.

As a future enhancement, data from the band scanner should also create
pips on the map display.
