# PowerPoint

A PowerPoint deck as an input, as vMix's PowerPoint input: played by
**PowerPoint itself**, on the Mac running Nida, so its transitions,
animations and videos are exactly as PowerPoint plays them — and moved on
from Nida, or by a presentation clicker plugged into this Mac. No
presentation laptop, no HDMI from the lectern. It needs **Microsoft
PowerPoint for Mac** installed on this Mac.

## Adding one

**Add Input → PowerPoint → Choose a deck…**, or drop a `.pptx` from Finder
anywhere on Nida: each deck becomes an input of its own, named after its
file. As a layer, **Input Settings → + Add layer → PowerPoint**.

Nida opens the deck in PowerPoint (in the background) and starts its slide
show in a window of its own — PowerPoint's "browsed in a window" show — the
size of Nida's picture in the slide's shape (16:9, 4:3), on the screen where
it comes out sharpest (see [Which screen it plays on](#which-screen-it-plays-on)),
then shows that window without its title bar, as **Desktop Capture** would.
PowerPoint stays behind Nida, and the window may sit under Nida's: it is
still shown, its transitions as smooth (tried: a fade at 60 frames a second
after the window had been hidden a minute). It is PowerPoint's: leave it
open.

The first time, macOS asks **whether Nida may control PowerPoint**: allow
it. Refused, the input says so; allow it later in **System Settings →
Privacy & Security → Automation → Nida → Microsoft PowerPoint**. Nida also
needs **Screen Recording**, as for Desktop Capture.

## Moving it on

- **◀ ▶** on its tile and on its column's header; in its layer's settings
  (**Properties**) **⏮ ◀ ▶ ⏭**, **Black** and **White**.
- **PageDown** and **PageUp** — what a presentation clicker sends — and **B**
  (a clicker's black-screen button, and back): a clicker plugged into this
  Mac moves the show **on air** (on Main or a bus), else the one moved last.
  Not while a text field is being typed into, and only when there is a
  PowerPoint input. Bind other keys with **Keyboard learn**.
- **▶** is PowerPoint's own next: the next animation, or the next slide when
  the slide has none left, as a clicker does in PowerPoint.
- Past the last slide PowerPoint shows its black end screen; **▶** there does
  nothing more, so the input never goes blank in the middle of a show.

On its tile, **3 / 22**: the slide among those the show plays (hidden slides
left out), and **black**, **white** or **end** when it is. Its layer's
settings show the slide's **title** and **speaker notes**, read from the
file, and the **next** slide it plays.

## A video on a slide

When the slide on screen has a video, a **Video** row comes up in the
PowerPoint column (and Input Settings): **⏮** from its start, **⏪** back 10
seconds, **▶ / ⏸** play or pause, **⏩** forward 10 seconds, and where it
has got (`1:15 · paused`). It is PowerPoint's own player that is told, so the
video's picture stays in the slide as the show has it. Its sound is
PowerPoint's: **PowerPoint's sound to the mixer** in the same column takes it
into the [audio mixer](16-audio-mixer.md) as this input's — its level and
routing with the rest, heard as the input goes on air (Audio Auto), gone
with the input — and the Mac's own speakers then stay quiet. (**+ Input → App
sound → Microsoft PowerPoint** there does the same, not tied to the input.)
A slide with more than one video: the row works its first.

## It stays up

Nida reads each show three times a second. A show ended in PowerPoint (Esc),
the deck closed there, or PowerPoint quit: Nida plays it again where it was,
a couple of seconds later. **Restart show** in its settings does it at once.

The deck is kept with the show, and the slide it was on: opening the show
opens the deck in PowerPoint again, at that slide. Removing the input ends
the show and closes the deck — without saving: Nida only changed how it
plays — if Nida opened it; a deck already open in PowerPoint stays open.

## Which screen it plays on

PowerPoint draws the slide the size of its window, and macOS keeps a window
on its screen — below the menu bar, above the Dock — so the screen a show
plays on sets how sharp its slide is. **Plays on**, in the input's settings:

- **Automatic** (as it starts): where the slide comes out sharpest — a spare
  screen (connected to this Mac, with no output on it) when that is as sharp
  as this Mac's own, which is then left to Nida.
- **This Mac's screen**: the one with the menu bar.
- **A screen by its name**: that one while it is connected and no output is
  on it; else as Automatic.

Under it, where the show plays and the slide's size there: "On DELL U2720Q ·
slide 1920 × 1080". When that changes — a screen plugged in or out, an output
put on the screen — the show moves at once, and its picture jumps for a
moment as it does: plug screens in before the talk.

For a 1920 × 1080 picture:

| The screen it plays on | The slide |
|---|---|
| This Mac's, a 1080p monitor at 1× | about 1675 × 942 |
| A spare 1080p screen at 1× (a monitor, a TV) | about 1810 × 1018 |
| A Retina MacBook's own | 1920 × 1080 |
| A spare 4K screen, as macOS sets it (looks like 1920 × 1080) | 1920 × 1080 |
| A spare 1440p screen, or 4K looking like 2560 × 1440 | 1920 × 1080 — for a 3840 × 2160 picture, 3840 × 2160 |

A spare screen can be any: a monitor beside the desk, a TV, or an HDMI dummy
plug that macOS takes for a 4K screen. The show's window sits on it with its
title bar and the menu bar above — on a screen nobody watches.

## Presenter view

**+ Presenter view** in a PowerPoint input's settings makes an input for the
speaker's own screen — a confidence monitor, a TV at the stage — as
PowerPoint's presenter view does, from Nida:

- the slide on screen, **live**, its animations as they play;
- across the top, **3 / 22**, how long the talk has run and the time of day;
- beside it the slide's **title** and **speaker notes** in large type —
  smaller when there is much, cut with "…" when even that is too long;
- under it what comes **next**;
- over the slide **BLACK**, **WHITE** or **END** when that is what the room
  sees, or why the show is not running.

Put it on the screen the speaker sees like any input: on a bus shown on that
output. The talk's time starts when the show first plays; **Restart talk
clock** in the presenter view's settings — the **Source** tab of its Input
Settings, or **Properties** with its layer selected — puts it back to 0:00.
Removed, its PowerPoint input leaves it saying so.

Its settings also lay it out, kept with the show and undone like any
change:

- **Talk timer** and **Time of day** on or off, and **Clocks** at the **Top**
  or the **Bottom** (with the slide's number);
- **Next slide** on or off — off, the slide takes its room;
- **Sizes**: **Slide** (its share of the screen; the notes have the rest),
  **Notes** (their type) and **Next** (the line saying what comes next).

## Limits

- The slide is as big as the show's window, which macOS keeps on its
  screen: on a 1080p monitor at 1× (not Retina) about 1675 × 942, scaled up
  to a 1920 × 1080 picture — a little soft. A spare, Retina or 4K screen
  plays it sharper (see [Which screen it plays on](#which-screen-it-plays-on)).
  PowerPoint cannot be told to play a show full screen on a given screen.
- Going to a slide (`{"slide": n}`, or the show opened again at its slide)
  steps PowerPoint there — it will not jump to a slide during a show — so
  the slides and builds on the way flash past on that input: about a second
  for ten slides.
- A step reaches PowerPoint about a quarter of a second after the press.
- Old `.ppt` files play, without notes (notes are read from `.pptx`).
- The presenter view names the next slide; it does not show its picture.

## Over the API

```
POST /api/inputs   {"source": {"kind": "powerpoint", "path": "/Talks/keynote.pptx"}}
POST /api/command  {"PresentationGo": {"to": "next"}}
POST /api/command  {"PresentationGo": {"deck_uuid": "…", "to": "previous"}}
POST /api/command  {"SetPresentationScreen": {"screen": "black"}}
POST /api/command  {"TogglePresentationScreen": {"screen": "black"}}
POST /api/command  {"RestartPresentation": {"deck_uuid": "…"}}
POST /api/command  {"SetPresentationPlaysOn": {"deck_uuid": "…", "plays_on": "auto"}}
POST /api/command  {"PresentationVideo": {"control": "toggle"}}
POST /api/command  {"PresentationVideo": {"control": {"skip": -10}}}
POST /api/inputs   {"source": {"kind": "presenter_view", "deck_uuid": "…"}}
POST /api/command  {"RestartPresenterClock": {"deck_uuid": "…"}}
POST /api/command  {"SetPresenterLayout": {"deck_uuid": "…", "layout": {"timer": false, "clocks_at": "bottom"}}}
```

`to` is `"next"`, `"previous"`, `"first"`, `"last"` or `{"slide": 5}`;
`screen` is `"black"`, `"white"` or `"slide"`; `plays_on` is `"auto"`,
`"main"` (this Mac's screen) or `{"screen": "…"}` with a screen's `identity`
from `outputs.monitors` in `GET /api/state`; `control` (the video on the
slide on screen) is `"toggle"`, `"play"`, `"pause"`, `"restart"` or
`{"skip": seconds}`. Without a `deck_uuid`, the show on air, else the one
moved last — as a clicker. `AddPresentationDeck`
(`channel_uuid`, `path`) adds one as a layer.

A deck's `presentation` in `GET /api/state`: `status` (`opening`, `showing`,
`failed` with a `message` saying why, `no_window`), `slide` (PowerPoint's
number), `slides`, `position` and `shown` ("3 / 22"), `screen` (`slide`,
`black`, `white`, `ended`), the slide's `title` and `notes`, the
`next_slide` with its `next_title`, `plays_on`, and `playing_on` (the
screen its window is on) with `slide_pixels` (the slide's size there), and
`video` while the slide on screen has one: its `state` (`playing`, `paused`,
`stopped`, `not_ready`) and `position_ms`. A presenter view's layer has `presenter_of`: the PowerPoint input's layer it
shows. The presenter view is `presenter_view` with that layer's `deck_uuid`
to `POST /api/inputs` (`AddPresenterDeck` as a layer);
`RestartPresenterClock` takes the presenter view's own layer, as does
`SetPresenterLayout`: its `layout` has `timer`, `clock` and `next` (true or
false), `clocks_at` (`top`, `bottom`), `slide_share` (0.35–0.8), and
`notes_size` and `next_size` (0.5–2) — any left out as a new view has them —
and comes back as the layer's `presenter_layout` in `GET /api/state`.

`--no-powerpoint` starts Nida without ever opening or driving PowerPoint.
