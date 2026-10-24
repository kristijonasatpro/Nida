# Input Settings

The window vMix opens when an input is double-clicked. In Nida an input is a
**channel**, and the pictures stacked in it are its **layers**. Open it by:

- double-clicking a channel — an **Inputs** tile (Broadcast layout), a **Layers**
  row's name (VJ layout), a channel's name in the **Channels** panel, or
  the **Preview** picture;
- right-clicking a channel → **Settings…**;
- double-clicking a **deck card**, or **Adjust…** on it — the window opens with
  that layer picked;
- double-clicking a clip's name in the **Audio Mixer**, or **Input settings…**
  in its ⚙.

A channel holding one clip (a video or a sound file) opens on that clip's
**Playback** tab — what it does going on and off air — as vMix opens a video
input on its General tab; one holding one picture opens on its **Position**.

The window **follows the selection**: click another tile, channel or card and
it moves there. Drag it anywhere and resize it from its corner; close it with
its **×** or **Esc**. Inside it, drag the **dividers** — between the layer list
and the rest, and between the picture and the tabs — to make one part bigger;
**Controls: Beside / Below** puts the tabs beside the picture (as it opens)
or under it; the choice stays for every show, and after Nida restarts. Its
settings are named in large, light text and the sections in larger, and the
sliders take the width there is, for fine moves. Files, cameras and streams
dropped on it go into its
channel as a new top layer; an effect dragged from the Library onto it goes on
whatever it is editing.

## Layers

Down the left: the channel's **result** (every layer composited), then the
channel itself, then its layers, listed as Premiere lists its tracks: **the
layer in front at the top**, Layer 1 at the back at the bottom, each higher
one drawn over it. Every list of a channel's layers — the Channels panel, the
Layers panel's rows, the chips on its tile — puts the layer in front first.
Files dropped together keep their order: the first is in front.

| On a layer's row | |
|---|---|
| Click | Edit this layer |
| ☑ | Shown — untick to hide it, keeping all its settings |
| ⏶ ⏷ | Move up, in front of the layer above it / down, behind the layer below it |
| 🗑 | Delete the layer |
| Right-click | Take this layer alone to Main or a bus, **Copy position** / **Paste position**, copy, paste |

**+ Add layer** opens the [Add Input](23-adding-inputs.md) window for this
channel, with the same kinds as a new input — a clip, a picture, a camera,
NDI, a screen, a stream, a colour, a generator, **another input** — and adds
what is chosen on top.

### Another input as a layer

**+ Add layer → Another input** puts another input's picture into this one as
a layer — the same clip on a second input with its own background and at its
own size, say, without adding the clip again. It is drawn once and shown in
both, so nothing is decoded or rendered twice, and it is this frame's picture
in both. Size and place it with Crop and Transform like any layer; where the
input is clear (a fitted clip's bars) the layers under it show. An input that
already shows this one, itself or through another, is greyed out: the two
would show each other round a loop. (It is a channel [tap](09-streaming-and-io.md#program-tap),
listed in the layer's Source as **🔁** and the input's name.)

Deleting the input takes the layers showing it in other inputs with it — one
**Undo** brings both back. A layer whose input is not in the show (a show
saved with it gone) draws nothing, its border and effects included.

## A layer

The right side shows a **preview** — the layer alone, the channel, the Main
output or any screen. **Drag** the picture to move it; **Shift-drag** or
**scroll** to resize it; **Reset position & size** puts it back. The outline
shows what shows of the picture — what the crop keeps — even when its edges
are transparent.

**Snapping**, as Apple's apps line things up: a dragged layer's edges and
centre snap to the frame's edges and middle and to the other layers' edges and
centres, and a yellow guide shows each line it met. Resizing snaps to full
size and to the width of another layer. Hold **⌘** while dragging to place it
freely.

| Tab | |
|---|---|
| **Source** | How the picture is scaled into the frame — **Fit** (all of it, the rest transparent: a 9:16 phone clip stands in the middle with nothing either side), **Fill** (fills the frame, cutting the edges), **Stretch**, **Center** (its own size) — how often and how large the layer is drawn, a capture's or tap's controls (a tap's: which input it shows, and **Transparent** — off flattens it over black), and a generator's own parameters with a Reset. New pictures and videos come in fitted |
| **Position** | Opacity, blend mode, **Crop** (left, right, top, bottom, feather), **Border** and **Transform** (position, scale — or separate X / Y — rotation, and **Flip horizontal** / **Flip vertical** for a webcam that comes in mirrored or a camera hung upside down; a crop is mirrored with the picture). **Copy position** / **Paste position** carry crop, border and transform to another layer, of this channel or any other |
| **Playback** | For a video or sound file: what it does **on air** (below), then play / pause, scrub, speed, loop, in and out points. A sound file has only this tab |
| **Colour** | **Colour correction** (brightness, contrast, saturation, hue, gamma), **Colour balance** (red, green, blue of the shadows, midtones and highlights — vMix's lift, gamma, gain) and **Levels** |
| **Key** | **Chroma key** (a green or blue screen: target colour, tolerance, softness) and **Luma key** (dark — or, inverted, bright — parts). Beside the target colour — and every colour setting — **Pick**, then click the green in the picture (or anywhere on the screen) to take it (macOS) |
| **Effects** | Every other effect, run top to bottom: on / off, ⏶ ⏷ to run earlier or later, **Remove**, and each one's controls. **+ Add effect** lists the Library's effects; type to find one |

A tab's effect that is not there yet has a button to add it.

### On air

A clip added to Nida waits on its first frame; it does not start by itself.
What it does as it goes **on air** — on Main, or on a bus's Program, on its own
or as a layer of a channel that is there — is set at the top of its
**Playback** tab, as in vMix:

| | |
|---|---|
| **Play when it goes on air** (Auto Play) | It starts playing as it is cut or faded there |
| **Pause when it comes off air** (Auto Pause) | It stops as it leaves |
| **Back to the start when it comes off air** (Auto Restart) | It goes back to its in-point, ready for next time |
| **Sound on while on air** (Audio Auto) | Its sound switches on with it and off when it leaves, fading with a fade — see [Audio Mixer](16-audio-mixer.md#audio-auto). **On air on** picks where it looks: Main or any bus, Main only, or one bus |

**Starts and stops on** picks where on air counts for the first three:
**Main or any bus** (the default: cut to the LED's bus, it plays there without
touching the stream), or **Main only** (on a bus it neither starts nor stops
by itself — one left on a bus still stops when Main leaves it).

All four are on for a new clip. They are saved with the clip (the first three
in the scene, Audio Auto with the mixer). **Loop** decides what it does at its
end: loop, play once and stop, or hold the last frame.

### Blend

**Blend with layers below** mixes the layer with the layers under it in its
channel — so Layer 1, with nothing under it, has none. **Blend over Main**
(an overlay channel) or **Blend over the channels below** (the layered mixer)
blends the whole channel: a logo or title drawn on black shows only its light
parts with **Screen** or **Add**. A PNG with a transparent background needs
neither — its transparency is kept, and so are the empty sides a fitted
picture leaves.

### Border

Tick **Border** for a coloured edge round the layer, as vMix's layer border:
**Colour** (with its opacity), **Thickness** and **Roundness** — rounded
corners cut the picture's corners away too; at its top, the ends are fully
round. **Circle** makes it the largest circle inside what the crop keeps,
centred, border and all: a speaker in a ringed circle, as podcasts show
them, whatever shape the crop is (Thickness 0 for a circle with no ring).
The border runs round what the crop keeps, just inside its edge. Its sizes
are pixels of a 1080-line picture whatever the resolution, measured before
Transform, so a picture shrunk to a third shows a 6 px border as 2 px.

### Masks

Every effect but Crop and Transform — a colour change, a key, a blur — has a
**Mask** row under it: **None**, **Rectangle**, **Ellipse** or **Draw**. With
a mask the effect shows only inside the shape: blur a face, recolour one
corner, key only part of the picture. **Invert** puts it outside instead.

Picking **Rectangle** or **Ellipse** puts one in the middle; drag on the
picture above to draw it where it should be (hold **Shift** for a square or a
circle). **Draw** traces an outline by hand as you drag — up to 64 points,
simplified when it is longer. **Feather** softens the edge: the width of the
fade across it, in pixels of a 1080-line picture, half inside the shape and
half out. The outline shows on the picture while you draw; **Done** or
**Esc** finishes, and **Redraw** draws it again. While a mask is being drawn
the drag draws it and does not move the layer.

A mask is kept with the show and copied with its effect
(`PUT /api/effects/{uuid}/mask` sets or clears it).

### Mask (the layer's own)

Under Transform, **Mask** cuts a shape out of the layer itself, so what is
under it shows through: a window in a full-screen graphic for a camera
behind it, a hole for a logo below. The same shapes as an effect's mask —
**Rectangle**, **Ellipse** (Shift for a circle) or **Draw**, drawn on the
picture — with **Feather** for a soft edge that fades the layer out rather
than darkening it. **Invert** keeps only the shape instead, the rest of the
layer gone. **None** takes it away.

The shape is placed on the layer's frame as you see it in Input Settings, so
it stays where it was drawn when the layer is moved or scaled. It goes
through everything on the layer — its effects, its border — and is kept with
the show (`PUT /api/decks/{uuid}/mask` sets or clears it). For a circle with
a ring round it, the Border's **Circle** is simpler.

### Copy and paste

Crop, Border and Transform each have **Copy** and **Paste**: copy one layer's
transform, say, and paste it on a layer of this channel or any other, leaving
that layer's crop and border as they are. **Copy position** / **Paste
position** (at the top of Position, and in the right-click menu of a layer
row, a deck card or an input's deck) carry all three at once. The Paste
buttons light up once there is something of theirs to paste.

### Reset

Crop, Border and Transform each have a **Reset**, and so has every colour,
key and other effect. **Reset all** on the Position tab puts crop, border,
transform, opacity and blend back to how a new layer starts.

## The whole channel

| Tab | |
|---|---|
| **General** | The channel's name (**Rename…**); Cut / Fade to Main, or an overlay's On / Off; the Overlay role; opacity and blend where they count (the layered mixer, or an overlay); Cut / Fade to every bus |
| **Layout** | vMix's layer templates: **Full screen**, **Side by side**, **Top and bottom**, **PiP top right**, **PiP bottom right**, **2 × 2 grid**. One click places the layers — Layer 1 first — by their transforms; then drag them to fine-tune. A layer without a Transform is left where it is |
| **Colour**, **Key**, **Effects** | As for a layer, on the channel's result |

Every change is an ordinary edit: **Undo** takes it back, and a drag on the
preview is one undo step.
