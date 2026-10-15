#!/usr/bin/env python3
"""Read a present trace (`{"TracePresents": {"secs": 20}}`, see docs/10) and
say, for each output window and the control window, how evenly its frames
came up and what each late one went through.

    scripts/present-trace.py ~/Library/Logs/Nida/present-trace-<stamp>.json [--frames N]

Per window: frames presented and shown, hitches (up longer than 1.5x the
screen's refresh or the show's frame, whichever is longer), frames handed
over but replaced, kept back, or with no frame to draw into; the spread of
each step (hand-over -> taken -> acquired -> submitted -> presented ->
shown); where in the screen's refresh each step fell; and the frames around
each hitch. For the control window: how many engine frames each UI frame
moved on (0 = the picture repeated, 2+ = one skipped).
"""

import json
import statistics
import sys
from collections import defaultdict

STEPS = ("handed", "taken", "acquired", "submitted", "presented")


def ms(x):
    return f"{x * 1000:7.2f}"


def spread(values):
    if not values:
        return "-"
    values = sorted(values)
    pick = lambda q: values[min(len(values) - 1, int(q * len(values)))]
    return (
        f"mean {statistics.mean(values) * 1000:6.2f}  p50 {pick(0.5) * 1000:6.2f}  "
        f"p95 {pick(0.95) * 1000:6.2f}  max {values[-1] * 1000:6.2f} ms"
    )


def load(path):
    with open(path) as f:
        return json.load(f)


def by_window(events):
    windows = defaultdict(lambda: {"frames": defaultdict(dict), "shown": [], "kept": 0, "replaced": 0, "let_go": 0, "noframe": 0, "sampled": []})
    engine = []
    gpu = []
    for e in events:
        step = e["step"]
        name = e.get("output", "")
        if step == "frame":
            engine.append(e)
        elif step == "gpu":
            gpu.append(e)
        elif step == "shown":
            windows[name]["shown"].extend(t for t in e["times"])
        elif step == "kept_back":
            windows[name]["kept"] += 1
        elif step == "let_go":
            windows[name]["let_go"] += 1
        elif step == "replaced":
            windows[name]["replaced"] += 1
            windows[name]["frames"][e["frame"]]["replaced_one"] = e["at"]
        elif step == "no_frame":
            windows[name]["noframe"] += 1
            windows[name]["frames"][e["frame"]]["no_frame"] = e["at"]
        elif step == "sampled":
            windows[name]["sampled"].append((e["at"], e["frame"], e["engine_frame"]))
            windows[name]["frames"][e["frame"]]["sampled"] = e["at"]
            windows[name]["frames"][e["frame"]]["engine_frame"] = e["engine_frame"]
        elif step in STEPS:
            windows[name]["frames"][e["frame"]][step] = e["at"]
    return windows, engine, gpu


def pair_shown(presented, shown):
    """Each presented frame with the time it was shown (0: never), in order.
    Shown times for frames presented before the trace began come first; a
    frame cannot be shown before it was presented."""
    if not presented:
        return []
    first = presented[0][1]
    times = list(shown)
    while times and 0 < times[0] < first:
        times.pop(0)
    return [(frame, at, times[i] if i < len(times) else None) for i, (frame, at) in enumerate(presented)]


def report(path, around):
    trace = load(path)
    outputs = {o["name"]: o for o in trace.get("outputs", [])}
    frame_s = trace.get("frame_ms", 16.667) / 1000
    events = trace["events"]
    if not events:
        print("no steps in the trace")
        return
    t0 = events[0]["at"]
    span = events[-1]["at"] - t0
    print(f"{path}\nclock: {trace.get('clock')}  frame {frame_s * 1000:.3f} ms  span {span:.1f} s  steps {len(events)}")
    windows, engine, gpu = by_window(events)

    if engine:
        starts = [e["at"] for e in engine]
        gaps = [b - a for a, b in zip(starts, starts[1:])]
        late = [e["at"] - e["refresh"] for e in engine if e.get("refresh")]
        print(f"\nengine: {len(engine)} frames  start-to-start {spread(gaps)}")
        if late:
            print(f"        start after its refresh {spread(late)}")

    for name, w in sorted(windows.items()):
        info = outputs.get(name, {})
        refresh = (info.get("refresh_ms") or 0) / 1000
        expected = max(frame_s, refresh)
        presented = sorted(((f, d["presented"]) for f, d in w["frames"].items() if "presented" in d), key=lambda x: x[1])
        paired = pair_shown(presented, w["shown"])
        shown = [s for _, _, s in paired if s]
        never = sum(1 for _, _, s in paired if s == 0)
        intervals = [b - a for a, b in zip(shown, shown[1:])]
        hitches = [i for i, d in enumerate(intervals) if d > 1.5 * expected]
        title = f"{name}" + (f"  (display {info.get('display')}, refresh {info.get('refresh_ms'):.3f} ms, follows clock {info.get('follows_clock')}, hold {info.get('frame_hold')})" if info else "")
        print(f"\n== {title}")
        print(f"presented {len(presented)}  shown {len(shown)}  never shown {never}  hitches {len(hitches)}  kept back {w['kept']}  replaced {w['replaced']}  let go {w['let_go']}  no frame {w['noframe']}")
        if intervals:
            print(f"shown-to-shown {spread(intervals)}")
        frames = w["frames"]
        for a, b in zip(("sampled",) + STEPS, STEPS + ("shown",)):
            vals = []
            for f, at, s in paired:
                d = frames[f]
                end = s if b == "shown" else d.get(b)
                start = d.get(a)
                if start is not None and end:
                    vals.append(end - start)
            if vals:
                print(f"  {a:>9} -> {b:<9} {spread(vals)}")
        # Where each step fell in the screen's refresh: phase of the step
        # against the grid the shown times make.
        if len(shown) > 10 and refresh > 0:
            grid0 = shown[0]

            def phase(t):
                return ((t - grid0) / refresh) % 1.0

            print("  phase in the screen's refresh (0 = a refresh this window's frames came up at), made vs late:")
            made = {f for i, (f, _, s) in enumerate(paired[1:], 1) if s and paired[i - 1][2] and s - paired[i - 1][2] <= 1.5 * expected}
            for step in STEPS:
                ok = [phase(frames[f][step]) for f, _, s in paired if s and step in frames[f] and f in made]
                bad = [phase(frames[f][step]) for f, _, s in paired if s and step in frames[f] and f not in made]
                if ok or bad:
                    hist = lambda xs: " ".join(f"{sum(1 for x in xs if k / 10 <= x < (k + 1) / 10):3d}" for k in range(10))
                    print(f"    {step:>9} made [{hist(ok)}]")
                    print(f"    {'':>9} late [{hist(bad)}]")
        # An output window's copy reads the one picture the engine draws for
        # it each frame: submitted after the engine handed over the next
        # frame (drawn by then), the copy showed the next frame's picture.
        handed = sorted((d["handed"], f) for f, d in frames.items() if "handed" in d)
        next_handed = {f: handed[i + 1][0] for i, (_, f) in enumerate(handed[:-1])}
        overwritten = [f for f, d in frames.items() if "submitted" in d and f in next_handed and d["submitted"] > next_handed[f]]
        if handed:
            print(f"  copied after the next frame was drawn: {len(overwritten)} (builds before each frame had a picture of its own showed the next frame's picture for these)")
        if w["sampled"]:
            steps = [b[2] - a[2] for a, b in zip(w["sampled"], w["sampled"][1:])]
            count = defaultdict(int)
            for s in steps:
                count[s] += 1
            print("  engine frames moved on per UI frame: " + "  ".join(f"{k}: {v}" for k, v in sorted(count.items())))
        # The frames around each hitch, and the GPU work (uploads, this
        # window's passes) that ended in the frame before the late one.
        for i in hitches[:around]:
            print(f"  hitch: shown {ms(shown[i] - t0)} then {ms(shown[i + 1] - t0)} ({(shown[i + 1] - shown[i]) * 1000:.1f} ms)")
            near = [
                g for g in gpu
                if shown[i] - expected <= g["end"] <= shown[i + 1]
                and ("PendingWrites" in g["label"] or name.split(" ")[0] in g["label"] or "Control Window" in g["label"])
            ]
            for g in near[:6]:
                print(f"     gpu {g['label'][:34]:<34} {ms(g['start'] - t0)} -> {ms(g['end'] - t0)}")
            lo, hi = max(0, i - 1), min(len(paired), i + 3)
            for f, at, s in paired[lo:hi]:
                d = frames[f]
                parts = "  ".join(f"{k[:4]} {ms(d[k] - t0)}" for k in ("sampled",) + STEPS if k in d)
                print(f"     frame {f:6d}  {parts}  shown {ms(s - t0) if s else '   never'}")
    if gpu:
        labels = defaultdict(list)
        for g in gpu:
            labels[g["label"]].append(g["end"] - g["start"])
        top = sorted(labels.items(), key=lambda kv: -sum(kv[1]))[:8]
        print("\nGPU command buffers (start to end, includes waiting):")
        for label, durs in top:
            print(f"  {label[:40]:<40} {len(durs) / span:7.1f}/s  {spread(durs)}")


if __name__ == "__main__":
    args = sys.argv[1:]
    around = 8
    if "--frames" in args:
        i = args.index("--frames")
        around = int(args[i + 1])
        del args[i : i + 2]
    if not args:
        sys.exit(__doc__)
    for p in args:
        report(p, around)
