# Main mode

Nida's master can be made two ways:

- **Main** — the vMix way: **one channel on air at a time**, taken with a cut
  or a fade, with **overlay channels** (a logo, a lower third) on top of
  whatever is on it. The channels sit in one row, wide, with large pictures,
  and there is no mixer column.
- **Layered** — the VJ mixer: every channel shows at its fader and a
  crossfader blends two.

Switch with **Main mode…** at the top of the layered mixer's centre
column, and back with **Layered mix…** at the right of the Main toolbar.
Switching to Main with nothing on it takes the channel the layered mix was
showing on top, so the output does not go black. The mode is saved with the
scene.

Everything that takes **Master** — the Main Output preview, screens, buses
showing Master, recordings and the stream — carries Main.

## Taking a channel to Main

1. **Click a channel** (its name, or any deck in it). The toolbar shows it as
   **NEXT** in green, the channel header gets a green **NEXT** tally, and the
   bottom bar previews it — exactly what will go out.
2. Press **Space** to **cut** it to Main, or **Enter** to **fade** it over the
   fade time (the menu beside **FADE**, 1 s by default). The toolbar's
   **CUT (Space)** and **FADE (Enter)** buttons do the same.

What is on Main shows as **ON MAIN** in red: in the toolbar, on the channel
(red frame and tally), under the Main Output preview, and in the bottom bar's
**Send to** column, whose **Main** row has **Cut** and **Fade** too.

After every take, **Preview and Program swap**, as in vMix: what was on Main
becomes the selection — Preview — so the next Space or Enter puts it back.
Taking from anywhere swaps (a tile's Cut, a right-click menu, the API); an
open Input Settings window stays where it is. A clip going on or off air
plays, pauses and sounds as its [On air](21-input-settings.md#on-air)
settings say.

Space and Enter do nothing while you are typing in a text field — nor does
any key without ⌘, nor the Enter that finishes a name. They can be re-bound
like any shortcut with **KB Learn** (actions *Cut to Main* and *Fade to
Main*).

### Transitions

The menu beside the fade time picks what **Enter** — and the second take
button, named after it — does, as vMix's transitions:

| | |
|---|---|
| **Fade** | The next picture dissolves in over this one |
| **Merge** | The layers both inputs have — the same camera, clip, image, stream or window — move and resize from where they are to where they will be; the rest fade. A picture-in-picture grows to full screen, or two speakers trade places and sizes. An input shown as a layer of the next grows into, or shrinks to, that layer's place |
| **Wipe** | The next picture wipes across from the left |
| **Slide** | The next picture slides in from the right, pushing this one out |
| **Zoom** | This picture zooms in and away, the next behind it |
| **Iris** | A circle opens from the middle onto the next picture |

All but Fade gather speed and then settle (ease in and out); Fade goes evenly.
A transition takes the fade time. Merge takes inputs apart layer by layer:
an input with effects of its own, layers blended other than Normal, or a
layer that transitions by itself is faded instead — and a notice says which,
and why. A layer both inputs show that fades across all the same is named
in a notice too, each with its reason. A layer whose effects do more than crop, move and scale it (a
mirror, a turn) moves as the whole frame. A camera, a screen, an NDI
source or another input (a tap) moves with its framing: cropped, bordered,
rounded or round in one input and framed otherwise in the next, it is drawn
once, its place, size, crop and border all on the way from one to the
other — a round picture-in-picture opens into the whole picture. A tap
moves so whether it keeps its input's transparency or not: kept in one
input and not in the next, black comes in, or goes, where its input is
clear. It starts and ends exactly as each input shows it, so both need the same
[Resolution](10-resolution-and-monitoring.md#per-deck-resolution) (Full, Half or Quarter);
at different ones it fades across, and a notice says so. A picture,
clip or generator drawn alike in both inputs (the same crop, colours and
flip) moves whole, the part outside the frame on its way off the edge — or
in from outside — included; with a soft-edged or bordered crop that part
still fades. Framed or coloured differently, it fades across, and a notice
says so. Copies of one clip or generator in two inputs each play on their
own, so they show different frames: to move one playing clip between
layouts, put it in a channel of its own and show that channel in each layout
as a tap (**Add Input → Another input**). A timer, a solid colour and a
playlist match nothing and fade. Each merge's moves are written to the log.
Wipe, Slide, Zoom and Iris take whole inputs: a layer taken to Main on its
own fades. The transition is saved with the show; overlays always fade.

### With the screens, in one press

A step of the [Rundown](25-rundown.md) can take an input to Main, switch
overlays and change every screen of a wall together — one press for a moment
of the running order. Main is taken as FADE or CUT takes it here.

### One input alone

Right-click a deck card → **Cut to Main** / **Fade to Main** under *This input
alone* sends that deck by itself, without the other decks of its channel.

## Overlays

Tick **Overlay** in a channel's header to make it an overlay: it is no longer
taken to Main but switched **on and off over whatever is on Main** — through
cuts and fades underneath it. Its header shows **ON** (red) or **OFF**; click
it, or select the channel and press **Space** (cut on/off) or **Enter** (fade
on/off).

An overlay's **fader** (its opacity) and **blend mode** apply: a logo at 80 %,
a light leak in Add. A bus's **Master** menu still leaves an overlay off one
output — the lower third on the stream but not the recording.

## Channels

**+ Channel** in the toolbar adds a channel; drop files, folders or Library
items into it. There are no drop zones for new channels in Main mode.
Channels share the width of the desk between 240 and 340 pixels each, and
scroll sideways when there are more than fit.

## Saved, and safe from undo

The mode, what is on Main, the overlays that are on and the fade time are saved
with the scene, so a restart opens on the same picture. Which channels are
overlays is part of the channel and undoes like its other settings.

What is on Main is live: **undo never changes it**. Undoing an edit does not
put the previous speaker back or take a logo off.

## API

| Route | |
|---|---|
| `PUT /api/main/mode` | `{"mode": "program"}` (Main) or `"layered"` |
| `POST /api/main/take` | `{"source": {"Channel": "<uuid>"}, "fade": true}` — or `{"Deck": "<uuid>"}`, or `"Black"`. An overlay channel is switched on instead |
| `PUT /api/main/fade-time` | `{"secs": 1.5}`, 0–10 |
| `PUT /api/main/transition` | `{"transition": "merge"}` — or `fade`, `wipe`, `slide`, `zoom`, `iris` |
| `PUT /api/channels/{channel_uuid}/overlay` | `{"overlay": true}` |
| `PUT /api/channels/{channel_uuid}/overlay-on` | `{"on": true, "fade": false}` |

What is on air is in `GET /api/state` under `mixer.main`; each input in
`mixer.channels[]` has `on_air` while it is: `"main"`, or the UUID of the
first bus whose Program shows it — itself, or inside another input there.
