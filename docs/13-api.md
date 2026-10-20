# HTTP API & Headless Mode

## Overview

Nida's GUI and HTTP API are co-equal consumers of the same engine. The GUI reads state snapshots and emits actions; the API reads the same snapshots and sends the same commands. Neither is an afterthought — they share identical engine contracts.

The API runs on **port 8080** by default (configurable with `--port`).

## Security & Network Trust Model

**Nida trusts the network.** The HTTP API has **no authentication** and binds to
**all interfaces** (`0.0.0.0`), and the OSC input (default port 9000) does the
same. CORS is intentionally permissive (see [CORS](#cors)). This is a deliberate
design choice for the live-performance and installation use cases: a dedicated
front-of-house or show/installation network where controllers, control panels,
and automation scripts talk to the engine without credential friction.

Run Nida only on a network you control. If you need it reachable from a wider or
untrusted network, put it behind your own boundary. Bind the machine to a private
interface, use a firewall or VPN, or front it with an authenticating reverse proxy.


## Swagger UI

Browse all routes interactively at:

```
http://localhost:8080/api/docs
```

The OpenAPI 3.1 spec describes every typed route: its parameters, path variables and request bodies. `POST /api/command` takes any engine command; the recipes in this chapter show its JSON.

The same spec is in the repository as [`docs/api/openapi.json`](api/openapi.json), for reading without Nida running. It is generated from the code, and a test (`tests/api_docs.rs`) fails when it falls behind.

The routes that change something answer `200` with `{"status": "ok"}`, adding `"uuid"` when they made something and `"data"` when they report something. The routes that read (`GET /api/state` and the like) answer with what they read. When the engine refuses a command, the answer is `400`, `404`, `500` or `503` with `{"error": "invalid_input" | "not_found" | "internal" | "unavailable", "message": "…"}`. A request that cannot be read (bad JSON, a missing field, no JSON content type) gets a plain-text `400`, `415` or `422`, and a few routes answer some failures in plain text.

## Running a show remotely

The calls a control panel or a script makes during a show. The UUIDs come from `GET /api/state`: the inputs are `.mixer.channels[]` (`name`, `uuid`), their layers `.mixer.channels[].decks[]`, the rundown's steps `.rundown.steps[]`, the audio mixer's sources `.audio_mix.sources[]`.

```sh
H='Content-Type: application/json'

# Main mode (Preview and Program), the transition (fade, merge, wipe, slide,
# zoom or iris) and how long it takes
curl -X PUT http://localhost:8080/api/main/mode -H "$H" -d '{"mode": "program"}'
curl -X PUT http://localhost:8080/api/main/transition -H "$H" -d '{"transition": "merge"}'
curl -X PUT http://localhost:8080/api/main/fade-time -H "$H" -d '{"secs": 1.0}'

# Take an input to Main with the transition, or cut to it
curl -X POST http://localhost:8080/api/main/take -H "$H" -d '{"source": {"Channel": "<input uuid>"}, "fade": true}'
curl -X POST http://localhost:8080/api/main/take -H "$H" -d '{"source": {"Channel": "<input uuid>"}}'

# An overlay: make the input one (once), then put it on ("on": false takes it off)
curl -X PUT http://localhost:8080/api/channels/<input uuid>/overlay -H "$H" -d '{"overlay": true}'
curl -X PUT http://localhost:8080/api/channels/<input uuid>/overlay-on -H "$H" -d '{"on": true}'

# GO: the rundown's next step
curl -X POST http://localhost:8080/api/rundown/go -H "$H" -d '{}'

# STREAM and REC (…/stop to stop)
curl -X POST http://localhost:8080/api/stream/start
curl -X POST http://localhost:8080/api/record/start

# A microphone off ("muted": false puts it back)
curl -X PUT http://localhost:8080/api/audio-sources/<source uuid>/mute -H "$H" -d '{"muted": true}'

# A count down or count up: start (or pause, reset), or a minute more (secs
# below zero take time off). A clock and a count to a time run by the clock.
curl -X POST http://localhost:8080/api/command -H "$H" \
  -d '{"ControlTimer": {"deck_uuid": "<timer layer uuid>", "action": "start"}}'
curl -X POST http://localhost:8080/api/command -H "$H" \
  -d '{"ControlTimer": {"deck_uuid": "<timer layer uuid>", "action": {"adjust": {"secs": 60}}}}'
```

## Setting up a show with an AI agent

Everything a show is made of can be built over this API, so an AI agent that can run commands (a coding agent, say) can set one up from a description. Give it:

- where Nida is: `http://<the Mac's address>:8080`, or `localhost` on the Mac itself;
- the API's description: [`docs/api/openapi.json`](api/openapi.json) here, or `/api/openapi.json` from the running Nida, and this chapter;
- what the event needs, for example: *"Set up tomorrow's conference in Nida: the FaceTime camera and the capture card, the slides laptop over NDI, our logo in the corner as an overlay, a bus for the stage screen, the stream to YouTube (I'll give you the key), recording in HEVC, and a rundown: Opening, Keynote, Panel, Break. Save it as Conference day 1."*

It builds the show the way the window does. `POST /api/channels` makes an input, a layer goes into it (`/api/channels/{uuid}/decks/camera`, or `…/decks/video`, `…/decks/image`, `…/decks/ndi`, `…/decks/srt` and so on) and `PUT /api/channels/{uuid}/name` names it. Then come the overlays, Main mode and the transition, `POST /api/buses`, `PUT /api/stream/destinations/{index}`, `PUT /api/record/options` and `POST /api/rundown/steps`. Last, it saves the show:

```sh
curl -X POST http://localhost:8080/api/command -H "Content-Type: application/json" \
  -d '{"SaveShowAs": {"folder": "/Users/me/Documents/Nida Shows/Conference day 1"}}'
```

`GET /api/state` shows what it has made. Screens and LED walls have routes too (`/api/outputs`).

It can use only what the Mac running Nida has: pictures and clips are files on that Mac, and cameras, capture cards, NDI sources and calls need the device, the runtime and macOS's permission there. The spec also lists the web-page and depth-sensor routes, which the DMG has no engine for.

We tried it on a headless Nida. An agent made three named inputs (a camera's stand-in, slides and a logo overlay), switched on Main mode with Merge, and added a bus for the stage screen, a stream destination, HEVC recording and a rundown step. It saved the show, and the show opened again with all of it.

The API has no password (see [Security](#security--network-trust-model)). Keep Nida on a network you trust, and give the agent a stream key only if you want it to set the stream up.

## Headless Mode

Run Nida without a UI window — the engine renders on a timer-driven loop, controlled entirely via the API:

```sh
varda --headless --port 8080 --fps 60
```

In headless mode:
- No main window is created (output windows for projectors can still be created via API)
- The render loop runs at `--fps` rate using sleep-based throttling
- All outputs defined in `stage.json` auto-start on launch — NDI sends, SRT streams, HLS/DASH outputs, recordings, and display outputs (fullscreen on connected monitors) all activate automatically
- Graceful shutdown on SIGTERM/SIGINT or `POST /api/shutdown`

This enables the installation use case: configure in windowed mode, save, then deploy headless. All streaming, recording, and network I/O features work identically with or without the UI.

## WebSocket

Connect to the WebSocket endpoint for real-time state streaming:

```
ws://localhost:8080/api/ws
```

**On connect:** Full `EngineState` JSON snapshot.

**Subsequent frames (~30fps):** JSON Patch (RFC 6902) deltas — only changes since the last update:

```json
[
  { "op": "replace", "path": "/mixer/crossfader", "value": 0.75 },
  { "op": "replace", "path": "/mixer/channels/0/decks/0/opacity", "value": 0.5 }
]
```

**Client → Server:** Send `EngineCommand` JSON messages — the same bodies `POST /api/command` takes — with an optional `"id"` field beside the command for response correlation:

```json
{ "id": "req-1", "SetCrossfader": 0.5 }
```

## Common Patterns

### Get engine state

```sh
curl http://localhost:8080/api/state
```

### Get scene structure (channels, decks, effects, UUIDs)

```sh
curl http://localhost:8080/api/scene
```

### Set crossfader position

```sh
curl -X PUT http://localhost:8080/api/mixer/crossfader \
  -H "Content-Type: application/json" \
  -d '{"position": 0.75}'
```

### Add a shader deck to a channel

```sh
curl -X POST http://localhost:8080/api/channels/<ch_uuid>/decks/shader \
  -H "Content-Type: application/json" \
  -d '{"shader_name": "Sine"}'
```

### Add an HTML deck to a channel

```sh
curl -X POST http://localhost:8080/api/channels/<ch_uuid>/decks/html \
  -H "Content-Type: application/json" \
  -d '{"url": "https://example.com/overlay.html"}'
```

### Capture a display or window as a deck

Targets are addressed by name, never by platform handle, and are matched against the last enumeration. Scan first if you are not sure what is available. See [Screen & Window Capture](09-streaming-and-io.md#screen--window-capture).

```sh
curl -X POST http://localhost:8080/api/devices/screen/scan
curl http://localhost:8080/api/library/screen

curl -X POST http://localhost:8080/api/channels/<ch_uuid>/decks/screen \
  -H "Content-Type: application/json" \
  -d '{"target": {"kind": "window", "app": "Safari", "title": "Dashboard"}, "rate": 24}'

# Capture settings are ordinary parameter paths
curl -X PUT http://localhost:8080/api/params \
  -H "Content-Type: application/json" \
  -d '{"path": "deck/<deck_uuid>/capture/rate", "value": 0.5}'
```

### Feed Nida's own output back in

```sh
curl -X POST http://localhost:8080/api/channels/<ch_uuid>/decks/tap \
  -H "Content-Type: application/json" \
  -d '{"source": {"kind": "master_program"}}'
```

A channel tap shows the channel's picture of this frame, a master tap — or a tap round a loop — the frame before. See [Program Tap](09-streaming-and-io.md#program-tap).

### Add an effect, then tweak it

`POST` returns the new effect's UUID in `{"status": "ok", "uuid": "..."}`. Every
later call uses that UUID, so it keeps working after the chain is reordered.

```sh
# Append to a deck's chain (also /api/channels/<ch_uuid>/effects, /api/master/effects)
curl -X POST http://localhost:8080/api/decks/<deck_uuid>/effects \
  -H "Content-Type: application/json" \
  -d '{"shader_name": "blur"}'

# Bypass / re-enable it
curl -X POST http://localhost:8080/api/effects/<effect_uuid>/toggle

# Drive one of its parameters as MIDI and OSC do (see the parameter paths below).
# The number is a control position, not the value: for an effect's parameter
# 0 is its min and 1 its max. /api/state shows the values themselves.
curl -X PUT http://localhost:8080/api/params \
  -H "Content-Type: application/json" \
  -d '{"path": "deck/<deck_uuid>/effect/<effect_uuid>/param/radius", "value": 0.5}'

# Set a parameter to a value as /api/state shows it
curl -X POST http://localhost:8080/api/command \
  -H "Content-Type: application/json" \
  -d '{"SetEffectParam": {"effect_uuid": "<effect_uuid>", "name": "radius", "value": 4.0}}'

# Put its parameters back to their defaults (all, or only those named)
curl -X POST http://localhost:8080/api/command \
  -H "Content-Type: application/json" \
  -d '{"ResetEffectParams": {"effect_uuid": "<effect_uuid>", "names": []}}'

# Remove it — this also clears any modulation assigned to its parameters
curl -X DELETE http://localhost:8080/api/effects/<effect_uuid>
```

### Build a transition sequence

Sequences are addressed by UUID; steps are addressed by position *within* their
sequence, which is how the sequencer itself refers to them.

```sh
# Create an empty sequence (returns its uuid)
curl -X POST http://localhost:8080/api/sequences

# Append a fade between two channels
curl -X POST http://localhost:8080/api/sequences/<seq_uuid>/steps/fade \
  -H "Content-Type: application/json" \
  -d '{"from_channel_uuid": "<ch_uuid_a>", "to_channel_uuid": "<ch_uuid_b>"}'

# Set that step's duration (steps are addressed by position within the sequence)
curl -X PUT http://localhost:8080/api/sequences/<seq_uuid>/steps/0/duration \
  -H "Content-Type: application/json" \
  -d '{"value": 4.0, "unit": "Seconds"}'

# Play it
curl -X POST http://localhost:8080/api/sequences/<seq_uuid>/play
```

### Start an auto-crossfade

```sh
curl -X POST http://localhost:8080/api/mixer/auto-crossfade \
  -H "Content-Type: application/json" \
  -d '{"target": 1.0, "duration_secs": 2.0, "easing": "Linear"}'
```

### Set tonemap mode

```sh
curl -X PUT http://localhost:8080/api/mixer/tonemap \
  -H "Content-Type: application/json" \
  -d '{"mode": "Aces"}'
```

Modes: `Bypass`, `Aces`, `Reinhard`, `ReinhardExtended`, `HableFilmic`, `Uchimura`, `Lottes`, `AgX`, `KhronosPbrNeutral`

### Load a 3D LUT

```sh
curl -X PUT http://localhost:8080/api/mixer/lut \
  -H "Content-Type: application/json" \
  -d '{"filename": "my-look.cube"}'
```

Place `.cube` or `.3dl` files in `.varda/luts/`. The filename is relative to that directory.

### Unload the active LUT

```sh
curl -X DELETE http://localhost:8080/api/mixer/lut
```

### Create a macro and bind a target

A macro drives many parameters from one control. Create it, add a target, then drive it live (or map `macro/<uuid>/value` to MIDI/OSC). See [Control Surfaces & Macros](06-control-surfaces.md#macros).

```sh
# Create a knob macro (returns its uuid)
curl -X POST http://localhost:8080/api/macros \
  -H "Content-Type: application/json" \
  -d '{"kind": "Knob"}'

# Add a target parameter
curl -X POST http://localhost:8080/api/macros/<uuid>/targets \
  -H "Content-Type: application/json" \
  -d '{"path": "deck/<deck_uuid>/effect/<fx_uuid>/param/scale"}'

# Drive the macro (fans out to all targets)
curl -X PUT http://localhost:8080/api/macros/<uuid>/value \
  -H "Content-Type: application/json" \
  -d '{"value": 0.75}'
```

### Send any engine command

```sh
curl -X POST http://localhost:8080/api/command \
  -H "Content-Type: application/json" \
  -d '{"SetCrossfader": 0.5}'
```

### Save the workspace

```sh
curl -X POST http://localhost:8080/api/workspace/save
```

Saving from the API preserves the editor layout in `stage.json` — it writes back
whatever the UI last had, rather than resetting panels and grid to defaults.
It answers once the files are written, so a backup taken after it copies what
was saved; the outputs do not wait for the disk meanwhile.

Other shows, as the 🎬 menu does them (each saves the show open first):

```sh
curl -X POST http://localhost:8080/api/command -H "Content-Type: application/json" \
  -d '{"SaveShowAs": {"folder": "/Users/me/Documents/Nida Shows/Day 2"}}'
curl -X POST http://localhost:8080/api/command -H "Content-Type: application/json" \
  -d '{"NewShow": {}}'          # or {"NewShow": {"folder": "…"}}
curl -X POST http://localhost:8080/api/command -H "Content-Type: application/json" \
  -d '{"OpenShow": {"folder": "/Users/me/Documents/Nida Shows/Day 1"}}'
```

`SaveShowAs` answers once the copy is written and Nida has moved to the new
folder — or with why not — while the outputs go on. Another asked for before
then is answered `Unavailable`. `SaveDeckPreset` and `SaveChannelPreset`
answer once the preset's file is written.

### Shut down (headless)

```sh
curl -X POST http://localhost:8080/api/shutdown
```

### Curve a surface edge (Bezier)

Toggle an edge between a straight line and a cubic bezier (`to_cubic: false` straightens it again):

```sh
curl -X PUT http://localhost:8080/api/surfaces/<uuid>/edge/convert \
  -H "Content-Type: application/json" \
  -d '{"edge_idx": 0, "to_cubic": true}'
```

### Move a curve-path anchor

```sh
curl -X PUT http://localhost:8080/api/surfaces/<uuid>/path/anchor \
  -H "Content-Type: application/json" \
  -d '{"anchor_idx": 1, "pos": [0.3, 0.4]}'
```

### Move a cubic control handle

`handle` is `C1` or `C2` (the two control points of the cubic segment):

```sh
curl -X PUT http://localhost:8080/api/surfaces/<uuid>/path/handle \
  -H "Content-Type: application/json" \
  -d '{"segment_idx": 0, "handle": "C1", "pos": [0.6, 0.7]}'
```

### Warp a surface (per-surface)

Warp is a property of the surface, keyed by its UUID. Move a corner-pin corner:

```sh
curl -X PUT http://localhost:8080/api/surfaces/{uuid}/warp/corner \
  -H "Content-Type: application/json" \
  -d '{"corner_idx": 0, "position": [0.1, 0.1]}'
```

Clear a surface's warp (back to native position):

```sh
curl -X POST http://localhost:8080/api/surfaces/{uuid}/warp/reset
```

### Subdivide a surface's warp into a mesh

Converts the surface's warp to a `cols` × `rows` grid, preserving the current
deformation (a corner-pin becomes a bilinear grid). Dimensions clamp to `[2, 64]`.

```sh
curl -X PUT http://localhost:8080/api/surfaces/{uuid}/warp/subdivisions \
  -H "Content-Type: application/json" \
  -d '{"cols": 3, "rows": 3}'
```

### Move a mesh warp point

Moves a single grid point (row-major) of the surface's mesh warp. No-op if the
surface's warp is not currently a mesh.

```sh
curl -X PUT http://localhost:8080/api/surfaces/{uuid}/warp/mesh-point \
  -H "Content-Type: application/json" \
  -d '{"row": 1, "col": 1, "position": [0.6, 0.4]}'
```

### Bind/unbind the warp to the surface shape (auto-warp)

When `bound` is `true` the warp auto-conforms to the surface outline; setting it
`false` unbinds and materialises the conforming warp for manual fine-tuning.

```sh
curl -X POST http://localhost:8080/api/surfaces/{uuid}/warp/bind \
  -H "Content-Type: application/json" \
  -d '{"bound": false}'
```

### Bezier (curved) warp

Convert the surface's warp into a smooth bezier patch grid (seeded from the
current warp so the shape is preserved), then edit anchors and tangent handles or
resize the control cage. Bezier editing is meaningful only while the warp is
unbound.

```sh
# Convert to a bezier patch grid
curl -X POST http://localhost:8080/api/surfaces/{uuid}/warp/bezier

# Move a control anchor (row-major grid coords)
curl -X PUT http://localhost:8080/api/surfaces/{uuid}/warp/anchor \
  -H "Content-Type: application/json" \
  -d '{"row": 0, "col": 0, "position": [0.15, 0.25]}'

# Move a tangent handle. horizontal=true → edge (r,c)→(r,c+1); false → (r,c)→(r+1,c).
# which=0 near the start anchor, 1 near the end anchor.
curl -X PUT http://localhost:8080/api/surfaces/{uuid}/warp/handle \
  -H "Content-Type: application/json" \
  -d '{"horizontal": true, "row": 0, "col": 0, "which": 0, "position": [0.33, 0.05]}'

# Resize the anchor cage (adds/removes control points; dims clamp to [2, 64])
curl -X PUT http://localhost:8080/api/surfaces/{uuid}/warp/cage \
  -H "Content-Type: application/json" \
  -d '{"cols": 3, "rows": 3}'
```

### Set an output's calibration mode

Switches an output between `Off`, `Projector` (full-frame test card), and
`Surfaces` (per-surface test cards through each warp).

```sh
curl -X PUT http://localhost:8080/api/outputs/<output_uuid>/calibration \
  -H "Content-Type: application/json" \
  -d '{"mode": "Projector"}'
```

## Addressing

Every write names its target entity by **UUID**, never by position. State snapshots
carry a `uuid` on each channel, deck, effect, output, surface, and sequence, so a
client reads the UUID once and uses it for every subsequent write.

This matters for correctness, not just style. A positional address is only valid
until something ahead of it moves: if a client resolves "deck 3", another client
removes deck 0, and the first client's write arrives afterwards, a positional write
lands on a different deck with no error. A UUID either resolves to the entity the
caller meant or fails with `404 Not Found`.

Integers survive in two roles, both of which are payload rather than address:

- **Reorder ordinals** — `PUT /api/channels/{channel_uuid}/decks/reorder` takes
  `from_idx` and `to_idx`, the positions being swapped.
- **Sequence step indices** — a step's position within its own sequence, which is
  how the sequencer itself addresses steps.

See [/spec/api-addressing.md] for the full rationale.

## Route Reference

For request and response schemas, see the Swagger UI at `/api/docs`.

Analyzer routes cover frame analysis (brightness, face detection, depth sensor); see
[Frame Analysis & Preprocessors](14-frame-analysis.md#analyzer-http-api) for request
bodies and workflow.

<!-- BEGIN GENERATED ROUTES -->

<!-- Generated from ApiDoc::openapi() by tests/api_docs.rs.
     Regenerate with: UPDATE_API_DOCS=1 cargo test --test api_docs -->

Writes address entities by UUID. Positional integers appear only as reorder
ordinals and sequence step indices — see [/spec/api-addressing.md].

### Analyzers

| Method | Path | Description |
|---|---|---|
| `POST` | `/api/decks/{deck_uuid}/analyzers` | Attach an analyzer to a deck (reference-counted). Body: `{"analyzer_type", "options"}`. |
| `DELETE` | `/api/decks/{deck_uuid}/analyzers/{analyzer_type}` | Release an analyzer; it stops when the last consumer detaches. |
| `GET` | `/api/library/analyzers` | Analyzer types a deck can attach, with their names and parameter descriptors. |

### Arrangement

| Method | Path | Description |
|---|---|---|
| `POST` | `/api/arrangement/cues` |  |
| `PUT` | `/api/arrangement/cues/{uuid}` |  |
| `DELETE` | `/api/arrangement/cues/{uuid}` |  |
| `PUT` | `/api/arrangement/idle` |  |
| `POST` | `/api/arrangement/lanes/{deck_uuid}` |  |
| `DELETE` | `/api/arrangement/lanes/{deck_uuid}` |  |
| `PUT` | `/api/arrangement/lanes/{deck_uuid}/collapsed` |  |
| `POST` | `/api/arrangement/lanes/{deck_uuid}/regions` |  |
| `PUT` | `/api/arrangement/lanes/{deck_uuid}/regions/{index}` |  |
| `DELETE` | `/api/arrangement/lanes/{deck_uuid}/regions/{index}` |  |
| `POST` | `/api/arrangement/rearm` |  |
| `POST` | `/api/arrangement/rearm/{param_key}` |  |

### Audio

| Method | Path | Description |
|---|---|---|
| `POST` | `/api/audio/close` |  |
| `POST` | `/api/audio/open` |  |
| `POST` | `/api/audio/scan` |  |

### Audio Mixer

| Method | Path | Description |
|---|---|---|
| `POST` | `/api/audio-buses` |  |
| `DELETE` | `/api/audio-buses/{bus_uuid}` |  |
| `PUT` | `/api/audio-buses/{bus_uuid}/delay` |  |
| `PUT` | `/api/audio-buses/{bus_uuid}/device` |  |
| `PUT` | `/api/audio-buses/{bus_uuid}/gain` |  |
| `PUT` | `/api/audio-buses/{bus_uuid}/mute` |  |
| `PUT` | `/api/audio-buses/{bus_uuid}/name` |  |
| `POST` | `/api/audio-devices/rescan` |  |
| `POST` | `/api/audio-inputs` |  |
| `POST` | `/api/audio-inputs/app` | Take an app's sound — a Zoom or Teams call's — as a source, straight from |
| `DELETE` | `/api/audio-inputs/{input_uuid}` |  |
| `PUT` | `/api/audio-monitor/device` |  |
| `PUT` | `/api/audio-monitor/gain` |  |
| `PUT` | `/api/audio-monitor/listen` |  |
| `PUT` | `/api/audio-sources/{source_uuid}/auto` | Audio Auto: the source's sound switches on as it goes on air and off as |
| `PUT` | `/api/audio-sources/{source_uuid}/balance` |  |
| `PUT` | `/api/audio-sources/{source_uuid}/delay` |  |
| `PUT` | `/api/audio-sources/{source_uuid}/follow` |  |
| `PUT` | `/api/audio-sources/{source_uuid}/gain` |  |
| `PUT` | `/api/audio-sources/{source_uuid}/mute` |  |
| `PUT` | `/api/audio-sources/{source_uuid}/sends/{bus_uuid}` |  |
| `PUT` | `/api/audio-sources/{source_uuid}/solo` |  |

### Auto Transitions

| Method | Path | Description |
|---|---|---|
| `PUT` | `/api/decks/{deck_uuid}/auto-transition/duration` |  |
| `PUT` | `/api/decks/{deck_uuid}/auto-transition/enabled` |  |
| `PUT` | `/api/decks/{deck_uuid}/auto-transition/play-duration` |  |
| `PUT` | `/api/decks/{deck_uuid}/auto-transition/shader` |  |
| `PUT` | `/api/decks/{deck_uuid}/auto-transition/trigger` |  |

### Buses

| Method | Path | Description |
|---|---|---|
| `POST` | `/api/buses` |  |
| `DELETE` | `/api/buses/{bus_uuid}` |  |
| `POST` | `/api/buses/{bus_uuid}/auto` |  |
| `PUT` | `/api/buses/{bus_uuid}/auto-duration` |  |
| `PUT` | `/api/buses/{bus_uuid}/blackmagic` |  |
| `PUT` | `/api/buses/{bus_uuid}/canvas` |  |
| `POST` | `/api/buses/{bus_uuid}/cut` |  |
| `PUT` | `/api/buses/{bus_uuid}/layers/{channel_uuid}` |  |
| `PUT` | `/api/buses/{bus_uuid}/name` |  |
| `PUT` | `/api/buses/{bus_uuid}/preview` |  |
| `PUT` | `/api/buses/{bus_uuid}/screen` |  |

### Channels

| Method | Path | Description |
|---|---|---|
| `POST` | `/api/channels` |  |
| `DELETE` | `/api/channels/{channel_uuid}` |  |
| `PUT` | `/api/channels/{channel_uuid}/blend-mode` |  |
| `PUT` | `/api/channels/{channel_uuid}/move` |  |
| `PUT` | `/api/channels/{channel_uuid}/name` |  |
| `PUT` | `/api/channels/{channel_uuid}/opacity` |  |
| `POST` | `/api/inputs` | Add an input (vMix's Add Input): a camera, NDI source, screen, stream or colour. |

### Clipboard

| Method | Path | Description |
|---|---|---|
| `POST` | `/api/clipboard/copy` |  |
| `POST` | `/api/clipboard/duplicate` |  |
| `POST` | `/api/clipboard/paste` |  |

### Decks

| Method | Path | Description |
|---|---|---|
| `POST` | `/api/channels/{channel_uuid}/decks/camera` |  |
| `POST` | `/api/channels/{channel_uuid}/decks/dash` |  |
| `POST` | `/api/channels/{channel_uuid}/decks/hls` |  |
| `POST` | `/api/channels/{channel_uuid}/decks/html` |  |
| `POST` | `/api/channels/{channel_uuid}/decks/image` |  |
| `POST` | `/api/channels/{channel_uuid}/decks/ndi` |  |
| `PUT` | `/api/channels/{channel_uuid}/decks/reorder` |  |
| `POST` | `/api/channels/{channel_uuid}/decks/rtmp` |  |
| `POST` | `/api/channels/{channel_uuid}/decks/shader` |  |
| `POST` | `/api/channels/{channel_uuid}/decks/solid` |  |
| `POST` | `/api/channels/{channel_uuid}/decks/srt` |  |
| `POST` | `/api/channels/{channel_uuid}/decks/syphon` |  |
| `POST` | `/api/channels/{channel_uuid}/decks/tap` |  |
| `POST` | `/api/channels/{channel_uuid}/decks/video` |  |
| `DELETE` | `/api/decks/{deck_uuid}` |  |
| `PUT` | `/api/decks/{deck_uuid}/blend-mode` |  |
| `PUT` | `/api/decks/{deck_uuid}/color` |  |
| `POST` | `/api/decks/{deck_uuid}/html/interactive` |  |
| `POST` | `/api/decks/{deck_uuid}/html/reload` |  |
| `PUT` | `/api/decks/{deck_uuid}/mask` | Set or clear a layer's own mask: a shape cut out of it, or kept alone. |
| `POST` | `/api/decks/{deck_uuid}/move` |  |
| `PUT` | `/api/decks/{deck_uuid}/mute` |  |
| `PUT` | `/api/decks/{deck_uuid}/opacity` |  |
| `PUT` | `/api/decks/{deck_uuid}/render-fps` |  |
| `PUT` | `/api/decks/{deck_uuid}/render-scale` |  |
| `PUT` | `/api/decks/{deck_uuid}/scaling-mode` |  |
| `PUT` | `/api/decks/{deck_uuid}/solo` |  |
| `PUT` | `/api/decks/{deck_uuid}/tap/source` |  |
| `PUT` | `/api/decks/{deck_uuid}/transparent` |  |

### Depth Sensors

| Method | Path | Description |
|---|---|---|
| `POST` | `/api/channels/{channel_uuid}/decks/depth` |  |
| `POST` | `/api/devices/depth/scan` |  |
| `GET` | `/api/library/depth` | Depth sensors discovered by the last scan, as name and sensor id. |

### Devices

| Method | Path | Description |
|---|---|---|
| `PUT` | `/api/devices/audio/enabled` |  |
| `POST` | `/api/devices/audio/scan` |  |
| `POST` | `/api/devices/cameras/scan` |  |
| `PUT` | `/api/devices/midi/enabled` |  |
| `POST` | `/api/devices/midi/scan` |  |
| `POST` | `/api/devices/ndi/scan` |  |
| `POST` | `/api/devices/syphon/scan` |  |
| `DELETE` | `/api/midi/mappings` |  |
| `POST` | `/api/midi/mappings/remove` |  |

### Effects

| Method | Path | Description |
|---|---|---|
| `POST` | `/api/channels/{channel_uuid}/effects` |  |
| `PUT` | `/api/channels/{channel_uuid}/effects/reorder` |  |
| `POST` | `/api/decks/{deck_uuid}/effects` |  |
| `PUT` | `/api/decks/{deck_uuid}/effects/reorder` |  |
| `DELETE` | `/api/effects/{effect_uuid}` |  |
| `PUT` | `/api/effects/{effect_uuid}/mask` |  |
| `POST` | `/api/effects/{effect_uuid}/reset` |  |
| `POST` | `/api/effects/{effect_uuid}/toggle` |  |
| `POST` | `/api/master/effects` |  |
| `PUT` | `/api/master/effects/reorder` |  |

### Library

| Method | Path | Description |
|---|---|---|
| `GET` | `/api/library/cameras` | Camera devices discovered by the last scan, as name and device id. |
| `GET` | `/api/library/effects` | Effect (filter) shaders available in the registry, with their registry indices. |
| `GET` | `/api/library/generators` | Generator shaders available in the registry, with their registry indices. |
| `GET` | `/api/library/monitors` | Connected monitors available as output displays, with name, index, and pixel size. |
| `GET` | `/api/library/ndi` | Names of the NDI sources discovered by the last scan. |
| `GET` | `/api/library/syphon` | Names of the Syphon servers discovered by the last scan. |
| `GET` | `/api/library/transitions` | Names of the transition shaders the crossfader can use. |

### Macros

| Method | Path | Description |
|---|---|---|
| `POST` | `/api/macros` |  |
| `DELETE` | `/api/macros/{uuid}` |  |
| `PUT` | `/api/macros/{uuid}/button/behavior` |  |
| `PUT` | `/api/macros/{uuid}/button/triggers` |  |
| `PUT` | `/api/macros/{uuid}/kind` |  |
| `PUT` | `/api/macros/{uuid}/modulation` | Drive a Knob/Fader macro's value from a modulation source. The source adds a |
| `DELETE` | `/api/macros/{uuid}/modulation` | Remove all modulation driving this macro's value. |
| `DELETE` | `/api/macros/{uuid}/modulation/{source_id}` | Remove only one modulation source from this macro's value, leaving any other |
| `PUT` | `/api/macros/{uuid}/name` |  |
| `POST` | `/api/macros/{uuid}/targets` |  |
| `PUT` | `/api/macros/{uuid}/targets/{target_idx}` |  |
| `DELETE` | `/api/macros/{uuid}/targets/{target_idx}` |  |
| `PUT` | `/api/macros/{uuid}/value` |  |

### Main

| Method | Path | Description |
|---|---|---|
| `PUT` | `/api/channels/{channel_uuid}/overlay` |  |
| `PUT` | `/api/channels/{channel_uuid}/overlay-on` |  |
| `PUT` | `/api/main/fade-time` |  |
| `PUT` | `/api/main/mode` |  |
| `POST` | `/api/main/take` |  |
| `PUT` | `/api/main/transition` | What a take to Main that is not a cut does: fade, merge, wipe, slide, zoom or iris. |

### Mixer

| Method | Path | Description |
|---|---|---|
| `POST` | `/api/mixer/auto-crossfade` |  |
| `POST` | `/api/mixer/beat-crossfade` |  |
| `PUT` | `/api/mixer/crossfader` |  |
| `PUT` | `/api/mixer/lut` |  |
| `DELETE` | `/api/mixer/lut` |  |
| `PUT` | `/api/mixer/tonemap` |  |
| `PUT` | `/api/mixer/transition` |  |

### Modulation

| Method | Path | Description |
|---|---|---|
| `POST` | `/api/modulation/adsr` |  |
| `POST` | `/api/modulation/analyzer` |  |
| `POST` | `/api/modulation/assign` |  |
| `POST` | `/api/modulation/audio-band` |  |
| `POST` | `/api/modulation/automation` |  |
| `POST` | `/api/modulation/clear` |  |
| `POST` | `/api/modulation/lfo` |  |
| `POST` | `/api/modulation/mod-on-mod` |  |
| `POST` | `/api/modulation/mod-on-mod/remove` |  |
| `POST` | `/api/modulation/step-sequencer` |  |
| `DELETE` | `/api/modulation/{uuid}` |  |
| `PUT` | `/api/modulation/{uuid}/adsr/attack` |  |
| `PUT` | `/api/modulation/{uuid}/adsr/decay` |  |
| `PUT` | `/api/modulation/{uuid}/adsr/release` |  |
| `POST` | `/api/modulation/{uuid}/adsr/release-gate` |  |
| `PUT` | `/api/modulation/{uuid}/adsr/sustain` |  |
| `POST` | `/api/modulation/{uuid}/adsr/trigger` |  |
| `PUT` | `/api/modulation/{uuid}/analyzer/smoothing` |  |
| `PUT` | `/api/modulation/{uuid}/audio/freq-range` |  |
| `PUT` | `/api/modulation/{uuid}/audio/gain` |  |
| `PUT` | `/api/modulation/{uuid}/audio/mode` |  |
| `PUT` | `/api/modulation/{uuid}/audio/noise-gate` |  |
| `PUT` | `/api/modulation/{uuid}/audio/preset` |  |
| `PUT` | `/api/modulation/{uuid}/audio/smoothing` |  |
| `PUT` | `/api/modulation/{uuid}/audio/source` |  |
| `PUT` | `/api/modulation/{uuid}/breakpoints` |  |
| `PUT` | `/api/modulation/{uuid}/lfo/amplitude` |  |
| `PUT` | `/api/modulation/{uuid}/lfo/bipolar` |  |
| `PUT` | `/api/modulation/{uuid}/lfo/frequency` |  |
| `PUT` | `/api/modulation/{uuid}/lfo/phase` |  |
| `PUT` | `/api/modulation/{uuid}/lfo/waveform` |  |
| `PUT` | `/api/modulation/{uuid}/step-seq/bipolar` |  |
| `PUT` | `/api/modulation/{uuid}/step-seq/count` |  |
| `PUT` | `/api/modulation/{uuid}/step-seq/interpolation` |  |
| `PUT` | `/api/modulation/{uuid}/step-seq/rate` |  |
| `PUT` | `/api/modulation/{uuid}/step-seq/steps` |  |
| `PUT` | `/api/modulation/{uuid}/step-seq/value` |  |
| `PUT` | `/api/modulation/{uuid}/timebase` |  |

### Outputs

| Method | Path | Description |
|---|---|---|
| `POST` | `/api/outputs` |  |
| `POST` | `/api/outputs/headless` |  |
| `POST` | `/api/outputs/led` |  |
| `DELETE` | `/api/outputs/{output_uuid}` |  |
| `PUT` | `/api/outputs/{output_uuid}/audio-bus` |  |
| `PUT` | `/api/outputs/{output_uuid}/calibration` |  |
| `PUT` | `/api/outputs/{output_uuid}/display` |  |
| `PUT` | `/api/outputs/{output_uuid}/edge-blend` |  |
| `PUT` | `/api/outputs/{output_uuid}/edge-blend-mode` |  |
| `PUT` | `/api/outputs/{output_uuid}/led` |  |
| `POST` | `/api/outputs/{output_uuid}/led/screens/{screen_uuid}/bus` |  |
| `PUT` | `/api/outputs/{output_uuid}/presentation` |  |
| `PUT` | `/api/outputs/{output_uuid}/source` |  |
| `POST` | `/api/outputs/{output_uuid}/start` |  |
| `POST` | `/api/outputs/{output_uuid}/stop` |  |
| `POST` | `/api/outputs/{output_uuid}/surfaces` |  |
| `DELETE` | `/api/outputs/{output_uuid}/surfaces/{surface_uuid}` |  |
| `PUT` | `/api/outputs/{output_uuid}/target` |  |
| `POST` | `/api/screens/identify` |  |
| `PUT` | `/api/screens/name` |  |

### Params

| Method | Path | Description |
|---|---|---|
| `POST` | `/api/decks/{deck_uuid}/params/reset` | Put a layer's own generator parameters back to their defaults. |
| `PUT` | `/api/params` | Set a parameter by path as MIDI and OSC do: a number is a control position. |

### Record

| Method | Path | Description |
|---|---|---|
| `GET` | `/api/record` |  |
| `PUT` | `/api/record/options` |  |
| `POST` | `/api/record/start` |  |
| `POST` | `/api/record/stop` |  |

### Rundown

| Method | Path | Description |
|---|---|---|
| `PUT` | `/api/rundown/fade-time` |  |
| `POST` | `/api/rundown/go` | Take the next step: the one after the step taken last, or the one set as next. |
| `POST` | `/api/rundown/hold` | Stop a step going on to the next by itself; the rundown stays until told. |
| `PUT` | `/api/rundown/next` | Make a step the one GO takes next, without taking anything. |
| `POST` | `/api/rundown/steps` |  |
| `DELETE` | `/api/rundown/steps/{step_uuid}` |  |
| `POST` | `/api/rundown/steps/{step_uuid}/capture` | Make the step's picture what is on now, for the parts it sets. |
| `POST` | `/api/rundown/steps/{step_uuid}/duplicate` | Put a copy of the step right after it; replies with the copy's UUID. |
| `PUT` | `/api/rundown/steps/{step_uuid}/main` |  |
| `POST` | `/api/rundown/steps/{step_uuid}/move` |  |
| `PUT` | `/api/rundown/steps/{step_uuid}/name` |  |
| `PUT` | `/api/rundown/steps/{step_uuid}/overlays/{channel_uuid}` |  |
| `PUT` | `/api/rundown/steps/{step_uuid}/plays/{deck_uuid}` |  |
| `PUT` | `/api/rundown/steps/{step_uuid}/screens/{bus_uuid}` |  |
| `POST` | `/api/rundown/steps/{step_uuid}/take` | Take a step, all of it or none; refused, the reply says why. |
| `PUT` | `/api/rundown/steps/{step_uuid}/then` |  |

### Scene

| Method | Path | Description |
|---|---|---|
| `GET` | `/api/scene` | Full scene: channels, crossfader, master effects, modulation, macros, sequences, and streams. |
| `GET` | `/api/scene/channels` | Every channel with its UUID, opacity, blend mode, decks, and effects. |
| `GET` | `/api/scene/channels/{channel_uuid}` | A single channel, addressed by UUID. |
| `GET` | `/api/scene/channels/{channel_uuid}/decks` | Every deck in one channel, addressed by channel UUID. |
| `GET` | `/api/scene/channels/{channel_uuid}/decks/{deck_uuid}` | A single deck, addressed by its channel's UUID and its own UUID. |
| `GET` | `/api/scene/macros` | Every macro control with its kind, current value, and parameter targets. |
| `GET` | `/api/scene/modulation` | Modulation sources, their current output values, and parameter assignments. |
| `GET` | `/api/scene/sequences` | Every transition sequence with its steps and playback state. |
| `GET` | `/api/scene/streams` | Active stream receivers with their URL, mode, and connection status. |

### Screen Capture

| Method | Path | Description |
|---|---|---|
| `POST` | `/api/channels/{channel_uuid}/decks/screen` |  |
| `POST` | `/api/devices/screen/permission` | Trigger the platform screen-recording permission request. |
| `POST` | `/api/devices/screen/scan` |  |
| `GET` | `/api/library/screen` | Displays and windows found by the last capture scan. |

### Sequences

| Method | Path | Description |
|---|---|---|
| `POST` | `/api/sequences` |  |
| `DELETE` | `/api/sequences/{sequence_uuid}` |  |
| `POST` | `/api/sequences/{sequence_uuid}/play` |  |
| `POST` | `/api/sequences/{sequence_uuid}/steps/fade` |  |
| `POST` | `/api/sequences/{sequence_uuid}/steps/goto` |  |
| `POST` | `/api/sequences/{sequence_uuid}/steps/move` |  |
| `POST` | `/api/sequences/{sequence_uuid}/steps/wait` |  |
| `DELETE` | `/api/sequences/{sequence_uuid}/steps/{step_idx}` |  |
| `PUT` | `/api/sequences/{sequence_uuid}/steps/{step_idx}/duration` |  |
| `PUT` | `/api/sequences/{sequence_uuid}/steps/{step_idx}/easing` |  |
| `PUT` | `/api/sequences/{sequence_uuid}/steps/{step_idx}/from-ch` |  |
| `PUT` | `/api/sequences/{sequence_uuid}/steps/{step_idx}/goto-target` |  |
| `PUT` | `/api/sequences/{sequence_uuid}/steps/{step_idx}/shader` |  |
| `PUT` | `/api/sequences/{sequence_uuid}/steps/{step_idx}/to-ch` |  |
| `POST` | `/api/sequences/{sequence_uuid}/stop` |  |
| `POST` | `/api/sequences/{sequence_uuid}/toggle` |  |

### Stage

| Method | Path | Description |
|---|---|---|
| `GET` | `/api/stage` | Full stage: surfaces, output windows, and connected monitors. |
| `POST` | `/api/stage/detect/camera` | POST /api/stage/detect/camera — detect contours from a camera snapshot. |
| `POST` | `/api/stage/detect/confirm` | POST /api/stage/detect/confirm — create surfaces from detected contours. |
| `POST` | `/api/stage/detect/dxf` | POST /api/stage/detect/dxf — detect contours from DXF data. |
| `POST` | `/api/stage/detect/image` | POST /api/stage/detect/image — detect contours from a raster image. |
| `POST` | `/api/stage/detect/svg` | POST /api/stage/detect/svg — detect contours from SVG data. |
| `GET` | `/api/stage/outputs` | Every output window with its target, activity, and surface assignments. |
| `GET` | `/api/stage/outputs/{uuid}` | A single output window, addressed by UUID. |
| `GET` | `/api/stage/surfaces` | Every surface with its geometry, warp, and source assignment. |
| `GET` | `/api/stage/surfaces/{uuid}` | A single surface, addressed by UUID. |

### State

| Method | Path | Description |
|---|---|---|
| `GET` | `/api/state/arrangement` | Arrangement state: authored lanes and regions, whether the arrangement holds authority, and which parameters a performer is holding by hand. |
| `GET` | `/api/state/audio` | Audio analysis state: level, band energies, FFT bins, detected BPM, and input devices. |
| `GET` | `/api/state/cameras` | Camera devices discovered by the last scan. |
| `GET` | `/api/state/clock` | Clock state: resolved BPM, beat phase, active source, and detected clock sources. |
| `GET` | `/api/state/depth` | Depth sensors discovered by the last scan. |
| `GET` | `/api/state/macros` | Every macro control with its kind, current value, and parameter targets. |
| `GET` | `/api/state/midi` | MIDI state: devices, mappings, and whether learn mode is active. |
| `GET` | `/api/state/mixer` | Mixer state: channels, crossfader position, master effects, active transition, and sequences. |
| `GET` | `/api/state/modulation` | Modulation state: sources, their current output values, and parameter assignments. |
| `GET` | `/api/state/ndi` | NDI runtime availability and the source names found by the last scan. |
| `GET` | `/api/state/outputs` | Output state: output windows, surfaces, and connected monitors. |
| `GET` | `/api/state/performance` | Render loop counters: measured FPS, total frames rendered, the configured target FPS, and `pacing` — what starts frames (a screen's refresh or the timer), the spread of frame intervals over the last five seconds with frames started late, and each output window's presents, pace-skipped and unavailable frames. |
| `GET` | `/api/state/registry` | Shader registry: generator and filter shader names with their indices. |
| `GET` | `/api/state/screen_capture` | Screen capture state: enumerated targets, permission state, backend, and active session count. |
| `GET` | `/api/state/streams` | Active stream receivers with their URL, mode, and connection status. |
| `GET` | `/api/state/surfaces` | Every surface with its geometry, warp, and source assignment. |
| `GET` | `/api/state/syphon` | Syphon framework availability and the server names found by the last scan. |
| `GET` | `/api/state/timecode` | Timecode diagnostics: every LTC and MTC input being listened to with its own position and run state, which one is driving the transport, and the current preference and LTC patch. |
| `GET` | `/api/state/transport` | Transport state: absolute position, timecode, run status, loop region, and follower count. |

### Stream

| Method | Path | Description |
|---|---|---|
| `GET` | `/api/stream` |  |
| `PUT` | `/api/stream/destinations/{index}` |  |
| `PUT` | `/api/stream/options` |  |
| `POST` | `/api/stream/start` |  |
| `POST` | `/api/stream/stop` |  |

### Streams

| Method | Path | Description |
|---|---|---|
| `POST` | `/api/streams/dash/library` |  |
| `DELETE` | `/api/streams/dash/library` |  |
| `POST` | `/api/streams/hls/library` |  |
| `DELETE` | `/api/streams/hls/library` |  |
| `POST` | `/api/streams/library` |  |
| `DELETE` | `/api/streams/library` |  |
| `POST` | `/api/streams/rtmp/library` |  |
| `DELETE` | `/api/streams/rtmp/library` |  |

### Surfaces

| Method | Path | Description |
|---|---|---|
| `POST` | `/api/surfaces/circle` |  |
| `POST` | `/api/surfaces/combine` |  |
| `POST` | `/api/surfaces/polygon` |  |
| `POST` | `/api/surfaces/rect` |  |
| `DELETE` | `/api/surfaces/{uuid}` |  |
| `PUT` | `/api/surfaces/{uuid}/circle/radius` |  |
| `PUT` | `/api/surfaces/{uuid}/circle/sides` |  |
| `PUT` | `/api/surfaces/{uuid}/content-mapping` |  |
| `PUT` | `/api/surfaces/{uuid}/contour-vertices` |  |
| `POST` | `/api/surfaces/{uuid}/convert-to-polygon` |  |
| `POST` | `/api/surfaces/{uuid}/duplicate` |  |
| `PUT` | `/api/surfaces/{uuid}/edge/convert` |  |
| `POST` | `/api/surfaces/{uuid}/flip-horizontal` |  |
| `POST` | `/api/surfaces/{uuid}/flip-vertical` |  |
| `POST` | `/api/surfaces/{uuid}/holes` |  |
| `DELETE` | `/api/surfaces/{uuid}/holes/{index}` |  |
| `PUT` | `/api/surfaces/{uuid}/move` |  |
| `PUT` | `/api/surfaces/{uuid}/name` |  |
| `PUT` | `/api/surfaces/{uuid}/output-type` |  |
| `PUT` | `/api/surfaces/{uuid}/path/anchor` |  |
| `PUT` | `/api/surfaces/{uuid}/path/handle` |  |
| `POST` | `/api/surfaces/{uuid}/punch` | "Make Hole" (8i.7): convert the surface identified by `uuid` into a cut-out |
| `POST` | `/api/surfaces/{uuid}/reorder` | Change a surface's global stacking order (8i.12): move it front/back/up/down |
| `PUT` | `/api/surfaces/{uuid}/rotate` |  |
| `PUT` | `/api/surfaces/{uuid}/scale` |  |
| `PUT` | `/api/surfaces/{uuid}/source` |  |
| `PUT` | `/api/surfaces/{uuid}/vertices` |  |
| `POST` | `/api/surfaces/{uuid}/vertices/insert` |  |
| `PUT` | `/api/surfaces/{uuid}/warp/anchor` |  |
| `POST` | `/api/surfaces/{uuid}/warp/bezier` |  |
| `POST` | `/api/surfaces/{uuid}/warp/bind` |  |
| `PUT` | `/api/surfaces/{uuid}/warp/cage` |  |
| `PUT` | `/api/surfaces/{uuid}/warp/corner` |  |
| `PUT` | `/api/surfaces/{uuid}/warp/handle` |  |
| `PUT` | `/api/surfaces/{uuid}/warp/mesh-point` |  |
| `POST` | `/api/surfaces/{uuid}/warp/reset` |  |
| `PUT` | `/api/surfaces/{uuid}/warp/subdivisions` |  |

### System

| Method | Path | Description |
|---|---|---|
| `PUT` | `/api/clock/manual-bpm` |  |
| `PUT` | `/api/clock/preference` |  |
| `POST` | `/api/command` | Applies any `EngineCommand` sent as JSON, and answers as every route does |
| `PUT` | `/api/domemaster/resolution` |  |
| `GET` | `/api/health` |  |
| `POST` | `/api/perf-profile` |  |
| `POST` | `/api/redo` |  |
| `PUT` | `/api/resolution` |  |
| `PUT` | `/api/resolution/auto` |  |
| `POST` | `/api/shutdown` |  |
| `GET` | `/api/state` | The full engine state snapshot. |
| `PUT` | `/api/target-fps` |  |
| `POST` | `/api/undo` |  |
| `POST` | `/api/workspace/load` |  |
| `POST` | `/api/workspace/save` |  |

### Timecode

| Method | Path | Description |
|---|---|---|
| `PUT` | `/api/timecode/ltc-input` | Name the audio input carrying LTC, or stop listening for it. |
| `PUT` | `/api/timecode/preference` | Choose which incoming timecode signal the transport follows. |

### Transport

| Method | Path | Description |
|---|---|---|
| `POST` | `/api/transport/cue/next` |  |
| `POST` | `/api/transport/cue/prev` |  |
| `POST` | `/api/transport/cue/{uuid}` | Locate to one named cue, leaving the transport running or stopped as it was. |
| `POST` | `/api/transport/locate` |  |
| `PUT` | `/api/transport/loop` |  |
| `POST` | `/api/transport/play` |  |
| `PUT` | `/api/transport/rate` |  |
| `PUT` | `/api/transport/record` | Arm or disarm automation recording. Arming from a stop also rolls the show, |
| `PUT` | `/api/transport/source` |  |
| `POST` | `/api/transport/stop` |  |

### Video

| Method | Path | Description |
|---|---|---|
| `PUT` | `/api/decks/{deck_uuid}/video/auto` | What a clip does by itself going on and off air: Auto Play, Auto Pause, |
| `DELETE` | `/api/decks/{deck_uuid}/video/in-out-points` |  |
| `PUT` | `/api/decks/{deck_uuid}/video/in-point` |  |
| `PUT` | `/api/decks/{deck_uuid}/video/loop-mode` |  |
| `PUT` | `/api/decks/{deck_uuid}/video/out-point` |  |
| `PUT` | `/api/decks/{deck_uuid}/video/seek` |  |
| `PUT` | `/api/decks/{deck_uuid}/video/speed` |  |
| `POST` | `/api/decks/{deck_uuid}/video/toggle-play` |  |
| `PUT` | `/api/decks/{deck_uuid}/video/transport-sync` |  |

<!-- END GENERATED ROUTES -->

## CORS

Permissive CORS is enabled on all routes:

```
Access-Control-Allow-Origin: *
Access-Control-Allow-Methods: GET, POST, PUT, PATCH, DELETE, OPTIONS
Access-Control-Allow-Headers: Content-Type, Authorization
```

Browser-based control panels work from any origin without configuration. This
pairs with the [trusted-network model](#security--network-trust-model): there is
no auth, so origin restrictions would add friction without a security benefit on
a trusted LAN. Do not expose the port to untrusted networks.

---

[← Prev: ISF Shader Authoring](12-isf-authoring.md) · [Home](README.md) · [Next: Frame Analysis & Preprocessors →](14-frame-analysis.md)
