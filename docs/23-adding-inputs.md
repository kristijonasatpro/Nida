# Adding Inputs

**Add Input** — the first button over the **Inputs** tiles — opens one window
for every kind of input, as vMix's does. Kinds down the left, what there is of
the kind on the right; **OK**, or a double-click on what is listed, adds it.

A new input is a channel of its own at the end of the row, called after what
it shows — the camera, the NDI source, the window. Type a **Name** to call it
something else; rename it later by double-clicking its name bar. One **Undo**
takes a new input away again.

The same window opens from a channel's **Input Settings → + Add layer**, and
then adds the source to that channel as a layer instead — a camera with a
lower third over it, say. It offers the same kinds, all but **Audio Input**.

| Kind | |
|---|---|
| **Video / Sound** | **Choose files…** — each clip or sound file picked becomes an input of its own, named after the file. Clips come in paused on their first frame (see [Input Settings](21-input-settings.md)) |
| **Image** | **Choose files…** — each picture an input of its own |
| **Playlist** | Clips, sound files and pictures played one after another in one input, as vMix's List — below. **Choose files…** makes it with them; **OK** makes an empty one to fill in its window |
| **Camera** | Cameras and capture cards, with their **Format** and **Audio** — below |
| **NDI** | NDI sources on the network — below |
| **Desktop Capture** | A whole display or one window, at a frame rate, with or without the mouse pointer |
| **Stream / SRT** | An SRT stream (**Listen** for the sender to call in, or **Call** out to it), an RTMP stream (**Listen** for OBS or an encoder to push to this computer, or **Pull** from a server), or an HLS address |
| **Web Browser** | A web page by its address (in builds with the web engine) |
| **Colour** | A solid colour. **Pick** beside it, then a click anywhere on the screen — a picture in Nida too — takes that colour (Esc cancels; macOS). A colour layer's colour can be changed later in its **Source** tab |
| **Generator** | A generator from the shader library, found by typing part of its name |
| **Timer / Clock** | A count down, a count to a time of day, a count up or the time of day, over a see-through background — below |
| **PowerPoint** | A PowerPoint deck played by PowerPoint on this Mac, in a window Nida shows, moved on from Nida or a clicker (macOS, with PowerPoint installed) — see [PowerPoint](26-presentations.md#powerpoint) |
| **Presentations** | A Slides input: a conference's talks — PowerPoint decks, PDFs, web pages, clips, pictures — in running order, each started with one press in the Presentations panel (macOS) — see [Presentations](26-presentations.md) |
| **Another input** | As a layer: that input's picture inside this one, sized and placed like any layer (see [Another input as a layer](21-input-settings.md#another-input-as-a-layer)). As a new input: a copy of it, called "*name* (copy)", that can be sized and placed on its own — vMix's virtual input. Either way it is drawn once for both. An input that shows the one being added to is greyed out |
| **Audio Input** | A sound device on its own — a microphone, a mixer's output — as a strip in the [Audio Mixer](16-audio-mixer.md), with the pair of channels (or one channel, mono) it takes |

Files dropped from Finder still work as before: on a tile they go into that
input, on the empty tile into a new one. PowerPoint decks, PDFs and web pages
are talks of the show's Slides input wherever they land (see
[Presentations](26-presentations.md#adding-talks)).

## Playlists

A playlist input plays its clips, sound files and pictures one after another:
each picture stays up for **Pictures stay** (5 s to start with), each clip
plays to its end, and each gives way to the next by the **Transition**:

- **Cut** — at once;
- **Fade** — one dissolves into the next;
- **Slide** — the next pushes the last off the screen, from the right, left,
  top or bottom.

**Transition time** is how long a fade or slide takes; with **Speed ramp** on
it starts slowly, speeds up and slows down into place, rather than moving at
one speed. A clip's transition starts that long before its end, so it ends as
the clip does. **Loop** starts again from the first item after the last;
without it the list stops on its last item, and **▶** plays it again from the
top.

**▶** on the input's picture — its tile, its column header, its Properties —
starts it without opening anything, and turns to a green **■** to stop it —
its sound fading out first, over 2 seconds unless its right-click chooses
**Cut** or another fade (see [Audio Mixer](16-audio-mixer.md)).
Beside it, the item on screen and the time before it moves on ("2 / 12 ·
0:04"). A new playlist starts stopped on its first item.

**☰** beside ▶ opens the playlist's window: the items in order, the one on
screen marked **▶**, with **⏶ ⏷** to move one and **🗑** to take it out;
**double-click** one to show it now (with its transition while playing, at
once while stopped). **⏮ ⏭** show the item before or after. **+ Add files…**
adds clips and pictures, and files or folders dropped from Finder on the
window go to the end of the list. The window also opens from the layer's
**Properties** (**Edit list…**).

The next item is opened ahead, while the one before it plays, so moving on
never waits on a file. A file that will not open (moved, deleted) is marked
**⚠** and skipped. Editing the list while it plays keeps what is on screen
there: the list plays on from it. A clip's sound comes through the playlist's
strip in the Audio Mixer, switching on as it goes on air like any clip's.

Over the API: `{"source": {"kind": "playlist", "items": [{"path":
"/show/intro.mp4"}, {"path": "/show/slide1.png"}]}}` to `POST /api/inputs`, then
`{"SetPlaylistPlaying": {"deck_uuid": …, "playing": true}}`,
`{"ShowPlaylistItem": {"deck_uuid": …, "index": 2}}`, `SetPlaylistItems` and
`SetPlaylistSettings` (`transition` `cut`/`fade`/`slide`, `slide_from`,
`speed_ramp`, `transition_secs`, `picture_secs`, `loop`) to `POST
/api/command`.

## Timers

A timer input shows one of:

- **Count down** — from a length (5:00 to start with), started and paused by
  hand;
- **Count to a time** — down to a time of day: "starts in";
- **Count up** — from 0:00, started and paused by hand: how long a talk has
  run;
- **Time of day** — the clock, as 14:05 or 2:05 PM, with or without seconds.

It is drawn over a see-through background, so as a layer over a camera only
the numbers show; as an input on its own a screen shows them over black.
Put it over another input with **Another input** (or add it there directly
with **+ Add layer**), and size and place that layer like any other.

Each timer has a **control panel** of its own, opened when the timer is made:
what it reads, big, **▶ Start**, **⏸ Pause** and **⏮ Reset**, **−1 min
+1 min −10 s +10 s**, and its **Length** — dragged, typed (5, 5:00 or
1:30:00), or one of 5 10 15 20 30 45 60 minutes in one click (back to the top,
ready to start). For a count to a time it holds the time instead. Under them
are its **Settings** — what it counts and how it looks, as in its window
below — so a timer changed often needs no window; fold them away with their
heading. The panel's tab is named after the timer's input; drag it beside any panel — the Program
monitor, the Switcher — and it stays there with the show's layout. **Layout ▸
Timers** opens and closes each timer's panel, as does **Control panel** in
the timer's settings and Properties.

**▶** (turning to a green **⏸**) on the input's picture — its tile, column
header, Properties — starts and pauses it too, and **⚙** beside it opens its
settings:

- **Counts** — which of the four; the length or the time; **At zero** — stop
  on 0:00, or count on over time as −0:12; **Warn** — the warning colour this
  many seconds before zero (60 to start with, 0 for never); **Shown as** —
  4:59 (hours when there are), 00:04:59 or 04:59; for the clock 24-hour or
  12-hour, and seconds.
- **Look** — any **font** installed on the computer (fonts added in Font Book
  are listed too), **Bold**, **Italic**; **Size** — the digits' height as a
  share of the picture's; **Place** — click one of the nine spots, kept **in**
  from the edges by a share of the height; **Colour** of the numbers, of the
  warning seconds and of zero and over time; an **Outline** round the numbers
  for reading them over a picture; a **Box** behind them — at 0 % none, and the
  background is see-through; a **Caption**, a line above ("Back in").

A count down stopped on zero and given a new length starts from its top. A
timer comes back with the show paused where it was; the clock and a count to
a time always run.

Over the API: `{"source": {"kind": "timer", "settings": {"mode": "countdown",
"duration_secs": 600}}}` to `POST /api/inputs` (`mode` `countdown`,
`count_to` with `target_secs` after midnight, `count_up`, `clock`), then
`{"ControlTimer": {"deck_uuid": …, "action": "start"}}` — `"pause"`,
`"reset"`, `{"adjust": {"secs": 60}}` — and `SetTimerSettings` to `POST
/api/command`. A deck's `timer` in `GET /api/state` has what it reads now
(`text`, `state` `normal`/`warn`/`over`, `remaining`).

## Cameras and capture cards

Anything the computer sees as a camera is listed: a webcam, a USB capture card
(Blackmagic, Magewell, Elgato), an ATEM's USB output. Plug it in and press
**Refresh**.

**Format** lists the sizes and frame rates the device offers. **Best** takes
the largest picture that moves smoothly — 25 frames a second or more — at its
fastest rate up to 60: a webcam's 1080p at 30 rather than its 4K at 5, a
capture card's 1080p at 60 rather than its 640×480 at 120. Choose one when the
source sends something else, a laptop at 1080p50 for instance. The format is
kept with the input and asked for again when the show is opened. A camera
already in use by another input stays at the format it was opened at; change
it under **Format** in its Input Settings (see [Input
Settings](21-input-settings.md#format)), which every input on it takes.

**Audio** is the camera's own sound. It starts on the sound device named like
the camera — a capture card's usually is — or **None**. The sound becomes a
strip in the Audio Mixer named after the input, with **Audio Auto** on: heard
as the camera goes on air, off as it leaves. Removing the input removes it.
A camera added as a layer of another input (a picture-in-picture) is offered
its sound the same way, switched with that layer. Either way it starts on
**None** when the mixer takes that device's sound already — the camera's own
input has it — so the sound is never there twice.

### Blackmagic inputs

A Blackmagic box or card that takes video in — an UltraStudio Recorder, a
DeckLink card — is listed twice when Desktop Video is installed: as macOS
lists it, and as **UltraStudio Recorder 3G (Blackmagic)**, read through
Blackmagic's own API. Read that way:

- It follows what comes in: a source switched from 1080p50 to 1080p59.94,
  or to 720p, is taken as it comes, without choosing its format again. Its
  **Format** is where it starts.
- Its sound comes with its pictures, two channels at 48 kHz: a strip
  appears in the Audio Mixer by itself, named after the input, with the
  rest of a live source's controls. No sound device is chosen for it.
- With nothing plugged into it, it shows black — not its last picture —
  and the log says there is no signal.
- A box with more than one input (HDMI and SDI) finds the one its source
  is on: with no signal for two seconds, it tries the next, round and
  round, and stays on the one with a signal. That is the box's own setting,
  as in Desktop Video Setup: it keeps it after Nida closes.
- It needs no Camera access from macOS.

Use one of the two for a box, not both.

Rates are the device's own: an ATEM at 60p that says 59.9999 is listed as
60p and opened at exactly that, and 59.94 stays 59.94.

An input keeps **its** camera. Each device is known by the ID the computer
gives it, kept with the show, so plugging others in and out never switches an
input to another camera, and two of the same model each keep their own. A
camera that is unplugged, or stops sending pictures, is started again by
itself as soon as it is back — at the format chosen, or at its best when its
format was changed on the device meanwhile. A camera that has been sending
pictures at its rate for a couple of seconds, then stops, is started again
after a quarter of a second without one (8 of its frames at slower rates;
as long as it takes to start, for a camera slow to start) — started again
it is usually back in a fraction of a second, and stays quick to start
again if it stops again soon. A camera just opened, or a source that sends
a picture only as it changes (a virtual camera, say — Nida notices one by
a long wait between its pictures), is given five seconds, as before. The
log says how long each stood still. Removing an input never waits on a
device that has stopped answering.

An ATEM switcher's webcam (it shows as "Blackmagic Design") keeps stopping
while **ATEM Setup** is open on the Mac: the ATEM stops putting pictures
into its USB stream until it is started again. Nida starts it again at
once, but each stop still shows as a hitch of about half a second. When a
camera keeps stopping while ATEM Setup is open, Nida names it and says to
quit ATEM Setup during the show; ATEM Software Control is fine.

A camera added during a show starts in the background: its input is black
for the moment macOS takes to start it — about a second, longer for a device
that hangs — and the outputs carry on meanwhile. One that does not start is
said once; its input waits for it, tried every two seconds. A camera another
input already shows is there at once.

On a Mac cameras are read straight from macOS (AVFoundation), in the
device's own colour matrix: SD at BT.601, HD at BT.709. The app asks for
**Camera** access the first time it looks for cameras; if nothing is listed,
allow it in System Settings → Privacy & Security → Camera. Nida's releases
are not signed by an Apple developer, so macOS asks again — for the camera,
the microphone and screen recording — after each update: open the new
version before the show and click Allow. While the show's cameras wait for
access, Nida says so on screen.

A camera's picture is read in the range and colour matrix the camera says it
sends. Some say one thing and send another: an ATEM's USB picture is tagged
limited range and BT.601 even in HD. Select the camera's layer and set
**Camera picture** → **Colour range** and **Colour matrix** by hand: when its
blacks look crushed and its highlights clipped next to the camera's own output
(its HDMI out, say), choose **Full (0–255)**; when it looks washed out,
**Limited (16–235)**; HD is usually **BT.709**. **Auto** goes back to what
the camera says. Every layer on that camera takes the setting — they share
one capture — and it is kept with the show and undone like any other change.
The log says once, per camera opened, what it sends and how it is read
(`Camera '…' frames: '420v' 1920×1080, matrix Bt601 (tagged ITU_R_601_4),
read as limited range`). The same is `{"SetCameraColour": {"deck_uuid":
"…", "colour": {"range": "full", "matrix": "bt709"}}}` to `POST /api/command`.

A camera set to a log or HLG picture sends a flat, grey image that keeps its
highlights for grading; **Camera picture** → **Picture profile** turns it into
ordinary Rec.709 video for the show, so it matches the other cameras. Set it to
what the camera is set to — Nida cannot tell:

- **Normal (Rec.709)** — the picture as it comes (the default).
- **Sony HLG** — HLG, HLG1, HLG2 or HLG3 (BT.2020 colours). Brought down to
  SDR as broadcasters do (ITU-R BT.2408): HDR reference white, 75% HLG,
  becomes SDR white.
- **Sony S-Log2 (S-Gamut)**, **Sony S-Log3 (S-Gamut3.Cine)** — Sony's usual
  pairing — and **Sony S-Log3 (S-Gamut3)**.
- **Blackmagic Film Gen 5** — Film on cameras with Blackmagic's Generation 5
  colour science: URSA Broadcast G2, Pocket Cinema Camera 4K and 6K, Cinema
  Camera 6K and URSA Mini Pro 12K, with current camera software.
- **LUT: …** — a LUT file (.cube or .3dl): one in the show's LUT folder
  (`.varda/luts/`, listed in the menu), or any file with **Load LUT…**. Use it
  for a camera whose curve comes only as a LUT — the first URSA Broadcast's
  Film, Generation 4 camera software — or for the manufacturer's own look
  (Sony's s709 or LC-709, Blackmagic's Film to Video). It is applied to the
  picture as the camera sends it, after **Colour range**.

The built-in profiles are Nida's neutral conversion: the camera's curve and
colours undone to the light of the scene, then made into video as a Rec.709
camera would — 18% grey at 41%, with a knee above 85% that bends the
highlights the log picture kept smoothly into the top instead of clipping
them. A manufacturer's LUT gives its own look instead. The conversion runs on
the GPU for each new frame of the camera, and every layer on the camera takes
it. The same is `{"SetCameraProfile": {"deck_uuid": "…", "profile": {"kind":
"sony_s_log3_cine"}}}` (or `{"kind": "lut", "file": "…"}`) to `POST
/api/command`.

A camera's frames come on its own clock, never exactly in step with the show,
and each arrives a little early or late. Each show frame shows the newest camera
frame captured a steady time before it — by the time macOS stamped it, not when
it happened to arrive — so someone moving in front of a camera at the show's
rate moves one frame per show frame; a camera a little slower or faster (59.94
against 60) repeats or skips one frame now and then, never two together. That
time is how late the camera's frames have come lately, at worst: a camera whose
frames always come in time is shown as soon as they come, no later than before;
only one whose frames sometimes come too late is shown a frame later, and that
comes back down once they have come in time again for ten seconds. A camera late
again within a minute of that, or two minutes of its last late frame, waits
twice as long the next time, up to ten minutes, so it is not shown a frame
sooner and later by turns; each minute it then keeps coming in time halves the
wait again. How far
behind its show frame each camera's picture was captured is in `GET
/api/state/performance` (`pacing.inputs[].behind_ms`), the camera's own delay
on top.

**Lowest latency** (in the camera layer's settings, under its colour) does not
wait for the few pictures that come much later than the rest: about a frame
sooner, but motion judders whenever one of those comes. A camera over USB (an
ATEM's webcam output) brings most pictures 27 ms after they are taken and a
few in 40–50 ms, so the even delay waits for those, and the lowest latency
shows each of them a frame late — about two judders a second. A camera whose
pictures keep arriving right as the show draws its frames — an UltraStudio
Mini Recorder did — would judder all the time that way, so there it holds a
frame, as without the setting, and stays smooth. Switching it takes effect at
once. Every layer on that camera takes the setting, it is kept with the show,
and `{"SetCameraLowestLatency": {"deck_uuid": "…", "on": true}}` to `POST
/api/command` sets it too.

macOS can change any camera's picture inside the app: **Portrait** (blurred
background), **Studio Light**, **Edge Light**, **Background** and
**Reactions** (hand gestures that draw balloons, hearts or fireworks into the
picture). Whatever is on goes to air with the camera. The app built with
`scripts/make-local-app.sh` starts with gesture reactions off; the others only
the user can switch, in the menu bar's green camera icon → Video Effects,
while the app has a camera open. While a camera is open Nida looks every few
seconds and says which are on, and Add Input's Camera tab names them with a
**Video Effects…** button that opens that menu. Center Stage (a camera that
pans and zooms after people by itself) is kept off. Check it is all off
before a show.

## Broadcast backgrounds

**Broadcast** lists calm, moving backgrounds for events, by mood, kept apart
from the Generators:

| Mood | Looks |
|---|---|
| Tech | Hex field, Node network, Circuit grid |
| Cyber | Data streaks |
| Warm | Sunset waves, Drifting bokeh |
| Calm | Aurora, Silk ribbons |
| Colours | Gradient, Vignette plate (a plain colour is **Colour**) |

Each look has the same few controls, in Input Settings (Gradient and
Vignette plate have their own: angle, style, edge darkening):

- **Speed** — 0 stands still, 1 is slow, 3 brisk, 10 fast. Changing it never
  makes the picture jump.
- **Density** — how much of it there is: discs, cells, streaks, curtains.
  It costs the GPU the same however much.
- **Calm centre** — how much the middle and the lower third go toward a flat,
  dim plate of the background, so a slide or a name over it stays easy to
  read; every look calms alike (at 1, a third to three quarters dimmer there),
  never darker than LED panels take.
- **Palette** — twelve sets of three colours (Background, Main, Accent):
  Corporate Blue, Graphite Teal and Violet Tech for tech events; Ice, Neon
  and Terminal for cyber; Gala Gold, Sunset and Rosé for warm; Nordic Aurora,
  Lagoon and Lavender Mist for calm. **Custom** takes the three colours
  below it: the background as picked (only kept off black), and lines, discs
  and streaks in the Main and Accent picked — dimmer, but their own colour,
  not mixed with the background (Vignette plate's plate is the Background).
  Picking a colour while a preset is chosen switches to Custom by itself,
  the other two colours starting as that preset's. Any look takes any palette: Node network in Ice reads as cyber.

They are made for LED walls and streams alike: any size and shape — a
32:9 wall, a 4:1 strip — nothing thinner than a few pixels, nothing that
flashes, never a large area near black (patchy on LED panels), dithered so
dark gradients do not band, and their motion stays as smooth hours into an
event as at its start: about every 11 hours at Speed 1 (about every hour at
10) a look moves on to a fresh picture in one cut, the price of that smoothness (Gradient and
Vignette plate, when they move, loop exactly and seamlessly). Each costs
the GPU little at 4K.

A background is added as an input named after its look ("Hex field"), or
dragged from **Library → 📺 Broadcast** onto a channel. Over the API it is a
generator: `{"source": {"kind": "generator", "shader_name":
"broadcast_hex_glow"}}`; `GET /api/state/registry` lists them under
`broadcast`, with their titles and moods.

## Text

**Text** adds lines of text — a break ("PERTRAUKA / Tęsiame 14:00"), a room,
the next session — drawn over a see-through background, so over a camera or
a background only the text shows. Type the first lines in the Add window, or
just add it ("Text") and type in its **Input Settings**:

- **The text** — as many lines as you like (Enter starts one). It is
  edited live: each letter goes on air at once, the text before staying up
  until the new is drawn, so fixing a typo or a time during the event never
  blinks. A run of typing is one Undo step.
- **Shown** — off fades the input clear (and on, back in) over **fade**
  seconds; 0 cuts.
- **Show on** — where it comes on, each a toggle: **Main** (in Main mode:
  its input an overlay over Main, switched on), and buses — over whatever
  each shows, in the part each one's screens show (an LED wall's band),
  laid out for that part. Shown fades it on and off there too. **Send to**
  is different: it puts the input on a bus *in place of* what it shows.
- **Look** — any installed **font** (⚠ if it lacks Lithuanian letters),
  **Bold**, **Italic**; **Size** as a share of the picture's height;
  **Colour**; the **lines** left, centred or right, and their **spacing**;
  **Place** — one of nine spots, kept **in** from the edges; a **Box**
  behind the text (how solid, its colour, its **corners** and the **room**
  inside it); an **Outline** round the letters and a soft **Shadow** under
  them, for reading over a picture.

Size and place it further like any input, with its Position and Crop. It
costs the GPU next to nothing: drawn once per change, placed each frame.

Over the API: `{"source": {"kind": "text", "settings": {"text":
"PERTRAUKA\nTęsiame 14:00"}}}` to `POST /api/inputs`, then
`{"SetTextSettings": {"deck_uuid": …, "settings": {…}}}` to `POST
/api/command` (the whole settings: `text`, `visible`, `fade_secs`, `look`,
and `targets` — the bus UUIDs it shows on). A deck's `text` in `GET
/api/state` has them. Its `pending` flag says a current drawing is still
being prepared for the input or a bus. If a drawing fails, `error` explains
that any previous picture is retained; the same message appears in Input
Settings. A successful redraw clears the error.

## Lower thirds

**Lower third** adds a names input: a person's name, title and a second line
(an organisation) on a plate, brought on air one at a time. Like a timer it is
drawn over a see-through background: put it over a camera with **Another
input** or **+ Add layer**, or key it over Program like any input with
transparency.

**▶** on its picture (tile, column, Properties) brings the **cued** name on
air and cues the one after it; it turns into a red **■**, which takes it off.
**⏭** beside it cues the next name without showing it, and **Next:** names the
one ▶ will show. Showing another name while one is on air takes the first
off, then brings the other on.

Its **Input Settings** hold the rest — and the **Lower thirds** panel
(**Layout → Panels → Lower thirds**) holds every lower third of the show in one place, by input:
on air, its names each shown with one press, Show, Hide and Next, and folded
away its list file and look. Put it beside the Switcher for the day's names.

- **Show on** — where it comes on, each a toggle: **Main** (in Main mode:
  its input an overlay over Main, switched on), and buses — over whatever
  the bus shows (its dissolve too), in the bus's **names area** — the part of
  it its LED screens show (the band a wall shows of a 16:9 picture), else
  the whole bus — and laid out for that area's shape, so on an LED wall's
  screen the name sits inside what the wall shows. Several buses at once
  come and go together. For a bus showing Main use Main or the bus, not
  both, or the name shows twice there. A bus
  with no name live costs nothing extra.
- **Names** — a list with the one on air marked red ● and the cued one ▸;
  **▶** on a row shows that one now. Type them in (**+ Add a name**, **×**
  removes one), or **Read a list…**: a CSV or TSV file — an Excel sheet saved
  as CSV works, including the Windows-1257 kind Lithuanian Excel writes with
  `;`, so ą č ę ė į š ų ū ž come through. A header row names the columns, in
  English or Lithuanian (*Name / Vardas, pavardė / Pranešėjas*, *Title /
  Pareigos*, *Organisation / Organizacija / Įmonė*); without one the columns
  are name, title, second line. The file is **read again whenever it is
  saved**, so the organiser can fix a name in Excel during the event and the
  next Show has it, the cue staying on the same person when rows move. A file
  that will not read (saved empty, moved, a drive gone) keeps the names before,
  says why with ⚠, and is tried again every two seconds. Names read from a
  file are edited in the file, and Undo never takes a followed list away
  (it can go back to a list chosen before); **Stop following** keeps them and lets you edit
  them here.
- **Style** — **Plate** (a plate with its bar), **Two-tone** (the name on a
  band of the bar's colour, the lines under it on the plate's), **Ribbon**
  (slanted ends), **Pill** (round ends), **Text only** (no plate, a soft
  shadow under the letters), or **Picture as plate**: **Choose picture…** takes a
  PNG or JPEG with the event's design as the plate — kept to its own shape,
  made wider only for a name that needs it. A picture the size of a frame (16:9, 1920 or more across) offers **Use Full-frame picture**: it was made for the full frame. **Text after the plate** brings
  the plate in first, then the name and lines (on by itself for Picture
  as plate); out, the other way round.
- **Full-frame picture** — your design over the whole frame, exactly as
  drawn: make a PNG at the show's size (1920×1080 or 3840×2160), clear but
  for your plate, and **Choose picture…**. It is scaled only to the frame,
  kept in shape, so the plate lands where you drew it and a long name never
  stretches it. **Picture** — its size (× 1 as drawn) and how far it is
  moved; **Text box** — where the name and lines go over it (left, top,
  width, height, as shares of the frame) and how they line up, kept a
  little in from its edges: a line too long for the width is made smaller,
  to three quarters, then wrapped (smaller still if its lines must, to fit
  the height), the other lines keeping the size you set. **Picture
  in** and **Text in** — each its own motion (cut, fade, slide, wipe), time
  and delay: the picture wipes in, say, then the text fades in at 0.3 s;
  out, the same backwards. On a bus (Show on) the whole design is fitted
  inside the part its screens show, at its foot.
- **Look** — **Colours**: the twelve palettes of the [Broadcast
  backgrounds](#broadcast-backgrounds), so names match the background, or
  **Custom** (plate, bar, name, title); **Plate** — how solid, 0 % is text
  alone; **Bar** — an accent bar on the plate's edge, along its top, under the
  name, or none; **Place** — lower left, lower centre, lower right, upper left,
  kept inside the **Safe area** (5 % in from the edges, as on TV; off: 2 %);
  **Size** — the name's height as a share of the picture's (the title is
  smaller); **Font** — any installed font; ⚠ beside it means the font lacks
  some Lithuanian letters, which would show as boxes; **Motion** — **Cut**,
  **Fade**, **Slide** in from its side, or **Wipe** (the plate opens from its
  edge, then the text fades in), and how long it takes; **Off after** — taken
  off by itself after so many seconds, or *stays on*.

Fix the row on air itself — a typo in its name or title, typed in the list
or sent with `SetLowerThirdSettings` — and it changes on air at once, in
place, with no going out and coming in. Otherwise a name on air stays as it
went on until it is taken off, even if rows are added, moved or removed
meanwhile or the list file is read again; a change of look shows at once. A long name
is made smaller to fit, down to six tenths of its size. The
pictures are drawn only when a name or the look changes, so a lower third costs
the GPU next to nothing while it moves. It comes back with the show off air,
its names, cue and look as they were.

Over the API: `{"source": {"kind": "lower_third"}}` to `POST /api/inputs`,
then to `POST /api/command` — `{"LowerThirdShow": {"deck_uuid": …}}` (add
`"row": 2` for a given one, counting from 0), `{"LowerThirdHide": {"deck_uuid":
…}}`, `{"LowerThirdStep": {"deck_uuid": …, "by": 1}}`, `{"ImportLowerThirdRows":
{"deck_uuid": …, "path": "/Users/…/names.csv"}}` and `SetLowerThirdSettings`
for the names, look and `targets` (the bus UUIDs it shows on). A deck's `lower_third` in `GET /api/state` has them,
with `live` (the name on air, if any), `on_air` (where it is in the list) and
`problem`.

## NDI

NDI needs the free **NDI runtime** on the computer: on a Mac, install NDI
Tools or the NDI SDK from ndi.video (or `brew install --cask libndi`); on
Linux, the NDI SDK's `libndi.so`. Without it the NDI list says so. Start the
app again after installing.

The list shows the sources on the network as they announce themselves —
within a second or two, and it keeps up while the window is open. Looking
never holds up the picture.

An NDI input brings its **sound** too: a strip in the Audio Mixer named after
the source, with Audio Auto on, like a clip's. See [Audio Mixer](16-audio-mixer.md).
Its picture keeps its own shape (4:3, portrait, 16:10 are not stretched), and
a keyed graphic sent with transparency shows what is under it.

### Sending NDI

**Setup → Outputs → + Stream → NDI** sends Main, or a bus (its **Source**), to
the network under the name you give it. An NDI output carries an audio bus —
Master when it is made; change it under **Audio**, even while it runs, or
choose **None (silent)**. Pictures go out at video levels in BT.709 (BT.601
below 720 lines), as NDI receivers such as vMix expect; the 10-bit option
sends P216.

## Desktop capture

Lists the displays and the windows open now; **Refresh** looks again. On a
Mac, the first capture asks for **Screen Recording** access (System Settings →
Privacy & Security → Screen Recording), and the app has to be started again
after it is granted. Capturing a display leaves the app's own windows out.

## Over the API

`POST /api/inputs` adds an input:

```sh
# The capture card at 1080p50, with its own sound
curl -X POST localhost:8080/api/inputs -H 'Content-Type: application/json' -d '{
  "source": {"kind": "camera", "camera_id": 1,
             "mode": {"width": 1920, "height": 1080, "fps": 50}},
  "name": "Stage cam",
  "audio": {"device": "Blackmagic Design", "left": 0, "right": 1}
}'

# An NDI source
curl -X POST localhost:8080/api/inputs -H 'Content-Type: application/json' \
  -d '{"source": {"kind": "ndi", "source_name": "LAPTOP (Screen 1)"}}'
```

Other kinds: `{"kind": "screen_capture", "target": {"kind": "display", "name":
"Display 1"}}`, `{"kind": "colour", "color": [0, 0, 0, 1]}`, `{"kind": "srt",
"url": …, "mode": "Listener"}`, `{"kind": "rtmp", "url": …, "mode": "Listen"}`,
`{"kind": "hls", "url": …}`, `{"kind": "html", "url": …}`, `{"kind":
"generator", "shader_name": "checkerboard"}`, `{"kind": "input",
"channel_uuid": …}` (a copy of that input). `PUT /api/decks/{uuid}/color`
with `{"color": [r, g, b, a]}` changes a colour layer's colour. Camera ids and
formats come from `GET /api/state/cameras`, NDI names from
`POST /api/devices/ndi/scan` then `GET /api/state/ndi`.
