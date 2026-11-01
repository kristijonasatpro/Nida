# Recording

The **REC** button in the top bar, beside **STREAM**, works like vMix's Record:
press it, pick where files go and how they are encoded, and start. Files are
encoded on the Mac's media engine, so recording costs almost no CPU.

| Button | Meaning |
|---|---|
| Plain **REC** | Not recording |
| **Red** `REC 0:12:34` | Recording, all well |
| **Yellow, blinking** | Recording, but something needs attention: the disk is nearly full (under 5 GB), the encoder is falling behind, or it had to start again |

While recording, beside the button: the take's size and the free space
(`1.2 GB · 230.0 GB free`, yellow under 5 GB), and frames the encoder could
not take, if any (`dropped 12`, red); in a narrow window, the size alone.

Hover over the button for the file being written. Click it to open the
**Record** window: **Start recording** / **Stop recording** at the top (stop
takes two clicks, as a stop by accident splits the take), the time and file
size, the file name with **Show in Finder**, and the free space.

## Files

Each recording is a new file named by when it started —
`Nida 2026-09-26 14-05-09.mp4` — in the **Folder** (Movies ▸ Nida by default;
**Choose…** to change it). Nothing is ever overwritten.

If the encoder stops by itself mid-recording, Nida carries on in a new file a
second later — `… (2).mp4` — and the window says the take is in two files.

## Settings

| Setting | What it does |
|---|---|
| **Picture** | What is recorded: **Master** or any bus's Program. Can be changed while recording |
| **Sound** | The audio-mixer bus recorded (Master by default). With no bus the file carries silence, so it always has a sound track |
| **Codec** | **H.264** (plays everywhere), **HEVC** (about 60 % of the size), **ProRes 422** or **ProRes 422 HQ** (large files, light to edit and grade) |
| **Format** | **MP4** — plays everywhere and **survives a crash**: it is written in one-second pieces, so if Nida or the Mac goes down, everything up to the last second plays. **MOV** — QuickTime. **MKV** — survives a crash too; VLC and editors that read it. ProRes is always MOV |
| **Quality** | **Auto** picks a bit rate for the size and frame rate (12 Mbit/s for 1080p30 with H.264, about 68 Mbit/s for 4K60; 60 % of that for HEVC), or set one; **Presets** for common cases. A rate set under half of Auto's for the picture (8 Mbit/s at 4K) blurs in motion: the settings say so. ProRes has its own data rate |
| **Encoder** | **Hardware** (the Mac's media engine) or **Software** |
| **Size** | Nida's own resolution, or 720p, 1080p, 1440p, 4K |
| **Frame rate** | Nida's own (the default), or 24, 25, 30, 50, 60 fps. A whole share of Nida's rate takes the show's frames evenly — 30 from a 60 fps show is every second frame, 30 to 60 each frame twice — counted frame by frame, not by when each was drawn (its frames fell on the edges of the file's, and the least delay took one in or left it out). A rate that does not divide Nida's (50 from 60) leaves frames out unevenly and motion stutters: the settings say so and name the rates that fit. The same holds for a stream's rate |
| **Audio** | AAC 128–320 kbps; uncompressed PCM beside ProRes, as editors expect |
| **Reminder** | **Blink NOT RECORDING behind the take buttons**: while nothing is being recorded, NOT RECORDING blinks red behind Cut and Fade — in the middle of a show, Record is easily forgotten. Off to start with |

Only the picture and the reminder can change while recording; the rest waits
for a stop. The settings are saved with the stage.

## Sound and picture in step

Both tracks start at the moment the sound starts being taken. Each frame goes
into the file where the moment it was drawn falls after that — however long
the encoder took to start, or a frame took to get to it — so a clip's sound
lands within a frame of its picture in every take. A frame the show missed is
filled with the one before it; the log line when a take finishes counts them.

H.264 and HEVC on the hardware encoder are encoded inside Nida, each frame
with its own time, as the stream is. When the media engine has more to do
than it can — a **4K recording beside a 4K stream** at 60 fps can be more
than one Mac manages — the recording skips ahead to the newest picture
instead of filling the file with repeats of an old one: it keeps fresh
pictures at a lower rate while the engine is full, and the stream beside it
keeps its share. (Before, a recording fell behind for good, filled itself
with repeats — two fresh pictures a second — took the engine from the stream,
and went on encoding them for most of a minute after **Stop**.) Gaps of up
to a second — a moment the show was busy — are still filled, so the file
keeps its frame rate; only a file made while the engine was full has a
varying one there. ProRes, and the software encoder, go through FFmpeg as
before.

What Nida cannot know is how late a camera's picture is before it arrives. An
ATEM or a capture card over USB delivers its picture several frames after the
moment it shows, while a microphone into a USB audio interface arrives within
a few milliseconds — so a presenter's voice comes before their lips move, in
the recording and on the stream. Hold the microphone back by the difference
with its **Delay** (Audio Mixer ▸ the microphone's ⚙): record a few seconds
of someone clapping on camera, find how far the clap's sound is ahead of the
hands meeting, and set that — an ATEM over USB with a mic into a USB
interface measured 170 ms. Only that microphone waits; clips stay with their
pictures. (The bus's **Delay** holds back everything on the bus, its sound
device too: it is for a picture that arrives late at a screen.)

## API

| Route | |
|---|---|
| `GET /api/record` | Settings and status (file, size, free space, problems) |
| `PUT /api/record/options` | The settings above |
| `POST /api/record/start` | Start a new file |
| `POST /api/record/stop` | Stop; the file is finished in the background |

The older **Recording** output in the Outputs panel still exists (software
encoders, one fixed path); the REC button replaces it for events.
