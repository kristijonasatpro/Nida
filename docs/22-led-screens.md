# LED Screens

An event's LED is rarely one 16:9 screen: a 5×3 m wall and two 1×1 m cubes,
say, all driven by one LED processor through one HDMI cable. The processor
shows a fixed rectangle of that one signal on each screen. Nida sends that
signal — the **feed** — with every screen's picture placed at exact pixels,
as Resolume's Advanced Output does.

Each screen shows **Main** or a **bus**. Give each screen a bus of its own and
it becomes a place in the switcher: put a logo or a loop on the cubes' bus
once, then cut slides and cameras to the wall's bus all show long — nothing is
remapped.

Open it from **Setup → Outputs → LED screens…**.

## The feed

**+ New feed** opens one:

- **On** a screen output — the HDMI port the processor is connected to. The
  feed goes fullscreen there at that output's size, one feed pixel per HDMI
  pixel.
- **In a window**, to set it up away from the venue.
- **Use** an output you already have: it stops showing what it showed and
  carries the screens.

| Setting | |
|---|---|
| **Feed size** | What the processor takes in — 1920 × 1080, 3840 × 2160, or the size of the screen output it is on. Screens are placed in these pixels |
| **Test card** | Every screen shows its name, size and place in the feed, a white one-pixel edge (a cut edge shows at once), a 64-pixel grid, diagonals and a circle (a stretched screen shows an oval), each in its own colour — to line the processor up with |
| **Not an LED feed** | The output goes back to what it showed before |

With several feeds (two processors), pick the one to edit under **Feed**.

## Screens

The list on the left, each in its colour — the same colour as on its test
card — and under each name where it sits in the feed, its size and what it
shows (`at 1411,92  118×675  Bar 3`): the list keeps the order screens were
made in, so the position tells which piece of the wall each one is.
**+ Add screen** adds a 300-pixel square beside the others (under them
when the feed is full); the first screen takes the whole feed.

| For the selected screen | |
|---|---|
| **Name** | What the switcher and the test card call it |
| **Size** | Its size in LED pixels, as the LED technician gives it: a 5×3 m wall at P2.6 is 1920 × 1152 |
| **At** | Its top-left corner in the feed, in pixels — where the processor reads it from |
| **Shows** | **Main** or a bus. **New bus for this screen** makes a bus named after it, with its own Preview and Program in the switcher, and points the screen at it |
| **How** | **Fit** — the whole picture as large as fits, black around it (slides keep their edges) · **Fill** — the screen covered, the overflow cut off (a square screen is filled by default) · **Stretch** — the whole picture pulled to the screen's shape · **Part of the picture** — its part of Main or its bus, see below |
| **Duplicate**, **Remove** | |

**Arrange → Pack into the feed** places the screens side by side from the top
left in list order, starting a new row when one would run off the edge.

## Feed map

The feed as the processor gets it, with the live picture behind the boxes.
Drag a screen to move it: it snaps to the feed's edges and to the other
screens' edges, so screens butt up against each other exactly. The arrow
keys move the selected screen one pixel, ten with Shift, while the pointer is
over the map.

## One picture across several screens

Set several screens' **How** to **Part of the picture** and give them the same
source: each shows its own part of one image, so a visual continues from one
cube across the wall to the other cube, as they stand in the room.

The **Picture** map shows that source's picture with each screen's part on it.
Drag a part to move it; drag its corner to size it (it keeps the screen's
shape). **1:1** makes the part one picture pixel per LED pixel.
**Arrange → Parts of … in a row** stands them in list order from the left,
centred, one pixel per pixel — or smaller, all alike, when the row does not fit
in the picture.

A part that hangs off the picture is black there, not a smear of the edge.

## Changing every screen at once

With a bus each, the parts of a wall are taken one by one in the Switcher —
and change one after another. To change them as one picture — the sides to
the slides and the middle to the logo, with one fade — make each such moment
a step in the [Rundown](25-rundown.md): a step sets what every bus shows, and
is cut or faded to with one press, every screen on the same frame. Main, and
with it the stream, is left as it is unless the step says otherwise.

## Over the API

The feed is part of the output: `GET /api/stage/outputs/{uuid}` has it under
`led`.

```sh
# A new feed fullscreen on the processor's HDMI output
curl -X POST http://localhost:8080/api/outputs/led \
  -H "Content-Type: application/json" \
  -d '{"monitor_name": "HDMI", "layout": {"feed": [1920, 1080], "screens": [
        {"name": "Wall", "size": [1152, 648], "at": [0, 0], "source": "Master", "fit": {"mode": "fit"}},
        {"name": "Cube L", "size": [300, 300], "at": [1152, 0], "source": "Master", "fit": {"mode": "fill"}}]}}'

# Change it (the whole layout), or give the output back with null
curl -X PUT http://localhost:8080/api/outputs/<output_uuid>/led \
  -H "Content-Type: application/json" -d '{"layout": null}'

# A bus of its own for one screen
curl -X POST http://localhost:8080/api/outputs/<output_uuid>/led/screens/<screen_uuid>/bus
```

A screen's `source` is `"Master"` or `{"BusProgram": "<bus_uuid>"}`; `fit` is
`{"mode": "fit"}`, `"fill"`, `"stretch"`, or
`{"mode": "part", "x": 0, "y": 0, "width": 300, "height": 300}` in the
picture's pixels.

## Limits

- Up to 32 screens on a feed, each up to 16384 pixels each way.
- A screen shows Main or a bus. To put one input on a screen, put it on the
  screen's bus.
- Screens are rectangles, placed pixel for pixel. For a curved or angled
  surface, use the stage editor's surfaces and warp (see
  [Projection Mapping](08-projection.md)).
