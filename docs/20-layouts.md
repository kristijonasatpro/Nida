# Screen Layouts

Everything under the top bar is a set of **panels**, each with a tab. The
**Layout** button in the top bar (`Layout: Classic`) picks how they are
arranged, and any panel can be dragged somewhere else.

| Layout | What it looks like |
|---|---|
| **Classic** | Nida's own: Library left, channel columns in the middle with Properties under them, Main Output, Switcher (the Rundown a tab behind it) and Audio Mixer on the right |
| **Broadcast** | **Preview** and **Program** across the top with **CUT**, **FADE** and the fade time between them; a **tile per channel** below; the Audio Mixer beside the tiles, with the Switcher, Rundown, Properties, Library and Setup as tabs behind it |
| **VJ** | **Layers** as rows, top layer on top, decks across like clips; the Main Output top right; Properties bottom left and the Library, Switcher, Rundown, Audio Mixer and Setup bottom right |

The layout is part of the workspace: it is there when Nida opens again.
Layouts change only what is on screen — never the show, the mixer or what is
on air.

## Panels

| Panel | |
|---|---|
| **Library** | Generators, effects, cameras, streams — drag into a channel. **L** opens and closes it |
| **Channels** | Channel columns, decks stacked in each (Classic) |
| **Layers** | Channels as rows, decks across (VJ) |
| **Inputs** | A tile per channel (Broadcast) |
| **Preview / Program** | The next channel and Main side by side, with the take buttons |
| **Main Output** | The Main picture. Click it for the master's effects; right-click to send it to a bus |
| **Switcher** | The buses |
| **Rundown** | The show in order: steps taken with one press, and GO (see [Rundown](25-rundown.md)) |
| **Audio Mixer** | Faders, meters, monitoring |
| **Properties** | Settings of whatever is selected |
| **Setup** | Tonemap, modulation, MIDI, stage layout, outputs |
| **⏱ *timer*** | One per timer: start, pause, reset, its length (see [Timers](23-adding-inputs.md#timers)). Listed under **Layout ▸ Timers** |

The Stage Editor and the Arrangement timeline open in place of the first
channel view on screen (Channels, Layers or Inputs).

## Making your own

- **Drag a tab** onto another panel to put it in that group, or to an edge of
  a panel to split it there. Dragging a tab away from everything opens that
  panel in a window of its own (see below).
- **Drag a divider** to resize.
- **×** on a tab closes it. **Layout ▸ Panels** opens or closes any panel.
- **Layout ▸ Save as** keeps the arrangement under a name. Saved layouts are
  kept for you rather than for one workspace — every show can use them — in
  `~/Library/Application Support/Varda/layouts.json` (set
  `VARDA_LAYOUTS_FILE` to keep them elsewhere).

Once a layout is changed, the button shows a `*`. **Layout ▸ Update "…"** saves
the changes into a layout of your own; **Reset to "…"** puts the panels back as
the layout has them. The 🗑 beside a saved layout deletes it.

**Lock panels** stops tabs being dragged, closed or floated, so a stray drag
during a show can't rearrange the screen. Dividers still resize.

## Panels in windows of their own

The **Switcher**, **Audio Mixer**, **Preview / Program**, **Main Output**,
**Rundown**, **Presentations** and **timer** panels can each be in a window of
their own — to put on another screen, moved and sized like any window.

- **Open one**: drag its tab out of the panels, away from all of them; or
  right-click its tab ▸ **Open in a window**; or **Window** beside it in
  **Layout ▸ Panels**.
- **Put it back**: close its window, or **Put back** at the top of it (or
  beside it in **Layout ▸ Panels**). It goes back among the panels where it
  is most likely wanted.
- The windows are kept with the show and the workspace, each where it was.
  One whose screen is no longer connected opens beside the main window.
- A built-in layout chosen — **Classic**, **Broadcast**, **VJ** — or
  **Reset to "…"** one puts every panel back in the main window, as it has
  them.
- **Save as** keeps the panels in windows of their own too, each where its
  window is: save one layout for one screen and another for two — Audio Mixer
  and Switcher on the second — and choosing it puts them back so, the other
  panels in the main window. A window moved since does not count as a change
  (no `*`); a panel put in a window, or back, does.
- The show's keyboard shortcuts work from a panel's window too — unless you
  are typing in a field there. Files dragged from Finder onto the
  Presentations panel in its window become talks, as in the main window.

The Library, Channels, Layers, Inputs, Properties and Setup stay in the main
window: things are dragged into and out of them, and a drag reaches no further
than its own window. A Rundown in a window opens its steps' settings beside
it, in that window; other settings windows (an input's, a timer's) open in
the main window. With **Lock panels** on, no panel goes into a window or
back, and a panel window's close button is off.

## Broadcast: Preview / Program and Inputs

Laid out the way a vMix user expects.

Preview is the channel selected — or the one holding the selected deck — and
is what **Space** cuts and **Enter** fades to Main (see [Main](18-main.md)).
Clicking a tile puts it in Preview; the tile's own **Cut** and **Fade** take it
to Main at once. A tile's red frame means ON MAIN, green means Preview.

A tile holding a clip (video or sound) has its own controls on the picture,
as a playlist has: **⏮** back to the start, **▶ / ⏸** (green while it plays),
and the time gone and left, with the playhead along the foot of the picture —
start, stop and watch a clip without putting it in Preview. **⏸** there does
not stop the sound dead: it fades it out over 2 seconds, then pauses (amber
while it fades; press again to pause at once). Right-click it to choose
**Cut** or a fade of 0.5 to 5 seconds — one choice for every Stop, on the
tiles and in the [Audio Mixer](16-audio-mixer.md). A sound, which
has no picture, shows a **♪** and its name. The chips under a tile mark each
clip, **▶** a video and **♪** a sound (green while it plays); a picture has no
mark. A layer that is covered (under a full-screen one) still has its chip's
picture: it is drawn for it once, and again every two seconds or so.
Every tile is live: a channel on Main, in Preview or selected is drawn every
frame, and one that is only a tile is still drawn 20 times a second — a clock
keeps ticking, a slide show moves on — at a third of the cost. A preview is
made for the control window only while it is on screen (not scrolled away,
not in a closed panel), only when it has been drawn since (a channel's),
and at the size it is shown at — a tile's is a fraction of the Program
picture's: making every preview full size every frame, shown or not, was the
GPU's biggest cost on a MacBook.

Beside the Program picture stand meters for each place the sound goes:
**REC**, **STREAM**, and every audio bus that plays on a sound device (the
speakers), named over its meter, with the bus a recording or stream carries —
or the device a bus plays on — under it. REC and STREAM show their bus even
before they start, dimmed, and turn red while recording or live; meanwhile
they meter the bus they started with (a bus removed since reads silence, as
its sound has stopped). A source
sent to the speakers' bus but not to the stream's shows as a STREAM meter
standing still while the speakers' moves. The **Main Output** panel has the
same meters beside its picture.

With **REC ▸ Reminder** ticked, **NOT RECORDING** blinks red behind the take
buttons — between Preview and Program, on each Switcher card and in the
channel desk's toolbar — for as long as nothing is being recorded.

Under the Preview picture is the transport of its clip (video or sound) —
the selected layer when it is a clip, else the input's top clip — and under
the Program picture the same for the clip on air, so a clip playing on Main
can be paused there and its time left seen (none in a layered mix):

| | |
|---|---|
| Bar | The whole clip; the part between the in and out points lighter, marked with yellow brackets; what has played of it blue; the playhead white. Click or drag to move the playhead |
| ⏮ | Back to the in-point |
| ⏪ ⏩ | Back or forward 5 seconds |
| ▶ / ⏸ | Play or pause |
| Times | Where the playhead is, and what is left to play to the out-point |
| **[ In**, **Out ]** | Start or end the clip where the playhead is |
| **×** | Clear the in and out points: play all of it |
| **Loop** | Play it round and round; off, it stops at its end |

In a panel too narrow for the whole row — a Preview / Program window on a
small screen, say — the transport keeps the bar, ⏮ ⏪ ▶ ⏩ and the times; In,
Out and Loop are then in the clip's settings.

Along the foot of each tile are the decks layered in the channel: click one
for its settings, double-click to adjust it, right-click to take it to Main on
its own. **Double-click a tile** for its [Input Settings](21-input-settings.md)
— layers, position, crop, colour, keys and effects, as vMix opens them.
Files and Library items dropped on a tile go into that channel. Files dropped
anywhere else — beside the tiles, or on the empty tile at the end — are an
input each, named after its file; a Library item dropped on the empty tile
makes a new channel holding it.

With the layered mixer on, the take buttons are replaced by a **Main mode**
button, and every channel mixes to the output as before.

A tile's **name bar** is its handle:

| On the name bar | |
|---|---|
| Drag | Move the channel along the row; a blue line shows where it lands. Drop it on the empty tile at the end to make it last |
| Double-click | Rename the channel (Enter keeps the name, Esc leaves it) |
| Right-click | Fold the tile to a narrow strip with its name down it, to make room for the channels in use; right-click the strip to open it out. A strip is framed red on Main and green in Preview, takes clicks, double-clicks and drops like a tile, and stays folded when Nida opens again |
| × | Delete the channel, at once: **Undo** brings it back. One that is on air — on Main or a bus's Program, itself or inside another input there, fading or not — asks first, saying where ("Ch 24 is on Main"). The last two channels stay |

A channel's right-click menu (in any layout) also has **Rename…**, **Colour**,
**Move earlier** and **Move later**. **Colour** gives the input a colour of its
own from ten — its tile, its settings and its switcher buttons wear it —
and **Automatic** gives it back the one its place gives. The colour is saved
with the show.

## VJ: Layers

Laid out the way a Resolume user expects.

The last channel is the top row, because a later channel is drawn over an
earlier one, like a higher layer. Each row has the layer's name, and in Main
mode its tallies, **Cut** and **Fade**; in the layered mixer, its opacity and
blend mode. The deck cards are the same as in the Channels panel. Drop on a
row to add to that layer, or on **+ Layer** at the bottom for a new one.
