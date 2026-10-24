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
2. **Leveler**: rides each speaker slowly to the same average level (the
   **Target**, −20 dB by default), up to **Most boost** and **Most cut**, as a
   radio station's processor does, so a soft speaker after a loud one needs no
   hand on the fader. It turns down three times as fast as it turns up
   (**Speed**). Below **Hold below** it takes the sound for a pause or the room
   and holds still, so it never pumps the room's noise up between sentences.
   The panel shows the gain it applies now.
3. **Compressor**: above its threshold the voice rises only 1/ratio as much,
   evening out what the leveler leaves. Stereo linked, from the louder
   channel's peak.

**Speech preset** sets all three for a talk's microphone: low cut at 100 Hz,
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
left of its name (green while it plays): what is playing is seen, and paused,
in the mixer. Each bus has its own fader,
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
  seconds of the device coming back. An output that takes a device's sound
  straight, not through a bus, stays without it until it is started again.
  Plugging a device in
  never moves an input onto it, unless the two have the same name; one
  plugged in while Nida runs is listed after **Rescan** and gives sound once
  Nida is reopened. Right-click an input's name to remove it.
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
  output pair (**outputs 3-4** and so on). A device that is unplugged shows
  **⚠** and is reconnected automatically. After plugging a device in, click
  **Rescan** to see it in the list.
- **Delay** 0–300 ms, to hold the sound back until the picture catches up —
  useful when an LED processor or projector adds a few frames. See below for
  how to measure it.

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
PUT  /api/audio-sources/{deck-or-input}/gain         {"gain_db": -6}
PUT  /api/audio-sources/{deck-or-input}/sends/{bus}  {"on": true}
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
PUT  /api/audio-sources/{deck-or-input}/leveler      {"enabled": true, "target_db": -20, "max_boost_db": 12, "max_cut_db": 12, "speed_secs": 3, "hold_below_db": -50}
PUT  /api/audio-sources/{deck-or-input}/compressor   {"enabled": true, "threshold_db": -18, "ratio": 3, "attack_ms": 5, "release_ms": 150, "knee_db": 6, "makeup_db": 3}
PUT  /api/audio-buses/{bus}/limiter                  {"enabled": true, "ceiling_db": -1, "release_ms": 100}
POST /api/audio-buses/{bus}/loudness/reset           (its loudness readings)
POST /api/audio-buses/{bus}/meters/reset             (its held peak)
```

A processing route takes the whole setting: fields left out take their
defaults — `enabled` false, except the limiter's, which is on.
