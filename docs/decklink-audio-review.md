# DeckLink audio review fixes

Local branch: `codex/decklink-audio-review-fixes`.
Base: PR #7's actual head, `1e6ccbfc6c4ddfd967f2ed2fd1984a6eece1b427`
on `pingvis/Nida-Windows:codex/windows-decklink-output`. Its upstream base was
`c74d97e838bc41f936e233873cd4c0132eb13c06`. The integration fork's main was
not used as a patch base. Investigation and validation were local. The user
subsequently authorized pushing the reviewed fixes to PR #7; this does not
authorize merging or posting the draft maintainer reply.

Both findings remained present at that head. Repository CONTRIBUTING guidance,
the PR diff/history, the maintainer's PR #7 comment and the Windows-support
summary on PR #1 were reviewed before editing. Commit `1a06baa` had already
changed source delay from three frames to two, while leaving three-frame audio
buffering. That was not a complete alignment fix.

## Scheduling contract and correction

With frame period D, startup schedules pictures at 0, D and 2D. Completion of
the first picture occurs after D; its replacement, using the current source
picture, is scheduled at 3D. The presentation lead is therefore 2D. Increasing
source delay to 3D would put sound one frame behind this replacement picture.

The problem was the independent audio high-water mark: filling 3D ahead while
mapping source sound with a 2D delay reads up to D into the future. A live tap
cannot provide that sound, so silence is scheduled and later-arriving samples
are discarded as late. The old native test supplied arbitrarily future samples
and could not expose this.

The scheduler now derives source delay from `video_queue_frames - 1` and caps
audio headroom/preroll at that delay. Existing bounds, partial/zero-write
handling, hardware-clock recovery and failure propagation are retained. Video
still uses the SDK's rational timestamps. Audio uses continuous absolute sample
positions with the lead rounded up once; it does not accumulate rounded samples
per frame. Only the existing 48 kHz stereo, signed 32-bit integer SDK format is
enabled. The scratch array still holds 2048 stereo sample frames and the slow-mode
audio headroom cap remains 24,000 sample frames.

A related boundary bug was found in the production Rust tap: converting a sample
delay to an Instant/nanoseconds and back could lose one sample. The delay is now
kept and subtracted as an integer sample-frame count. Tap-open time, source losses,
silence insertion and late-sample discard retain their existing timeline.

The native regression uses a live source that rejects requests beyond its
produced sample count. It checks initial preroll and 10,000 simulated video
completions at each of 24, 25, 30, 50, 60, 24000/1001, 30000/1001 and 60000/1001 Hz.
It also checks contiguous stereo samples, exact accumulated counts, partial SDK
writes, and video replacement timestamps. These are simulated SDK timelines,
not real-time output or physical synchronization measurements.

## Legacy configuration and explicit consent

Both live/API `OutputTarget::DeckLink` and persisted `OutputTargetConfig::DeckLink`
now have `embedded_audio`, deserializing to false when absent and omitted on save
when false. Old `audio_bus` values and `running: true` are retained but never
enable scheduled playback. New outputs also remain video-only despite inheriting
a default mixer bus.

The production startup policy ignores a DeckLink bus unless explicitly enabled.
The existing Audio selector reflects that effective policy, so a legacy bus is
shown as None until deliberately selected. A stopped output's SetOutputAudioBus
command enables audio on bus selection and disables it on None. Running changes
are rejected; changing the explicit target flag stops the sender instead of
passing through the live Levels-only shortcut. Mode/Levels changes preserve
consent. Explicit audio without a bus, with a missing bus, or rejected by the
SDK fails rather than silently falling back to video-only.

No audio settings/EQ, GPU decoding, UI layout, Windows portability, SDK files,
or Cargo.lock changes are included. NDI/recording bus policy is unchanged. The
same gating and native scheduler apply to Windows DeckLink and macOS UltraStudio.

## Local validation

- Original isolated Rust module: 24 passed, one ignored conversion measurement.
  Original behavioral C++ shim: passed, including missing-driver mode.
- New live-source native regression failed against the original scheduler during
  preroll because it requested future audio. It passes after the fix.
- The new sample-boundary test failed against the original production Pull with
  sound one sample early. It passes with integer delay handling.
- Updated production-source Rust harness: **66 passed, zero failed, one ignored**.
  This includes existing module/value tests, the two new tap regressions, and
  three new persistence tests. Real OutputConfig/OutputTargetConfig serde types,
  target conversion functions and routing policy are used, not a second model.
  Only priority/tracing and unavailable UUID generation are isolated; config
  fixtures provide explicit UUIDs and the harness panics if generation is needed.
- Persistence cases cover legacy Windows/Mac devices with valid, missing and
  absent buses, saved-running outputs, explicit true/false, API round trips,
  save/load target conversions, and preservation of source, levels and bus data.
- C++ tests passed with `-O2 -Wall -Wextra -Werror`, and with AddressSanitizer plus
  UndefinedBehaviorSanitizer. Ten injected initialization/start/write/count
  failure stages release callbacks/frames and permit a clean restart at time zero.
  Existing stop/late-callback, video-only, missing-driver and clock recovery checks
  remain in the suite. Leak detection was disabled for documented process-lifetime
  discovery storage.
- Python harness compilation, Rust syntax/DeckLink initializer checks and
  `git diff --check` passed. Workflow YAML/shell checks also passed. Syntax
  checking is not application type checking.
- Full application check was attempted with `cargo check --offline --locked
  --lib --no-default-features`; it stopped at the uncached `arc-swap` dependency.
  FFmpeg/ALSA development packages and CMake are also absent. The new
  application-state regression and strengthened mode/Levels UI tests
  have therefore **not** been compiled or executed in the complete app here.
- Production-source harness Clippy (`--lib -- -D warnings`) and focused rustfmt
  checks on audio.rs and decklink_audio_tests.rs passed. The new application
  state test was also formatted. The broader `--tests -- -D warnings` check
  reports one pre-existing `clippy::assert_is_empty` finding at the unchanged
  `src/internal/decklink/mod.rs:2001`; it was not suppressed or changed.
- Tool installation initially failed because automatic approval review hit the
  usage limit. After its stated reset time passed, the reviewed installation
  was retried successfully, allowing the lint/format checks above.

No current Windows/MSVC or macOS/SDK compilation was performed. Historical
platform CI results in windows-decklink.md are not results for this patch. No
hardware playback, lip sync, underrun behavior or sustained rate is validated.

## Remaining platform commands

Run from this branch's worktree, with current stable Rust plus rustfmt/Clippy,
Python 3.11+, and the native compiler. SDK interfaces are vendored; Desktop Video
is needed to use hardware, not to compile. Windows requires an MSVC Developer
PowerShell, Windows SDK/MIDL; macOS requires Xcode command-line tools.

Windows module/real SDK and COM checks:

```powershell
$decklinkCheck = Join-Path $env:TEMP 'nida-decklink-audio-review'
python tests/decklink-native/prepare.py $decklinkCheck
cargo test --manifest-path "$decklinkCheck/Cargo.toml" -- --test-threads=1 --skip an_outputs_threads_run_in_real_time
cargo clippy --manifest-path "$decklinkCheck/Cargo.toml" --lib -- -D warnings
./tests/decklink-native/check-windows-platform.ps1
```

macOS module/real SDK and shared behavioral checks:

```sh
decklink_check_dir=$(mktemp -d)
python3 tests/decklink-native/prepare.py "$decklink_check_dir"
cargo test --manifest-path "$decklink_check_dir/Cargo.toml" -- --test-threads=1 --skip an_outputs_threads_run_in_real_time
cargo clippy --manifest-path "$decklink_check_dir/Cargo.toml" --lib -- -D warnings
clang++ -std=c++17 -O2 -Wall -Wextra -Werror -pthread -Itests/decklink-native tests/decklink-native/shim_test.cpp -o "$decklink_check_dir/shim-test"
"$decklink_check_dir/shim-test"
"$decklink_check_dir/shim-test" --missing-driver
```

Full application integration, on each platform after installing the documented
FFmpeg development/runtime and other native prerequisites; a working GPU adapter
is required for the app/UI checks:

```sh
cargo check --locked --lib --no-default-features --features screen-capture
cargo test --locked --no-default-features --features screen-capture --lib persistence::decklink_audio_tests -- --test-threads=1
cargo test --locked --no-default-features --features screen-capture --lib blackmagic_audio_is_explicit_and_cannot_change_under_a_running_sender -- --test-threads=1
cargo test --locked --no-default-features --features screen-capture --lib a_blackmagic_outputs_ -- --test-threads=1
cargo test --locked --no-default-features --features screen-capture --test ui_integration -- --test-threads=1
cargo clippy --locked --lib --no-default-features --features screen-capture -- -D warnings
rustfmt --edition 2021 --check src/internal/decklink/audio.rs src/internal/persistence/decklink_audio_tests.rs
git diff --check
```

Record baseline/native dependency failures separately. Do not import unrelated
Windows portability changes to make this review branch green, or treat an app
test skipped for lack of an adapter as validation of its behavior.

## Physical hardware checklist — Windows and Mac

1. Record OS, DeckLink/UltraStudio model, Desktop Video version, SDK/build commit,
   connector, mode and receiver. Test each platform with its actual device.
2. Load a legacy show with saved audio_bus and a running Blackmagic output:
   verify synchronous video-only behavior, no unexpected audio initialization,
   unchanged Main/bus routing, and safe restore even with the saved bus removed.
3. Explicitly select audio while stopped, start from cold, and inspect the first
   seconds of preroll/startup using distinct L/R tones and a flash/click clip.
   Verify channel order, initial silence, no lost first sound, levels/mute/delay,
   and record measured audio/video offset at the receiver.
4. Run 24/25/30/50/60 and 23.976/29.97/59.94 where the device supports them.
   Sustain output for at least 30 minutes at the intended mode; measure lip sync
   near the start/end and capture audio discontinuities, silence and drift.
5. Stop/start repeatedly; change mode while stopped; toggle None/bus; save/reload
   both opt-in and video-only shows. Check old consent is not invented and new
   consent is retained. Levels changes must preserve the current audio choice.
6. Test missing bus, driver/device unavailability, unsupported audio setup,
   unplug/reconnect, source underflow/recovery and load-induced callback stalls.
   Confirm errors/cleanup/restart; specifically measure resynchronization after
   clock recovery. Portable tests do not prove this recovery's physical sync.

## Draft maintainer reply (not posted)

Thanks for catching both issues. I reproduced the future-audio request and kept
the actual two-frame video presentation lead, deriving audio preroll/headroom
from the same queue contract instead of independently buffering three frames.
I also added a default-off embedded_audio flag, so an old saved audio bus cannot
enable the new path on either Windows DeckLink or Mac UltraStudio.

Portable scheduling/configuration regressions and sanitizer checks pass locally.
Current Windows/macOS application builds and physical startup, sustained sync and
restart checks are still outstanding. This remains local fork correctness work;
I understand Windows support is deferred upstream and am not assuming acceptance.
