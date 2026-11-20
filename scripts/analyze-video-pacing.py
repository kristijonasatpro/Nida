"""Summarize a single-clip NIDA_VIDEO_PACING_TRACE JSONL, without dependencies.

Usage: python scripts/analyze-video-pacing.py trace.jsonl > analysis.json
Rows outside measurement_begin/end are warmup/teardown, not measured discards.
The app has no measurement markers: use --start/--seconds to select steady state.
No GPU timestamps or actual scanout times are inferred from CPU observations.
"""
import argparse
import bisect
import collections
import json
import math


def stats(values):
    values = sorted(values)
    if not values:
        return None
    return {"n": len(values), "p50": values[len(values) // 2],
            "p95": values[math.ceil((len(values) - 1) * .95)],
            "p99": values[math.ceil((len(values) - 1) * .99)], "max": values[-1]}


def readiness_summary(rows):
    """No PTS joins: also valid for a multi-clip benchmark's shared budget."""
    waits = [r for r in rows if r[2] == "readiness_end"]
    frames, active = [], {}
    for ns, thread, event, a, b in rows:
        if event in ("render_tick", "engine_frame"):
            if thread in active:
                frames.append(active[thread])
            active[thread] = {"wait_ms": 0.0, "waits": 0, "recovered": 0,
                              "first_begin": None, "last_end": None}
        elif thread in active:
            frame = active[thread]
            if event == "readiness_begin" and frame["first_begin"] is None:
                frame["first_begin"] = ns
            elif event == "readiness_end":
                frame["wait_ms"] += a * 1000
                frame["waits"] += 1
                frame["recovered"] += int(b)
                frame["last_end"] = ns
    frames.extend(active.values())
    return {"waits": len(waits), "recovered": sum(int(r[4]) for r in waits),
            "wait_ms": stats([r[3] * 1000 for r in waits]),
            "waiting_per_frame_ms": stats([f["wait_ms"] for f in frames]),
            "waits_per_frame": stats([f["waits"] for f in frames]),
            "window_span_per_frame_ms": stats([(f["last_end"] - f["first_begin"]) / 1e6
                for f in frames if f["first_begin"] is not None and f["last_end"] is not None]),
            "limits": "CPU observations; includes OS scheduling excess. No PTS or scanout inference."}


def analyze(rows, start=None, seconds=None, pts_start=None):
    rows.sort(key=lambda r: r[0])
    begin = next((r[0] for r in rows if r[2] == "measurement_begin"), rows[0][0])
    end = next((r[0] for r in rows if r[2] == "measurement_end"), rows[-1][0])
    if start is not None:
        begin = rows[0][0] + start * 1e9
    if pts_start is not None:
        begin = next(r[0] for r in rows if r[2] == "encode" and r[3] >= pts_start)
    if seconds is not None:
        end = begin + seconds * 1e9
    if end <= begin or begin < rows[0][0] or end > rows[-1][0]:
        raise ValueError("requested measurement interval is not covered by the trace")
    measured = [r for r in rows if begin <= r[0] <= end]
    events = collections.defaultdict(list)
    for r in measured:
        events[r[2]].append(r)
    duration = (end - begin) / 1e9

    def spacing(name):
        by_thread = collections.defaultdict(list)
        for r in events[name]:
            by_thread[r[1]].append(r[0])
        return stats([(b - a) / 1e6 for times in by_thread.values()
                      for a, b in zip(times, times[1:])])

    durations = collections.defaultdict(list)
    opened = {}
    pairs = {"copy_complete": "copy_begin", "submit_end": "submit_begin",
             "publish": "decode_begin", "decode_none": "decode_begin",
             "present_end": "present_begin", "ui_present_end": "ui_present_begin"}
    for ns, thread, event, a, b in measured:
        if event in pairs and (thread, pairs[event]) in opened:
            durations[pairs[event] + "_to_" + event + "_ms"].append(
                (ns - opened.pop((thread, pairs[event]))) / 1e6)
        opened[thread, event] = ns
    # Native copy completion -> conversion/consuming callback. Single clip only;
    # PTS keys cannot distinguish concurrent clips or repeated loop generations.
    copies = {r[3]: r[0] for r in events["copy_complete"]}
    for r in events["encode"]:
        if r[3] in copies:
            durations["copy_observed_to_encode_ms"].append((r[0] - copies[r[3]]) / 1e6)
    encodes = {r[3]: r[0] for r in events["encode"]}
    for r in events["gpu_complete"]:
        if r[3] in encodes:
            durations["encode_to_callback_ms"].append((r[0] - encodes[r[3]]) / 1e6)
    publications = [r[0] for r in events["publish"]]
    for r in events["take"] + events["take_ready"]:
        if r[3] == 0:
            i = bisect.bisect_right(publications, r[0])
            if i < len(publications):
                durations["empty_to_next_publish_ms"].append((publications[i] - r[0]) / 1e6)
    gaps = [(b[3] - a[3]) * 60 for a, b in zip(events["encode"], events["encode"][1:])]
    ticks = events["render_tick"]
    sampled_tick_gaps = [(b[0] - a[0]) / 1e6 for a, b in zip(ticks, ticks[1:])
                         if int(b[3]) % 30 == 0]
    other_tick_gaps = [(b[0] - a[0]) / 1e6 for a, b in zip(ticks, ticks[1:])
                       if int(b[3]) % 30 != 0]
    # Distance of each sampled playhead phase from the half-frame eligibility
    # cutoff. A constant-rate worker can straddle it as wake-up jitter varies.
    decisions = collections.defaultdict(list)
    active = {}
    for r in measured:
        ns, thread, event, a, b = r
        if event == "playhead":
            active[thread] = {"phase_ms": ((a * 60) % 1 - .5) * 1000 / 60,
                              "next_due_delta_ms": (b - a - 1 / 120) * 1000 if b >= 0 else None,
                              "selected": 0}
        elif event == "select" and thread in active:
            active[thread]["selected"] += 1
        elif event in ("publish", "decode_none") and thread in active:
            d = active.pop(thread)
            kind = "none" if event == "decode_none" else ("supersede" if d["selected"] > 1 else "single")
            decisions[kind].append(d)
    # Same producer trace, ideal 60 Hz latest-mailbox consumers at different
    # phases. This is a counterfactual model, not a second playback measurement.
    phase_sweep = []
    for phase_ms in (0, 2, 4, 6, 8, 10, 12, 14, 16):
        previous = bisect.bisect_right(publications, begin - 1)
        fresh = ticks = 0
        tick = begin + phase_ms * 1e6
        while tick < end:
            current = bisect.bisect_right(publications, tick)
            fresh += current > previous
            previous = current
            ticks += 1
            tick += 1e9 / 60
        phase_sweep.append({"phase_ms": phase_ms, "fresh": fresh, "ticks": ticks})
    return {
        "readiness": readiness_summary(measured),
        "wall_s": duration, "events": {k: len(v) for k, v in events.items()},
        "encoded_fps": len(events["encode"]) / duration,
        "unique_encoded_pts": len({r[3] for r in events["encode"]}),
        "repeated_encoded_pts": len(events["encode"]) - len({r[3] for r in events["encode"]}),
        "backward_encoded_pts_steps": sum(g < 0 for g in gaps),
        "selected_superseding_previous": sum(r[4] == 1 for r in events["select"]),
        "mailbox_replacements": sum(r[4] == 1 for r in events["publish"]),
        "unencoded_payload_drops": sum(r[4] == 0 for r in events["payload_drop"]),
        "empty_takes": sum(r[3] == 0 for r in events["take"] + events["take_ready"]),
        "worker_spacing_ms": spacing("worker_wake"),
        "publish_spacing_ms": spacing("publish"), "encode_spacing_ms": spacing("encode"),
        "render_spacing_ms": spacing("render_tick"),
        "render_spacing_after_process_sample_ms": stats(sampled_tick_gaps),
        "render_spacing_other_ms": stats(other_tick_gaps),
        "app_consumer_spacing_ms": spacing("take_ready"),
        "ui_present_spacing_ms": spacing("ui_present_end"),
        "decoder_fences_ready_at_submit": sum(r[4] == 1 for r in events["decoder_fence_at_submit"]),
        "pts_decision_phase": {kind: {
            "ticks": len(values),
            "distance_from_half_frame_ms": stats([d["phase_ms"] for d in values]),
            "lookahead_minus_due_ms": stats([d["next_due_delta_ms"] for d in values
                                            if d["next_due_delta_ms"] is not None]),
        } for kind, values in decisions.items()},
        "worker_timeout_excess_ms": stats([(r[3] - r[4]) * 1000 for r in events["worker_timeout"]]),
        "engine_timer_wait_ms": stats([r[3] * 1000 for r in events["engine_timer_return"]]),
        "engine_timer_wait_excess_ms": stats([(r[3] - r[4]) * 1000 for r in events["engine_timer_return"]]),
        "engine_baton_wait_ms": stats([r[3] * 1000 for r in events["engine_frame"]]),
        "engine_gpu_wait_ms": stats([r[4] * 1000 for r in events["engine_frame"]]),
        "copy_sleep_ms": stats([r[4] * 1000 for r in events["copy_sleep"]]),
        "copy_event_wait_ms": stats([r[4] * 1000 for r in events["copy_event_wait"]]),
        "copy_wait_fallbacks": len(events["copy_wait_fallback"]),
        "copy_poll_counts": dict(collections.Counter(int(r[4]) for r in events["copy_complete"])),
        "consumed_pts_step_frames": stats(gaps),
        "consumed_pts_missing_frames": sum(max(0, round(g) - 1) for g in gaps if g > 0),
        "durations": {k: stats(v) for k, v in durations.items()},
        "modeled_consumer_phase_sweep": phase_sweep,
    }


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("trace")
    parser.add_argument("--start", type=float)
    parser.add_argument("--seconds", type=float)
    parser.add_argument("--pts-start", type=float)
    parser.add_argument("--readiness-only", action="store_true",
                        help="Multi-clip-safe wait-budget summary without PTS joins")
    args = parser.parse_args()
    with open(args.trace, encoding="utf-8-sig") as stream:
        rows = [json.loads(line) for line in stream if line.strip()]
    if args.readiness_only:
        rows.sort(key=lambda r: r[0])
        begin = next((r[0] for r in rows if r[2] == "measurement_begin"), rows[0][0])
        end = next((r[0] for r in rows if r[2] == "measurement_end"), rows[-1][0])
        result = readiness_summary([r for r in rows if begin <= r[0] <= end])
    else:
        result = analyze(rows, args.start, args.seconds, args.pts_start)
    print(json.dumps(result, indent=2))
