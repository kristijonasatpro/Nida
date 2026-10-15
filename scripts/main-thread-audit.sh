#!/bin/zsh
# Lists every place in Nida's own code that calls a winit function which, on
# macOS, waits for the main thread when called from another (winit runs it
# there with dispatch_sync). The engine runs on a thread of its own and must
# never make such a call: the main thread may be waiting for the engine, and
# the two would wait on each other for ever (see
# src/usecases/ui/runner/engine_thread.rs and `OsWindow`).
#
# Each caller listed must run on the main thread, inside an
# `OsWindow::on_main` block, or behind an `on_main_thread()` check.
#
# Usage: scripts/main-thread-audit.sh   (builds the debug binary first)
set -e
cd "$(dirname "$0")/.."
CARGO=${CARGO:-$(command -v cargo || echo "$HOME/.cargo/bin/cargo")}
if ! PATH=/opt/homebrew/opt/ffmpeg-full/bin:$PATH "$CARGO" build --no-default-features \
    --features screen-capture --offline >/dev/null 2>&1; then
    echo "The debug build failed: run cargo build to see why" >&2
    exit 1
fi
BIN=target/debug/varda
TMP=$(mktemp -d)
objdump -d --no-show-raw-insn "$BIN" 2>/dev/null | awk '
/^[0-9a-f]+ <.*>:$/ { cur=$2; next }
/\tbl\t/ && (/13MonitorHandle/ || /6Window/) && cur ~ /5varda/ { match($0, /<[^>]*>/); print cur " -> " substr($0, RSTART, RLENGTH) }
' | sort -u > "$TMP/calls.txt"
python3 - "$TMP/calls.txt" <<'PY'
import re, subprocess, sys
pairs = []
for line in open(sys.argv[1]):
    m = re.match(r"<(.+?)>:? -> <(.+?)>", line.strip())
    if m:
        pairs.append((m.group(1), re.sub(r"\+0x[0-9a-f]+$", "", m.group(2))))
names = sorted({n for p in pairs for n in p})
out = subprocess.run(["xcrun", "llvm-cxxfilt"], capture_output=True, text=True,
                     input="\n".join(n[1:] if n.startswith("__R") else n for n in names)).stdout.split("\n")
demangled = dict(zip(names, out))
# winit's Window methods all wait for the main thread off it (window.rs
# `maybe_wait_on_main`); a monitor's size, position and scale factor do too.
waits = re.compile(r"winit::window::Window>::(?!default_attributes)([a-z_]+)"
                   r"|MonitorHandle>::(size|position|scale_factor)")
found = set()
for caller, callee in pairs:
    c = re.sub(r"::h[0-9a-f]{16}", "", demangled.get(caller, caller))
    d = re.sub(r"::h[0-9a-f]{16}", "", demangled.get(callee, callee))
    m = waits.search(d)
    if "varda" in c and m:
        api = m.group(0).replace("winit::window::Window>::", "Window::").replace(">::", "::")
        found.add((api, re.sub(r"\{closure#\d+\}", "{closure}", c)))
for api, caller in sorted(found):
    print(f"{api:30} <- {caller}")
PY
rm -rf "$TMP"
