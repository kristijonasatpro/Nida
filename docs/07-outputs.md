# Outputs

An **output** renders its assigned surfaces (or the master mix directly) onto a destination such as a window, a fullscreen display, a network stream, or a recording file. Nida supports many simultaneous, independent outputs. This page covers creating outputs, choosing where they draw from, rotation, and multi-output recording. For projector alignment (surfaces, warp, edge blending) see [Projection Mapping](08-projection.md); for stream protocol details see [Streaming & I/O](09-streaming-and-io.md).

## Creating an Output

The output panel (right side) has three buttons:

- **+ Windowed** — a floating, resizable output window
- **+ Recording** — a headless recording output (defaults to H.264 → `output.mp4`)
- **+ Stream** — a headless network sender (defaults to NDI)
- **+ *device name*** — one per Blackmagic device connected, e.g. **+ UltraStudio Monitor 3G**: see [Blackmagic Outputs](#blackmagic-outputs)

## Output Targets

Any output can be retargeted after creation:

| Target | Notes |
|--------|-------|
| **Windowed** | Floating OS window |
| **Display (Fullscreen)** | Borderless fullscreen on a chosen monitor |
| **Recording** | Video file via ffmpeg. For recording an event, use the top bar's **REC** button instead — see [Recording](19-recording.md) |
| **SRT / HLS / DASH / RTMP** | Network streams (video-only). For streaming an event to platforms, use the top bar's **STREAM** button instead — see [Streaming](17-streaming.md) |
| **NDI** | Network Device Interface sender |
| **Syphon** | macOS inter-app texture sharing |
| **Blackmagic** | A Blackmagic DeckLink card or UltraStudio box: its HDMI and SDI — see [Blackmagic Outputs](#blackmagic-outputs) |

### Display Selection

A windowed output has a **Display:** dropdown listing **Windowed** plus every connected monitor, formatted `Name (W×H)` — e.g. `Built-in Retina Display (1920x1200)`. Selecting a monitor sends the window borderless-fullscreen to it. The mouse pointer is hidden while it is over an output on a monitor, so a stray move of the mouse never lands on the picture; it shows again as soon as it leaves.

Monitors are re-enumerated every frame, so hot-plugging works without restart. If an output is configured for a monitor that isn't connected at startup, it falls back to a window and shows a notice (`Monitor '<name>' not connected — output '<name>' opened as a window and moves there when it connects`), keeps that monitor as its saved target, and goes fullscreen on it by itself once it appears.

### Naming Screens

macOS names an external display by its model number, so the pickers would list `Monitor #0` —
and two identical displays (two inputs of one switcher, say) the same, told apart only by their
place (`Monitor #0 @ 1728,0`). So each screen is listed by what macOS calls it ("Color LCD",
"BMD HDMI"), with its size and refresh rate, and you can give each one a name of your own.

**Outputs → Screens…** lists the screens connected. Type a name for one ("Stage LED", "ATEM in
2") and press Enter; empty the field to take it away. The name then shows wherever screens are
listed — a bus card's **Screen:**, an output's **Display:**, the LED feeds' **+ New feed**.
Names are kept on this computer, for any show: the screens belong to the desk. Each is kept by
the display's own identity (its UUID, which macOS keeps), or — for two identical displays — that
and its place on the desktop.

**Identify** puts each screen's name big on it for a few seconds, so you can see which is which
before naming them.

### Staying Awake

For as long as Nida runs it keeps the computer and its displays awake, as a
switcher or a full-screen slideshow does: no system sleep, no display sleep, no
screensaver or lock. A sleeping display is an HDMI port with no signal, so
without this an idle machine blacked out the LED wall and TV mid-talk. On macOS
this is a user-initiated activity (it also stops App Nap throttling rendering
when Nida's windows are covered) listed in `pmset -g assertions`; on Linux a
`systemd-inhibit` idle + sleep lock listed in `systemd-inhibit --list`. It is
released when Nida quits.

An output opened while its display sleeps waits as a window and moves onto the
display as soon as it wakes.

### When macOS stops showing an output

An output on a monitor stays on it whichever app you switch to, Stage Manager
on or off: its window joins every Space and every app's set of windows. Should
macOS still stop showing it — on a MacBook with Stage Manager on, an app
switch once left the LED output taking every frame and showing none, frozen
until it was sent to its screen again — Nida notices within a second (frames
presented, none of them shown) and makes the output's window again from its
own settings, in its place: a blink instead of a frozen picture. The log says
`Output '<name>': its screen has shown none of its frames …`, and a notice
says so once. If it keeps happening, Nida waits longer each time before
making the window again (1, 2, 4 … 60 s). `{"ReopenOutput": {"output_uuid":
"…"}}` to `POST /api/command` makes an output's window again on request.

Each output window's frames go on its screen from a thread of the window's
own, so a screen that holds Nida's frames up — macOS did for a second at
every app switch with Stage Manager on — costs that output those frames and
nothing else: the other outputs, the control window, the recording and the
stream keep going.

## SDR Precision

Every output has an **SDR precision** choice:

- **8-bit SDR** is the compatibility default.
- **10-bit SDR** requests higher output precision without changing gamut, brightness range, or tonemapping.

The **Dither** checkbox is enabled by default. It applies a stable, destination-aware pattern before
the final integer conversion, which reduces visible banding without temporal noise.

The output card always reports what is actually being delivered. A 10-bit request uses RGB10 on a
display only when the GPU surface exposes both RGB10A2 and an sRGB color-space contract. Otherwise
the output continues in 8-bit and shows the reason. This status reports Nida's configured signal
format, not unverified monitor panel depth or cable behavior.

Recording and network outputs apply the same request to their codec or protocol. See
[10-bit delivery](09-streaming-and-io.md#10-bit-sdr-delivery) for the supported combinations.

## Levels

A screen output — a window, a monitor, an LED feed — has a **Levels:** choice:

- **Full (0–255)**, the default: black at code 0, white at 255, as a computer monitor takes it.
- **Video (16–235)**: black at 16, white at 235 (64 and 940 at 10 bits), for gear that reads
  video levels — some LED processors, a monitor set to limited range. Sent full levels, such
  gear crushes the blacks and clips the whites; the picture looks harder, as if through a LUT.

Nothing picks it for you: choose it per output, for the gear that output feeds — what one
desk needs, the next may not. It is saved with the output. Recordings, streams and NDI are
video levels already and have no choice; a Blackmagic output has its own (see
[Blackmagic Outputs](#blackmagic-outputs)).

To check it, set the output's **Calibration** to **Projector**: the test card's foot has four
patches, codes 0, 16, 235 and 255, each labelled. On gear that reads video levels, at Full the
first two look the same black and the last two the same white; at Video all four are apart.
On gear that reads full levels, Full shows all four apart.

### The test card

**Calibration → Projector** shows a test card over the whole output, made at the output's own
pixels — so its grid cells stay square and its circle round on any screen shape, as a media
server's card does: a grid of square cells about the centre, corner-to-corner diagonals, a
circle as high as the card, a centre cross, the output's name and size in the middle, colour
bars along the top and grey steps along the foot, each bar apart from the next, the levels
patches beside them, and a white edge with the output's colour inside it.

## Output Rotation

Each output has a **Rotation:** control with four options — **0°**, **90°**, **180°**, **270°** — applied at the final blit. Use 90°/270° for portrait projectors/displays; those values swap width/height for the effective render. Rotation is GPU-only (no mesh recompute).

## Source Routing

Outputs draw the **surfaces** assigned to them, and each surface pulls from a content source chosen in the Stage Editor (**Source:** picker). The options:

| Source | What it is |
|--------|-----------|
| **Master** | Final composited output (all channels + master effects) |
| **Channel** | A single channel's post-FX output |
| **Channels** | A sub-mix of several channels (see below) |
| **Deck** | A single deck's raw output (pre-blend, pre-FX) |
| **Domemaster** | Fisheye equidistant-azimuthal projection |
| **Bus program** | Whatever a production bus has on air (see [Buses and Routing](#buses-and-routing)) |

To assign surfaces to an output, use **+ Assign Surface**; remove one with **x**, or reset its warp with **↺**.

### The "Channels" Sub-Mix

Selecting multiple channels (via checkboxes in the source picker) creates a **Channels** sub-mix: those channels are composited together using each channel's own **opacity and blend mode**, exactly as in the master compositor — but **master effects are not applied**. This lets a surface show, say, only channels 0+1 with a live crossfade between them while another surface shows the full master. Selecting a single channel collapses to **Channel**; selecting none falls back to **Master**. Sub-mix textures are cached per unique channel set and reused across frames.

## Buses and Routing

The **Switcher** panel has one card per production bus —
for example PGM, STREAM, REC, LED, TV. Each bus is like one of vMix's Outputs:
it has its own Preview and Program, its own CUT/AUTO, and it can take any
source on its own:

- **Black**, **Master** (the main output, every layer composited) or any
  **channel** — the card's buttons. Clicking one stages it on the bus's
  Preview; CUT or AUTO takes it to air. A single input (deck) is not a
  button, so a show of many inputs stays readable; send one to a bus from
  its right-click menu, below.
- **Right-click** a deck card, a channel header or the **Main Output** preview
  for **Cut to / Fade to** every bus — the quick way to send one source to one
  screen without touching the others.
- With a channel or one of its decks selected, the bottom bar's **Send to**
  column, beside the channel's preview, has the same **Cut** and **Fade** for
  every bus as buttons.

A new bus shows **Main** — Master on its Preview and Program — until you take
something else to it, so a bus sent to a screen shows the show straight away.
A new show has one, **PGM**, covering the whole picture at the show's size.
(**+ Bus** in the Switcher adds more; an LED screen's **New bus for this
screen** too.) When a show opens only in part and its buses are held black, a
bus added then is held black with them.

**Double-click** a bus's name, or right-click it for **Rename bus…**, to rename
it; **Remove bus** is in the same menu. The bus's own screen — the output and
surface its **Screen:** picker made, named after it — takes the new name too,
unless you have renamed that output yourself.

**Screens** follow a bus through the card's **Screen:** picker. It lists the
monitors and each connected Blackmagic device — **UltraStudio Monitor 3G
(Blackmagic)**, say: chosen, the bus goes out on that device's HDMI and SDI
(see [Blackmagic Outputs](#blackmagic-outputs)). A bus is on one of them at a
time: choosing another moves it, and **Off** takes it off. **Recording,
stream, NDI and Syphon outputs** follow one through their **Source:** picker
in the Outputs panel — **Master** by default, or any bus. You can change it
while recording or streaming; the size of the picture stays the same. An
output that follows a bus sends only that bus's region of the master, as a
screen does.

### Leaving a layer off one bus

A bus showing **Master** can leave channels out: open the card's
**Master: all** menu and untick them (it then reads, say, **Master: 1 left
out**). It only changes what **Master** is on that bus — a channel button
shows its channel whatever is ticked. Only that bus changes — the main output and every other
bus still show those layers. vMix does the same thing with the overlay
checkboxes on each output.

The usual use is a clean recording: put the lower third or logo in a channel
of its own, then

1. set both the **STREAM** and **REC** buses to **Master**,
2. untick the graphics channel in **REC**'s **Master** menu,
3. set the stream output's **Source:** to STREAM and the recording's to REC.

The stream gets the graphics and the recording does not. To record something
else altogether — a camera, a slide deck — put that source on REC instead.

A bus that hides layers draws the layers it still shows with the same
opacities and crossfade as the master, but without **master effects**. It
costs one extra composite per frame at the master resolution. Hidden layers
only matter while the bus shows Master; a bus on a single channel or deck
shows it whole.

### LED screens

Several LED screens driven by one processor through one cable — a wall and two
cubes — go out as one **LED feed**: each screen at its own pixels of the
signal, showing Main or a bus of its own. See [LED Screens](22-led-screens.md).

### Several buses at once

Each bus is taken by itself in the Switcher. To take several together — every
part of a wall with one cut or one fade — make the moment a step in the
[Rundown](25-rundown.md).

## Blackmagic Outputs

A Blackmagic DeckLink card or UltraStudio box — an **UltraStudio Monitor 3G** on
Thunderbolt, say — is one more screen for the show, beside the two displays a
MacBook Pro drives itself. Its picture goes out on its HDMI and SDI at once.

- **Setting up.** Install Blackmagic's **Desktop Video** and allow its system
  extensions (System Settings → General → Login Items & Extensions; macOS asks
  for each one in turn). Without Desktop Video there are no Blackmagic devices
  in Nida, and nothing else changes.
- **Adding one.** Choose the device in a bus card's **Screen:** picker to send
  that bus to it — the device's output is then the bus's, named after it, and
  starts at once (renamed, the bus renames it). Or use the device's own **+**
  button in the output panel, then pick its **Source:**. A new output's
  format is **Auto**: 1080p at the show's rate (1080p60 for a 60 fps show),
  or the nearest 1080p rate the device has. A device shows one output at a
  time.
- **Levels.** **Video (16–235)**, the default, as video gear takes it; or **Full (0–255)**,
  for a display set to take full levels (its HDMI black level at Full or High). Changed
  while the output runs, it is taken up at the next frame. **Test card** puts the test card
  on the device in place of the picture, to check the levels there.
- **Format.** **Auto (1080p60)** follows the show: the format for the show's
  rate as the output starts, chosen again when the show's rate changes — a
  running output is started again in it, a moment of black on the device.
  While the output is stopped, **Format:** also lists the device's formats,
  to pick one by hand; it then stays, whatever the show's rate. A format at
  another rate than the show's says what it does — **⚠ 1080p50 on a 60 fps
  show: 10 of every 60 frames are left out** — as the box takes frames at
  its own rate. An output saved before **Auto** follows the show. Its frame
  is the format's size; the picture — Main, or a bus — is fitted into it
  whole, its shape kept, with black around when the shapes differ. It is
  never stretched.
- **▶ Start / ⏹ Stop.** Start opens the device and puts the show on it; Stop
  lets it go. Both happen away from the show's frames, so the other screens
  never wait for the device. A device another output is using says so. An
  output you started stays started until you stop it: it is saved running,
  starts again when the show opens, as a screen comes back on, and — its
  device missing then, or unplugged meanwhile — starts by itself once the
  device is plugged in. Every stop is in the log, with why.
- **No queue.** Each frame goes to the device as soon as it is ready, and the
  device shows it at its next frame: no added delay. The device keeps its own
  clock, so now and then a frame is shown twice or skipped, as on any screen
  not clocked by the show.
- **Keeping up.** An UltraStudio Monitor 3G takes some 12 ms to receive a
  1080p frame. Each frame is turned into the device's 8-bit YUV (by Apple's
  Accelerate, about a millisecond) while the frame before is still going to
  the device, so the output keeps up with 60 a second. Both run in real
  time, as macOS runs sound, so that another app in front — or the screen
  locked — does not slow them. Every 10 seconds the log says how it fares:
  frames put up and let go, how evenly the show handed them over and how
  long they waited for the device, how long converting and the device took
  and how much of that was work on a CPU, and the frame time of the
  device's own clock. Frames let go or handed over late with the GPU busy
  (a 4K show on a laptop): **Latency → Normal** in the frame rate menu, or a
  lower resolution.
- **Unplugged.** A device that stops taking pictures for half a second, or
  stops answering for a second — unplugged, say — stops its output with a
  notice; **⚠ *device* is not connected** shows on an output whose device is
  missing. Plugged in again, the output starts by itself. A device that
  stopped answering is used again only once it has been plugged in again.
- **Devices are seen as they come and go.** Nida asks Desktop Video once to
  say when a device arrives or goes, and keeps each device it is told of: a
  box plugged in shows within a second, and nothing is asked of the driver
  while outputs run. A device there all along that will not take
  pictures is not tried again and again: press Start, or unplug and plug it
  in.
- **Sound.** Not yet: a Blackmagic output carries picture only.

## Recording Outputs

Click **+ Recording** to add a recording output; each runs its own ffmpeg subprocess, so several feeds can record at once. Codecs, file-path rules, and the **▶ Start / ⏹ Stop** controls are covered in [Recording](09-streaming-and-io.md#recording).

> **Sound.** Recording and all streaming targets carry an audio bus from the [Audio Mixer](16-audio-mixer.md) — **Bus: Master** by default — or a capture device directly, chosen in the output's **Audio:** menu. See also [Audio Passthrough](09-streaming-and-io.md#audio-passthrough).

## Persistence

Outputs, their targets, rotation, SDR precision request, dithering preference, surface assignments,
the bus a recording or stream follows, each bus's hidden layers, and warp calibration are saved in
`stage.json` (the venue layout), separate from your scene/show.
Existing stages default to 8-bit SDR with dithering enabled. See
[Persistence](02-concepts.md#persistence).

---

[← Prev: Control Surfaces & Macros](06-control-surfaces.md) · [Home](README.md) · [Next: Projection Mapping →](08-projection.md)
