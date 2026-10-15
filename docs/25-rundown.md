# Rundown

A show follows a running order: the opening, a walk-on, the talk, a panel.
Each moment wants several things at once — every part of the LED wall
changed together, a camera taken to Main, a sting started. The **Rundown**
panel holds those moments as **steps**, prepared ahead and each taken with
one press.

A step sets any of:

- **Screens** — what each bus shows. A wall in three parts is three screens
  on three buses (see [LED Screens](22-led-screens.md)); a step changes all
  three on the same frame, cut or faded as one picture, as a Resolume column
  does.
- **Main** — an input taken to Main, and overlays switched on or off.
- **Starts and stops** — clips, playlists and timers.

Whatever a step does not set is left as it is. A step that sets only the
screens changes the wall and never touches Main — nor the stream, unless it
is made to set the bus the stream takes; Main stays its own 16:9 picture,
switched by hand or by the steps that say so.

Open the panel with **Layout ▸ Panels ▸ Rundown**. In the built-in layouts it
is a tab beside the Switcher.

## Making steps

Set the screens up as the first moment wants them — each bus on its input in
the [Switcher](07-outputs.md#buses-and-routing) — then press **+ Step**:

| In the New step window | |
|---|---|
| **Name** | The next letter free — `A`, then `B` — ready to type over: `B Walk-on` |
| A tick for each bus | Ticked, the step keeps what that bus shows now (where it is heading, if it is mid-fade). A bus the stream or the recording takes starts unticked, so a step made for the wall cannot change what goes out. A bus showing an input that is not in the show is kept as it is, and the step says **⚠** until it is put right |
| **Main, and every overlay on or off** | Ticked, the step keeps what is on Main too, and switches each overlay as it is now. Unticked, taking the step leaves Main alone |

Set up the next moment and press **+ Step** again — or right-click a step ▸
**Duplicate** and change the copy.

**Double-click a step** (or right-click ▸ **Edit…**) for what it sets, without
putting anything on a screen:

| | |
|---|---|
| **Name** | |
| **Screens** | For each bus: **Leave as it is**, Black, Master, or a channel |
| **Main ▸ Takes** | **Leave as it is**, Black, or a channel. Under it, each overlay channel: **Leave as it is**, **On** or **Off** |
| **Starts and stops** | **+ Clip, playlist or timer** adds one. Each: **Start from the beginning** (a clip from its in-point, a playlist from its first item, a timer from its top), **Carry on**, **Pause**, or **Stop, back to the beginning**. **×** takes it out |
| **Then go on to the next step after** | Ticked, the step after this one is taken by itself that many seconds later — see [Going on by itself](#going-on-by-itself) |
| **Update from what is on now** | The screens the step sets, and Main if it sets it, become what is on now |

A step's right-click menu also has **GO takes this next**, **Move earlier**,
**Move later** and **Remove**. Undo brings back a step removed or changed.

## Running the show

| | |
|---|---|
| **GO ▸ *step*** | Fades to the next step: the one after the step taken last (**NOW**), marked **NEXT** |
| **Cut**, beside it | Cuts to the next step |
| A step's own **Cut** and **Fade** | Take that step, in or out of order. The rundown goes on from it |
| **Fade:** | How long the screens take to fade to a step |
| Right-click ▸ **GO takes this next** | Moves **NEXT** without taking anything — back to `A` for the second half, say |

At the end of the rundown GO takes nothing: it never starts the show again by
itself.

A fade moves every screen the step sets over the one fade time, on one curve,
from one instant — each bus's own AUTO time does not come into it — so the
parts of a wall dissolve as one picture and land on the same frame. A bus
already showing what the step wants is left alone. Main is taken as its own
**FADE** takes it, with its own [transition and fade time](18-main.md#transitions),
so a Merge on Main is still a Merge.

To bind a key to GO, enter [Keyboard Learn](06-control-surfaces.md#keyboard-shortcuts),
click **GO** and press the key. GO has no key until then.

### All of it, or none

A step is taken whole. Before anything changes, everything it sets is
checked; if something is not there, nothing is changed and a notice says
what:

- **Something it sets is not there** — a channel that is an overlay now (or
  no longer one), a timer set to the time of day, a clip put on the timeline,
  Main mode switched off, or a bus or an input missing from a show that
  opened without it. The step's row shows **⚠** — hover it for what — and
  its buttons wait until it is put right in **Edit…**, where what is not
  there is marked and can be taken out or set to something else.
- **A screen is still fading** — a fade cannot start on a bus in the middle
  of another, and the other screens going without it would not land together.
  **Fade** and **GO** wait; **Cut** goes at once, and lands every screen.
  The same for Main in the middle of its own fade, and for an overlay still
  fading the other way: one on its way on is not turned round before it has
  been seen.

So a second GO pressed by mistake during a fade is refused, where the next
step would change a screen that is still fading. (A step that changes none of
them — one that only starts a clip, say — is taken.)

An input deleted from the show, or a bus removed, is taken out of the steps
that used it — for an input a notice names them — and they go on setting the
rest. Undo puts it back.

The step the show is on, removed, leaves GO on the step that followed it; put
back by undo, the show is on it again. While it is removed, GO takes the
first of the steps that were still to come — and of those made since: one
added, or a copy of one still to come — wherever they are moved. Once a step is taken the show is on it, and GO goes
by the list from there, as ever: the step marked **NEXT** is always the one
it takes.

### Going on by itself

A step with **Then go on to the next step after** ticked takes the step after
it without a press: a walk-on that starts the sting and the wall, then four
seconds later takes the camera to Main, is two steps, the first going on to
the second.

- While it waits, the panel counts down — **Then *step* in 3 s** — beside
  **Hold**.
- **GO** before the time is up goes on at once.
- **Hold**, any step taken by hand — even one that is refused — or **GO
  takes this next** on another step stops it: the rundown stays where it is
  until told.
- The wait is counted from the take, and is never shorter than the step's own
  fade — the screens', and Main's and its overlays' by Main's fade time:
  faded to over two seconds and set to go on after one, it goes on when its
  fade is done.
- If something is still fading when its turn comes, the next step waits for
  it, for a second at most. A step that cannot be taken then — or that sets
  something that is not there — says so and is not tried again. A step that
  was cut to goes on with a cut, which nothing holds up.
- Changing the rundown while a step waits — a step edited, moved, added or
  removed — stops the wait.

## Clips, playlists and timers

A step starts a clip whether it is on a screen or not: a sting on no screen
plays from its beginning and plays on to its end.

- **To be heard**, a clip on no screen needs its strip in the
  [Audio Mixer](16-audio-mixer.md) switched on and **Auto** off: with Auto,
  a clip sounds only while its input is on Main or a screen. The step's
  window says so under a clip that would play unheard.
- A clip that is on a screen goes by its [On air](21-input-settings.md#on-air)
  settings as ever: it plays as it goes on, and pauses and goes back to its
  start as it leaves — whichever step started it.
- A step that puts a clip on a screen — or on Main — and has it on **Pause**
  or **Stop** shows it still: Auto Play gives way to the step as that step
  brings it on. A later step with **Start from the beginning** or **Carry
  on** starts it — a video up on the wall, waiting for its cue. (For that
  moment — until the clip first arrives on air, five frames after the step
  at most — it stays still wherever it is put on air, by another step or by
  hand; after that, Auto Play is as ever.)
- A clip or playlist that follows the timeline or has a row in the
  [arrangement](15-arrangement.md), and a timer showing the time of day,
  cannot be started by a step.
- One clip shown in several places is one clip: started again, it starts
  again everywhere.

A countdown started by the step a talk begins with is **Start from the
beginning** on the timer.

## Saved, and safe from undo

The steps, the fade time and where the show is are saved with the show: after
a restart **NOW** is where it was, and GO goes on from it. (A show saved with
the step it was on removed has no **NOW** after a restart; GO takes what
followed that step.) A wait is not carried over a restart, nor into another
show.

Taking a step is live: **undo never takes one back** or changes what is on a
screen, and the show stays where it is. Undo covers the steps themselves —
added, removed, renamed, moved, edited. (Only when undo takes away the very
step the show is on does **NOW** go with it, and come back with it.) Undo and
redo stop a step that is waiting to go on by itself, as any change to the
rundown does.

## Over the API

The rundown is in `GET /api/state` under `rundown`: the fade time, each step
with what it sets, why it cannot be taken (`missing`) or faded to just now
(`fading`), the step taken last, the next, and a wait with its seconds left.

```sh
# A step of what every bus shows now (a bus the stream takes left out)
curl -X POST http://localhost:8080/api/rundown/steps \
  -H "Content-Type: application/json" -d '{"name": "B Walk-on"}'

# What one bus shows in it, and what it takes to Main
curl -X PUT http://localhost:8080/api/rundown/steps/<step_uuid>/screens/<bus_uuid> \
  -H "Content-Type: application/json" -d '{"input": {"Channel": "<channel_uuid>"}}'
curl -X PUT http://localhost:8080/api/rundown/steps/<step_uuid>/main \
  -H "Content-Type: application/json" -d '{"main": {"Channel": "<channel_uuid>"}}'

# Take it with a fade; or the next step, whichever it is
curl -X POST http://localhost:8080/api/rundown/steps/<step_uuid>/take \
  -H "Content-Type: application/json" -d '{"fade": true}'
curl -X POST http://localhost:8080/api/rundown/go \
  -H "Content-Type: application/json" -d '{}'
```

| Route | |
|---|---|
| `POST /api/rundown/steps` | `{"name": "A", "buses": ["<bus_uuid>"], "main": false}` — a step of what is on now. Without `buses`, every bus but one the stream or recording takes |
| `DELETE /api/rundown/steps/{step_uuid}` | |
| `POST /api/rundown/steps/{step_uuid}/duplicate` | A copy right after it |
| `PUT /api/rundown/steps/{step_uuid}/name` | `{"name": "B Walk-on"}` |
| `POST /api/rundown/steps/{step_uuid}/move` | `{"later": true}` |
| `PUT /api/rundown/steps/{step_uuid}/screens/{bus_uuid}` | `{"input": "Black"}`, `{"input": "Master"}`, `{"input": {"Channel": "<uuid>"}}`, `{"input": {"Deck": "<uuid>"}}` — or `{"input": null}` to leave the bus as it is |
| `PUT /api/rundown/steps/{step_uuid}/main` | `{"main": {"Channel": "<uuid>"}}`, `{"main": {"Deck": "<uuid>"}}`, `{"main": "Black"}` — or `{"main": null}` to leave Main as it is |
| `PUT /api/rundown/steps/{step_uuid}/overlays/{channel_uuid}` | `{"on": true}`, `{"on": false}` — or `{"on": null}` to leave it as it is |
| `PUT /api/rundown/steps/{step_uuid}/plays/{deck_uuid}` | `{"action": "start"}`, `{"action": "resume"}`, `{"action": "pause"}`, `{"action": "stop"}` — or `{"action": null}` for nothing |
| `PUT /api/rundown/steps/{step_uuid}/then` | `{"secs": 4}` — or `{"secs": null}` to wait for GO |
| `POST /api/rundown/steps/{step_uuid}/capture` | Update from what is on now |
| `POST /api/rundown/steps/{step_uuid}/take` | `{"fade": true}`. Refused (503) with the reason when it cannot be taken whole |
| `POST /api/rundown/go` | `{"fade": true, "expect": "<step_uuid>"}` — the next step. With `expect`, only if that is the step: a press that arrives after the rundown went on by itself is refused rather than taking the step after |
| `PUT /api/rundown/next` | `{"uuid": "<step_uuid>"}` — the step GO takes next |
| `POST /api/rundown/hold` | Stop a step going on by itself |
| `PUT /api/rundown/fade-time` | `{"secs": 1.5}` |

## Limits

- A step sets which input a bus shows, not how a screen shows it: a screen's
  place and **How** (Fit, Fill, Part of the picture) are the LED feed's, the
  same in every step.
- A step does not move a fader or switch a microphone: the Audio Mixer stays
  by hand, or by [Audio Auto](16-audio-mixer.md).
- GO fades. A step that should always be cut is taken with its own **Cut**.
- These steps are not the timeline's cue points (see
  [Arrangement Mode](15-arrangement.md#cue-points)).
