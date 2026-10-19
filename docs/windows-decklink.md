# Windows DeckLink video output

This change adds a Windows COM backend to Nida's existing Blackmagic output.
It is prepared separately against upstream Nida main at `c74d97e`, without
the Windows integration fork's other features. No PR has been opened for it.

## Behavior

Blackmagic Desktop Video must be installed for the application's architecture.
Without its registered COM server, discovery returns no devices and Nida keeps
running. Restart Nida after installing Desktop Video.

Output-capable devices and supported 8-bit YUV modes appear through the same
Blackmagic screen/output controls used on macOS. Devices are retained from
discovery notifications, with a fresh arrival identity after reconnect. Their
name, modes and identity are copied together under the discovery lock.

The output worker initializes a multithreaded COM apartment and keeps it until
the worker exits. Discovery belongs to its own permanent MTA thread, so the UI
thread's apartment and short-lived API requests cannot destroy it. Device and
frame references are released by the output worker before its apartment ends.
The existing independent conversion/device threads, bounded queues, claims
and disconnect recovery remain in use.

Windows uses the portable RGBA-to-UYVY converter. Video levels stay 16–235;
full levels use the same 1–254 clamps as the macOS implementation. HD uses
BT.709 and SD uses BT.601. NDI continues to use video levels. The existing two
full-range regressions now exercise the portable implementation too.

This is video output at the existing 8-bit YUV precision. Embedded audio,
DeckLink capture, HDR/10-bit output, scheduled/genlocked playback and automatic
profile switching are separate features. In particular, a device configured
for input-only use must be configured for output in Desktop Video first.

## Building

Use an MSVC Rust target and Visual Studio C++ Build Tools with a Windows SDK.
`build.rs` discovers `midl.exe` alongside the compiler environment from `cc`;
`MIDL` may select an explicit compiler path. There is no separate DeckLink SDK
installation or driver installation required to compile. The vendored IDL
files retain Blackmagic's licence; generated headers and GUIDs stay in OUT_DIR.

The rest of upstream Nida still has its existing Windows portability gaps.
This change does not import the integration fork's complete Windows core PR.
Use that portability work when validating the complete Windows application.

## Checks without hardware

The focused `DeckLink backend` workflow builds the actual module on Windows,
Linux and macOS. Its isolated harness copies the module, tests, portable converter,
NDI conversion, build script and SDK sources verbatim. Only thread-priority
requests and presentation tracing are no-ops. It does not validate application
integration, actual thread priority or real-time performance. The macOS thread
priority integration test is deliberately excluded from this no-op scheduling
harness; it remains part of the complete application suite.

On Windows the workflow also compiles and runs `windows_platform_test.cpp`
against the real COM APIs and MIDL-generated SDK types. It checks MTA setup,
rejection of an existing STA, independent MTA discovery from an STA caller,
and Unicode name conversion including truncation at UTF-8 boundaries. This
requires neither Desktop Video nor hardware.

To run the same harness locally with Python and Rust:

```text
python tests/decklink-native/prepare.py <temporary-directory>
cargo test --manifest-path <temporary-directory>/Cargo.toml -- --test-threads=1
```

The C++ SDK double exercises the real shared shim's discovery, mode filtering,
frame row copies, cleanup on failed starts, removal and stale-arrival rejection:

```sh
g++ -std=c++17 -Wall -Wextra -Werror -pthread -Itests/decklink-native tests/decklink-native/shim_test.cpp -o /tmp/nida-decklink-shim-test
/tmp/nida-decklink-shim-test
/tmp/nida-decklink-shim-test --missing-driver
```

## Hardware qualification still required

On a Windows PC, record the card/box, Desktop Video version, GPU and driver,
output connector, mode and receiving device. Verify discovery, start/stop and
restarts; check HD/SD colour bars and black/white patches in both level modes;
run sustained playback at the intended rate; unplug/reconnect and confirm
recovery; and check the existing source/bus routing and saved-show restore.
Audio must be checked separately through the show's actual audio path.

Do not use a successful SDK build or a fake-SDK test as evidence of a passing
physical SDI/HDMI output, sustained frame rate, colour measurement or sync test.

## Current validation

Checked code revision: `2ef4e044f4f93bb6a8ee60a75913bd73547c7e62`.
All three jobs in [this focused CI run](https://github.com/pingvis/Nida-Windows/actions/runs/39309216791)
passed. This document is recorded separately after the checked code revision.

- Windows Server 2022: real SDK/MIDL build, 19 module tests and production
  module Clippy pass. Real COM apartment and Unicode adapter tests pass.
- Ubuntu 24.04: 19 module tests, production module Clippy, new-file Rust
  formatting and behavioral C++ shim tests pass.
- macOS 14: real SDK/Accelerate build, 20 module tests and production module
  Clippy pass, including portable/full-range Accelerate parity. One priority
  integration test is excluded because scheduling is a no-op in this harness.
- One existing manual performance measurement is ignored on each platform.
- The C++ behavioral tests also pass locally under AddressSanitizer and
  UndefinedBehaviorSanitizer, with leak detection disabled for deliberately
  process-lifetime discovery storage.
- The complete application build/test matrix is not claimed green by these
  focused checks. Existing upstream Windows portability work is separate.
- No physical DeckLink hardware has been exercised for this change.

### Conversion timing

The existing ignored measurement now warms each converter and runs 100
1080p RGBA-to-UYVY conversions in the release profile. The final CI run reported:

| Hosted runner | Video levels | Full levels |
| --- | ---: | ---: |
| Windows Server 2022 | 8.48 ms/frame | 5.68 ms/frame |
| Ubuntu 24.04 | 8.88 ms/frame | 5.66 ms/frame |
| macOS 14 (Accelerate) | 0.95 ms/frame | 1.18 ms/frame |

These are per-conversion means on different hosted machines, not a platform
comparison, a before/after application benchmark, or a sustained output-rate
claim. The measurement includes no GPU readback, device transfer or display.
Video range still calls the existing NDI converter; full range previously had
no working portable implementation. SDK files on the review branch were also
checked by Git blob SHA against the pinned OBS sources and are unmodified.
