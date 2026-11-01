# Audio Mixer

## Reading the meters

Input and output strip meters use a **dBFS** scale, separate from fader gain.
The numbered scale includes a highlighted **−6 dBFS** mark. Green indicates
levels below −12 dBFS, yellow the remaining headroom; red is a **0 dBFS or
higher** warning, not a −6 dBFS warning. These are sample peaks, not true peak
or perceived loudness (LUFS).

Stereo bars show decaying post-fader levels. White markers and the **Peak**
number below each meter hold the highest level since reset (the number is the
higher of L/R). Hover for separate L/R values; click the meter to reset its
peak markers and full-scale warning without changing audio. Peaks are captured
on the audio thread, so a transient between UI refreshes is not lost. They are
session telemetry, not saved show settings. Source replacement/show load clears
them; ordinary fader edits do not.

**Playing but not heard.** An input's meter still moves when nobody hears it,
showing what it plays — before its fader — in a colour that says why:

| | |
|---|---|
| **Blue** | Sent to no bus: none of **M**, **A**, **B**… lit |
| **Pink** | Silenced: its speaker off, **Auto** keeping it down while its input is off air, or every bus it goes to off |

Pink is not the red of a peak at 0 dBFS. Hover the meter for which it is.
While it is unheard the number below says **Plays** — the higher of what it
plays now, L or R — and the white peak markers are hidden: they hold what is
heard, after the fader. Heard, the meter is green and yellow, after its
fader, with its **Peak**, as above.

For a target such as −6 dBFS, watch the **output bus** peak readout. A fader at
0 dB means unity gain, not a signal at 0 dBFS. Internal floating-point inputs
can exceed full scale without necessarily clipping until delivery; the red
warning shows lost headroom, not proof of where distortion occurred. The
Processing page uses the same scale. On an input its Pre and Post FX are both
before the fader; on a bus, Pre FX is before its compressor and Post FX after
its limiter, past the fader. A bus's own meter shows what goes out: after its
limiter. Clicking a meter resets only its held peak.

## Processing

Conference sound goes wrong in the same few ways: rumble and hum under a lapel
or lectern microphone, speakers far louder or softer than one another, and a
voice whose peaks jump out of the stream. Each strip's gear opens its settings;
the square beside **×** expands them, and **Processing** on the left holds:

**On an input**, in this order, before its fader:

1. **Low cut**: takes out what is below the voice: rumble, handling noise,
   hum. 100 Hz suits speech; 12 or 24 dB an octave below it. A new frequency
   or slope glides in over about 10 ms, so it can be moved on air without a
   click.
2. **Gate**: in a pause it turns the room down — the hum, the shuffle, the
   other microphones' spill — by **Down by** (15 dB; up to 30), never cut
   off, so a voice's soft tail is turned down, not chopped. It opens as soon
   as the sound comes over **Opens above** (−50 dBFS to start with: set it
   just over the room with no one talking), and closes once the sound has
   stayed 3 dB under that for its **Hold** (120 ms), over its **Release**
   (250 ms: a time constant in dB — about 95% of the way down in three times
   that) — so a voice hovering at the threshold does not flutter it. Its
   attack is a 0.5 ms time constant and no delay is added, so the first
   millisecond or two of a sharp sound (a click, a plosive) comes through a
   little turned down. It
   listens to what the low cut leaves, so rumble does not hold it open.
   Off unless switched on; the panel shows how far down it turns the sound
   now.
3. **EQ**: the voice's tone — a **Low** shelf (150 Hz), two **Bells** (350
   Hz and 3 kHz, each with its width, **Q**) and a **High** shelf (8 kHz),
   each up or down by up to 18 dB. Before the leveler, so tone is set before
   level; after the gate, so a lift does not open it. A band moved on air
   glides there in about 20 ms (no click); off or flat, the sound passes
   untouched.
   Mind the low cut under a low shelf cut: together they can thin a voice.
4. **Leveler**: rides each speaker slowly to the same average level (the
   **Target**, −20 dB by default), up to **Most boost** and **Most cut**, as a
   radio station's processor does, so a soft speaker after a loud one needs no
   hand on the fader. It turns down three times as fast as it turns up
   (**Speed**). Below **Hold below** it takes the sound for a pause or the room
   and holds still, so it never pumps the room's noise up between sentences.
   The panel shows the gain it applies now.
5. **Compressor**: above its threshold the voice rises only 1/ratio as much,
   evening out what the leveler leaves. Stereo linked, from the louder
   channel's peak.
6. **De-esser**: an "s" over its threshold turns the top down by up to its
   range (8 dB), the voice under it untouched. It listens around its
   frequency (**Listens at**, 6.5 kHz; 3–10 kHz) and turns down a shelf
   halfway down half an octave under it, so the "s" there loses about 80%
   of the reduction and everything above all of it. Over its threshold
   (**Acts above**, −30 dBFS) the band rises a quarter as much, eased in
   over 6 dB; it catches an "s" in about 1 ms and lets go over 60 ms. Last,
   after the compressor, which brings sibilance up. "band −N dBFS" shows
   how loud the band is: set the threshold a few dB under it on an "s".

**Speech preset** sets the low cut, leveler and compressor for a talk's
microphone (the gate, EQ and de-esser stay as they are: they depend on the
room and the voice): low cut at 100 Hz,
the leveler to −20 dB, gentle compression (3:1 from −18 dB, 3 dB made up).
Pre-fader listen hears the processed sound, as on a console.

**On a bus**: its compressor before the fader; after it, the **limiter** and
the **loudness meter**.

- **Limiter**: no sample past its **Ceiling** (−1 dBFS), whatever arrives: a
  shout, a dropped microphone, a loud clip. (Switched on, it fades in over
  10 ms rather than cutting a loud sound down at once, and a lower ceiling
  glides down over 10 ms; it holds the ceiling from then on.) It is on for every bus, shows saved
  before it included; it only acts on what would come within 1 dB of clipping.
  It looks 1.5 ms ahead to turn down before a peak arrives, so a bus's sound
  is 1.5 ms later — on or off, so switching it never shifts the timing, and far
  inside anything heard as out of step with the picture.
- **Loudness**: momentary (400 ms) and short-term (3 s) LUFS, updated every
  10 ms, and integrated LUFS, gated as ITU-R BS.1770 and EBU R 128 say, of
  what goes out. The integrated reading keeps every 400 ms block, each gated
  by its own loudness exactly as the standard says (about 3.5 MB for a
  12-hour day).
  Streams usually aim at about −16 LUFS, broadcast at −23. **Reset loudness**
  starts the readings again, as a show starts; the held peak stays.

Switching any of these on or off, and every change of setting, is smoothed so
nothing clicks. The processing runs on the mixing thread with no allocation
and nothing that can fail mid-show; if the mixer stalls, it starts afresh
rather than playing what it held. The settings are saved with the show; the
readings are not.

The **Audio Mixer** panel (under the Switcher in the Classic layout, beside the
inputs in vMix's) is laid out as vMix's: **OUTPUTS** — Master and the other
buses — then **INPUTS**, one upright strip each. Strips keep their own
size, as vMix's do, whatever the panel's: as many go across as fit, row after
row, and the panel scrolls up and down. A clip's strip has **▶ / ⏸** at the
left of its name, and a playlist's **▶ / ■** (green while it plays): what is
playing is seen, and stopped, in the mixer. A Stop there — and on an input's
tile — does not cut the sound dead: it fades it out over 2 seconds, then
pauses (the button amber while it fades; press again to stop at once).
Right-click it to choose **Cut** or a fade of 0.5 to 5 seconds: one choice
for every Stop, saved with the show's sound. A clip the timeline plays (it
has a lane in the arrangement) is paused at once, as before. Each bus has its own fader,
On button and delay, and plays on a sound device and/or feeds recordings and
streams.

## Inputs

- **Clips with sound** — videos, and sound files (MP3, WAV, AAC, M4A, FLAC,
  AIFF…) — appear by themselves. The sound stays in step with the picture: it
  loops, jumps and pauses with the clip. A paused, reversed or sped-up clip is
  silent. A sound file is added like a video (drop it in, or **Video or
  sound…**); it has nothing to see, so it can sit on any channel as a layer.
- **NDI sources** — an NDI input's sound appears by itself, named after the
  source, with Audio Auto on like a clip's. It is held about 60 ms deep, since
  NDI sends sound a video frame at a time.
- **Inputs** — a microphone, a line input, a USB mixer's return. Click
  **+ Input**, pick the device, then **Stereo 1-2** or **Mono N** (a lectern
  mic on input 2 of a USB interface is **Mono 2**), or use **Add Input →
  Audio Input**. An input whose device is unplugged shows **not connected**
  and comes back by itself when it is plugged in again — unplugged mid-show
  too: Nida says the sound stopped, and takes it again within a couple of
  seconds of the device coming back. A device plugged in for the first time
  after Nida started is listed as macOS sees it, in about a second. When the
  devices change, an input that then sends nothing but exact silence for ten
  seconds (its device made anew by its driver) is opened again, once. An output that takes a device's sound
  straight, not through a bus, stays without it until it is started again.
  Plugging a device in never moves an input onto it, unless the two have the
  same name. Devices are opened and closed beside the show, never in its
  frames: plugging one in, adding an input or a Reset input does not make
  the picture stutter (each used to hold the outputs for about a tenth of a
  second). Right-click an input's name to remove it.
- **A camera's sound** — chosen under **Audio** when adding a camera or
  capture card ([Adding Inputs](23-adding-inputs.md)): an input like the one
  above, named after the camera's input, with Audio Auto on so it switches
  with the camera's picture. It goes when the camera's input is removed.
- **An app's sound** — a Zoom or Teams call's, a browser's: **+ Input → App
  sound** lists the apps that have made sound (**● sound** beside those making
  it now). The sound is taken straight from the app, with no virtual cable or
  driver; macOS asks once whether Nida may record other apps' sound (Privacy
  & Security → Screen & System Audio Recording). While Nida takes it, the app
  is quiet on the Mac's own speakers, so a room does not hear a call twice: it
  is heard through Nida's buses. An app quit and started again is taken again
  by itself; while it is not running, its strip says **not running**. Needs
  macOS 14.2 or later. See [Zoom and Teams calls](24-calls.md).

Each input strip, top to bottom:

| Control | What it does |
|---|---|
| Name | Red while the clip is on air. Double-click a clip's name for its [Input Settings](21-input-settings.md) |
| Balance | Drag left or right; double-click to centre. The far side is turned down, the near side stays at full |
| ⚙ | Its settings: level typed in dB, balance, **Delay**, Audio Auto and where it looks, **Input settings…** (or **Remove input**) |
| **S** | Solo: hear it on the headphones only, before its fader (yellow when on) — see below |
| Arrows | **Audio Auto** (lit green when on) — see below. Microphones have none: they never go on air (a camera's sound has, with its picture) |
| Speaker | **On** (green): the sound goes to its buses. Out, it is off |
| Fader | Off to +12 dB, 0 dB three quarters up. Drag the cap (Shift for fine), double-click for 0 dB |
| Meter | Level after the fader, left and right: green, yellow from -18 dB, red from -6 dB |
| **M**, **A**, **B**… | Send to Master, Bus A, Bus B… (green when sent). A source can go to any number of buses |

### Audio Auto

As in vMix, a clip's sound switches on as it goes on air and off as it leaves:
Audio Auto presses its **On** button as the clip is cut or faded to Main or to
a bus's Program — on its own, or as a layer of a channel that is there — and
releases it as the clip leaves. During a fade the sound fades with the
picture. The button can still be pressed by hand in between: turn a clip off
while it is on air and it stays off until it next leaves and comes back.

Audio Auto is on for every new clip. In a clip's ⚙ (or its Input Settings),
**On air on** says where it looks: **Main or any bus** (the default), **Main**
only, or one bus. Turn Audio Auto off and the **On** button is yours alone.

The clip's picture plays and stops the same way — see
[Input Settings → Playback](21-input-settings.md).

## Outputs

A fresh workspace has one bus, **Master**. **+ Bus** adds more (Bus A, B…).
Double-click a bus's name, or right-click it for **Rename bus…**, to rename it
(Master too); **Remove bus** is in the same menu. Like every mixer change, a
rename is not undone by Undo — rename it back. Each bus strip has its fader, meter and
**On** button; its ⚙ holds:

- **Plays on** — the sound device: the Mac's speakers, HDMI into the ATEM or a
  TV, a USB interface into the desk. On a multi-channel interface, pick the
  output pair (**outputs 3-4** and so on); the list offers only the pairs the
  device has. A pair set that the device does not have (a show made on a
  bigger interface) is marked **⚠** and plays on the device's last pair —
  outputs 1-2 on a two-output interface — rather than on one side; hover
  the **⚠** to see where it plays. A device that is unplugged shows **⚠** and
  is reconnected automatically (hover: why it would not open). After
  plugging a device in, click **Rescan** to see it in the list.
- **Live** — for the room's speakers and stage monitors (see
  [Live: the room's speakers](#live-the-rooms-speakers) below). Offered once
  the bus plays on a device.
- **Monitor mix (before the faders)** — for a wedge, in-ears or a booth:
  each source reaches this bus at its send's level, before its fader, so
  riding a fader for the room leaves this mix as it is (see
  [Sends and monitor mixes](#sends-and-monitor-mixes) below).
- **Effect** — an Audio Unit plug-in first in the bus's processing (see
  [Plug-ins](#plug-ins-audio-units) below).
- **Delay** 0–300 ms, to hold the sound back until the picture catches up —
  useful when an LED processor or projector adds a few frames. See below for
  how to measure it.

### Live: the room's speakers

A bus feeding the PA or the stage monitors needs the speaker's voice back in
the room at once: heard much later than they speak, the room sounds like an
echo. The usual way a bus reaches its device takes about 50 ms (the mix's
10 ms blocks, then each device's own buffer). Switch the bus to **Live** in
its ⚙ and the microphones on its own sound device — a Scarlett, a USB mixer,
whose inputs and outputs are one device — are mixed inside that device as
their sound arrives: about **7 ms** from mic to speakers (5.1 ms measured
on a Scarlett Solo at 64 frames, plus the limiter's 1.5 ms look-ahead).

- Each mic keeps its strip's low cut, leveler, compressor, fader, **On** and
  balance; the bus keeps its compressor, fader, limiter and **Delay** (delay
  towers). The limiter looks 1.5 ms ahead, on or off. A mic's own **Delay**
  is left out on a live bus: that one meets a late camera's picture, and the
  room wants the voice as the speaker talks.
- The rest of the bus — clips, NDI, a remote guest, mics on other devices —
  is still heard on it, at the usual delay.
- The buffer beside **Live**: 64 frames (1.3 ms) at first; 32 is sooner, 128
  or 256 ask less of the Mac. The buffer is the whole device's, so its
  inputs get it too, and two live buses on one device both play at the
  smaller of theirs. A device that will not go as low plays at its smallest.
- **● live** shows it is playing live. **not live** and the **⚠** on the
  strip say why not (hover): no mic on the bus's own device sent to it yet,
  the device's inputs not at 48 kHz, more than eight of its mics sent to the
  bus, or the device refusing the stream (one whose smallest buffer is over
  1024 frames, say) — then the bus plays the usual way. Mics on other
  devices never stop it: they are the rest of the bus.
- The bus's meter and its recordings are the bus as mixed the usual way.

### Plug-ins (Audio Units)

A bus can run one Audio Unit effect — Apple's own (AUNBandEQ,
AUPeakLimiter, AUDelay…) or one installed on the Mac (a de-esser, an EQ, a
FabFilter) — chosen in its ⚙ ▸ **Effect**, first in its processing, before
its compressor and limiter (the limiter stays last, whatever the plug-in
does).

- The plug-in runs **outside Nida**, in macOS's own plug-in host: one that
  crashes takes its host down, not the show. Nida hands it each 10 ms of
  sound on a thread of its own and never waits for it, so the bus plays
  **20 ms later** with an effect, plus the plug-in's own delay (a
  look-ahead, a voice isolator's: shown beside it, **+N ms**). The sound
  without it — bypassed, a block late, or the plug-in failed — is held back
  just as much, so the bus's timing does not move; only once, as the
  plug-in loads and says its own delay. Lining it up with the picture:
  count that in the bus's **Delay** (less delay needed by that much).
- A block the plug-in has not finished in time is heard without it, and
  counted (**late ×N** beside it). One that will not load — not installed
  on this Mac, say — shows **⚠** (hover: why) and the bus is heard
  without it; the show keeps it, for the Mac that has it.
- **Bypass**: it keeps running, the bus is heard without it, faded over
  10 ms either way.
- Not on a **Live** bus: a plug-in that hangs would stop the device; a live
  bus with one plays the usual way (its ⚠ says so).
- **Open** shows the plug-in's own window (or Apple's plain list of its
  controls, for one without a window) in the middle of the screen Nida's
  own window is on, above it — never over a full-screen output, so the
  audience never sees it. What is set there
  is the plug-in's own: read from it every 2 seconds, saved with the show,
  and given back to it when the show is opened again — kept as it is on a
  Mac without the plug-in.

### Sends and monitor mixes

The bus buttons under each input strip turn its send to a bus on or off — at
the send's level and point as set, at first full level and as the bus takes
it (after the fader, unless the bus is a monitor mix). Each input's ⚙ ▸
General ▸ **Sends** sets them, per bus:

- the tick — sent there or not;
- its **level**, down to **off** or up to +10 dB — kept while the send is
  off, so it comes back where it was;
- where it takes the input: **as bus** (after the fader, or before it on a
  monitor-mix bus), **before fader** or **after fader**, whatever the bus.

Before the fader leaves out the fader only: the input's **On**, Audio Auto
and Stop/rundown fades still count. A stage microphone feeding a wedge wants
Audio Auto off, so switching cameras does not take it out of the speaker's
ear. Every change — a level, on or off, before or after — glides over 10 ms,
so a mix can be built while the show runs. A monitor mix on a **Live** bus is
mixed in the device's callback, as its speakers need it: its own input's
send levels, the fader left out; a level there glides too (about 10 ms). The
first time a microphone is sent to a Live bus its device opens again (a short
gap for the whole bus), so do that before the show; after that, turning its
send off and on only glides its level (up to eight microphones a bus).

A Live bus opening plays nothing for its first moment and then fades in: a
device that changes its buffer may replay old sound or swallow the start, so
it is given about 0.3 s of silence before a smaller buffer and 0.12 s more
after it starts. An open takes about half a second; once open, nothing of
this happens again.

To build a monitor mix fast, put its sends **on the faders**: right-click a
bus's name ▸ **Sends on the faders**, or pick it under **Faders:** at the end
of the inputs. The inputs' spine turns orange — **SENDS → WEDGE 1** — and each
input's fader shows and moves its level to that bus (double-click: 0 dB); a
send that is off is dimmed, and moving it keeps the level for when it is on.
The On buttons and meters are still the inputs' own — On turns an input off
on every bus, and the meter (marked "in") shows its own level (after its
fader, or before it when it is not heard), not its send. Click the orange spine to go back to the main mix. Only
the mixer's faders flip: the API, OSC and MIDI keep their meaning.

On the usual path a bus takes each input after its **Delay** (the picture
sync), before the fader or after; a Live bus takes its mics straight away.

## Headphones

Under Master: the knob is the operator's headphones level (drag up or down,
double-click for 0 dB); the headphones button opens where they play and which
bus they hear — Master by default. Nothing on the headphones reaches a bus, a
recording or a stream.

**S** on an input (vMix's *Solo*, pre-fade listen) puts that input alone on the
headphones, as it comes in: before its fader, even while it is off. Use it to
check a microphone before opening it, or a clip's level — play it by hand
first, since a clip off air waits paused. While anything is soloed the
headphones button is yellow; several inputs can be soloed at once. Solo is
never saved, so a show never reopens with the headphones stuck on one input.

## Recordings and streams

A recording or stream carries an audio bus, chosen in its **Audio:** menu in
the Outputs panel: **Bus: Master** by default for a new output, any other bus,
a capture device directly (the older passthrough), or **None**. Choose it
while the output is stopped. The sound is 48 kHz stereo, and a recording made
while a clip plays carries the clip's sound at the fader levels you set. The
[STREAM button](17-streaming.md)'s window has its own **Sound** choice.

What each one hears is metered beside the Program picture (and the Main
Output panel's): **REC**, **STREAM** and every bus that plays on a sound
device, so a bus sent to the speakers but not to the stream is seen at once
— see [Screen Layouts](20-layouts.md#broadcast-preview--program-and-inputs).

A typical conference routing:

| Bus | Sources | Goes to |
|---|---|---|
| Master | clips, desk return | stream and recording |
| ROOM | clips only | USB interface → FOH desk |
| HDMI | clips | the ATEM, over HDMI |

Mics usually go straight into the desk; the desk's mix comes back into Nida
on a USB input and is sent only to Master, so the stream gets the mics and the
room does not hear them twice.

## Lining up sound and picture

Everything Nida mixes is in step inside Nida. What arrives late is a
picture, in one of two places:

- **A camera's picture, coming in.** An ATEM or a capture card over USB
  delivers its picture several frames after the moment it shows, while a
  microphone into a USB audio interface arrives within a few milliseconds:
  the voice comes before the lips. Hold that **input** back: its ⚙ ▸
  **Delay**, 0–1000 ms, typed or dragged (as OBS's *Sync Offset* and vMix's
  input audio delay). Only that microphone waits; a clip's sound stays with
  its own picture. To measure it, record a few seconds of someone clapping on
  camera and find how far the clap's sound is ahead of the hands meeting.
  Fades, cuts and Audio Auto still happen when they are made — only the sound
  itself is held back — and **S** (pre-fader listen) hears it at once. Set it
  before going on air: changed while live, the sound jumps — a longer delay
  inserts a moment of silence, a shorter one skips ahead — as on a console.
- **The picture going out**, on the LED or projector, because the processor
  adds frames. Film the stage monitor and the LED together in slow motion and
  count the frames between them (see the latency notes in
  [Outputs](07-outputs.md)), then set that **bus's** **Delay** to the
  difference.

## Saved with the stage

Buses, their devices and delays, inputs, the headphones' device, level and
listen bus, and every strip's fader, On button, balance, delay, sends and
Audio Auto are saved in `stage.json` with the rest of the venue's routing, so they come
back after a restart. A workspace saved before Audio Auto opens with it on for
every clip that followed a bus. The mixer is a live console: **undo does not
move faders** or change routing.

## API

Every control has an HTTP route (tag *Audio Mixer* in [API](13-api.md)), and
`GET /api/state` includes the whole mixer under `audio_mix`, meters included:

```
POST /api/audio-buses                                {"name": "ROOM"}
PUT  /api/audio-buses/{bus}/device                   {"device": "USB Audio", "first_channel": 0}
PUT  /api/audio-buses/{bus}/live                     {"live": true, "frames": 64}   (frames optional: 32, 64, 128, 256)
PUT  /api/audio-buses/{bus}/insert                   {"component": "aufx:lpas:appl"}   (null takes it out)
PUT  /api/audio-buses/{bus}/insert/bypass            {"bypass": true}
POST /api/audio-buses/{bus}/insert/window            (its window, on the screen of Nida's own)
DELETE /api/audio-buses/{bus}/insert/window          (closes it)
PUT  /api/audio-sources/{deck-or-input}/gain         {"gain_db": -6}
PUT  /api/audio-sources/{deck-or-input}/sends/{bus}  {"on": true}   or any of {"on", "gain_db": -6, "mode": "inherit" | "pre" | "post"} (a level never turns a send on)
PUT  /api/audio-buses/{bus}/pre-fader                {"pre_fader": true}
PUT  /api/audio-sources/{deck}/auto                  {"auto": true}
PUT  /api/audio-sources/{deck}/follow                {"bus_uuid": "main"}   (a bus, "main", or null for anywhere)
PUT  /api/audio-sources/{deck-or-input}/balance      {"balance": -0.5}
PUT  /api/audio-sources/{deck-or-input}/delay        {"delay_ms": 170}
PUT  /api/outputs/{output}/audio-bus                 {"bus_uuid": "<bus>"}
PUT  /api/audio-sources/{deck-or-input}/solo         {"solo": true}
PUT  /api/audio-monitor/device                       {"device": "Scarlett Solo 4th Gen"}
PUT  /api/audio-monitor/listen                       {"bus_uuid": "<bus>"}
POST /api/audio-sources/{deck-or-input}/speech-preset
PUT  /api/audio-sources/{deck-or-input}/low-cut      {"enabled": true, "hz": 100, "slope_db": 24}
PUT  /api/audio-sources/{deck-or-input}/gate         {"enabled": true, "threshold_db": -50, "range_db": 15, "attack_ms": 0.5, "hold_ms": 120, "release_ms": 250}
PUT  /api/audio-sources/{deck-or-input}/de-esser     {"enabled": true, "hz": 6500, "threshold_db": -30, "range_db": 8}
PUT  /api/audio-sources/{deck-or-input}/eq           {"enabled": true, "bands": [{"hz": 150, "gain_db": -3}, {"hz": 350}, {"hz": 3000, "gain_db": 2, "q": 1}, {"hz": 8000}]}
PUT  /api/audio-sources/{deck-or-input}/leveler      {"enabled": true, "target_db": -20, "max_boost_db": 12, "max_cut_db": 12, "speed_secs": 3, "hold_below_db": -50}
PUT  /api/audio-sources/{deck-or-input}/compressor   {"enabled": true, "threshold_db": -18, "ratio": 3, "attack_ms": 5, "release_ms": 150, "knee_db": 6, "makeup_db": 3}
PUT  /api/audio-buses/{bus}/limiter                  {"enabled": true, "ceiling_db": -1, "release_ms": 100}
POST /api/audio-buses/{bus}/loudness/reset           (its loudness readings)
POST /api/audio-buses/{bus}/meters/reset             (its held peak)
```

A processing route takes the whole setting: fields left out take their
defaults — `enabled` false, except the limiter's, which is on.

`audio_mix.output_channels` lists each output device's outputs by name (the
pairs a bus can be set to; 0 when the device would not say). A bus or the
headphones playing other than as set carries `device_note`: the pair it
plays on instead, why it is not playing live, or why its device would not
open. A bus set **Live** carries `live`: `true` while it plays live.
`audio_mix.effects` lists the Audio Unit effects installed (name, maker,
version, `component` codes); a bus with an effect carries `insert`
(as set: `component`, `name`, `bypass`, and `settings`, the plug-in's own
as a base64 property list) and `insert_status` (`state`: loading, playing, bypassed or
failed, with `why`; `late_blocks`).
