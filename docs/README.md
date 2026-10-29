# Nida

Nida is a free, open-source live production switcher for conferences and events, on the Mac, written in Rust. Cameras, slides, clips, NDI, screens and remote speakers become inputs built from layers. They go through Preview and Program — Cut, Fade, Merge and other transitions, with overlays on top — to the stage screens, LED walls and Blackmagic outputs, to recordings and to up to four streaming destinations, with an audio mixer for the room, the stream and the call. A rundown and timers keep the show in order. [Part II](#part-ii--running-a-conference-or-event) is all of that.

Nida is built on [Varda](https://github.com/im-knots/varda), and Varda's engine is still underneath: a broadcast-style signal matrix (Deck → Channel → Mixer → Surface → Output), ISF shaders and effect chains, modulation, projection mapping, MIDI, OSC and a REST/WebSocket API.

A scene can be worked two ways, and they are two views of the same thing rather than two projects. **Performance mode** is the mixer: channels, decks, faders, everything by hand. **[Arrangement mode](15-arrangement.md)** is that scene against show time, where regions decide when a deck is up, curves automate any parameter, cue points mark the moments worth returning to, and the transport can follow SMPTE timecode. Use either on its own or both together, and when both are running your hand wins: touch an automated control and it hands over until you re-arm it.

---

## Manual

### Part I — Getting Started

- **1. [Getting Started](01-getting-started.md)**
  - [Install](01-getting-started.md#install) — the Mac DMG, or build it yourself
  - [Workspace & Content](01-getting-started.md#workspace--content) — project layout, supported formats
  - [Build from Source](01-getting-started.md#build-from-source)
  - [UI Layout](01-getting-started.md#ui-layout) — panel map
  - [Load Content](01-getting-started.md#load-content) — Library, or drop files and folders in from Finder
  - [Adjusting a deck against the result](01-getting-started.md#adjusting-a-deck-against-the-result) — the channel result stays in the bottom bar with Cut/Fade to every bus; double-click a deck for its layer in Input Settings
  - [Output to a Display](01-getting-started.md#output-to-a-display)
  - [Audio Reactivity](01-getting-started.md#audio-reactivity) — input device, beat detection
  - [Next Steps](01-getting-started.md#next-steps)
  - [CLI Flags](01-getting-started.md#cli-flags)
- **2. [Core Concepts](02-concepts.md)**
  - [The Signal Flow](02-concepts.md#the-signal-flow) — Deck, Channel, Mixer, Surface, Output, routing flexibility, clip triggering without clip triggering
  - [Source Types](02-concepts.md#source-types) — ISF shaders, video, camera, NDI, SRT, HLS, DASH, RTMP, compute, Syphon, HTML
  - [Blend Modes](02-concepts.md#blend-modes) — 15 compositing modes
  - [Effect Chains](02-concepts.md#effect-chains) — deck, channel, and master FX levels
  - [Modulation](02-concepts.md#modulation) — LFO, audio bands, ADSR, step sequencer, analyzer
  - [Copy and Paste](02-concepts.md#copy-and-paste) — right-click or Cmd+C/V/D on decks, channels, effects
  - [The Nida workspace](02-concepts.md#the-nida-workspace) — scene vs stage, presets, asset handling

### Part II — Running a conference or event

New to Nida? Start with [Main mode](18-main.md), [Screen Layouts](20-layouts.md) and [Adding Inputs](23-adding-inputs.md).

- **16. [Audio Mixer](16-audio-mixer.md)** — vMix-style strips: buses, sends, solo, balance, Audio Auto
  - [Inputs](16-audio-mixer.md#inputs) — clips, sound files and input devices
  - [Audio Auto](16-audio-mixer.md#audio-auto) — a clip's sound on and off with its picture
  - [Outputs](16-audio-mixer.md#outputs) — buses: sound devices, faders, delay
  - [Headphones](16-audio-mixer.md#headphones) — listen bus and solo
  - [Recordings and streams](16-audio-mixer.md#recordings-and-streams) — which bus an output carries
  - [Lining up sound and picture](16-audio-mixer.md#lining-up-sound-and-picture)
- **17. [Streaming](17-streaming.md)** — the STREAM button: one encode to four RTMP/RTMPS destinations, red when healthy, blinking yellow on trouble
  - [Setting up](17-streaming.md#setting-up) — picture, sound, size, bit rate, hardware encoder
  - [Destinations](17-streaming.md#destinations) — URL, stream key, username and password
  - [While live](17-streaming.md#while-live) — states, reconnecting, stopping
  - [If a destination will not connect](17-streaming.md#if-a-destination-will-not-connect)
- **18. [Main mode](18-main.md)** — one channel on Main at a time, Space cuts, Enter takes the chosen transition, overlay channels on top; wide channel row with + Channel
  - [Taking a channel to Main](18-main.md#taking-a-channel-to-main) — NEXT and ON MAIN tallies, one input alone
  - [Overlays](18-main.md#overlays) — logo and lower third over whatever is on Main
  - [Saved, and safe from undo](18-main.md#saved-and-safe-from-undo)
- **19. [Recording](19-recording.md)** — the REC button: date-named files, hardware H.264/HEVC/ProRes, crash-safe MP4, red when recording, blinking yellow on trouble
- **20. [Screen Layouts](20-layouts.md)** — Classic, Broadcast (Preview / Program over input tiles) and VJ (layers as rows); drag panels anywhere and save your own
  - [Panels](20-layouts.md#panels) — what each one is
  - [Making your own](20-layouts.md#making-your-own) — drag, save, update, reset, lock
  - [Broadcast: Preview / Program and Inputs](20-layouts.md#broadcast-preview--program-and-inputs)
  - [VJ: Layers](20-layouts.md#vj-layers)
- **21. [Input Settings](21-input-settings.md)** — like vMix's double-click window: a channel's layers (hide, reorder, delete, add), each layer's position, crop, colour, keys and effects, and the channel's own — with layout templates
  - [Layers](21-input-settings.md#layers) — listed as Premiere lists tracks: the one in front at the top, 1 at the back
  - [A layer](21-input-settings.md#a-layer) — Position, Playback, Colour, Key, Effects
  - [The whole channel](21-input-settings.md#the-whole-channel) — General, Layout templates, Colour, Key, Effects
- **22. [LED Screens](22-led-screens.md)** — like Resolume's Advanced Output: several LED screens on one HDMI feed to the processor, each at its own pixels, showing Main or a bus of its own
  - [The feed](22-led-screens.md#the-feed) — on the processor's output, in a window, test card
  - [Screens](22-led-screens.md#screens) — size, place, what it shows, Fit / Fill / Stretch / Part
  - [One picture across several screens](22-led-screens.md#one-picture-across-several-screens)
  - [Over the API](22-led-screens.md#over-the-api)
- **23. [Adding Inputs](23-adding-inputs.md)** — like vMix's Add Input: clips, pictures, cameras and capture cards, NDI, desktop capture, streams, web pages, colours and sound devices, each a new input
  - [Cameras and capture cards](23-adding-inputs.md#cameras-and-capture-cards) — format (size and frame rate), the camera's own sound
  - [NDI](23-adding-inputs.md#ndi) — the runtime, sources on the network, their sound; sending NDI with an audio bus
  - [Desktop capture](23-adding-inputs.md#desktop-capture) — displays and windows, Screen Recording access
  - [Over the API](23-adding-inputs.md#over-the-api) — `POST /api/inputs`
- **24. [Zoom and Teams Calls](24-calls.md)** — a remote speaker over the ordinary call app: their sound straight from the app, their camera and slides as window inputs, Nida's programme back as the call's camera (through NDI Virtual Input, OBS or a capture device) with its sound, and a bus that keeps the call from hearing itself
  - [The call into Nida](24-calls.md#the-call-into-nida) — app sound, window capture
  - [Nida back to the call](24-calls.md#nida-back-to-the-call) — NDI Virtual Input
  - [The call must not hear itself](24-calls.md#the-call-must-not-hear-itself) — a mix-minus bus
- **25. [Rundown](25-rundown.md)** — the show in order: steps that change every screen of a wall together, take Main and start clips, each taken with one press, and GO for the next
  - [Making steps](25-rundown.md#making-steps) — + Step keeps what is on now; Duplicate; the step's window
  - [Running the show](25-rundown.md#running-the-show) — GO, Cut and Fade; all of it or none; going on by itself
  - [Clips, playlists and timers](25-rundown.md#clips-playlists-and-timers) — a sting on no screen, a talk's countdown
  - [Over the API](25-rundown.md#over-the-api) — `/api/rundown/…`
- **26. [Presentations](26-presentations.md)** — a conference's talks in one Slides input — PowerPoint decks, PDFs, web presentations, clips, pictures — in running order, each started with one press in the Presentations panel; a PiP or presenter view showing it follows the talk on
  - [The Presentations panel](26-presentations.md#the-presentations-panel) — ON AIR, NEXT, the running order
  - [Starting a talk](26-presentations.md#starting-a-talk) — what is on stays on until the new talk's picture is there
  - [PDF talks](26-presentations.md#pdf-talks) and [web talks](26-presentations.md#web-talks) — drawn by Nida; played by the Mac's WebKit off screen
  - [Checked before the show](26-presentations.md#checked-before-the-show) — linked videos not copied, fonts this Mac lacks, 4:3 decks, pages that need the internet
  - [PowerPoint](26-presentations.md#powerpoint) — a deck played by PowerPoint on this Mac, its transitions, animations and videos as PowerPoint plays them; as a talk or an input of its own
  - [Moving it on](26-presentations.md#moving-it-on) — ◀ ▶, PageDown / PageUp and B, black and white
  - [It stays up](26-presentations.md#it-stays-up) — a show ended in PowerPoint comes back; kept with the show
  - [Over the API](26-presentations.md#over-the-api) — `StartTalk`, `AddTalks`, `PresentationGo`, `slides` and `presentation` in the state
- **27. [The dot on output screens](27-privacy-dot.md)** — macOS's green, orange and purple capture dot, and Apple's switch that keeps it off LED walls and stage monitors

Parts III–VI are the engine underneath, from Varda, with Nida's additions.

### Part III — Performing

- **3. [Library Panel](03-library-panel.md)** — content browser
  - [Sections](03-library-panel.md#sections) — generators, effects, images, video, cameras, screen capture, taps, streams, HTML, presets
  - [Drag-and-Drop](03-library-panel.md#drag-and-drop) — drop onto a channel to create a deck
  - [Stream Sources](03-library-panel.md#stream-sources) — NDI/SRT/HLS/DASH/RTMP grouping, status indicators
  - [HTML Sources](03-library-panel.md#html-sources) — add web page URLs, drag-to-channel
  - [Cameras](03-library-panel.md#cameras) — rescan, resolution selector
  - [Screen Capture](03-library-panel.md#screen-capture) — displays and windows, rescan, permission prompt
  - [Taps](03-library-panel.md#taps) — Nida's own master program and channels as sources
- **4. [Performance & Automation](04-performance.md)**
  - [Video Playback](04-performance.md#video-playback) — loop modes, speed, scrub, hardware decoding on the Mac media engine (incl. AV1), HAP codecs, ping-pong cache
  - [Deck Auto-Transitions](04-performance.md#deck-auto-transitions) — timed/clip-end triggers, transition shaders
  - [Transition Sequences](04-performance.md#transition-sequences) — multi-step channel automation, easing, simultaneous sequences
  - [Undo / Redo](04-performance.md#undo--redo) — 50-level snapshot history
  - [Presets](04-performance.md#presets) — save/load deck and channel configurations
- **5. [Modulation & Audio Reactivity](05-modulation.md)**
  - [Creating Sources](05-modulation.md#creating-sources) — the ➕ buttons, source colors
  - [Modulation Sources](05-modulation.md#modulation-sources) — LFO, Audio, ADSR, Step Sequencer, Analyzer
  - [Routing](05-modulation.md#routing) — the 〰 assign button, live ghost indicator, stacking
  - [Modulator-on-Modulator](05-modulation.md#modulator-on-modulator) — recursive chaining up to 4 levels
  - [Audio System](05-modulation.md#audio-system) — FFT analysis, beat detection, ISF audio uniforms
- **6. [Control Surfaces & Macros](06-control-surfaces.md)**
  - [MIDI](06-control-surfaces.md#midi) — learn mode, APC Mini, multi-device
  - [OSC](06-control-surfaces.md#osc) — input/output, bidirectional feedback
  - [Keyboard Shortcuts](06-control-surfaces.md#keyboard-shortcuts) — learn mode, default bindings, param toggle
  - [Clock Synchronization](06-control-surfaces.md#clock-synchronization) — MIDI/OSC/audio/manual BPM, priority resolution
  - [Parameter Paths](06-control-surfaces.md#parameter-paths)
  - [Macros](06-control-surfaces.md#macros) — one knob/fader/button drives many parameters
    - [Creating a Macro](06-control-surfaces.md#creating-a-macro) — knob, fader, button cards
    - [Binding Targets](06-control-surfaces.md#binding-targets) — per-target range, curve, invert
    - [Buttons](06-control-surfaces.md#buttons) — momentary, toggle, trigger actions
    - [Mapping to MIDI / OSC / Keyboard](06-control-surfaces.md#mapping-a-macro-to-midi--osc--keyboard)
    - [Macro HTTP API](13-api.md#create-a-macro-and-bind-a-target)

### Part IV — Output & Display

- **7. [Outputs](07-outputs.md)**
  - [Creating an Output](07-outputs.md#creating-an-output) — windowed, recording, stream
  - [Output Targets](07-outputs.md#output-targets) — display selection, hot-plug, naming screens
  - [Levels](07-outputs.md#levels) — Full (0–255) or Video (16–235) per screen, for video gear
  - [Output Rotation](07-outputs.md#output-rotation) — 0/90/180/270 for portrait
  - [Source Routing](07-outputs.md#source-routing) — Master / Channel / Channels sub-mix / Deck
  - [Blackmagic Outputs](07-outputs.md#blackmagic-outputs) — an UltraStudio or DeckLink as one more screen (HDMI + SDI)
  - [Recording Outputs](07-outputs.md#recording-outputs)
- **8. [Projection Mapping](08-projection.md)**
  - [Basic Projection](08-projection.md#basic-projection) — drawing tools, surfaces, corner-pin warp, combine/multi-contour
  - [Advanced Projection](08-projection.md#advanced-projection) — multi-output edge blending (auto/manual), multi-channel routing, mesh warp
  - [Dome Projection](08-projection.md#dome-projection) 🧪 — domemaster, slicer presets, 3D preview navigation, content rotation
  - [Surface Auto-Detection](08-projection.md#surface-auto-detection) 🧪 — file import and live camera detection
- **9. [Streaming, Recording & Network I/O](09-streaming-and-io.md)**
  - [NDI](09-streaming-and-io.md#ndi)
  - [SRT](09-streaming-and-io.md#srt-secure-reliable-transport)
  - [HLS & DASH](09-streaming-and-io.md#hls--dash)
  - [Recording](09-streaming-and-io.md#recording)
  - [Stream Input Reliability](09-streaming-and-io.md#stream-input-reliability) — dedup, stall detection, reconnect
  - [HTML / Web Content](09-streaming-and-io.md#html--web-content)
  - [Syphon](09-streaming-and-io.md#syphon-macos)
  - [Screen & Window Capture](09-streaming-and-io.md#screen--window-capture) — displays and windows as decks, permissions, crop and rate, capturing Nida itself
  - [Program Tap](09-streaming-and-io.md#program-tap) — Nida's own output or another channel as a source, feedback loops
- **10. [Resolution, Settings & Monitoring](10-resolution-and-monitoring.md)**
  - [Render Resolution](10-resolution-and-monitoring.md#render-resolution) — presets, custom sizes
  - [Per-Deck Scaling](10-resolution-and-monitoring.md#per-deck-scaling) — fill, fit, stretch, center
  - [Performance Monitoring](10-resolution-and-monitoring.md#performance-monitoring) — FPS, GPU, CPU/RAM

### Part V — Reference

- **11. [Shader Library](11-shader-library.md)** — catalog of bundled generators, filters, transitions, and compute shaders
- **12. [Shader Authoring](12-isf-authoring.md)**
  - [Shader Types](12-isf-authoring.md#shader-types) — generator, filter, transition
  - [Metadata Format](12-isf-authoring.md#metadata-format) — JSON header, input types
  - [Built-in Uniforms](12-isf-authoring.md#built-in-uniforms) — TIME, RENDERSIZE, audio, phase accumulators
  - [Porting an ISF Shader](12-isf-authoring.md#porting-an-isf-shader) — dialect differences, the vertical flip, what isn't supported
  - [Multi-Pass Rendering](12-isf-authoring.md#multi-pass-rendering) — persistent buffers, feedback loops, substepping
  - [Compute Shaders](12-isf-authoring.md#compute-shaders) — `.comp` shaders, storage buffers, dispatch
  - [Hot-Reload](12-isf-authoring.md#hot-reload) — live editing workflow
  - [File Location](12-isf-authoring.md#file-location) — shader directory precedence hierarchy, `--shader-dir`
- **13. [HTTP API & Headless Mode](13-api.md)**
  - [Swagger UI](13-api.md#swagger-ui)
  - [Running a show remotely](13-api.md#running-a-show-remotely)
  - [Setting up a show with an AI agent](13-api.md#setting-up-a-show-with-an-ai-agent)
  - [Headless Mode](13-api.md#headless-mode)
  - [WebSocket](13-api.md#websocket)
  - [Common Patterns](13-api.md#common-patterns)
  - [Route Reference](13-api.md#route-reference)
- **14. [Frame Analysis & Preprocessors](14-frame-analysis.md)** — one analyzer engine, two paths
  - [What's Implemented](14-frame-analysis.md#whats-implemented) — brightness, face_detect, depth_sensor; planned types
  - [Analysis as Modulation](14-frame-analysis.md#analysis-as-modulation-performers) — brightness/face scalars drive any parameter
  - [Depth Sensor](14-frame-analysis.md#depth-sensor-performers) — near/far framing, per-deck controls
  - [Preprocessors](14-frame-analysis.md#preprocessors-concept) — structured data as shader textures
  - [Analyzer HTTP API](14-frame-analysis.md#analyzer-http-api)

### Part VI — Arranging

- **15. [Arrangement Mode](15-arrangement.md)** — the mixer turned ninety degrees, laid out against show time
  - [What Changes and What Doesn't](15-arrangement.md#what-changes-and-what-doesnt) — only the central area
  - [Anatomy](15-arrangement.md#anatomy) — transport strip, ruler, groups, lanes, navigation
  - [Cue Points](15-arrangement.md#cue-points) — mark a moment, walk the show with the arrows, and hit it from a pad in Performance mode
  - [Reordering Decks](15-arrangement.md#reordering-decks) — drag a lane header, shared with the mixer's order
  - [Regions](15-arrangement.md#regions) — draw, move, resize, fades, frame snapping
  - [Automation Lanes](15-arrangement.md#automation-lanes) — breakpoints, curve shapes, copy/paste
  - [Reusing a Shape](15-arrangement.md#reusing-a-shape) — one curve per parameter, copied between lanes
  - [Authority and Override](15-arrangement.md#who-is-driving-authority-and-override) — grabbing a control back, re-arm
  - [Chasing a clip to the show](15-arrangement.md#chasing-a-clip-to-the-show) — lock a video deck to the transport
  - [Idle Behaviour](15-arrangement.md#idle-behaviour) — what plays before the show starts
  - [Undo, Saving, and Load](15-arrangement.md#undo-saving-and-load) — scene version 7, memory
  - [Sleeping Clips](15-arrangement.md#sleeping-clips) — why a distant clip stops decoding, and what that changes

---

## Additional Resources

- **API Reference** — interactive Swagger UI at [`http://localhost:8080/api/docs`](http://localhost:8080/api/docs) (when Nida is running)
- **ISF Shader Format** — [isf.video](https://isf.video) (external)
- **Contributing** — architecture overview, entity/address scheme, engineering conventions, and the benchmarking harness live in [`CONTRIBUTING.md`](../CONTRIBUTING.md)
