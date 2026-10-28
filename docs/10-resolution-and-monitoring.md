# Resolution, Settings & Monitoring

This page covers render resolution and per-deck scaling, set from the **top bar**, and the live performance metrics, which live in the **top bar**.

## Where Settings Live

| Setting | Where | How |
|---------|-------|-----|
| Render resolution | Top bar (📐 W×H) | Click to pick a preset or enter a custom size |
| Audio input device | Audio modulator → device dropdown | Selected per modulator; capture is automatic (a device runs only while referenced) |
| MIDI devices | Setup panel → 🎹 MIDI | Enable/disable, rescan |
| MIDI mappings | Right-click → MIDI learn | Visual mapping (purple glow) |
| Keyboard shortcuts | Right-click → Keyboard learn | Visual mapping (orange glow) |
| Clock source | Top bar → BPM display | Auto priority + manual override |
| Transport | Top bar → position readout | Play/stop, return to zero, source, timecode rate |
| Tonemap curve and 3D LUT | Setup panel → 🎨 Tonemap | Pick a curve; LUTs are read from `.varda/luts/` |
| OSC port / feedback | `.varda/osc.json` or `--osc-port` | Config file (see [Control Surfaces](06-control-surfaces.md#osc)) |
| Shader library | `shaders/` directory | Filesystem convention, hot-reloaded |

## Render Resolution

The **render resolution** is the master size at which all decks, channels, and the mixer composite. It is a scene-level setting saved in `scene.json` (`render_width` / `render_height`).

Set it from the 📐 control in the top bar:

### Auto — match the output screens

**Auto** makes the render resolution follow the largest screen an output is
on: connect a 4K TV and put an output on it, and the whole picture is made in
4K; on Full HD screens it is made in Full HD. The top bar then reads
`📐 Auto 3840×2160`. What resolution a screen runs at is decided by the
operating system (macOS *Displays* settings, the monitor line in Hyprland) —
Nida follows it rather than changing it.

A change waits for the screen to hold its new size for half a second, so a
display renegotiating its mode, or a cable being reseated, does not rebuild
every texture twice mid-show. Outputs still waiting for their screen don't
count. Picking any preset or custom size turns Auto off; the choice is saved
with the scene (`render_resolution_auto`). Over the API:
`PUT /api/resolution/auto` with `{"enabled": true}`.

Every output fills its screen whatever the render resolution. When the two
differ the picture is scaled on the way out, so a Full HD screen beside a 4K
one shows the 4K picture scaled down — mixing sizes costs the smaller screen
its pixel-for-pixel match. Rendering in 4K is also about four times the GPU
work of Full HD.

### Presets

**Landscape**

- 1280×720 (720p)
- 1920×1080 (1080p) — default
- 2560×1440 (1440p)
- 3840×2160 (4K)

**Vertical & square**

- 1080×1920 (9:16) — Instagram Reels, TikTok, YouTube Shorts, Stories and Facebook Reels all take this exact size, so it is the one vertical master to record if you only record one
- 2160×3840 (9:16 at 4K)
- 1080×1350 (4:5) — Instagram feed posts, which take up more of the scroll than square
- 1080×1080 (1:1) — square

### Custom Resolution

Choose **Custom…** to enter freeform width × height for LED walls, vertical strips, or unusual aspect ratios. There is no aspect-ratio lock and **no artificial maximum**. The render size is bounded only by your GPU's maximum texture size (commonly 8192² or 16384²), so capable hardware can render at 8K and beyond.

Changes take effect immediately: the engine resizes every render texture and confirms with a toast (e.g. "📐 Resolution changed to 3840×2160"). Scenes saved without these fields default to 1920×1080.

### What outputs do when you change resolution

Everything you send out conforms to the resolution you set, not to the shape it happened to have when you created it.

**Recordings, NDI, Syphon and streams** are resized to match. Anything already running through ffmpeg such as a recording or a stream, is **stopped**, and a toast names which ones. The encoder's frame size is fixed when it starts, so it can't follow you mid-take; stopping is safer than writing frames it will misread, and safer than restarting, which would reopen the file and wipe the take you already have. Start it again and it comes back at the new size. NDI and Syphon keep publishing across the change without interruption.

**Output windows** letterbox. The window is whatever size you or the OS made it, so a 9:16 project on a 16:9 projector is centred with black bars rather than stretched. The projector calibration card still covers the full output — it has to, for alignment to mean anything — and surfaces you place on the stage carry their own shape, so neither is letterboxed. A brand-new output window opens at the master's aspect ratio, so a vertical or square project doesn't start life in a 16:9 window you have to drag into shape. Once you have resized it, that size is saved with the stage and used from then on.

**Production buses** keep showing the same part of the picture: each bus's
region scales with the master, so a full-frame bus stays full frame at 4K and
an LED strip stays the same strip.

**The dome is the one exception.** A domemaster is square by definition, so it has its own size setting (1K/2K/4K) in the Stage Editor's dome toolbar rather than following the master. See [Projection Mapping](08-projection.md).

### What outputs do with frame rate

Everything runs at the **Target FPS** you set in the top bar — there are no per-output frame-rate settings and nothing runs on its own clock.

On a Mac, each frame is started by the refresh of the screen the show goes to — the first output window's (the TV), else the control window's — whenever a whole number of its refreshes makes the Target FPS (60 on a 60 Hz or 120 Hz screen, 30 on a 60 Hz one). A frame is then drawn for every refresh the screen shows, so movement — a playlist's slide — glides rather than hitching when a timer drifts against the screen. Otherwise (50 fps on a 60 Hz screen, say) Nida's own timer starts frames — but no timer makes such a rate move evenly on that screen: some frames stay one refresh longer than others. So Nida says so, once for each output screen, naming the fix: set the screen to the show's rate or a multiple (System Settings → Displays → Refresh Rate, where a MacBook Pro offers 50 Hz as well as ProMotion) or — where the screen's rate is fixed, as an LED processor's often is — run the show at a rate that fits (the top bar's Target FPS). `GET /api/state/performance` says which (`pacing.clock`), how evenly frames have started over the last five seconds, and each output window's presents and skipped frames. On a Mac it also says what each screen did with them, from macOS's own report of when every frame came up (`on_screen` for each output, `pacing.control_on_screen` for the control window): frames shown, dropped (presented but never shown), hitches (up more than half a frame too long — the one before repeated, which is what an eye sees in a moving slide) and early ones, with the mean and spread of the time between frames. A smooth output shows `hitches` standing still. Where the GPU's time goes is in `pacing.gpu_time` (macOS): over the last second, `busy_pct` — how much of it Nida's work was running on the GPU, overlapping work counted once (`pacing.gpu_busy_pct` is the whole GPU's, other apps and the screens' compositing included) — and milliseconds of GPU time per second by part of the app (`groups`: `show`, `control window`, `previews`, `outputs`, `wgpu (uploads, clears)`) and by pass (`parts`, e.g. `Mixer Composite Pass`, `Control Window Pass`, `Preview Encode Pass`, `Output 'LED' Swap Blit`), most first, each with how many times it ran a second (`runs_per_s`) — from when Metal says each pass ran on the GPU. Passes overlap on the GPU, so each moment is shared equally among the passes running then: the parts add up to `busy_pct`. Mind the GPU's clock: with less to do it slows down, and the same passes then take longer — compare counts, not only milliseconds. Every pass costs ~0.15 ms however little it draws. It also says where frames' time goes — each part of the frame loop's mean and worst (`pacing.parts`: the UI, the wait for the GPU, the mixer, each window's wait for a frame to draw into and its drawing) — and the last frames that ran long, part by part (`pacing.slow_frames`), which also go to the log as `[PACING] slow frame`. **Latency** (in the Target FPS menu of the top bar, kept with the show): **Low** — the default — waits for the GPU to finish each frame before starting the next; **Normal** lets the next frame be prepared while the GPU still draws this one, and **High** two ahead — each a frame later on screen (the menu says how much: +17 ms, +33 ms at 60 fps), but a GPU too busy to finish a frame in a frame's time (a 4K show on a laptop) no longer holds each frame up and stutters. Measured on a MacBook Pro: a 3840×2160 show spent 13–14 ms of each 16.7 ms frame waiting for the GPU on Low. If it stutters, pick Normal — or a lower resolution. The same is `{"SetFramesInFlight": {"frames": 2}}` to `POST /api/command`. (`SetFramePacing` no longer does anything: the show's own thread starts every frame on the screen's refresh.) A layer left on Auto render rate is drawn every frame unless the show's layers together run over a frame's time; only then is an expensive one drawn every other or third frame.

On a Mac, each frame is held on screen for a frame's time — Metal's own pacing (`presentDrawable:afterMinimumDuration:`) — whenever a screen's refresh starts frames (the rate a whole number of its refreshes: 60 on a 60 or 120 Hz screen). Measured, frames put up as soon as they were drawn let the display link skip one refresh in ten or twenty on a Mac mini (a slow slide stuttered, mostly with no output window open), and on a ProMotion MacBook they pulled the screen's own refresh about, for every app. An output window is held only on a screen refreshing faster than the show (a 120 Hz screen at 60 fps): on one at the show's rate — an LED wall or TV at 60 Hz for a 60 fps show — each frame is up one refresh anyway, and holding them, each output's frames now going up from a thread of the window's own, cost an LED wall 4–13 hitches a second under load against 0–2 without (measured on a MacBook). `{"SetFrameHold": {"on": false}}` turns it off; `{"SetControlVsync": {"on": true}}` makes the control window wait for its screen's refresh too. With every frame held, the control window's queue of frames waiting to be shown stays full once it has filled — each frame adds one as each shown takes one away — and the frame loop waits for its oldest to come off the screen (`ui acquire` in `pacing.parts`, ~10 ms of each frame on a MacBook; a longer queue only moved the wall). `pacing.control_queue.waiting` says how many control frames wait to be shown (3, measured). That wait no longer touches the outputs: skipping control frames to drain the queue made the control screen visibly stutter, so the outputs are drawn on a thread of their own instead (below). `pacing.display_link` lists the display link's last callbacks — when each came and the refresh it was stamped for — and counts the frames they asked for, drew and merged. In the windowed app the show runs on a thread of its own, apart from the control window: that thread, woken by the refresh (or the timer), takes in commands and inputs, draws the mixer and every output, and publishes the state the API reads; the control window lays itself out and draws on the main thread at its own pace, borrowing the show for a moment before (to read what it shows) and after (to apply what the operator did). It draws every frame while a hand is on it (and for a second after: the mouse, a key, a scroll, a file dragged — in it or a panel's window) or pictures move in the show (a clip or playlist playing, a camera or other live source, a generator or effect whose shader reads the clock, a take, fade or crossfade, the timeline running, anything modulated); about 30 times a second while only sound moves (the meters); and about 15 times a second while the show stands still — sooner when an animation in the window asks. Nothing is drawn under its panels. An empty show on an M5 Max with a 4K control window spent most of its GPU time redrawing the unchanged window 60 times a second, the show's 4K picture blitted beneath panels that hide it. A camera or a captured screen counts as moving only while new pictures come — one not plugged in or without signal, a screen not there, or a still PowerPoint slide (macOS sends a capture's frames only as it changes) stands still — and another input shown as a layer moves only with what it shows. `pacing.control_rate` says how often the window is drawn now and why: `motion` (`pictures`, `sound` or `still`), `why` (the first thing found moving, e.g. "'CAM' in 'Camera' moves"), `period_ms` (0 is every frame), `hands_on` and `widgets_ms`. `pacing.control_parts` and `pacing.control_slow_frames` judge each frame against the wait it was drawn for, so a still show's frames are not slow ones. A busy moment of UI, a menu or a dialog held open, or the control window's wait for its screen no longer costs the TV a frame — measured beforehand in a test app, a main thread frozen 25 ms every second cost 13–16 hitches in 19 s with the outputs drawn on it, and 0–1 with them on their own thread. `pacing.parts` is then the show thread's frame — with `ui hold` (the wait for the control window to hand the show back), `link to engine` (from the refresh's callback to the thread waking), `prepare`, `gpu wait`, `mixer`, `outputs`, `publish`, and `<window> acquire` (each output window's wait for its window to free a frame to draw into — made on a thread of the window's own, so neither the show nor the control window waits on a screen, nor do the other outputs, the recording and the stream when macOS holds on to one window's frames: a second at every app switch with Stage Manager on, measured on a MacBook; frames wait their turn in the order drawn, each with a copy of its picture of its own, so none goes up with the next frame's picture; a frame is let go only when more than three wait — the oldest — or to catch up with the screen: the frame right after a refresh the screen missed (its wait for a frame to draw into a refresh and a half or more), so the eye sees one jump where the screen stalled rather than a pause then and a skip later, or after two seconds a frame behind with no refresh missed, or once when its frames have sat queued a whole frame deep at its screen for two seconds — a second output on a clock apart from the show's settled a frame later than the first for good — each one let go counted in the output's `pace_skipped`) — and `pacing.control_parts` / `pacing.control_slow_frames` the control window's (`engine wait` is its wait for the show thread to finish a frame; `ui` laying out; `ui acquire` its wait for a frame of its own to draw into). To see the difference for yourself, start Nida with `NIDA_TEST_UI_STALL=25` — the control window freezes 25 ms once a second, the show left to its thread — or `=25:hold`, holding the show meanwhile as when both shared one thread, and compare the outputs' `on_screen.hitches`. A window's resize is the one exception: wgpu configures a surface only once the GPU has finished, and refuses the configure if another thread submitted meanwhile — the window keeps its old size, and the control window drew beyond it: three GPU errors ("Failed to wait for GPU to come idle before reconfiguring the Surface", then a scissor rect "not contained in the render target") at each step of a resize. So a configure — a window resized, moved to another screen, or changing how it presents — holds every submit back until it is done (`renderer::submit_gate`), and dragging the control window's edge during a show can cost the outputs a frame or two a step, as before the show had a thread of its own. Recordings and streams declare that rate to their encoder, and NDI advertises it to receivers, so a Nida running at 60 shows up as 60 in OBS or Studio Monitor rather than claiming some fixed number.

On a Mac, Nida also sets the screen the control window is on to the show's rate for as long as it runs — 60 Hz for a 60 fps show — and macOS puts it back when Nida quits (even if it crashes). A MacBook Pro's ProMotion screen at 120 Hz had macOS composite it twice for each show frame: with a screen captured at 60 fps, every screen then missed refreshes (an LED wall 8 times in 20 s, the control window 21), and none at 60 Hz. It is what video apps advise doing by hand (Apple, Resolume, vMix: every screen at the show's rate). Nida says so once when it does it, and never changes an output's screen — an LED processor or TV would go black for a moment on air. **Keep this screen at the show's rate** in the frame-rate menu turns it off; `{"SetControlScreenRate": {"on": false}}` to `POST /api/command` is the same.

Night Shift warms every screen of a Mac in the evening — its outputs too: an LED wall, a TV or a timer on HDMI turns yellowish with it. macOS warms all its screens alike and lets no app leave one out, so while an output window is on a screen Nida turns Night Shift off, as Control Center does — this Mac's own screen too — and gives it back when no output is on a screen any more, or Nida quits: on again if it is due on by then (within its schedule's hours, or between sunset and sunrise; with no schedule, at night, as it was turned on by hand). Not due on, it stays off until its schedule next turns it on, and so it does if Nida crashes. Coming on during the show — its schedule, at sunset — it is turned off again within seconds. An output on a Blackmagic device is not warmed and does not count. Nida says so once when it first turns Night Shift off. **Keep Night Shift off for the outputs** in the frame-rate menu, kept with the show, turns this off; `{"SetNightShiftOff": {"on": false}}` to `POST /api/command` is the same.

To see why a screen's frames come unevenly, send `{"TracePresents": {"secs": 20}}` to `POST /api/command`. For that long (60 s at most) Nida notes every frame's way to each screen, then writes it to a JSON file beside the log; the command's answer names the file, and the log says when it is written. The file has: when the engine started each frame, and for which refresh; each output window's frames, kept back to its screen's rate or handed to the window's own thread (and whether the oldest waiting was let go for it, or the frame let go to catch up), then taken, a frame to draw into had (or none), the copy submitted, presented, and when macOS showed it; the control window's frames, each with the engine frame it was drawn after; each camera's and captured screen's pick for every frame (the picture's capture time, the delay it was picked by, and how many newer waited); when each camera picture was ready to pick, and when macOS handed it over (the time before that is the camera's and macOS's, after it Nida's); and when each of the GPU's command buffers ran. Every time is on the host clock macOS's own times use (macOS).

**Uncapped** is the one case where a real number has to be invented, because a file or a stream has to state a frame rate and "as fast as possible" isn't one. Outputs declare 60 in that mode. If you are recording or streaming and you care about exact timing, set an explicit target rather than leaving it uncapped.

When the renderer misses a frame, recordings repeat the previous one to keep the file's running time honest, rather than producing a file that plays back faster than the session really ran.

## Per-Deck Resolution

Each deck has a **Resolution** setting in its Params (**Full**, **Half** or
**Quarter** of the render resolution; `PUT /api/decks/{uuid}/render-scale`).
A generator computes every pixel every frame, which makes it the costliest
thing in a 4K show; at Half it draws a quarter of the pixels and is scaled up
where the mix reads it, and soft procedural content looks almost the same.
The deck keeps its fraction when the render resolution changes, and the choice
is saved with the scene. Center scaling still means native size on air.

## Covered Layers

A layer under an opaque one is drawn only where it shows. Each frame Nida
works out which parts of each layer the opaque layers over it leave showing,
and draws the layer only there — its shader, its effects and its blend into
the channel. A background with a camera picture over most of it costs the
strips round the camera; one covered completely costs nothing at all. The
picture comes out the same to the pixel. (A 4K generator under a picture
covering 72% of the frame: 18 ms a frame drawn whole, 9 ms drawn round it.)

A layer covers what is under it where it is opaque for certain:

- at full opacity with the **Normal** blend mode;
- a camera, NDI or other live source (unless marked transparent), a solid
  colour, or an image without transparency — where it is fitted, so an image
  with bars at its sides covers only between them;
- cut by its **Crop** and placed by its **Transform**, when these are not
  rotated, wrapped or moved by modulation.

A clip or a generator never covers, since either can have transparency, and
neither does a layer with any other effect on it. The edges of a covering
picture are allowed a pixel or two, so the layer under it is drawn slightly
beyond them.

Some layers are drawn whole all the same:

- a layer shown on its own — on a bus, on Main, fed by itself to an output,
  an LED screen or a surface, or open in its settings;
- a layer with an effect that reads round about each pixel (blur, warps,
  glitches — colour corrections read only their own pixel and do not count),
  a multi-pass or compute generator, or analyzers attached;
- a still picture, which is drawn once and kept until it changes.

Where a layer is covered, its thumbnail in the Inputs list keeps the last
picture drawn there.

## Per-Deck Scaling

Every deck renders to a texture at the render resolution, regardless of its source's native resolution. ISF shaders are resolution-independent (they receive `RENDERSIZE` and render directly at the deck size). Video and image sources are scaled once on the GPU using the deck's **scaling mode**:

| Mode | Behavior |
|------|----------|
| **Fill** (default) | Scale to fill the deck, cropping edges if the aspect ratio differs |
| **Fit** | Scale to fit inside the deck, letterbox/pillarbox if the aspect ratio differs |
| **Stretch** | Stretch to exactly match deck dimensions (distorts mismatched aspect ratios) |
| **Center** | No scaling — center at native size, black borders if smaller, crop if larger |

Because scaling happens once on load rather than every frame, the compositing pipeline and all effect chains operate at a single consistent resolution.

**SVG is the exception, and deliberately so.** Vector art has no native pixel size, so instead of being decoded once and scaled, it is *redrawn* to fit the deck — and redrawn again whenever you change the render resolution. A logo that looks crisp while you build the set at 720p is re-rendered at 4K when you switch the master up for the show, rather than being magnified. The drawing's own proportions are preserved, so the scaling mode above applies to an SVG exactly as it does to a photograph.

## Performance Monitoring

All five readouts sit together in the **top bar**, after STREAM and REC, reading left to right:

```
[FPS] [🖥 GPU Load%] [CPU%] [RAM] [decks + VRAM estimate]
```
The top bar is the same in every screen layout, so however the panels are arranged the frame rate stays in view. Hover any of them for the full label.

FPS and GPU Load still have clickable drill-down popovers.

### FPS

Real-time frame-rate counter, color-coded: green (>55), yellow (30–55), red (<30). Click the **⏱ Render Pipeline** popover for per-channel stats (FPS, active deck count, render time in ms). A channel's FPS is its slowest moving layer: a still image is drawn again only when it changes, so it is left out rather than pulling the number down. Per-deck FPS is tracked with an exponential moving average over a 60-frame rolling window.

### GPU Load

Render load as a percentage of the frame budget: `(total_render_ms / 16.67ms) × 100%`, color-coded green (<50%), yellow (50–80%), red (>80%). The **🖥 GPU Details** popover shows device name, backend (Metal/Vulkan), driver info, device type (discrete/integrated), and render-load ms.

### CPU / RAM

CPU percentage and RAM usage (used/total), both color-coded. These are sampled once per second (not per frame) to avoid measurement overhead.

### Decks & VRAM

Deck count across every channel, followed by an estimate of the GPU memory their color targets hold at the current render resolution. It is deliberately an estimate: video decoders and camera buffers hold their own memory on top of it, and the number is there to answer "is this scene getting heavy" rather than to account for the card.

It matters most in [Arrangement Mode](15-arrangement.md), where every deck in a two-hour show holds its targets for the whole show whether or not a region is covering it. A deck the arrangement has put to sleep stops decoding but keeps its memory, so this number does not move when one does. Hover for the channel count and the target size.

---

[← Prev: Streaming, Recording & Network I/O](09-streaming-and-io.md) · [Home](README.md) · [Next: Shader Library →](11-shader-library.md)
