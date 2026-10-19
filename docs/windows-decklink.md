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

The focused `DeckLink backend` workflow builds the actual module on Windows
and Linux. Its isolated harness copies the module, tests, portable converter,
NDI conversion, build script and SDK sources verbatim. Only thread-priority
requests and presentation tracing are no-ops. It does not validate application
integration, actual thread priority or real-time performance.

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

- Linux C++ behavioral tests pass with warnings treated as errors.
- Native Windows and isolated Rust module checks are pending.
- No physical DeckLink hardware has been exercised for this change.
