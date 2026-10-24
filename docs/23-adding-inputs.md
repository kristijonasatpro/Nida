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
| **PowerPoint** | A PowerPoint deck played by PowerPoint on this Mac, in a window Nida shows, moved on from Nida or a clicker (macOS, with PowerPoint installed) — see [PowerPoint](26-presentations.md) |
| **Another input** | As a layer: that input's picture inside this one, sized and placed like any layer (see [Another input as a layer](21-input-settings.md#another-input-as-a-layer)). As a new input: a copy of it, called "*name* (copy)", that can be sized and placed on its own — vMix's virtual input. Either way it is drawn once for both. An input that shows the one being added to is greyed out |
| **Audio Input** | A sound device on its own — a microphone, a mixer's output — as a strip in the [Audio Mixer](16-audio-mixer.md), with the pair of channels (or one channel, mono) it takes |

Files dropped from Finder still work as before: on a tile they go into that
input, on the empty tile into a new one.

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
starts it without opening anything, and turns to a green **■** to stop it.
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
already in use by another input stays at the format it was opened at.

**Audio** is the camera's own sound. It starts on the sound device named like
the camera — a capture card's usually is — or **None**. The sound becomes a
strip in the Audio Mixer named after the input, with **Audio Auto** on: heard
as the camera goes on air, off as it leaves. Removing the input removes it.

Rates are the device's own: an ATEM at 60p that says 59.9999 is listed as
60p and opened at exactly that, and 59.94 stays 59.94.

An input keeps **its** camera. Each device is known by the ID the computer
gives it, kept with the show, so plugging others in and out never switches an
input to another camera, and two of the same model each keep their own. A
camera that is unplugged, or stops sending pictures, is started again by
itself as soon as it is back — at the format chosen, or at its best when its
format was changed on the device meanwhile. Removing an input never waits on
a device that has stopped answering.

A camera added during a show starts in the background: its input is black
for the moment macOS takes to start it — about a second, longer for a device
that hangs — and the outputs carry on meanwhile. One that does not start is
said once; its input waits for it, tried every two seconds. A camera another
input already shows is there at once.

On a Mac cameras are read straight from macOS (AVFoundation), in the
device's own colour matrix: SD at BT.601, HD at BT.709. The app asks for
**Camera** access the first time it looks for cameras; if nothing is listed,
allow it in System Settings → Privacy & Security → Camera.

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

**Lowest latency** (in the camera layer's settings, under its colour) shows the
camera's newest picture every frame instead: about a frame sooner, but motion
judders whenever a picture comes late. A camera over USB (an ATEM's webcam
output) brings most pictures 27 ms after they are taken and a few in 40–50 ms,
so the even delay waits for those, and the lowest latency shows each of them a
frame late — about two judders a second. Every layer on that camera takes the
setting, it is kept with the show, and `{"SetCameraLowestLatency":
{"deck_uuid": "…", "on": true}}` to `POST /api/command` sets it too.

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
