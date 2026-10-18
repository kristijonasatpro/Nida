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

Each recording is a new file with its **Recording name** before the timestamp,
such as `Clean feed 2026-09-26 14-05-09.mp4`, in the **Folder** (Movies ▸ Nida
by default; **Choose…** changes only the folder). The default name is **Nida**.
Nothing is ever overwritten. The name is kept between takes and saved with the
show; editing it while recording applies to the next take. Recovery files
continue using the current take's original name.

If the encoder stops by itself mid-recording, Nida carries on in a new file a
second later — `… (2).mp4` — and the window says the take is in two files.

## Settings

| Setting | What it does |
|---|---|
| **Picture** | What is recorded: **Master** or any bus's Program. Can be changed while recording |
| **Sound** | The audio-mixer bus recorded (Master by default). With no bus the file carries silence, so it always has a sound track |
| **Recording name** | Filename prefix before the timestamp. Can be edited while recording; applies to the next take |
| **Codec** | **H.264** (plays everywhere), **HEVC** (about 60 % of the size), **ProRes 422** or **ProRes 422 HQ** (large files, light to edit and grade) |
| **Format** | **MP4** — plays everywhere and **survives a crash**: it is written in one-second pieces, so if Nida or the Mac goes down, everything up to the last second plays. **MOV** — QuickTime. **MKV** — survives a crash too; VLC and editors that read it. ProRes is always MOV |
| **Quality** | **Auto** picks a bit rate for the size and frame rate (12 Mbit/s for 1080p30 with H.264, about 68 Mbit/s for 4K60; 60 % of that for HEVC), or set one; **Presets** for common cases. ProRes has its own data rate |
| **Encoder** | **Hardware** (the Mac's media engine) or **Software** |
| **Size** | Nida's own resolution, or 720p, 1080p, 1440p, 4K |
| **Frame rate** | Nida's own, or 24, 25, 30, 50, 60 fps |
| **Audio** | AAC 128–320 kbps; uncompressed PCM beside ProRes, as editors expect |

The picture can change live, and the name can change for the next take; other settings wait for a stop. The
settings are saved with the stage.

## Sound and picture in step

Both tracks start at the moment the sound starts being taken. Each frame goes
into the file where the moment it was drawn falls after that — however long
the encoder took to start, or a frame took to get to it — so a clip's sound
lands within a frame of its picture in every take. A frame the show missed is
filled with the one before it; the log line when a take finishes counts them.

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

## Recording more than one feed

In **Library → Outputs**, **+ Recording** adds an independent recorder. Each
has its own **Source**, **Audio**, codec, name, folder and **Start / Stop** controls.
For example, record Main with graphics on one output and a clean bus on another.
The top-bar **REC** button still controls only its own recording.

New recording outputs default to Videos/Movies → Nida. Click the **pencil** next
to a recorder's title to rename it in place: Enter or clicking outside saves,
Escape cancels. This title is also its filename prefix. Renaming a running
recorder changes only the next take; it does not rename its current file or
restart the encoder. **Folder → Choose…** selects only the destination folder
and is disabled while recording. Each Start adds the timestamp and, when needed,
a collision number. The codec supplies the extension: MP4 for H.264/HEVC/AV1,
MOV for ProRes/HAP. Two recorders can share a name and folder without overwriting
each other's takes.

Existing shows keep their output titles and destination folders. Their old
configured file paths remain readable, but the title now supplies the prefix
and the codec supplies the extension. Existing relative folders still resolve
against the application's working folder.

**Writing** shows the full actual path while recording; **Last take** retains it
after Stop until the app closes. **Show file** opens its location in the file
manager, and **Copy path** copies that path. The configured name, folder, source and
other output settings continue to be saved with the show.

Recording outputs retain rotation, surfaces, edge blending, SDR precision and
the existing codec choices. Eight-bit H.264/HEVC use GPU NV12 conversion when
the picture size supports it, with the same bounded buffering policy as REC.
Ten-bit, alpha and other formats retain their corresponding readback paths.
These recording outputs still use software encoders and fixed-quality settings;
they do not inherit the top-bar recorder's bitrate or encoder settings. Each
additional recorder adds encoding work, so check dropped frames with the actual
show and simultaneous recordings before using them live.
