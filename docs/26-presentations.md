# Presentations

A conference's talks in one input — **Slides** — in the order they are
given: PowerPoint decks, PDFs, web presentations (an HTML deck, AI-made or
reveal.js), a speaker's clips and pictures. They are put in order before the
show and each started with one press, in the **Presentations** panel. A
picture-in-picture, a side-by-side or a presenter view that shows the Slides
input shows whichever talk is on: it never has to change. No presentation
laptop, no HDMI from the lectern.

PowerPoint decks are played by **PowerPoint itself** (Microsoft PowerPoint
for Mac, on the Mac running Nida), so their transitions, animations and
videos are exactly as PowerPoint plays them; PDFs are drawn by Nida; web
pages by the Mac's own web engine, as Safari plays them. A single deck can
also be an input of its own, as vMix's PowerPoint input — see
[PowerPoint](#powerpoint).

## Slides

### Adding talks

- Drop PowerPoint decks, PDFs and web pages (`.html`) from Finder anywhere
  on Nida: they are talks of the show's Slides input, made when there is
  none (called **Slides**). Dropped on a Slides input's tile, clips and
  pictures are talks too.
- In the **Presentations** panel (**Layout ▸ Panels ▸ Presentations**; in
  the built-in layouts a tab beside the Rundown): **+ Add files…**, or drop
  files on it — between two talks, to put them there.
- **Add Input ▸ Presentations**: an empty Slides input, or **Choose files…**.

A talk is named after its file; **Rename…** gives it the speaker's name.
Files nothing plays (a Keynote file, a text file) are not added, and say
why.

### The Presentations panel

| | |
|---|---|
| **ON AIR** | The Slides input's picture beside the talk on: where it is (**4 / 31**, a clip's time), **⏮** (back to its first slide — the talk from the start, all tried out before the show), **◀ ▶**, **Black**, **White**, the slide's title and notes (PowerPoint), how long the talk has run; a talk starting. Red when the Slides input is on air, and where |
| **NEXT** | The talk **Start next talk** takes: the one after the talk on, unless another was picked — and whether it is **ready** (PowerPoint has the deck open, the page has loaded). **⏮ Previous talk** goes back to the talk before |
| **Running order** | Each talk: **▶** starts it now (or double-click its name), **⏶ ⏷** move it, **🗑** takes it out. Its name's right-click: **Start this next**, **Rename…**, **Replace file…** (a speaker's new version keeps its place and name), **Show in Finder**, **Remove** |
| **Between talks** | **Cut**, **Fade** or **Slide**, and how long |
| **+ Presenter view** | An input for the speaker's screen that follows each talk as it comes on (see [Presenter view](#presenter-view)) |

With more than one Slides input, a menu at the top picks which the panel
shows. Every change to the list is one undo step; starting a talk is live —
undo never takes a talk off.

The Slides input's tile has **◀ ▶** and **⏭** (start the next talk), and
says where it is: `2/6 · 3 / 22` — the second of six talks, slide 3 of 22.

### Starting a talk

**Start next talk** — or **⏭** on the tile, or `StartTalk` over the API —
starts the talk marked NEXT, on air too: **what is on stays on until the
new talk's picture is there**, then the input goes to it by its cut, fade or
slide. The room never sees black, a desktop or PowerPoint's window.

- A PowerPoint talk's deck is opened ahead (the next talk, and the one
  before the talk on), so starting it takes only its show: about half a
  second (measured on a Mac mini, a 39 MB deck: its show started and its
  first picture there in 0.44 s).
- A web talk's page is loaded ahead; a PDF's first page is drawn as it
  starts (50–90 ms a page of photographs).
- The talk before stays ready for questions: **⏮ Previous talk** goes back to
  it at the slide it was left on. A talk two away is closed.
- **The speaker's clicker never changes the talk**: past its last slide a
  talk stays on its end; only **Start next talk** takes the next.

### What follows the talk

- **A picture-in-picture**: another input showing the Slides input as a layer
  ([Another input as a layer](21-input-settings.md#another-input-as-a-layer))
  keeps its place, crop and border, whatever talk is on. A talk of another
  shape (4:3 in a 16:9 show) shows fitted inside the same picture, its sides
  clear.
- **The clicker**: PageDown, PageUp and B move the talk on air, whatever
  plays it — PowerPoint's show, a PDF's page, a web page. With another app
  in front too — Finder, to add a talk — once it is learned (see
  [The clicker with another app in front](#the-clicker-with-another-app-in-front)).
- **Black** and **White** go over the talk on, from Nida, whatever plays it.
- **A presenter view** of the Slides input shows the talk on: its slide and
  notes for a PowerPoint talk, its page — and the next page's picture — for a
  PDF; its talk clock starts again with each talk.

### PDF talks

Nida draws each page itself — macOS's own PDF engine, at the picture's size,
however big — so a page is sharp at 4K too, and **◀ ▶** or a clicker goes to
the next page at once (the pages either side are drawn ahead). No
animations or video: a PDF is still pages. A PDF locked with a password is
said so, not played.

### Web talks

An HTML presentation — a deck an AI made, reveal.js — played by the Mac's own
web engine (WebKit, as Safari plays it) in a window of Nida's that no screen
shows: at the picture's own size whatever the screens, its animations and
videos as Safari plays them.

- A page is started only once it has loaded and set itself up — its fonts
  loaded, a reveal.js deck ready — and gone back to the slide it was left
  on: until then the talk before stays on. A page not set up in 30 seconds
  says so (often what it loads from the internet cannot be reached); one
  whose script never lets go has its process ended, so it does not keep the
  Mac busy.
- **◀ ▶** and a clicker step a reveal.js deck by its own next and previous —
  through its builds and down its columns of slides; other pages are sent
  the arrow keys, as a keyboard would, and most HTML decks go on with them.
  A reveal.js deck shows where it is (**2 / 4**); others are moved on all
  the same.
- A page opened ahead has its sound and video paused; a page plays sound
  only after its first step on, as a browser waits for a click.
- **Web talks' sound to the mixer** (in the Slides input's Properties) takes
  their sound into the [audio mixer](16-audio-mixer.md): one strip for every
  Slides input — the pages' sound all comes from the one web engine — heard
  while a Slides input is on air (Audio Auto). With two Slides inputs, the
  one off air is not heard in it.
- The picture made another size (**Resolution**), a page open is laid out
  again at it, where it is.
- A page that loads its fonts or scripts from the internet needs the
  internet when it is shown.

### Checked before the show

Each talk's file is looked over as it is added, and again when the file
changes, for what goes wrong on the day and can still be put right: a talk
with something to check says **⚠ check** — pressed, what was found is said
under the talk till pressed again; on hover too. It plays all the same.

- **A PowerPoint deck**: a video or sound it links to (rather than holds) that
  was not copied with it; fonts this Mac does not have, which PowerPoint
  draws in others — a deck that carries its fonts is not looked at for them.
- **A talk of another shape** than the show, and how much of it the talk
  fills: a 4:3 deck in a 16:9 show fills 75 % of its width, with black bars
  at its sides; a PDF of A4 pages standing up (a document, not slides) 40 %.
  Nothing to put right when that will do; else ask for the talk in the
  show's shape.
- **A web page**: the internet it needs — the sites it loads scripts, styles
  and fonts from — and pictures or files of its own that are not beside it.

**Check again**, under the running order, looks every talk over again —
after copying a missing video beside its deck, or installing a font.

### Kept with the show

The running order, the talk on and where each talk was left are kept with
the show: opened again, the talk on is started again at its slide. A talk
taken out of the list while it is on stays on — and is kept with the show
so — until another is started.

### Limits

- A PowerPoint talk's show starts when the talk is started (its deck opened
  ahead): its first slide's own entrance animations play as it comes on,
  but the input changes about half a second after the press.
- A web talk stays drawing off screen only through a switch of WebKit's
  that Apple does not document; on a macOS without it the log says so and
  the page may stand still.
- A web talk's slide number shows only for a deck that says it (reveal.js);
  going to slide *n* of another deck steps there from its first, a key at a
  time (at most 300).
- A web talk's sound reaches the mixer only through **Web talks' sound to
  the mixer**; without it the page sounds from the Mac. With it, the first
  moment of a page's sound — before the Mac has handed it over — can still
  come from the Mac's own output, as with PowerPoint's sound.

## PowerPoint

A single PowerPoint deck can also be an input of its own, as vMix's
PowerPoint input. What follows holds for a Slides input's PowerPoint talks
too: how PowerPoint plays a deck, moves it on, plays its videos and which
screen its show plays on.

### Adding one

**Add Input → PowerPoint → Choose a deck…**: an input of its own, named
after its file (dropped from Finder, a deck is a talk of the Slides input
instead). As a layer, **Input Settings → + Add layer → PowerPoint**.

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

### Moving it on

- **◀ ▶** on its tile and on its column's header; in its layer's settings
  (**Properties**) **⏮ ◀ ▶ ⏭**, **Black** and **White**.
- **PageDown** and **PageUp** — what a presentation clicker sends — and **B**
  (a clicker's black-screen button, and back): a clicker plugged into this
  Mac moves the show **on air** (on Main or a bus), else the one moved last.
  Not while a text field is being typed into, and only when there is a
  PowerPoint input. Bind other keys with **Keyboard learn**. With another
  app in front, once the clicker is learned (see
  [The clicker with another app in front](#the-clicker-with-another-app-in-front)).
- **▶** is PowerPoint's own next: the next animation, or the next slide when
  the slide has none left, as a clicker does in PowerPoint.
- Past the last slide PowerPoint shows its black end screen; **▶** there does
  nothing more, so the input never goes blank in the middle of a show.

On its tile, **3 / 22**: the slide among those the show plays (hidden slides
left out), and **black**, **white** or **end** when it is. Its layer's
settings show the slide's **title** and **speaker notes**, read from the
file, and the **next** slide it plays.

### A video on a slide

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

### It stays up

Nida reads each show three times a second. A show ended in PowerPoint (Esc),
the deck closed there, or PowerPoint quit: Nida plays it again where it was,
a couple of seconds later. **Restart show** in its settings does it at once.

The deck is kept with the show, and the slide it was on: opening the show
opens the deck in PowerPoint again, at that slide. Removing the input ends
the show and closes the deck — without saving: Nida only changed how it
plays — if Nida opened it; a deck already open in PowerPoint stays open.

### Which screen it plays on

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

### The clicker with another app in front

A clicker's keys go to the app in front. With Nida in front they move the
talk; with Finder or a browser in front — a talk being added — they would
go there. So Nida can hear the clicker itself: **Learn clicker** in the
Presentations panel, then a button on the clicker. From then on that clicker
— only it, never the Mac's keyboard or another one, so typing a B or
scrolling with PageDown in another app moves nothing — moves the talk on air
whatever app is in front, with the keys **Keyboard learn** binds.

- macOS asks once to let Nida use **Input Monitoring**: allow Nida in System
  Settings → Privacy & Security → Input Monitoring, then quit and reopen
  Nida, and **Learn clicker** again.
- The clicker is this Mac's, kept for every show; **Forget** stops it.
- While Nida runs, the clicker's keys the show uses do nothing in other
  apps — a PageDown no longer scrolls the browser in front, nor steps
  PowerPoint's own window a second time, nor plays the alert sound in
  Finder, nor types into a document: Nida has macOS turn them into keys
  macOS gives no app at all (F21 to F24), for that clicker only, and reads
  the clicker's real keys itself — with Nida in front too, also while a text
  field is typed into. Lifted while **Keyboard learn** is on (so its keys
  can be learned), on **Forget**, and when Nida quits; put back when the
  clicker's receiver is plugged in again. Should Nida stop unexpectedly,
  unplug and plug in the receiver to give its keys back.
- Each press moves the talk once. In the moment the keys are being turned
  or given back (Nida starting, the clicker learned, **Keyboard learn** on
  or off, the receiver plugged in) a press can go unheard — press again.
- **B** stays B for the other apps — a letter is never turned, so a keyboard
  sharing the clicker's receiver still types: with another app in front the
  clicker's black-screen button also types a "b" there. With Nida in front
  it is taken as the keyboard's B: not while a text field is typed into.

### Presenter view

**+ Presenter view** in a PowerPoint input's settings makes an input for the
speaker's own screen — a confidence monitor, a TV at the stage — as
PowerPoint's presenter view does, from Nida:

- the slide on screen, **live**, its animations as they play;
- across the foot, **3 / 22** and how long the talk has run (the time of day
  too, when switched on);
- what comes **next**: the next slide's **picture** across the head of the
  column beside the slide, its number under it, where Nida has one — a PDF
  talk's next page, a PowerPoint talk's next slide once its slides are saved
  as pictures (see below) — else a line under the slide naming it;
- the slide's **speaker notes** in large type under that — smaller when
  there is much, cut with "…" when even that is too long. Not the slide's
  title: the slide shows it;
- over the slide **BLACK**, **WHITE** or **END** when that is what the room
  sees, or why the show is not running.

Put it on the screen the speaker sees like any input: on a bus shown on that
output. The talk's time starts when the show first plays; **Restart talk
clock** in the presenter view's settings — the **Source** tab of its Input
Settings, or **Properties** with its layer selected — puts it back to 0:00.
Removed, its PowerPoint input leaves it saying so.

**Slide pictures** in the Presentations panel — **Make** — has PowerPoint
save each PowerPoint talk's slides as pictures (a PDF in Nida's folder,
"Slide pictures"), so the presenter view shows a PowerPoint talk's next
slide too: as it looks once its builds are done. Make them before the show,
or between talks: with a PowerPoint talk on — or any slide show running in
PowerPoint, started there by hand too — they are not made, as the first time
PowerPoint asks for access to that folder — click **Grant Access** in
PowerPoint — and holds its show till that is answered. While they are made
("making…") PowerPoint is busy: a talk started meanwhile waits for the deck
being saved, and the decks not yet begun are left — made again with **Make**
later. "3 of 5 PowerPoint talks" says how many have them; a deck changed
since needs them made again. A save cut short leaves nothing behind.

Its settings also lay it out, kept with the show and undone like any
change:

- **Talk timer** and **Time of day** on or off, and **Clocks** at the **Top**
  or the **Bottom** (with the slide's number) — a new presenter view has the
  talk timer on, the time of day off and the clocks at the bottom, as does
  one in a show saved before whose layout was never changed;
- **Next slide** on or off — off, the slide takes its room;
- **Sizes**: **Slide** (its share of the screen; the notes have the rest),
  **Notes** (their type) and **Next** (the next slide's picture — as wide as
  the column at first, smaller to the left — or the line saying what comes
  next).

### Limits

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
- A show ended in PowerPoint itself (Esc there), or PowerPoint quit, is
  brought back by Nida at its slide in a second or two; **⏮ ◀ ▶** pressed
  meanwhile do nothing — press again once it is back.
- Old `.ppt` files play, without notes (notes are read from `.pptx`).
- The presenter view shows the next slide's picture for PDF talks, and for
  PowerPoint talks once their slides are saved as pictures; for a web talk
  or a PowerPoint input it names the next slide in a line.

## Over the API

### Slides

```
POST /api/inputs   {"source": {"kind": "slides", "paths": ["/Talks/keynote.pptx", "/Talks/results.pdf"]}}
POST /api/command  {"AddTalks": {"paths": ["/Talks/late.pptx", "/Talks/deck.html"]}}
POST /api/command  {"StartTalk": {"to": "next"}}
POST /api/command  {"StartTalk": {"to": "previous"}}
POST /api/command  {"StartTalk": {"deck_uuid": "…", "to": {"id": "a1b2c3d4"}}}
POST /api/command  {"SetNextTalk": {"deck_uuid": "…", "talk_id": "a1b2c3d4"}}
POST /api/command  {"EditTalk": {"deck_uuid": "…", "talk_id": "a1b2c3d4", "name": "Ona J. — results"}}
POST /api/command  {"EditTalk": {"deck_uuid": "…", "talk_id": "a1b2c3d4", "path": "/Talks/results-v2.pdf"}}
POST /api/command  {"MoveTalk": {"deck_uuid": "…", "talk_id": "a1b2c3d4", "to": 0}}
POST /api/command  {"RemoveTalk": {"deck_uuid": "…", "talk_id": "a1b2c3d4"}}
POST /api/command  {"SetSlidesSettings": {"deck_uuid": "…", "settings": {"transition": "fade", "transition_secs": 0.5}}}
POST /api/command  {"CheckTalks": {"deck_uuid": "…"}}
POST /api/command  {"MakeSlidePictures": {"deck_uuid": "…"}}
```

`AddTalks` without a `deck_uuid` adds to the show's first Slides input —
made on a new input, called Slides, when there is none — at `at` (a place in
the list, from 0) or the end; its reply is the Slides layer's UUID.
`StartTalk`'s `to` is `"next"`, `"previous"` or `{"id": "…"}`; without a
`deck_uuid`, the Slides input on air, else the one started last.
`SetNextTalk` with `"talk_id": null` puts NEXT back to the talk after the one
on. `SetSlidesTalks` (`deck_uuid`, `talks`) sets the whole list at once;
`AddSlidesDeck` (`channel_uuid`, `paths`) adds a Slides layer to an input.
`PresentationGo`, `SetPresentationScreen`, `TogglePresentationScreen`,
`RestartPresentation`, `PresentationVideo` and `SetPresentationPlaysOn`
take a Slides input too, and act on its talk on. `CheckTalks` looks the
talks' files over again (without a `deck_uuid`, every Slides input's).
`MakeSlidePictures` has PowerPoint save the PowerPoint talks without slide
pictures as pictures (without a `deck_uuid`, every Slides input's) — refused
while a PowerPoint talk is on; a talk's `pictures` says it has them.

A Slides layer's `slides` in `GET /api/state`: its `talks` in order — each
`id`, `name`, `given_name`, `path`, `kind` (`powerpoint`, `pdf`, `web`,
`clip`, `picture`), `state` (`waiting`, `ready`, `starting`, `live`,
`failed` with a `message`), `missing`, `position` and `shown` ("4 / 31"), a
PowerPoint talk's `presentation` (as below) and `pictures` (its slides saved
as pictures), a clip's `clip` (`at`, `length`, `playing`) and `warnings` —
what was found when its file was looked over, in words; `live`, `starting`,
`next` and `previous` (places in the list); `next_picked`; `on` — the talk
on as it plays, in the list or not; `screen` (`slide`, `black`, `white`);
`talk_secs`; `making_pictures` (PowerPoint talks being saved as pictures
now); and `settings` (`transition`, `transition_secs`, `plays_on`).

### PowerPoint inputs

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
