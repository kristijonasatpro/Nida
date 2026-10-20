# Background effect loading

Interactive `AddEffect` commands now reserve the effect's UUID and chain position
immediately, then build its shader and GPU resources on a loader thread. This
applies to deck, channel and master effects through the same engine command,
whether the request comes from the UI or the API.

The loader has two workers and a queue of 32 jobs. A full queue rejects a new
request without inserting a half-added effect. Loading and failed effects are
bypassed by rendering, occlusion decisions, still-frame caching and Merge's
layer inspection. The live shell owns parameters, enabled state, mask and UUID;
completion installs resources only. Reordering and edits made while loading
therefore survive completion. Pending settings use the existing saved-show
format, with no serialized worker or receiver.

Each shell owns its reply channel and a lifetime token. Removing it cancels work
that has not started and disconnects any running job's reply. Undo can restore
the same UUID without receiving the removed instance's old result. Errors stay
on the effect, are exposed as `load_state` in snapshots and generate a notice.
Worker-local wgpu error scopes keep build validation errors out of the live
renderer's GPU quarantine mechanism.

This slice covers interactive additions from the library. Show restoration,
effect paste/duplication, source loading and transition loading keep their
existing construction paths. It does not preload unused library effects.

## Validation

The focused tests use blocked loaders rather than timing-dependent shaders:
four render calls complete while every requested effect is still waiting. A
GPU readback checks that the blue channel picture survives bypass. Completion
is then checked after edits, reorder, removal and undo. Saved settings are
checked while the effect is pending.

Worker tests cover a full queue, cancelled queued jobs, disconnected workers,
and a malformed shader alongside a successful build. Production Clippy passes.
These checks establish non-blocking admission and correct rendering; they are
not a claim that GPU driver resource creation can never contend with drawing.
Interactive Windows testing remains necessary before publishing.
