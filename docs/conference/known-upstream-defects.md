# Known upstream defects

Found while building the conference production buses (see
`src/internal/bus/`), in code the bus work depends on. Each is real, each
predates this work, and each needs a design decision rather than a patch.

Recorded here, inside the repository, so the references from code comments
resolve for anyone reading the source.

### Undo rewinds what a live control was driving

Preserving a macro's `value` across a history restore keeps the control where
the operator left it, but the targets it drove — deck opacity, generator and
effect params, mute — are rewritten from the snapshot. The control and the
picture then disagree.

A fan-out replay was implemented and then **removed**, because it is the wrong
mechanism:

- Bank order is not last-writer order. A macro would snap back a value the
  operator had since set by hand, or that another macro set after it.
- Some routes are not idempotent: `deck/<uuid>/mute` toggles rather than sets,
  so replaying it flips a value the snapshot already had right.
- `modulated_fanout` returns nothing for button macros, so Momentary and Toggle
  buttons were never covered.

A correct fix needs last-writer authority per target, or a live-override layer
the restore does not write through. There is no parameter read-back in the
codebase today, which is the other prerequisite.

`undo_rewinds_what_a_live_macro_drives_known_defect` pins the current behaviour
so the defect stays visible and a future fix has something to flip.

The crossfader case *is* fixed, because it is a single well-known value rather
than an arbitrary target set.

### Acquisitions that fail before producing a deck leak

`restore_deck` acquires an external session (camera, NDI, HTML) *before* the
fallible deck constructor runs. When construction fails, `restore_scene` turns
it into a warning and carries on — but no completed deck exists, so the holder
reconciliation that retires sessions never sees it. The acquisition is held for
the life of the process.

Fixing it needs rollback guards around each acquisition, or manager-owned leases
that release on drop. Both are changes to the acquisition contract rather than
to the reconciliation, which is why they are out of scope here.

### HTML sources are outside shared resource teardown

`HeldResources` covers camera, depth, screen capture, SRT, NDI and Syphon.
`HtmlManager` also reuses instances by URL, and `stop_render` removes from an
index-addressed vector and shifts every later handle, so adding HTML to the shared release would corrupt surviving
HTML decks. It needs stable handles or tombstones first. This leaked before this
work and leaks the same amount now.

## Syphon late binding — FIXED, recorded for context

A Syphon source can appear minutes after a workspace loads, so the deck is
restored as pending and bound when the server shows up. That path used to call
the generic add, which mints a *fresh* deck UUID, discard the returned id, find
the deck by display name, and re-apply five slot fields.

Two consequences: a persisted `BusInputRef::Deck(uuid)` (or MIDI mapping, or API
caller) addressed a UUID that no longer existed, so the route held black
forever; and effects, transparency, render FPS and auto-transition were dropped
even though `PendingSyphonDeck` carried the whole config.

Late binding now adopts the saved UUID, restores configuration through the same
`patch_deck_slot` the scene merge uses (opacity, transparency, blend, mute,
solo, z-index, render FPS, generator params, effects), and reconciles bus
routing so the recovered source re-resolves. Same principle as the structural
merge: identity travels with the runtime it names.

**Still incomplete, and still defects:**

- **An unbound pending deck is deleted by the next save.** A pending Syphon deck
  is deliberately absent from the mixer until its server appears, and workspace
  and history snapshots serialise only the mixer. Saving before the server shows
  up therefore writes a `scene.json` without it, while the stage keeps the bus
  route — after which that route can never recover. Pending configs need to be
  merged into scene snapshots, or represented by a placeholder slot.
- **Effect preprocessors are not initialised.** Late binding attaches restored
  effects after the workspace load's analyzer-start pass has already run, and
  nothing re-runs the effect-preprocessor initialisation, so an effect needing
  one is inert.
- **Two same-server decks in one channel collapse.** Binding finds the deck by
  display name, and the add path treats a same-server deck in the channel as an
  idempotent no-op. Two legitimately distinct decks (added in separate channels,
  then one moved) can reach the same channel, and the second pending item then
  rewrites the first — losing one UUID. Fixing this needs either a
  single-server-per-channel invariant enforced everywhere including `move_deck`,
  or a late-binding insertion path keyed by the pending identity rather than the
  display name.

## Scope limitations of the bus work (not defects)

Referenced from `src/internal/bus/` and `src/engine/types.rs`, recorded here so
those paths resolve.

### Bus canvas is a region, not a second render target

Superseded by the region model, recorded because earlier notes and review rounds
assumed otherwise.

The original plan was for each bus to composite natively at its own resolution,
which needed the "deck-product split": decode once, render one deck product per
required resolution profile, plus per-bus channel and transition targets. That
was the highest-risk item in the whole design.

It is not needed. Everything composites once at the master resolution, and a
`BusCanvas` declares which rectangle of that image is the bus's picture. A
destination following the bus samples exactly that rectangle. This is how LED
processors expect to be fed — a standard display mode with a configured input
region — and it matches the "advanced output" model of comparable tools.

Sampling a sub-rectangle is lossless: a 1920x480 band of a 1920x1080 master is
those pixels 1:1, no resampling. What it costs:

- a bus cannot exceed the master raster, so a wall wider than the master needs a
  larger master (4K is supported; the ceiling is GPU maximum texture size);
- content is authored inside the master canvas rather than in its own coordinate
  space, so a 4:1 design lives in a band of a 16:9 image;
- two buses pointing at one channel still share that channel's decks, effects
  and opacity, so editing it affects both. Independent bus *composition* remains
  out of scope; this delivers independent switching, routing and framing.

### Audio is an identity only

Every bus owns an `AudioMixId`, and destinations are meant to follow their bus's
mix rather than binding a capture device of their own. No mixing, gain, routing
matrix or metering exists yet; the identity is reserved so video buses and audio
routing do not acquire incompatible ownership models later.
