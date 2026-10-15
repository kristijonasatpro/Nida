# Nida

A live switcher for conferences and events — vMix-style inputs, Preview and Program, buses, an audio mixer, streaming, recording, NDI and LED screens — with broadcast style routing for visuals underneath.

Nida is built on **[Varda](https://github.com/im-knots/varda)** by im-knots, a visual performance tool for VJs and installation artists, and keeps its MIT license (see [LICENSE](LICENSE)). Varda's community is on #varda at [Libera.Chat](https://web.libera.chat/#varda).

vMix and Resolume are trademarks of their owners. Nida is not affiliated with or endorsed by either; their names are used only to compare.

![Nida Performance Mode](docs/img/varda-performance.png)

![Nida Arrangement Mode](docs/img/varda-arrangement.png)

Nida applies broadcast video workflows to live visuals. Sources (video, cameras, generative shaders, streams, images) flow through a routing graph of decks, channels, and surfaces to reach outputs (projectors, streams, recordings). Instead of a clip-launch grid, you control what's live by adjusting opacity, blend modes, crossfaders, mute/solo, and effect chains. When a show has to land the same way every night, the same scene can be laid out against a timeline instead, and you can still take over manual control on any of it while it runs.

- **Routing matrix**: Sources > Decks > Channels > Mixer > Surfaces > Outputs. Any source to any output, split, branch, or sub-mix at every junction
- **Two views of one scene**: **Performance mode** is the mixer, played by hand with live performance controls. **Arrangement mode** is the same scene with a show timeline: regions say when each deck is up, automation curves draw any parameter over the timeline, cue points mark the moments worth jumping to, and the transport can chase SMPTE timecode. An edit in either view appears immediately in the other
- **Sources**: video (HAP GPU-native + ffmpeg), cameras, GLSL shaders (generators/filters, ISF-style metadata), NDI, SRT, HLS, DASH, RTMP/RTMPS, Syphon (macOS receive), screen and window capture, program taps of Nida's own output, images, and html/css/js sources (Servo)
- **Mixing**: N-channel compositing, A/B crossfader, per-deck opacity, 15 blend modes, linear-light HDR pipeline
- **Color**: 9 tonemap presets (ACES, AgX, Reinhard, Hable, etc.), 3D LUT support (.cube/.3dl) for color grading
- **Transitions**: ISF shader transitions between channels, deck auto-transitions (timer/clip-end triggers), multi-channel transition sequencer with beat-synced or timed triggers (seconds, minutes, hours). Allowing for quick automated live transitions or long running automated installations.
- **Effect chains**: 3-level hierarchy (deck > channel > master), drag-and-drop from library, reorderable
- **Modulation**: LFO, audio-reactive, ADSR, step sequencer, mod-on-mod chaining on any parameter
- **Analyzer preprocessors**: ML-powered analysis, sensor data interpretation, and more injected as shader data textures via ISF `PREPROCESSORS` declarations. CPU/GPU hybrid pipeline for effects that need structured data vanilla GLSL can't produce
- **Audio**: 2048-bin FFT, beat detection, bass/mid/treble bands, BPM with beat phase
- **Control**: MIDI, OSC, and HTTP API co-equal consumers of the same engine
- **Projection mapping**: 2D stage editor, polygon/circle surfaces, per-surface corner-pin warp, calibration cards, edge blending (Auto with precise polygon overlap detection, Manual per-edge)
- **Multi-output**: multiple windows, fullscreen on any display, headless outputs with surface assignments
- **Network I/O**: NDI, SRT, HLS, LL-HLS, DASH, and RTMP/RTMPS send/receive
- **Recording**: H.264, h.265, AV1, ProRes 422, HAP Q per-output
- **Presets**: save/load deck and channel presets with modulation recipes
- **Persistence**: full scene/venue/MIDI state saved and restored across sessions

Experimental:
- **Dome projection**: fisheye to equirectangular (360°) and cubemap (3D) rendering with configurable lens correction and chromatic aberration.
- **Surface overlap zones**: manual and auto-detect modes for precise edge blending.
- **Compute shader support**: ISF compute shaders for particle simulations, fluid dynamics, ect

## Install

### Download (macOS)

[Releases](https://github.com/kristijonasatpro/Nida/releases) have `Nida-<version>-macOS-arm64.dmg`, for Macs with Apple silicon and macOS 15.7.9 (Sequoia, up to date) or later. Open it and drag Nida to Applications. Nida is not signed by Apple, so the first time macOS says it cannot check it: open **System Settings → Privacy & Security** and click **Open Anyway** next to Nida — once. For NDI, install the free NDI runtime from [ndi.video](https://ndi.video) — the NDI SDK for Apple, or `brew install --cask libndi`.

The app carries its own FFmpeg (built with GPL parts), so the download as a whole is under the GPL version 3; the licences of everything inside are in `Nida.app/Contents/Resources/licenses`, and the exact source of every library inside is `Nida-<version>-third-party-source.tar.gz` beside the DMG. Nida's source stays MIT. A few of Varda's shaders, adapted from works whose licence is non-commercial or not stated, are left out of the download (they are in the source). `scripts/ci/build-macos.sh` makes the DMG, and the release workflow runs it for every `v*` tag.

### This computer (macOS)

Build the release and wrap it in an app, so it opens from the Finder or the Dock and macOS asks *Nida* for the camera, microphone, screen recording and local network:

```bash
cargo build --release --no-default-features --features screen-capture
scripts/make-local-app.sh --workspace ~/path/to/show --port 8080
open ~/Applications/Nida.app
```

The script puts `Nida.app` in `~/Applications`; its log is `~/Library/Logs/Nida/nida.log`.

So that macOS asks for the camera, microphone and so on only once, not after every build, make a signing certificate once: Keychain Access → **Certificate Assistant → Create a Certificate…**, name **Nida Local Signing**, Identity Type **Self Signed Root**, Certificate Type **Code Signing**. The script then signs every build with it (the first time, allow it to use the key: **Always Allow**). FFmpeg (`brew install ffmpeg-full`, for SRT; plain `ffmpeg` works without it) and, for NDI, the NDI runtime (`brew install --cask libndi`) are used from where Homebrew puts them.

### Upstream Varda releases

The [Varda releases](https://github.com/im-knots/varda/releases) (`Varda-macOS-universal.dmg`, `Varda-Linux-x86_64.tar.gz`, `Varda-Windows-x64.zip`) are the original project, without Nida's conference features.

## Getting Started
See the [manual](docs/README.md) for a complete guide to using Nida.

---

## Build from source

For bleeding edge features and bug fixes you can compile Nida from source.

Requires [Rust](https://rustup.rs/) (stable) and a GPU with Metal (macOS) or Vulkan (Linux) support.

### Ubuntu / Debian

```bash
sudo apt install build-essential cmake pkg-config libvulkan-dev libavcodec-dev libavformat-dev libavutil-dev libswscale-dev libswresample-dev libavdevice-dev libsrt-gnutls-dev libasound2-dev libv4l-dev libfreenect-dev libpipewire-0.3-dev libshaderc-dev libwayland-dev libxkbcommon-dev libx11-dev libxrandr-dev libxi-dev libgtk-3-dev
```

`libpipewire-0.3-dev` (screen capture on Wayland) and `libfreenect-dev` (depth
sensors) back default-on features, so omitting them fails the build rather than
quietly disabling anything. Both are in Ubuntu's `universe` component — run
`sudo add-apt-repository universe` first if `apt` cannot find them.

```bash
cargo build --release
./target/release/varda
```

#### Optional: NDI
NDI is proprietary and not available via apt. To enable NDI send/receive:
```bash
wget https://downloads.ndi.tv/SDK/NDI_SDK_Linux/Install_NDI_SDK_v6_Linux.tar.gz
tar -xzf Install_NDI_SDK_v6_Linux.tar.gz
sudo ./Install_NDI_SDK_v6_Linux.sh
sudo cp -P NDI\ SDK\ for\ Linux/lib/x86_64-linux-gnu/* /usr/local/lib/
sudo ldconfig
```
Without it, NDI features are silently disabled

### macOS

```bash
# FFmpeg. The plain `brew install ffmpeg` has no SRT; `ffmpeg-full` has
# (keg-only, beside it). When it is installed, the build links it
# (.cargo/config.toml) and Nida runs its `ffmpeg` for REC and STREAM.
brew install ffmpeg-full

# Optional: NDI
brew install --cask libndi
```

```bash
cargo build --release
./target/release/varda
```

### Run from source

```
cargo run --release
```


## CLI flags

```
varda [OPTIONS]

    --headless                Run without main UI window (API-only control)
    --port <PORT>             HTTP API port [default: 8080]
    --fps <FPS>               Target render FPS in headless mode [default: 60]
    --workspace <DIR>         Workspace root directory [default: cwd]
    --scene <PATH>            Scene file to load
    --stage <PATH>            Stage file to load
    --osc-port <PORT>         OSC input port (overrides osc.json)
    --osc-out <HOST:PORT>     OSC feedback target (repeatable)
    --no-osc                  Disable OSC
    --no-ndi                  Disable NDI
    --no-syphon               Disable Syphon (macOS)
    --no-screen-capture       Disable screen / window capture sources
    --shader-dir <DIR>        Extra shader library directory (repeatable)
```

`--shader-dir` is repeatable and layers on top of the built-in shader directories in a fixed precedence order. see [Shader Authoring → File Location](docs/12-isf-authoring.md#file-location) for the full hierarchy and override/hot-reload behavior.

> **Shader format:** Nida uses ISF's JSON metadata header with GLSL 450 (Vulkan) shader bodies, not ISF's GLSL ES dialect. Shaders from isf.video need a short mechanical port — see [Porting an ISF Shader](docs/12-isf-authoring.md#porting-an-isf-shader).

Headless mode runs the full engine without a UI window — controlled via the HTTP API. Outputs defined in `stage.json` auto-start on launch. Graceful shutdown on Ctrl-C or `POST /api/shutdown`.

```bash
# Headless on custom port with 30fps render
varda --headless --port 9090 --fps 30

# Separate workspace per venue
varda --workspace /shows/festival-2026

# Disable subsystems you don't need
varda --no-ndi --no-syphon --osc-port 7000

# Pull in extra shader folders (repeatable) — e.g. a USB stick + a show pack
varda --shader-dir /media/usb/shaders --shader-dir /shows/festival-2026/shaders
```

## HTTP API

The GUI and HTTP API are co-equal consumers of the same engine. The API runs on port 8080 (configurable with `--port`) alongside the GUI, or standalone in headless mode (`--headless`). Interactive docs at `/api/docs`, OpenAPI spec at `/api/openapi.json`. WebSocket at `/api/ws` streams state via JSON Patch (RFC 6902) deltas.


## Contributing

Bug reports, feature requests, and pull requests are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md) for the architecture overview, engineering practices, PR workflow, and [benchmarking harness](CONTRIBUTING.md#benchmarking).

## License
[MIT](LICENSE) for Nida's source, as for Varda's. The DMG under Releases also holds FFmpeg and the libraries it uses, so it is given away under the [GPL version 3](https://www.gnu.org/licenses/gpl-3.0.html); see `Nida.app/Contents/Resources/licenses`.