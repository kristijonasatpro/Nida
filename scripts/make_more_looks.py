"""Writes the Broadcast backgrounds added after the first ten. The
palettes, helpers and controls are those of make_broadcast_looks.py — its
head is run here, its looks are not written again (four of them were
changed by hand since: do not run that script over them).

No look takes a derivative (fwidth, dFdx): every look starts with a branch
that leaves early, after which the engine's shader checks allow none. How
wide a pixel is in a look's own units is worked out from its geometry.

usage: python3 -I scripts/make_more_looks.py <varda repo>
"""
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
head = (HERE / "make_broadcast_looks.py").read_text()
head = head[: head.index("# ---------------------------------------------------------------- Warm --")]
head = head.replace("def write(name, description, mood, inputs, phase_scale, body, soft=False, wrap=LOOK_WRAP):",
                    "def write(name, description, mood, inputs, phase_scale, body, soft=False, wrap=LOOK_WRAP, prefix=\"broadcast\"):")
head = head.replace('path = REPO / "shaders" / f"broadcast_{name}.fs"', 'path = REPO / "shaders" / f"{prefix}_{name}.fs"')
assert "prefix=" in head and "{prefix}_" in head
exec(compile(head, "make_broadcast_looks.py (head)", "exec"))

# Distance from p to the segment a–b.
SEG = """
float seg(vec2 p, vec2 a, vec2 b) {
    vec2 pa = p - a;
    vec2 ba = b - a;
    return length(pa - ba * clamp(dot(pa, ba) / max(dot(ba, ba), 1e-6), 0.0, 1.0));
}
// x times itself: pow(x, 2.0) is not defined for an x below 0.
float sq(float x) {
    return x * x;
}
// Lay `colour` over what is drawn so far, as much as `amount`: its light,
// and how much of the base it covers (the half-size pass of a look).
void lay(inout vec3 light, inout float cover, vec3 colour, float amount) {
    float a = clamp(amount, 0.0, 1.0);
    light = light * (1.0 - a) + colour * a;
    cover = cover * (1.0 - a) + a;
}
// 1 between lo and hi, the edges `aa` soft.
float band(float lo, float hi, float x, float aa) {
    return smoothstep(lo - aa, lo + aa, x) * (1.0 - smoothstep(hi - aa, hi + aa, x));
}
// A line of half-width w at distance d from its middle, the edge `aa` soft.
float stroke(float d, float w, float aa) {
    return 1.0 - smoothstep(w - aa, w + aa, d);
}
// How far outside (inside: below 0) a box of half-size b a point is.
float sdbox(vec2 p, vec2 b) {
    vec2 dd = abs(p) - b;
    return length(max(dd, 0.0)) + min(max(dd.x, dd.y), 0.0);
}
// Three octaves of the value noise, 0–1.
float bc_fbm(vec2 p) {
    float sum = 0.0;
    float amp = 0.5;
    for (int i = 0; i < 3; i++) {
        sum += amp * bc_noise(p);
        p = p * 2.03 + vec2(17.1, 9.3);
        amp *= 0.5;
    }
    return sum / 0.875;
}
"""
HELPERS = HELPERS.replace("// ---- end of helpers", SEG.strip("\n") + "\n// ---- end of helpers")

FINISH = """
    fragColor = bc_finish(bc_calm_all(col, base, uv, calm), frag);
"""

# ---------------------------------------------------------------- Tech --
look("plexus_wave", "Plexus wave — a mesh of glowing points rolling along the horizon", "Tech", 1, 0.1, START + """
    vec3 light = vec3(0.0);
    float cover = 0.0;
    // Drawn in the half-size pass only, a quarter the cost: its lines are
    // light, soft by nature.
    if (PASSINDEX == 0) {
        // A soft band of light where the mesh lies.
        lay(light, cover, mainc * 0.5, 0.3 * exp(-sq((uv.y - 0.66) / 0.2)));
        float lines = 0.0;
        float nodes = 0.0;
        float glow = 0.0;
        // Rows of points, far to near; each joined along its row and to
        // the row before it. Nothing of them is in the top third.
        if (uv.y > 0.34) {
            vec2 prev[3];
            float prev_on[3];
            for (int row = 0; row < 6; row++) {
                float fr = float(row);
                float depth = fr / 5.0;
                float cells = mix(13.0, 5.5, depth);
                float y0 = 0.56 + 0.3 * pow(depth, 1.5);
                float amp = mix(0.035, 0.09, depth);
                float x = p.x * cells + fr * 3.7 + t * mix(0.05, 0.16, depth);
                float id = floor(x);
                vec2 cur[3];
                float cur_on[3];
                for (int k = 0; k < 3; k++) {
                    float ci = id + float(k) - 1.0;
                    vec2 o = bc_hash22(vec2(ci, fr * 17.3));
                    float wave = sin(ci * 0.55 + t * 0.9 + fr * 1.3) * 0.6
                        + sin(ci * 0.21 - t * 0.5 + fr * 0.7) * 0.4;
                    float px = (ci + 0.3 + 0.4 * o.x - fr * 3.7 - t * mix(0.05, 0.16, depth)) / cells;
                    cur[k] = vec2(px, y0 + amp * wave + 0.02 * (o.y - 0.5));
                    cur_on[k] = step(bc_hash12(vec2(ci, fr * 5.1 + 2.0)), 0.5 + 0.5 * density);
                }
                float w = mix(0.0022, 0.0042, depth);
                float bright = mix(0.35, 1.0, depth);
                // A row far from this pixel is placed (the next row joins
                // to it) but not drawn.
                if (abs(uv.y - y0) > amp + 0.16) {
                    prev = cur;
                    prev_on = cur_on;
                    continue;
                }
                for (int k = 0; k < 2; k++) {
                    float d = seg(p, cur[k], cur[k + 1]);
                    lines += (1.0 - smoothstep(w * 0.5, w * 1.5, d)) * bright * cur_on[k] * cur_on[k + 1];
                }
                if (row > 0) {
                    // To the nearest of the row before: a mesh, not ladders.
                    for (int k = 0; k < 3; k++) {
                        float d = seg(p, cur[1], prev[k]);
                        float reach = 1.0 - smoothstep(0.1, 0.22, distance(cur[1], prev[k]));
                        lines += (1.0 - smoothstep(w * 0.5, w * 1.5, d)) * bright * 0.7 * reach
                            * cur_on[1] * prev_on[k];
                    }
                }
                for (int k = 0; k < 3; k++) {
                    float d = distance(p, cur[k]);
                    float r = mix(0.004, 0.009, depth);
                    nodes += (1.0 - smoothstep(r * 0.6, r * 1.4, d)) * bright * cur_on[k];
                    // Its glow: a soft hill that ends six radii out.
                    float hill = max(1.0 - d * d / (r * r * 40.0), 0.0);
                    glow += hill * hill * bright * cur_on[k] * 0.5;
                }
                prev = cur;
                prev_on = cur_on;
            }
        }
        // Each in its own colour over what is under it, not added to it.
        lay(light, cover, mainc * 0.6, min(glow, 1.0) * 0.5);
        lay(light, cover, mainc * 0.7, lines);
        lay(light, cover, mix(mainc, accentc, 0.75) * 0.8, nodes);
        // A few points of light above, out of focus.
        if (uv.y < 0.62) {
            vec2 q = p * 7.0 + vec2(t * 0.05, -t * 0.03);
            vec2 cid = floor(q);
            vec2 o = bc_hash22(cid + 3.1);
            float has = step(bc_hash12(cid + 9.4), 0.12 + 0.2 * density);
            float sd = length(fract(q) - 0.25 - 0.5 * o);
            float twinkle = 0.6 + 0.4 * sin(t * 0.8 + o.x * 6.28);
            lay(light, cover, accentc * 0.6,
                exp(-sd * sd * 90.0) * has * twinkle * (1.0 - smoothstep(0.4, 0.62, uv.y)));
        }
    }
""" + SOFT_FINISH, soft=True)

look("dot_horizon", "Dot horizon — a plane of points running to the horizon, waves passing", "Tech", 1, 0.1, START + """
    vec3 col = base;
    float horizon = 0.5;
    float away = abs(uv.y - horizon);
    // Floor and ceiling alike; the plane in its own units, running toward us.
    float z = 0.22 / max(away, 1e-3);
    vec2 world = vec2(p.x * z, z + t * 0.6);
    float spacing = mix(1.4, 0.7, density);
    vec2 g = world * 9.0 / spacing;
    vec2 f = fract(g) - 0.5;
    vec2 id = floor(g);
    // A dot is at least 0.3% of the height on screen: far ones fade instead
    // of shrinking to a shimmer.
    // The plane is seen flat: a dot's height on screen is the lesser.
    float on_screen = 0.12 * spacing * away * away / (9.0 * 0.22);
    float fade = smoothstep(0.0015, 0.004, on_screen) * smoothstep(0.02, 0.1, away);
    // A pixel's width in the plane's cells, the larger way (down the picture).
    float aa = 9.0 * z / (spacing * max(away, 1e-3) * max(RENDERSIZE.y, 1.0));
    float dot_ = 1.0 - smoothstep(0.12 - aa, 0.12 + aa, length(f));
    // Waves of light through the plane.
    float wave = 0.5 + 0.5 * sin(id.x * 0.45 + id.y * 0.3 - t * 1.4);
    float wave2 = 0.5 + 0.5 * sin(id.y * 0.17 + t * 0.6 + id.x * 0.08);
    float light = dot_ * fade * (0.25 + 0.75 * wave * wave2);
    // Haze at the horizon, then the dots in their own colour over it.
    col = mix(col, mainc * 0.45, 0.5 * exp(-away * away / 0.006));
    col = mix(col, mainc * 0.75, light);
    col = mix(col, accentc * 0.75, light * smoothstep(0.75, 1.0, wave * wave2) * 0.6);
""" + FINISH)

# --------------------------------------------------------------- Cyber --
look("grid_floor", "Grid floor — a wire floor running to the horizon, pulses crossing it", "Cyber", 4, 0.1, START + """
    vec3 col = base;
    float horizon = 0.42;
    float below = uv.y - horizon;
    float z = 0.2 / max(below, 1e-3);
    vec2 world = vec2(p.x * z, z + t * 0.5);
    float spacing = mix(1.6, 0.8, density);
    vec2 g = world * 5.0 / spacing;
    // How fast the floor's cells go by, per frame height, across and down
    // the picture: from it a pixel's width, and a line's least width.
    vec2 per_height = vec2(5.0 * z / spacing * (1.0 + abs(p.x) / max(below, 1e-3)),
                           5.0 * 0.2 / (spacing * max(below * below, 1e-6)));
    vec2 aa = per_height * 1.2 / max(RENDERSIZE.y, 1.0);
    vec2 d = abs(fract(g) - 0.5);
    // Lines never thinner on screen than 0.36% of the height; far ones,
    // which that would make as wide as their cells, fade before they crowd.
    vec2 w = max(vec2(0.03), per_height * 0.0018);
    vec2 line = 1.0 - smoothstep(w, w + aa, 0.5 - d);
    float fade = (1.0 - smoothstep(0.1, 0.28, max(w.x, w.y))) * step(0.0, below) * smoothstep(0.0, 0.06, below);
    float grid = max(line.x, line.y) * fade;
    // A pulse of light crossing the floor now and then.
    float pulse = exp(-sq(fract(g.y * 0.08 - t * 0.12) - 0.5) * 60.0);
    col = mix(col, mainc * (0.5 + 0.25 * pulse), grid);
    col = mix(col, accentc * 0.75, grid * pulse * 0.5);
    // Sky: a soft glow on the horizon and slow bars of light above it.
    float glow = exp(-below * below / 0.004);
    col = mix(col, mainc * 0.45, 0.6 * glow);
    float above = max(-below, 0.0);
    float bars = bc_noise(vec2(p.x * 1.4 + t * 0.06, uv.y * 9.0 - t * 0.05));
    col = mix(col, mainc * 0.4, 0.2 * smoothstep(0.5, 0.95, bars) * smoothstep(0.0, 0.25, above));
""" + FINISH)

look("radar_sweep", "Radar sweep — rings, a slow turning beam and points that fade behind it", "Cyber", 6, 0.1, START + """
    vec3 col = base;
    vec2 c = p - vec2(0.0, 0.5);
    float r = length(c);
    // At the very centre there is no angle: any will do there.
    float ang = r > 1e-6 ? atan(c.y, c.x) : 0.0;
    // Rings and spokes, wide enough for an LED wall.
    float rings = mix(5.0, 9.0, density);
    float ra = 1.5 * rings / max(RENDERSIZE.y, 1.0);
    float ring = 1.0 - smoothstep(0.035, 0.035 + ra, abs(fract(r * rings) - 0.5));
    float spokes = 12.0;
    float sa = abs(fract(ang / 6.2831853 * spokes + 0.5) - 0.5) * 6.2831853 / spokes * r;
    float spoke = (1.0 - smoothstep(0.002, 0.005, sa)) * smoothstep(0.05, 0.12, r);
    col = mix(col, mainc * 0.5, 0.6 * max(ring, spoke * 0.6));
    // The beam: bright at its edge, a long tail behind.
    float beam_at = t * 0.5;
    float behind = fract((beam_at - ang) / 6.2831853);
    float tail = exp(-behind * 9.0);
    col = mix(col, mainc * 0.55, 0.6 * tail * smoothstep(0.02, 0.1, r));
    // Points found by the beam, fading till it comes round again.
    vec2 pc = vec2(r * 6.0, ang / 6.2831853 * 24.0);
    vec2 pid = floor(pc);
    vec2 po = bc_hash22(pid + 1.7);
    float has = step(bc_hash12(pid + 5.5), 0.1 + 0.22 * density) * step(1.0, pid.x);
    float pr = (pid.x + 0.3 + 0.4 * po.x) / 6.0;
    float pa = (pid.y + 0.3 + 0.4 * po.y) / 24.0 * 6.2831853;
    vec2 pp = pr * vec2(cos(pa), sin(pa));
    float pd = distance(c, pp);
    float age = fract((beam_at - pa) / 6.2831853);
    float blip = (1.0 - smoothstep(0.006, 0.012, pd)) + 0.5 * exp(-pd * pd / 0.0006);
    col = mix(col, accentc * 0.75, clamp(blip * has * exp(-age * 3.5), 0.0, 1.0));
""" + FINISH)

# ---------------------------------------------------------------- Warm --
look("embers", "Embers — small warm sparks rising slowly over a soft glow", "Warm", 7, 0.1, START + """
    vec3 light = vec3(0.0);
    float cover = 0.0;
    // Drawn in the half-size pass only, a quarter the cost: sparks are
    // points of light, soft by nature.
    if (PASSINDEX == 0) {
        // A warm glow from below.
        lay(light, cover, mainc * 0.45, 0.6 * smoothstep(0.35, 1.05, uv.y));
        float sparks = 0.0;
        float halo = 0.0;
        for (int layer = 0; layer < 3; layer++) {
            float fl = float(layer);
            float cells = mix(5.0, 11.0, fl / 2.0);
            vec2 q = vec2(p.x, uv.y) * cells + vec2(fl * 9.3, t * (0.5 + 0.2 * fl) + fl * 4.1);
            q.x += 0.25 * sin(q.y * 0.7 + fl * 2.0);
            vec2 id = floor(q);
            vec2 f = fract(q);
            for (int dy = -1; dy <= 1; dy++) {
                for (int dx = -1; dx <= 1; dx++) {
                    vec2 cell = id + vec2(float(dx), float(dy));
                    float h = bc_hash12(cell + fl * 21.7);
                    if (h > 0.12 + 0.4 * density) {
                        continue;
                    }
                    vec2 o = bc_hash22(cell + 3.3);
                    vec2 at = vec2(float(dx), float(dy)) + 0.25 + 0.5 * o - f;
                    float d = length(at) / cells;
                    float size = mix(0.007, 0.0035, fl / 2.0) * (0.7 + 0.6 * o.x);
                    float glim = 0.65 + 0.35 * sin(t * (0.7 + o.y) + h * 40.0);
                    sparks += (1.0 - smoothstep(size * 0.5, size * 1.3, d)) * glim;
                    // Its halo: a soft hill that ends five and a half sizes out.
                    float hill = max(1.0 - d * d / (size * size * 30.0), 0.0);
                    halo += hill * hill * glim * 0.5;
                }
            }
        }
        // Fewer and dimmer toward the top.
        float rise = smoothstep(0.05, 0.6, uv.y);
        lay(light, cover, mainc * 0.65, min(halo, 1.0) * rise * 0.7);
        lay(light, cover, accentc * 0.75, min(sparks, 1.0) * rise);
    }
""" + SOFT_FINISH, soft=True)

look("light_rays", "Light rays — soft beams of light fanning slowly from above", "Warm", 8, 0.1, START + """
    vec3 col = base;
    vec2 from = vec2(-0.35 * aspect, -0.35);
    vec2 d = p - from;
    float ang = atan(d.x, d.y);
    float dist = length(d);
    float n = mix(5.0, 11.0, density);
    float rays = bc_noise(vec2(ang * n + t * 0.25, 1.3)) * bc_noise(vec2(ang * n * 2.3 - t * 0.18, 7.7));
    rays = smoothstep(0.12, 0.6, rays);
    float reach = exp(-dist * 0.9) * smoothstep(0.0, 0.35, dist);
    // The air glows a little near the light; the beams over it.
    col = mix(col, mainc * 0.5, 0.4 * exp(-dist * 2.2));
    col = mix(col, mainc * 0.65, clamp(rays * reach * 1.3, 0.0, 1.0));
    col = mix(col, accentc * 0.65, clamp(rays * rays * reach * exp(-dist * 1.2) * 0.7, 0.0, 1.0));
""" + FINISH)

# ---------------------------------------------------------------- Calm --
look("caustics", "Caustics — slow light as on the floor of a pool", "Calm", 11, 0.1, START + """
    vec3 col = base;
    vec2 q = p * mix(2.2, 4.2, density) + vec2(t * 0.05, t * 0.03);
    float light = 0.0;
    // Two nets of bent bright lines, one finer and dimmer, sliding over
    // each other.
    for (int n = 0; n < 2; n++) {
        float fn = float(n);
        vec2 w = q * (1.0 + 0.7 * fn) + vec2(fn * 3.1, fn * 1.7);
        w += 0.45 * vec2(sin(w.y * 1.3 + t * 0.5 + fn), cos(w.x * 1.1 - t * 0.4 + fn * 2.0));
        w += 0.25 * vec2(sin(w.y * 2.3 - t * 0.3), cos(w.x * 2.1 + t * 0.35));
        float a = abs(sin(w.x * 1.6 + 0.6 * sin(w.y * 1.2 + t * 0.3)));
        float b = abs(sin(w.y * 1.6 + 0.6 * sin(w.x * 1.4 - t * 0.25)));
        float net = pow(1.0 - a, 5.0) + pow(1.0 - b, 5.0);
        light += clamp(net, 0.0, 1.0) * (1.0 - 0.45 * fn);
    }
    light = clamp(light * 0.75, 0.0, 1.0);
    col = mix(col, mainc * 0.6, light);
    col = mix(col, accentc * 0.7, smoothstep(0.55, 1.0, light) * 0.5);
""" + FINISH)

look("flow_lines", "Flowing lines — wide contour lines bending slowly", "Calm", 12, 0.1, START + """
    vec3 col = base;
    vec2 o1 = vec2(t * 0.08, -t * 0.05);
    vec2 o2 = vec2(t * 0.06, t * 0.04);
    float field = bc_noise(p * 1.1 + o1) + 0.5 * bc_noise(p * 2.3 - o2);
    float bands = mix(5.0, 11.0, density);
    float v = field * bands;
    // A pixel's width in bands: the field a pixel along and a pixel down.
    float e1 = 1.0 / max(RENDERSIZE.y, 1.0);
    vec2 px1 = p + vec2(e1, 0.0);
    vec2 py1 = p + vec2(0.0, e1);
    float aa = bands * (abs(bc_noise(px1 * 1.1 + o1) + 0.5 * bc_noise(px1 * 2.3 - o2) - field)
        + abs(bc_noise(py1 * 1.1 + o1) + 0.5 * bc_noise(py1 * 2.3 - o2) - field));
    // Where the field is flat it is no width at all: never quite none.
    aa = max(aa, 1e-5);
    float d = abs(fract(v) - 0.5);
    // Lines as wide as the field lets them: never thinner than a few pixels.
    float w = max(0.07, aa * 1.5);
    float line = (1.0 - smoothstep(w, w + aa, d)) * (1.0 - smoothstep(0.25, 0.6, aa));
    float tone = 0.5 + 0.5 * sin(floor(v) * 1.9);
    col = mix(col, mainc * 0.4, 0.3 * fract(v));
    col = mix(col, mix(mainc, accentc, tone) * 0.65, line);
""" + FINISH)

# ------------------------------------------------------------- Colours --
look("colour_fields", "Colour fields — the three colours melting slowly into one another", "Colours", 1, 0.1, START + """
    vec3 col = base;
    vec2 pc = p - vec2(0.0, 0.5);
    // Fields of colour on slow loops, each over what is under it.
    vec2 c1 = vec2(-0.32 * aspect, -0.2) + 0.12 * vec2(cos(t * 0.31), sin(t * 0.27));
    vec2 c2 = vec2(0.3 * aspect, 0.22) + 0.14 * vec2(cos(t * 0.23 + 2.1), sin(t * 0.29 + 2.1));
    vec2 c3 = vec2(0.05 * aspect, -0.3) + 0.1 * vec2(cos(-t * 0.37 + 4.0), sin(-t * 0.33 + 4.0));
    float f1 = exp(-dot(pc - c1, pc - c1) / 0.28);
    float f2 = exp(-dot(pc - c2, pc - c2) / 0.22);
    float f3 = exp(-dot(pc - c3, pc - c3) / 0.1);
    col = mix(col, mainc * 0.55, f1 * 0.85);
    col = mix(col, mainc * 0.3, f2 * 0.6);
    col = mix(col, accentc * 0.6, f3 * (0.3 + 0.7 * density));
""" + FINISH)

# ------------------------------------------------- More, by mood again --
look("dot_globe", "Dot globe — a sphere of points turning slowly, arcs of light across it", "Tech", 1, 0.1, START + """
    vec3 col = base;
    // Most of it in the right half of the picture; room for a slide on the left.
    vec2 c = p - vec2(0.3 * aspect, 0.52);
    float radius = 0.62;
    float r2 = dot(c, c) / (radius * radius);
    // A halo round it.
    col = mix(col, mainc * 0.45, 0.3 * exp(-max(sqrt(r2) - 1.0, 0.0) * 5.0) * step(1.0, r2));
    if (r2 < 1.0) {
        float z = sqrt(1.0 - r2);
        float limb = mix(0.35, 1.0, z);
        col = mix(col, mainc * 0.35, 0.3 * (1.0 - z));
        float rows = floor(mix(16.0, 28.0, density) + 0.5);
        // The far side first, dimmer, then the near side over it.
        for (int side = 0; side < 2; side++) {
            vec3 s = vec3(c / radius, side == 0 ? -z : z);
            // Tilted toward us, turning about its own axis.
            vec3 v = vec3(s.x, s.y * 0.9394 - s.z * 0.3429, s.y * 0.3429 + s.z * 0.9394);
            // (At a pole there is no longitude: any will do there.)
            float lon = (abs(v.x) + abs(v.z) > 1e-6 ? atan(v.x, v.z) : 0.0) + t * 0.15;
            float lat = asin(clamp(v.y, -1.0, 1.0));
            float la = lat / 3.14159265 * rows;
            float row = floor(la + 0.5);
            float row_lat = row / rows * 3.14159265;
            float round_row = max(floor(rows * 2.0 * cos(row_lat) + 0.5), 1.0);
            float lo = lon / 6.2831853 * round_row;
            // How far, as an angle, from the nearest point of the row.
            vec2 off = vec2((fract(lo) - 0.5) / round_row * 6.2831853 * cos(row_lat),
                            (la - row) / rows * 3.14159265);
            float there = 1.0 - smoothstep(0.009, 0.016, length(off));
            float facing = side == 0 ? 0.3 : 1.0;
            float wave = 0.5 + 0.5 * sin(lon * 3.0 - t * 0.6 + row * 0.4);
            float lit = there * facing * limb;
            col = mix(col, mainc * 0.75, lit * (0.45 + 0.55 * wave));
            col = mix(col, accentc * 0.75, lit * smoothstep(0.8, 1.0, wave) * 0.6);
        }
        // Arcs across it, a point of light going to and fro along each.
        for (int k = 0; k < 3; k++) {
            float fk = float(k);
            vec2 ac = vec2(0.25 - 0.3 * fk, 0.75 + 0.2 * fk) * radius;
            float ar = (0.85 + 0.12 * fk) * radius;
            vec2 rel = c - ac;
            float arc = (1.0 - smoothstep(0.002, 0.005, abs(length(rel) - ar))) * step(rel.y, -0.1 * radius);
            col = mix(col, mainc * 0.7, arc * 0.8 * limb);
            float at = sin(t * (0.3 + 0.1 * fk) + fk * 2.0) * 0.8;
            vec2 bead = ac + ar * vec2(sin(at), -cos(at)) - c;
            col = mix(col, accentc * 0.8, clamp(exp(-dot(bead, bead) / 0.00015), 0.0, 1.0));
        }
    }
""" + FINISH)

look("fibres", "Light fibres — fine strands of light fanning out, points travelling along them", "Tech", 3, 0.1, START + """
    vec3 col = base;
    float strands = 0.0;
    float beads = 0.0;
    float count = mix(10.0, 22.0, density);
    float fan = pow(uv.x, 1.3);
    for (int i = 0; i < 22; i++) {
        float fi = float(i);
        // Each strand comes in as Density brings it.
        float weight = clamp(count - fi, 0.0, 1.0);
        if (weight <= 0.0) {
            break;
        }
        // Each strand's own two numbers, evenly spread and never the same.
        float h = fract(fi * 0.6180339 + 0.37);
        float h2 = fract(fi * 0.7548777 + 0.11);
        // Together at the lower left, fanning out toward the right, each on
        // its own slow curve — which is never more than 0.075 from the
        // middle of its way: too far from that, it cannot light this pixel.
        float mid = mix(0.98, 0.62, h) - (0.2 + 0.75 * h) * fan;
        if (abs(uv.y - mid) > 0.16) {
            continue;
        }
        float y = mid
            + 0.05 * sin(p.x * (1.2 + h2) + t * (0.2 + 0.2 * h) + fi)
            + 0.025 * sin(p.x * (2.9 + h) - t * 0.3 + fi * 2.3);
        float d = abs(uv.y - y) * 0.85;
        // Too far from this pixel to light it.
        if (d > 0.07) {
            continue;
        }
        float w = 0.0016 + 0.0014 * h2;
        // Its glow: a soft hill that ends 0.06 from it.
        float hill = max(1.0 - d * d / 0.0036, 0.0);
        strands += ((1.0 - smoothstep(w, w * 2.2, d)) * (0.4 + 0.6 * h2) + 0.2 * hill * hill) * weight;
        // A point of light going along it, off one side and in at the
        // other: worked out only near where it is now.
        float along = p.x - (fract(t * (0.04 + 0.05 * h2) + h) - 0.5) * aspect * 1.2;
        if (abs(along) < 0.05) {
            float bd = length(vec2(along * 0.6, d));
            beads += exp(-bd * bd / 0.00008) * weight;
        }
    }
    col = mix(col, mainc * 0.65, clamp(strands, 0.0, 1.0));
    col = mix(col, accentc * 0.75, clamp(beads, 0.0, 1.0));
""" + FINISH)

look("hex_tunnel", "Hex tunnel — rings of six sides coming slowly out of the distance", "Cyber", 5, 0.1, START + """
    vec3 col = base;
    vec2 c = abs(p - vec2(0.0, 0.5));
    // How far out, measured to a hexagon's side.
    float hexr = max(c.x * 0.866025 + c.y * 0.5, c.y);
    // Rings a constant ratio apart: each as wide as it is far out.
    float rings = mix(2.2, 4.5, density);
    float v = log(max(hexr, 1e-4)) * rings - t * 0.5;
    float aa = 1.2 * rings / (max(hexr, 1e-4) * max(RENDERSIZE.y, 1.0));
    float ring = 1.0 - smoothstep(0.045, 0.045 + aa, 0.5 - abs(fract(v) - 0.5));
    // Each ring glows at its own slow pace; the far ones fade before they
    // are thinner than 0.3% of the height.
    float which = floor(v + 0.5);
    float pace = 0.6 + 0.4 * sin(which * 2.4 + t * 0.7);
    float depth = smoothstep(0.0015, 0.004, 0.045 * hexr / rings);
    col = mix(col, mainc * 0.4, 0.4 * exp(-hexr * hexr / 0.02));
    col = mix(col, mainc * 0.65, ring * depth * pace);
    col = mix(col, accentc * 0.7, ring * depth * smoothstep(0.8, 1.0, pace) * 0.6);
""" + FINISH)

look("curtain", "Stage curtain — soft folds of light swaying, a glow falling from above", "Warm", 9, 0.1, START + """
    vec3 col = base;
    // Folds: bands of light side by side, swaying a little.
    float x = p.x * mix(3.0, 6.5, density) + 0.2 * sin(t * 0.5 + uv.y * 2.0);
    float fold = 0.5 + 0.5 * sin(x * 3.14159265 + 0.9 * sin(x * 0.7 + t * 0.15)
        + 1.5 * bc_noise(vec2(x * 0.5, t * 0.1)));
    float ridge = pow(fold, 2.2);
    col = mix(col, mainc * 0.55, ridge * mix(0.75, 0.4, uv.y));
    // The light from above, catching the folds.
    vec2 lc = vec2(p.x * 0.6, uv.y + 0.15);
    float spot = exp(-dot(lc, lc) * 2.2);
    col = mix(col, accentc * 0.55, spot * (0.35 + 0.4 * ridge) * ridge);
""" + FINISH)

look("ridges", "Far ridges — lines of hills one behind another under an evening sky", "Calm", 12, 0.1, START + """
    // The sky: toward the hills it takes the Main, and the Accent where
    // it meets them.
    vec3 sky = mix(base, mainc * 0.5, 0.7 * smoothstep(0.0, 0.5, uv.y));
    sky = mix(sky, accentc * 0.6, 0.6 * smoothstep(0.22, 0.5, uv.y));
    float layers = mix(4.0, 7.0, density);
    float edge = 1.5 / max(RENDERSIZE.y, 1.0);
    // From the nearest ridge back: a pixel is of the first that covers it,
    // and the ridges behind that one are not worked out; nor is one whose
    // crest cannot be as high as this pixel.
    vec3 hills = vec3(0.0);
    float covered = 0.0;
    for (int n = 0; n < 7; n++) {
        float fi = float(6 - n);
        // Each ridge comes in as Density brings it.
        float weight = clamp(layers - fi, 0.0, 1.0);
        if (weight <= 0.0) {
            continue;
        }
        float depth = fi / 6.0;
        // The near ones lower, wider, and drifting faster.
        float foot = mix(0.5, 0.86, depth);
        float rise = mix(0.12, 0.2, depth);
        if (uv.y < foot - rise - edge) {
            continue;
        }
        float x = p.x * mix(1.6, 0.8, depth) + fi * 9.7 + t * mix(0.08, 0.35, depth);
        // Its crest, 0 to 1 of its rise.
        float h = 0.6 * bc_noise(vec2(x * 1.6, fi * 3.1)) + 0.14 * (1.0 + sin(x * 3.7 + fi * 1.3))
            + 0.06 * (1.0 + sin(x * 2.3 + fi));
        float top = foot - h * rise;
        float below = smoothstep(top - edge, top + edge, uv.y) * weight;
        if (below <= 0.0) {
            continue;
        }
        // Far ridges pale, near ones dark; each lighter toward its foot,
        // where the mist lies.
        vec3 ridge = mix(mix(mainc * 0.55, accentc * 0.5, 0.25), base * 0.8, pow(depth, 0.7));
        ridge = mix(ridge, mainc * 0.55, 0.4 * smoothstep(0.0, 0.16, uv.y - top) * (1.0 - depth));
        hills += (1.0 - covered) * below * ridge;
        covered += (1.0 - covered) * below;
        if (covered > 0.999) {
            break;
        }
    }
    vec3 col = sky * (1.0 - covered) + hills;
""" + FINISH)

look("ink", "Ink in water — soft clouds of colour folding slowly into themselves", "Calm", 10, 0.1, START + """
    vec3 light = vec3(0.0);
    float cover = 0.0;
    // All of it soft: drawn in the half-size pass only, a quarter the cost.
    if (PASSINDEX == 0) {
        vec2 g = p * mix(1.6, 2.6, density);
        // A field folded into itself twice.
        vec2 w1 = vec2(bc_noise(g + vec2(0.0, t * 0.1)), bc_noise(g + vec2(5.2, 1.7 - t * 0.08)));
        vec2 w2 = vec2(bc_fbm(g + 2.2 * w1 + vec2(1.7, 9.2) + t * 0.04),
                       bc_fbm(g + 2.2 * w1 + vec2(8.3, 2.8) - t * 0.03));
        float f = bc_fbm(g + 2.0 * w2);
        cover = smoothstep(0.3, 0.75, f);
        light = mix(mainc * 0.55, accentc * 0.6, smoothstep(0.45, 0.8, w2.x) * 0.8) * cover;
    }
""" + SOFT_FINISH, soft=True)

look("halftone", "Halftone — rows of dots swelling and shrinking in slow waves", "Colours", 1, 0.1, START + """
    vec3 col = base;
    float cells = mix(14.0, 34.0, density);
    // Rows on the slant, as in print.
    mat2 turn = mat2(0.7071, -0.7071, 0.7071, 0.7071);
    vec2 g = turn * (p * cells);
    vec2 f = fract(g) - 0.5;
    // Each dot's size from where its middle is in the picture.
    vec2 at = (transpose(turn) * (floor(g) + 0.5)) / cells;
    float wave = (0.5 + 0.5 * sin(at.x * 2.2 + at.y * 3.1 - t * 0.8))
        * (0.5 + 0.5 * (0.5 + 0.5 * sin(at.y * 1.7 - at.x * 1.3 + t * 0.5)));
    float size = 0.08 + 0.36 * wave;
    float aa = cells * 1.5 / max(RENDERSIZE.y, 1.0);
    float there = 1.0 - smoothstep(size - aa, size + aa, length(f));
    col = mix(col, mainc * 0.6, there);
    col = mix(col, accentc * 0.6, there * smoothstep(0.7, 1.0, wave) * 0.6);
""" + FINISH)

look("iso_tiles", "Rising tiles — a floor of tiles lifting and sinking in a slow wave, a few alight", "Tech", 1, 0.1, START + """
    vec3 col = base;
    float cells = mix(5.0, 9.0, density);
    // The floor seen from above and aside: u and v run along its rows.
    vec2 g = vec2(p.x * 0.5 + uv.y, uv.y - p.x * 0.5) * cells + t * 0.03;
    vec2 id0 = floor(g);
    // A pixel's width in cells (g runs 1.5 cells' worth a frame height).
    float aa = 2.25 * cells / max(RENDERSIZE.y, 1.0);
    // Half a tile's top, and the tallest a tile stands, in cells.
    float m = 0.4;
    float tall = 0.45;
    // A tile over this pixel is one of four, the far ones first.
    for (int k = 0; k < 4; k++) {
        vec2 cell = id0 + vec2(k == 1 || k == 3 ? 1.0 : 0.0, k >= 2 ? 1.0 : 0.0);
        float h1 = bc_hash12(cell);
        // Its height: a slow wave across the floor, and a little of its own.
        float h = tall * (0.5 + 0.5 * sin(cell.x * 0.6 + cell.y * 0.35 - t * 0.7 + h1 * 2.0));
        // Going down the picture from this pixel by s, the tile's place on
        // the floor is crossed from s_lo to s_hi; its top is met at s = h.
        vec2 lo = cell + 0.5 - m - g;
        vec2 hi = cell + 0.5 + m - g;
        float s_lo = max(lo.x, lo.y);
        float s_hi = min(hi.x, hi.y);
        float glow_now = step(h1, 0.07) * (0.6 + 0.4 * sin(t * 0.8 + h1 * 60.0));
        vec3 top = mix(mix(base, mainc * 0.55, 0.35 + 0.65 * h / tall), accentc * 0.75, glow_now);
        // The top, or one of the two walls that face us, each its own shade.
        float on_top = smoothstep(0.0, aa, min(s_hi - h, h - s_lo));
        float on_wall = smoothstep(0.0, aa, min(min(s_hi, h - s_lo), h - s_hi));
        vec3 wall = mix(mainc * 0.3, mainc * 0.17, step(hi.x, hi.y));
        wall = mix(wall, accentc * 0.35, glow_now * 0.6);
        col = mix(col, wall, on_wall);
        col = mix(col, top, on_top);
    }
""" + FINISH)
