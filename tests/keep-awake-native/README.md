# Keep-awake compile and native tests

This small crate imports the actual application module and its existing tests.
It needs no FFmpeg, GPU, or capture SDK. Run from the repository root:

```sh
cargo check --manifest-path tests/keep-awake-native/Cargo.toml --tests
rustup target add x86_64-pc-windows-msvc
cargo check --manifest-path tests/keep-awake-native/Cargo.toml --target x86_64-pc-windows-msvc --tests
rustup target add aarch64-apple-darwin
cargo check --manifest-path tests/keep-awake-native/Cargo.toml --target aarch64-apple-darwin --tests
```

The Windows cross-check catches a missing `REASON` when combining the Windows
core and keep-awake patches. The constant's guard must include Windows; the
older core patch restricted it to Linux and macOS. If combining the patches
produces a conflict at that guard, keep all three platforms. Fork main already
has that combined guard.

Review validation on Linux: the combined source with the old two-platform
guard fails Windows compilation with `E0425` (`REASON` not found). With the
three-platform guard, Linux, Windows MSVC and Apple Silicon macOS compile
checks, including existing test bodies, pass. Pedantic Clippy passes on all
three targets. These are module checks, not full application builds.

Cross-compilation checks types and OS bindings; it does not test power policy.
On native Windows, run the existing lifecycle tests:

```sh
cargo test --manifest-path tests/keep-awake-native/Cargo.toml windows_ -- --test-threads=1
```

Then, in an elevated terminal with no other Nida instance running:

```sh
cargo test --manifest-path tests/keep-awake-native/Cargo.toml windows_os_lists_and_releases_power_requests -- --ignored --test-threads=1
```

On a normal Linux desktop or Mac, `cargo test` with this manifest runs the
existing OS assertion test. Linux needs a working systemd/logind session;
a cloud container cannot validate that behavior. Finally check an idle machine
keeps its displays and system awake while Nida runs and releases both on exit;
explicit user sleep and locking must remain possible.
