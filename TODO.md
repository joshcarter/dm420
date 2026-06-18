# Soon

Need to determine actual limits of audio offset for
transmit--currently a hard window, something like 1000-2000Hz.

FT4/FT8 mode switch. Also switch calling frequency for each band.

Waterslide view: as signals get decoded, we may need to shift the
vertical position of decodes when their text won't render cleanly
because of adjacent signals. We may need to bump the position of a
decode up/down so they are all readable--but without completely
rearranging all the decodes as DM780's "superbrowser" would do.
IMPORTANT: click to select signal must select the actual audio center,
not including any text shift factor.

Waterslide view: related to signal decode text not stepping on each
other, we need to control the text size. Set a reasonable min/max font
size. Min font size may need to be app-wide, e.g. for log book.

Waterslide view: indicator for unworked station calling CQ. (Highlight
using cyan secondary accent color?)

Note when tracking unworked stations: when Field Day starts we need a
way to quickly reset the log book and mark all stations as unworked
again. Also, "unworked" is specific to each band. Same station on a
different band counts as a different contact. Maybe when UI is
unlocked, have a "reset" button on the log book which moves the old
log book to a new file name.

Toggle switches (e.g. light/dark): hit box for switch should be entire
switch.

# Next

Unlocked view should show the keyboard shortcut for each panel.


# Optional

Consider: as audio is being received, use the signal intenity to guess
where a FT4/FT8 decode will occur, and do a Matrix-style "scrolling
letter" effect to indicate that signal is being decoded there. Also
vary the letter color intensity from dark orange to the accent color.
Then as the decode runs, replace those random letters with the actual
message.

