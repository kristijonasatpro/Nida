# The dot on output screens

While Nida uses a camera, a capture card, a microphone or a screen capture,
macOS draws a small coloured dot in the corner of the Mac's screens — on an
LED wall or a stage monitor driven as a Mac display too. This page says what
it is and how to keep it off the external screens.

## What the dot is

macOS shows it on the screens of the Mac that is capturing:

- **green** — a camera is in use. Capture cards count as cameras: a Magewell,
  an UltraStudio, an ATEM's USB webcam output.
- **orange** — a microphone is in use, a capture card's sound included.
- **purple** — the screen is being recorded (a screen or window input).

It is drawn over the screen, never into the picture: recordings, streams and
NDI do not carry it, and a presenter's laptop feeding a capture card shows no
dot of its own.

## Keeping it off external screens (macOS 14.4 and later)

Apple has a switch for this. It hides the dot on external displays only; the
Mac's own screen always shows it.

1. Start the Mac up in Recovery. On Apple silicon: shut down, then hold the
   power button until **Options** appears, and choose it.
2. In Recovery, choose **Utilities → Terminal** and run:

   ```
   system-override suppress-sw-camera-indication-on-external-displays=on
   ```

3. Restart.
4. In **System Settings → Privacy & Security → Camera**, turn off **Privacy
   Indicators**; then the same under **Microphone**.

Apple's page: [support.apple.com/118449](https://support.apple.com/118449).

Nida's outputs need nothing more: they cover their screens as they are, and
the switch applies to them as to any full-screen app (tested on macOS 27.2,
Mac Studio M5 Max, outputs on an external 1080p HDMI monitor).

## Checking it

In a normal start-up, this only reads the setting:

```
/usr/bin/system-override suppress-sw-camera-indication-on-external-displays
```

It says `on` once the switch is set.

## Undoing it

In Recovery's Terminal, run the same command with `=off`, or
`system-override reset`. (Apple's page does not describe undoing it; this is
from the tool's own help.)

## Other ways round it

- A **Blackmagic output** (UltraStudio, DeckLink) never shows the dot: it is
  not a Mac display. Neither do NDI and SRT outputs.
- Avoid old utilities that hide the dot by switching off System Integrity
  Protection.

## Limits

Apple describes the switch for the camera and microphone dots only. In the
test above a screen capture was running too and no dot showed on the
external screen — but that was one test, on macOS 27.2.
