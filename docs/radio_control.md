# Radio Control

This application's user interface should support multiple radios.
Therefore, I don't want to build radio elements into top-level user
interface constructs like the header row. Radio should be built into
individual panels so that the user can set up the panels to match
their system and intended use.

The most sensible location for radio UI is the "waterslide" panel.
Most of this panel will consist of the audio FFT and decoded data, but
the panel could also include:

- Mode select: FT4, FT8, PSK31, etc.

- Band select: 40m, 20m, 15m as appropriate for the radio and antenna.
  
- Radio frequency.

- Message to send text box and a "send" button.

## FT4/FT8 Message Send

In FT4/FT8 the message to send should be mostly automatic based on
context. This should default to "CQ" (calling all stations), but
when another station's traffic is clicked in the decoded message
view, the message should switch to calling that station. During the
FT8 contact the message exchange will auto-switch to the next
message in a proper contact. The "send" button will take action on
the next FT8 interval. It will also change to become a "cancel"
button during transmission.

Note: this will need to be a text box because the input box will also
be used for radio control commands (see below). Exact behavior when
typing into this box is still TBD.

## PSK31 Message Send

PSK31 does not have fixed message contents or timing. It also has the
capability of "live" sending, where you can enable transmit as you're
typing and the radio transmits as you go. This will require the
message send part of the panel to behave differently when in PSK31 mode.

## Configurable Options

The software's user interface will have locked and unlocked modes.
The user will unlock the UI for setup and configuration, then lock it
to operate. This reduces the number of UI elements visible during
normal operation, and prevents unintended configuration changes.

Radio configuration options will need to include:

- Bands the radio should be able to select between.

- Contest modes: normal FT8 traffic follows a set pattern, but ARRL
  Field Day has a different traffic flow, and other contests may
  require other templates.

- Audio device in/out, serial device for rig control, and radio type.
  Initially this will only support Kenwood TS-480SAT and TS-590S radios.

## Keyboard Navigation

When the panel is selected, keyboard input may be treated as message
to send or for radio control. The exact behavior is dependent on mode.
For FT8, the only real control is responding to a station or calling
CQ, then selecting "send". The "enter" key will select send or cancel
sending.

For radio control, we'll use "slash commands" to indicate non-message
control. Example commands:

`/f 14.074` or `/frequency 14.074`: set frequency to 14.074Mhz

`/b 20` or `/band 20`: set band to 20m

The parsing should be reasonably flexible, for example `/b 20`,
`\b 20m`, and `/band 20m` should all be considered valid input for
changing the band to 20m.





