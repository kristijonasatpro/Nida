# Running on Omarchy

Omarchy is Arch Linux with Hyprland. Verified on Omarchy (Hyprland 0.56, Lua
config) in the Try Omarchy VM on Apple silicon — ARM64 and a CPU renderer, so
this covers building, Wayland behaviour and output placement, not performance.

## Build

```sh
sudo pacman -S --needed base-devel cmake clang pkgconf rustup \
  vulkan-icd-loader vulkan-headers ffmpeg srt alsa-lib v4l-utils pipewire \
  shaderc wayland libxkbcommon libx11 libxrandr libxi gtk3
rustup default stable
cargo build --release --no-default-features --features screen-capture
```

`clang` is for bindgen (ffmpeg, PipeWire). libfreenect is AUR-only, hence no
`depth`; `html` (Servo) builds but takes a long time, so add it once the rest
works. Run with `--shader-dir <repo>/shaders` when not starting from the repo.

## Output windows and monitors

Pick each output's monitor in the Outputs panel, or over the API:

```sh
curl -X PUT localhost:8080/api/outputs/<output-uuid>/display \
  -H 'Content-Type: application/json' -d '{"monitor_name":"HDMI-A-1"}'
```

Monitor names are Hyprland's (`hyprctl monitors`) — connector names such as
`HDMI-A-1`, so always distinct. Outputs are matched to monitors by name alone;
on macOS, where two identical displays report the same name, they are told
apart by position (`Monitor #0 @ 1920,0`).
The choice is saved with the stage, and Nida holds it through the things that
happen on a show day:

- **Restart** — each output reopens fullscreen on its monitor.
- **Monitor not connected at start** (LED processor powered on after the
  laptop) — the output waits as a window on the operator's screen, the stage
  keeps its monitor, and the output moves there by itself when it connects.
- **Cable pulled mid-show** — Hyprland parks the output on a hidden workspace,
  so it does not cover the operator console, and returns it on reconnect.

Windows carry a Wayland app id — `nida` for the console, `nida-output` for
outputs — so Hyprland rules can target them. Install the rules Nida needs:

```sh
cp docs/conference/hyprland-nida.lua ~/.config/hypr/nida.lua
echo 'require("hypr.nida")' >> ~/.config/hypr/hyprland.lua
hyprctl reload config-only && hyprctl configerrors
```

Nida itself holds a `systemd-inhibit` idle + sleep lock for as long as it runs
(`systemd-inhibit --list` shows `Nida`), and hypridle honours it: no
screensaver, lock, screen-off or suspend mid-show. The rules back that up —
they stop Omarchy's screensaver and lock from starting while outputs are up,
and keep output windows from taking focus from the console. Without the second
rule, an output waiting for its monitor can open on another output's workspace
and knock that output out of fullscreen.

To rehearse without the real screens, `hyprctl output create headless LED`
adds a monitor named `LED`; `hyprctl output remove LED` unplugs it.
