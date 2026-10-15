# Streaming

The **STREAM** button in the top bar works like vMix's: one picture, encoded
once, sent to up to **four destinations** at the same time — YouTube, a CDN
such as Cloudflare, the client's own server — each with its own URL, stream
key and login.

The button's colour is the stream's health at a glance:

| Button | Meaning |
|---|---|
| Plain **STREAM** | Not streaming |
| **Red** `● STREAM 0:12:34` | Live, and every destination switched on is sending in real time |
| **Yellow, blinking** | Live, but something needs attention: a destination is connecting, reconnecting or falling behind, one cannot be sent to as entered, or the encoder cannot keep up |

Hover over the button to see each destination's state in one line. Click it to
open the **Stream** window.

## Setting up

The window's top line has **Start streaming** / **Stop streaming** and the
live clock; the settings scroll below it.

| Setting | What it does |
|---|---|
| **Picture** | What goes out: **Master** (the main output) or any bus's Program. Can be changed while live |
| **Sound** | The audio-mixer bus the stream carries (Master by default). With no audio bus at all the stream carries silence — some platforms refuse a stream without sound |
| **Size** | The picture size sent: 1280×720, 1920×1080 (default), 2560×1440, 3840×2160 or the project's own. Scaled on the GPU from the source |
| **Video** | **Codec** — **H.264** (every platform) or **HEVC (H.265)**, the same picture at about 60 % of the bit rate, for destinations that take it over Enhanced RTMP (YouTube; most others need H.264) — and the bit rate in kbps (6000 by default, constant), typed or from **Presets**; with the hardware encoder it can be changed while live. **Lower it while the upload cannot keep up** (on by default, hardware encoder): see [Unstable internet](#unstable-internet) |
| **Rate** | How the video bit rate is held: **Constant** (the default), **Variable** or **Average** — see [Constant, Variable or Average](#constant-variable-or-average). With the hardware encoder, Constant and Variable trade places while live |
| **Encoder** | **Hardware** (the Mac's media engine, no CPU cost — the default on a Mac) or **Software** (x264 / x265) |
| **Frame rate** | **Same as Nida**, or 24, 25, 30, 50, 60 fps. A talk at 25 or 30 needs half the upload of 50 or 60 |
| **Audio** | AAC bit rate (160 kbps by default) and **Keyframe every** (2 s by default, what most platforms ask for) |
| **Network buffer** | How far each destination may fall behind when the internet dips — and catch up afterwards — before any of the stream is lost: 3, 10 (default), 20 or 30 s |

The picture can be switched live. With the **Hardware** encoder the stream
is encoded inside Nida — wherever the media engine opens and the picture size
is one the GPU converts (all those offered are) — and then the video bit rate
can be changed live too — typed, dragged or from **Presets**, it goes out
from the next frame, and the destinations stay connected — and **Lower it
while the upload cannot keep up** switched on or off. Sound, size, codec,
frame rate, encoder, the audio settings and the buffer can only change while
the stream is stopped: they need a new encoder. So can the video bit rate
with **Software** (x264 / x265), or where the stream is not encoded in Nida
(the window says so: its bit rate box stays shut while live).

Seeing the upload struggle — a venue's Wi-Fi, a phone's hotspot — lower the
bit rate, say from 4000 to 2000: with the box ticked the new rate is the
most Nida sends, and it still goes lower by itself if the upload needs it;
with it unticked the rate is exactly what is set.

A platform keeps the size and frame rate a broadcast started with. After
changing either, end the broadcast on the platform and start a new one
rather than resuming the old one: `YouTube` kept a broadcast begun at 60 fps
as 1080p60 when it was resumed at 30.

### Moving backgrounds look blocky

A still slide needs few bits; a moving background, a camera or a video needs
all of them, and at 60 fps each frame gets half of what it gets at 30. The
bit rate set is what goes out: with nothing moving, the hardware encoder on
Apple silicon pads the stream to it rather than sending less. If motion
still breaks up:

- **Raise the bit rate**: 9000–12000 kbps for 1080p60 (what `YouTube` asks
  for), 6000 is enough for 1080p30
- **HEVC** where every destination takes it (`YouTube` does): the same
  picture at about 60 % of the bit rate
- **30 fps** for a talk: twice the bits for every frame

### Constant, Variable or Average

| Rate | What goes out |
|---|---|
| **Constant** (CBR) | The bit rate set, held. Where the picture needs less, the encoder pads the stream up to it — on a talk's slides and camera bubbles the media engine sent three quarters of a 4 Mbps stream as padding. What `YouTube` ("Bitrate encoding: CBR") and Cloudflare Stream ask for |
| **Variable** (VBR) | The same picture as Constant, without the padding: much less on slides and stills, about the rate set on busy pictures. With the media engine the pictures are exactly Constant's (measured frame for frame); with x264 and x265 their own rate control, held to the rate set over a few seconds of buffer — a single second can go over it, as Constant's can |
| **Average** (ABR) | The rate set on average: the picture gets all of it, a touch better than Constant on easy pictures, but busy stretches go over — a quarter over in a single second on normal motion; on very noisy pictures the media engine held 70 % over for as long as they lasted. Needs room in the upload. x264 and x265 hold it to one and a half times the rate over a few seconds of buffer, but the first busy second after a still can go much higher (measured: over 4 Mbps at 1 Mbps set) |

Constant stays the default, and is the safe choice for any platform. Variable
saves upload on talks and slides — a venue's Wi-Fi or a phone's hotspot carries
the same picture with room to spare — where the platform takes a stream whose
rate moves. Average is for an upload with room, where every bit should go into
the picture.

Encoded inside Nida (the **Hardware** encoder, above), all three change bit
rate while live, and **Lower it while the upload cannot keep up** works with
each. Constant and Variable are one encoding, padded or not, so they trade
places while live; Average is encoded apart and waits for a stop. With
**Software** the rate and the mode wait for a stop. Where the encoder cannot
hold the mode set — some Intel Macs' media engine holds no constant rate, so
Constant and Variable go out as Average there — the window says what goes out
instead.

Not offered, and why (measured at 1080p50):

- **Average with a ceiling** (OBS's "limit bitrate" on a Mac): after the rate
  was lowered and raised again, the media engine stayed at 1.7 of 4 Mbps on a
  busy picture — it cannot follow a rate changed while live.
- **The media engine's low-latency rate control**: the best picture for its
  bits and a hard ceiling, but where a picture cannot fit it drops frames
  instead — half to three quarters of them on a fast pan over noise.
- **Constant quality** (CRF): no ceiling at all, which no upload can be
  planned for. It belongs to recordings.

What the main platforms ask for, from their own pages — the window notes a
setting that does not match a destination switched on:

| Platform | Rate | Also |
|---|---|---|
| `YouTube` | Constant ("Bitrate encoding: CBR") | Keyframes every 2 s, never more than 4 |
| Cloudflare Stream | Constant preferred | 2–14 Mbps sound and picture together, H.264 and AAC, keyframes every 2–10 s (4–8 for standard playback, 2–4 for low-latency HLS). Variable on slides, or the rate lowered for a slow upload, can fall under 2 Mbps: a stream lowered to 0.3 Mbps stayed live, but what the platform makes of it is its own |
| LinkedIn Live | Not stated | 720p recommended, 1080p at most, 30 fps, 3.5 Mbps (6 at most), AAC 128 kbps, keyframes every 2 s |

Others: their own encoder page. When in doubt, Constant.

### Unstable internet

With **Lower it while the upload cannot keep up** on (the default, with the
hardware encoder), Nida does what an operator would: when a destination's
upload cannot carry the stream, the video bit rate comes down until it can
— a softer picture rather than one breaking up — and goes back up step by
step once the upload has room again, slowly near the rate that was too
much. It goes by the stream waiting at each destination, and follows the
slowest one sending. The **Stream** window's top line then says *Adapting to
the upload: 1.2 of 6.0 Mbps*. It never asks the encoder for more than the
rate set — though the encoder's own seconds can go over it, as above — nor under
what the picture's size still shows (about 400 kbps for 1080p50), and when
the upload cannot carry even that, the line turns red: *Bit rate as low as
it goes*. The encoder runs inside Nida for this, on the media engine. With
the box unticked the rate stays as set, and can be lowered by hand while
live; with **Software** chosen it stays as set until the stream is stopped.
Lowering can take the stream under what a platform asks for: Cloudflare
Stream lists 2–14 Mbps as its input requirement. A stream lowered to 0.3
Mbps there stayed live, but what the platform makes of it for viewers is its
own affair.

The **network buffer** rides out dips: if the upload stalls for a few seconds
and recovers, nothing is lost, and the destination catches up by sending
faster than real time for a while. That needs spare upload. An upload too
slow all the time needs a lower bit rate, which the setting above finds by
itself. Viewers on a platform are several seconds behind anyway, so a bigger
buffer costs them nothing visible. A connection dead for 10–20 seconds is
reconnected whatever the buffer — and at once when the network changes under
it: Wi-Fi moved to another network, or a cable pulled. Nida watches the
machine's addresses and the way out; a destination that stops sending after
such a change is connected again over the new way within a couple of
seconds, where it used to hang on the dead connection for half a minute.
One still sending was on a way that still works and is left alone; one
waiting to try again — the Wi-Fi gone a while — tries at once when a way
out comes back. With **Lower it while the upload cannot keep up** on, the
bit rate goes back to what the upload carried before — the rate it was cut
to while the old way died says nothing of the new one — and is lowered
again if the new way carries less.

A destination that falls further behind than the buffer holds jumps ahead:
it finishes the frame it was sending and resumes at the next keyframe, so
viewers see the picture skip, not break up into blocks. A destination
connecting starts at a keyframe the same way.

**Every destination uses the full bit rate of upload**: two destinations at
4000 kbps need about 8.5 Mbps. Keep the total to 60–70 % of what the venue's
upload measures, and use wired Ethernet rather than Wi-Fi where you can.

Between the picture and the encoder there is a separate fifth of a second of
slack, so a brief pause in the encoder costs nothing. **Encoder: N frames
dropped** under the window's top line counts frames it still missed; the
button only turns yellow for the encoder when more than one frame in a
hundred is missed over ten seconds.

### Destinations

Each of the four destinations has:

- an **on/off** checkbox (its number) — only the ones switched on are sent to,
  and a destination can be switched on or off **while live** without touching
  the others;
- a **Name**, for you;
- **URL** — the server and application, exactly as the platform gives it,
  starting `rtmp://` or `rtmps://`, e.g. `rtmps://live.example.com:443/live/`;
- **Stream key** — the key or stream name, also as given. If the platform
  gives one long URL that already ends in the key, paste it into **URL** and
  leave the key empty;
- **Login** — **username** and **password**, only for servers that ask for one
  (Wowza, Akamai, broadcasters' own servers). Nida answers the server's login
  challenge the way vMix and FMLE do.

**Show** reveals the key and password. Fields are sent when you leave them or
press Enter, so a half-typed URL is never used live; changing a live
destination's URL, key or login reconnects just that destination.

A destination that cannot be sent to as entered — no key, a URL that is not
RTMP, a password without a username — shows **Check settings** in orange with
the reason, before you start.

## While live

Beside the **STREAM** button, as OBS's status bar shows it:

- **Signal bars** for the destination doing worst: four green while live,
  fewer and yellow while connecting or struggling, one red while
  reconnecting, none for a destination that cannot start (its settings).
- The **rate** going out at the slowest destination sending (live or
  struggling), and in red how many destinations are **down** (reconnecting,
  or waiting for their settings).
- **Dropped** frames and their share of the time live: frames the encoder
  could not take, and the stream a slow upload lost, counted as time, then
  as frames at the rate the stream is sent (`~` marks that estimate).
  Yellow under 1 %, red above.

In a narrow window they shorten — the drops as their share, then the bars
alone — so they never run under the controls on the right. Hover over them
for every destination's line. In the **Stream** window, each destination
shows its state and the bit rate actually going out:

| State | Meaning |
|---|---|
| **Connecting…** | Connecting to the server; nothing has gone out yet |
| **Live** | Sending in real time |
| **Struggling** | Connected but not keeping up: the upload is too slow, parts of the stream are being lost, or the connection has stalled. Below what the stream needs it says both — *Struggling 1.6 of 2.7 Mbps* — and what is being done: *Nida is lowering the bit rate to fit*, or, with the rate held as set, if it goes on, lower the **Video** bit rate — while live with the hardware encoder, or by stopping, lowering it and starting again with **Software** (a smaller **Size** alone is still sent at that bit rate) |
| **Reconnecting…** | Lost; trying again after 1, 2, 4, 8, then every 10 seconds — at once after a change of network — with the reason (for example *The server refused the login — check the stream key, username and password*, or *The network changed — reconnecting*). Hover over the reason for ffmpeg's own words; the log has it too |

Under it, how often it reconnected and how much of the stream a slow upload
has lost, as time — reckoned at the bit rate the stream had when it was lost
(*Reconnected once · about 30 s of the stream lost to a slow upload*). Once
a destination has been live, the stream it missed while reconnecting counts
too: what waited for the dead connection is thrown away when it reconnects,
and viewers never get it.

A destination that drops reconnects **on its own**, while the others keep
going. One that stays connected but sends nothing for 20 seconds is
reconnected too. The encoder itself is restarted if it ever stops.

**Stop streaming** takes two clicks — the first turns it into **Click again to
stop** for three seconds — because stopping by accident ends the event for
everyone watching. Stopping tells every server the stream has ended.

Opening another workspace stops the stream; loading one never starts it.

## Where the settings are kept

With the stage (`.varda/stage.json`), in the clear, as vMix keeps them — they
belong to the event, not to the show. Keep the workspace folder private. The
API and `GET /api/state` never show keys or passwords: a key appears only by
its last four characters, a password only as set or not.

## API

| Route | |
|---|---|
| `GET /api/stream` | Settings (secrets masked) and each destination's state |
| `PUT /api/stream/options` | Picture, sound, size, bit rates, `rate_mode` (`constant`, `variable`, `average`), encoder, keyframes. While live only `source` changes — and, when `GET /api/stream`'s `rate_live` says the stream is encoded in Nida, `video_kbps`, `adaptive`, and `rate_mode` between `constant` and `variable`; anything else is refused with a 400 saying so. `GET /api/stream` says how the running stream is held (`rate_mode`) and why where that differs from the one set (`rate_note`) |
| `PUT /api/stream/destinations/{index}` | Destination 0–3. Leave `key` or `password` out to keep the saved one |
| `POST /api/stream/start` | Start to every destination switched on |
| `POST /api/stream/stop` | Stop |

## If a destination will not connect

- **The server refused the login** — the key, username or password is wrong,
  or the server wants the login in the URL instead: try
  `rtmp://user:password@server/app` with the Login fields empty.
- **Unknown server** / **refused the connection** — check the URL and port.
  RTMPS usually uses port 443: `rtmps://host:443/app`.
- **The secure connection (RTMPS) failed** — check the URL starts with
  `rtmps://` only if the platform says so.
- A username cannot contain `:`, and a username or password cannot contain
  `/`, `?` or `#` — RTMP logins cannot carry them. Nida says so before
  starting.

## Why not the older RTMP output?

The Outputs panel's **RTMP** output still exists. It streams one destination
at the project's full resolution, with no key or login fields, no sound unless
a device is picked, and no reconnect. The STREAM button replaces it for
events.
