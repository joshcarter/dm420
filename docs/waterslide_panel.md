# The "Waterslide" Panel

This document describes a user interface panel for our radio
communications software.

A "waterfall" FFT graph is common in digital radio communications, showing
time on the vertical axis, frequency on the horizontal, and color
shows signal intensity. I'm proposing a "waterslide" graph. It simply
rotates the view so that time is horizontal and frequency is vertical.

The reason for this is showing decoded signals next to the graph. The
FFT graph would take up the right side of the panel, and the signal decode
would be on the left. The panel should show all decoded signals
within the audio passband next to that signal in the waterslide.
Whatever the signal's vertical position within the graph, that will be
the text's vertical position. Thus it should be easy to visually
interpret multiple senders at the same time.

Since the waterslide view will show signals arriving over time, with
older signal data falling off one side, the text will likewise have to
scroll and have older data fall off the side. Let's try this: FFT
graph will read with most recent time at its left side; the decoded
text will read with most recent time at its right side. Since these
take up equal horizontal space within the panel, the center of the
panel will have time = "now".

The horizontal sliding of time would be matched with the two
halves--as best as possible, that is. On the FFT side it's
straightforward to zoom the time axis in/out, but on the decoded text
side we hit a limit with the bit rate of the digital mode and the
width of individual letters. The panel should do its best to match the
FFT data sliding right and the decoded text sliding left at the same
rate.

The graph also tracks a notion of "outgoing" frequency. This is where
the next transmission will go. Generally this does not change the
radio's center frequency, but rather is where transmitted audio will
be tuned to.

Cliking within the FFT graph will change the outgoing frequency based
on the vertical position that the user clicked. Clicking on decoded
traffic text, however, will snap the outgoing frequency to whatever
the frequency was for that decoded traffic.

The radio's next FT4/FT8 transmission will take place at that outgoing
frequency, either to answer a calling station or to do a new "calling
all stations" transmission.

An indicater should be shown for the current outgoing frequency.

During radio transmit, recieve data will not be present. However our
own transmission audo will be known. The graph should show our
transmission FFT at the appropriate center point and our own
transmitted data should show up in the decode panel. It should be
visually distinct from received data.

Traffic on HF radio does not slot cleanly into frequency channels,
however. Sometimes signals show up on top of one another, or
overlapping with another signal. Thus the packing of text vertically
will have to be approximate. The waterslide panel should make a best
effort to keep decoded text next to their signal.

Note: this panel UI could be extended to show PSK31, RTTY, and other
digital radio traffic.
