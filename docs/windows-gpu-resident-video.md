# Experimental Windows GPU-resident file playback

With the existing opt-in GPU-decoding preference enabled, file and playlist
decks try FFmpeg D3D12VA on the renderer's own DX12 device. Supported NV12/P010
pictures pass through a bounded three-slot GPU plane pool, WGSL colour conversion
and the compositor, with zero production picture downloads/uploads. Compressed
packets and small colour uniforms still use ordinary CPU memory.

The experimental label and default-off preference remain. Software override,
unsupported formats/backends, interlacing, ping-pong and native-error recovery
retain fallback behavior. Streams keep their existing decoder path. No saved
.varda format changes are introduced. API playback snapshots add gpu_resident
and primitive delivery diagnostics owned by the engine value layer.

## Lifetime and pacing

GPU copies use a private native queue and the renderer's submission gate. The
copy restores decoder subresources to COMMON. Only completed copies are
published; consuming command-buffer completion releases imported wrappers and
the plane lease. Full pools retain the pending picture, including while paused.
Seek generations reject old work. Format changes wait for old leases to finish.
Timeout retirement retains native dependencies and the registered event until
completion or device removal. No fourth slot or CPU-picture escape is added.

Windows timed decoder/engine waits use high-resolution waitable timers and
interruptible events, with existing coarse waits as safe API-failure fallback.
Deadline accounting avoids adding a full frame after over-budget work. Ordinary
forward playback can wake at the next PTS eligibility time without bypassing
existing source/effective-speed output caps. Pause/resume excludes paused preload
time. Reverse, chase/modulation, suspended and pending-pool-retry cases retain
bounded periodic behavior. The new clock policies are Windows-only; macOS/Linux
retain the inherited periodic and play-command clock behavior. Retained forward
seeking stays restricted to Windows, addressing the existing PR review.

One optional 1.5 ms readiness window is shared across an entire mixer video
update, including nested playlists/slides. It starts only on an eligible empty
mailbox, never restarts per deck, releases the mailbox lock while waiting, and
cancels when the seek generation changes. Parked CPU pictures stay owned.
Native D3D12 fence notification replaces ordinary 1 ms copy polling; an event
wake alone is never proof of completion. The worker rechecks the exact serial
and existing safety timeout. Registered event ownership follows in-flight work.

## Reproduction and evidence boundaries

Configure the normal Windows Rust/MSVC, FFmpeg 9 SDK and libclang prerequisites
from the build guide. CPU-only source/WGSL/ownership checks:

```powershell
cargo test --manifest-path tools/gpu-video-checks/Cargo.toml
cargo check --no-default-features --features screen-capture --lib --tests --benches
```

The hardware scripts require an explicitly supplied release test binary and
fixtures. Run GPU workloads serially, after checking for rendering/export/gaming.
Use scripts/measure-video.ps1 and scripts/measure-video-pacing.ps1 for controlled
release measurements, and scripts/analyze-video-pacing.py for opt-in JSONL traces.
NIDA_VIDEO_PACING_TRACE refuses to overwrite a trace. Traces contain monotonic
CPU timing and scalar PTS/serials, not pictures or actual scanout timestamps.
Legacy wait/sampling/readiness/copy-poll controls are test-only. Production apps
ignore the shader and copy-poll test overrides.

See [qualification](windows-gpu-resident-qualification.md) for integration results
and the distinction between tested fork binaries and this isolated upstream port.
GPU discarded-payload totals include warmup and cannot be compared directly with
the CPU path's zero GPU-payload-discard counter. Colour thresholds stay unchanged.
AMD/Intel qualification and removal of the experimental status remain deferred.
