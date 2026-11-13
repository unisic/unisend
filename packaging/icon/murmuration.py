#!/usr/bin/env python3
"""Generates the Murmur icon: a murmuration of starlings, one ribbon of birds with a half twist,
seen in three-quarter view on an evening sky built around violet #8006da.

Usage: murmuration.py OUTDIR
Writes the SVG masters render.sh rasterises (all on a 108-unit canvas, the Android adaptive size):
  icon.svg         desktop tile, fine birds, for 256 px and up
  icon-small.svg   desktop tile, larger birds so the flock holds together at 16 to 192 px
  foreground.svg   Android adaptive foreground: the small flock alone, centred on the 66-unit safe zone
  monochrome.svg   Android themed-icon layer: the same flock as a one-colour silhouette
  splash.svg       Android splash: sky and flock inside the 72-unit circle the splash screen shows
The sky of the Android background layer is android/app/src/main/res/drawable/ic_launcher_background.xml;
keep its colours equal to SKY_STOPS and GLOW.
Deterministic: a fixed seed gives the same flock on every run. Needs numpy.
"""
import math
import os
import sys

import numpy as np

HEAD = '<svg xmlns="http://www.w3.org/2000/svg" width="108" height="108" viewBox="0 0 108 108">'
SKY_STOPS = [(0, "#8006da"), (0.55, "#4c0196"), (1, "#24004a")]
GLOW = (54, 112, 64, 16, "#c2479e", 0.32)  # cx, cy, rx, ry, colour, opacity: warm light low on the horizon
# Far birds take on the sky's colour, near birds are near white, the edge-on seam is pure white.
BIRD_FAR, BIRD_NEAR, SEAM = "#a463ea", "#f7efff", "#ffffff"
HALF_PI = math.pi / 2

# Optical sizes: the same ribbon. With pixels to spare, near birds are few and large enough to show
# their wings and far birds are tiny; small sizes keep the flock denser so it reads as one shape.
LARGE = dict(n=5600, size=0.30, sfar=0.85, snear=3.0, near_density=0.3)
SMALL = dict(n=4600, size=0.50, sfar=0.7, snear=2.4, near_density=0.7)
FLOCK = dict(seed=5, thick=0.25, hjit=0.22, focal=140.0, op_far=0.5, trail=40, trailreach=5.0, fit=24.0, offset=(0.0, -1.0))
TILE_SCALE = 1.45  # a desktop tile has no launcher mask eating its edges, so the flock is drawn larger


def ribbon(u):
    """Centre line, twist and half width of the ribbon at u in [0, 1]. It runs from the far left
    to the near right, rising; twist pi/2 shows it face-on, 0 edge-on, where it crosses itself."""
    u = np.asarray(u, float)
    centre = np.stack([-28 + 56 * u, 14 - 28 * u - 3 * np.sin(math.pi * u), -12 + 24 * u], -1)
    twist = -HALF_PI * np.tanh((u - 0.5) * 2.6) / math.tanh(1.3)
    width = 17 * (1 + 0.35 * u) * np.clip(np.sin(math.pi * u), 0, 1) ** 0.5
    return centre, twist, width


def rotate(v, axis, ang):
    c, s = np.cos(ang)[:, None], np.sin(ang)[:, None]
    return v * c + np.cross(axis, v) * s + axis * np.sum(axis * v, 1)[:, None] * (1 - c)


def flock(p):
    """Birds spread over the ribbon in 3D, then projected. Where the ribbon turns edge-on they pile
    up into a bright seam, the way real murmurations show their folds."""
    rng = np.random.default_rng(p["seed"])
    uu = np.linspace(0, 1, 2001)
    c, _, w = ribbon(uu)
    speed = np.linalg.norm(np.gradient(c, uu, axis=0), axis=1)
    # fewer birds per area on the near lobe: they are drawn larger, so it stays as dense to the eye
    cdf = np.cumsum(w * speed * (1 + (p["near_density"] - 1) * uu))
    cdf /= cdf[-1]
    n = p["n"]
    u = np.interp(rng.random(n), cdf, uu)
    v = rng.uniform(-1, 1, n)
    th = rng.normal(0, 1, n) * p["thick"]
    eps = 1e-3
    t = ribbon(u + eps)[0] - ribbon(u - eps)[0]
    t /= np.linalg.norm(t, axis=1)[:, None]
    ref = np.tile([0.0, 0.0, 1.0], (n, 1))
    w0 = ref - t * np.sum(ref * t, 1)[:, None]
    w0 /= np.linalg.norm(w0, axis=1)[:, None]
    centre, twist, width = ribbon(u)
    wd = rotate(w0, t, twist)
    pos = centre + wd * (v * width)[:, None] + np.cross(t, wd) * th[:, None]
    seam = np.cos(twist) ** 30  # 1 where the ribbon is edge-on

    # a small group leaving the near tip, so the flock is going somewhere
    k = p["trail"]
    tip = ribbon(np.array([1.0]))[0][0]
    tt = ribbon(np.array([1.0]))[0][0] - ribbon(np.array([1 - 2 * eps]))[0][0]
    tt /= np.linalg.norm(tt)
    d = 2 + rng.exponential(p["trailreach"], k)
    jit = rng.normal(0, 1, (k, 3)) * (0.8 + 0.25 * d)[:, None] * np.array([1, 1, 0.5])
    pos = np.concatenate([pos, tip + tt * d[:, None] + jit])
    t = np.concatenate([t, np.tile(tt, (k, 1))])
    seam = np.concatenate([seam, np.zeros(k)])
    fade = np.concatenate([np.zeros(n), np.exp(-d / (2.5 * p["trailreach"]))])

    sc = p["focal"] / (p["focal"] - pos[:, 2])
    X, Y = 54 + pos[:, 0] * sc, 54 + pos[:, 1] * sc
    heading = np.arctan2(t[:, 1], t[:, 0]) + rng.normal(0, p["hjit"], len(X))
    core = fade == 0
    zmin, zmax = pos[core, 2].min(), pos[core, 2].max()
    depth = np.clip((pos[:, 2] - zmin) / (zmax - zmin), 0, 1)
    size = p["size"] * sc * (p["sfar"] + (p["snear"] - p["sfar"]) * depth ** 1.5) * rng.uniform(0.8, 1.15, len(X))
    # centre the core's robust bounding box and scale it to the target half extent
    x0, x1 = np.percentile(X[core], [0.5, 99.5])
    y0, y1 = np.percentile(Y[core], [0.5, 99.5])
    s = p["fit"] * 2 / max(x1 - x0, y1 - y0)
    X = 54 + p["offset"][0] + (X - (x0 + x1) / 2) * s
    Y = 54 + p["offset"][1] + (Y - (y0 + y1) / 2) * s
    size *= s
    flap = rng.uniform(-0.3, 0.45, len(X))
    fore = rng.uniform(0.55, 1.0, len(X))
    return dict(depth=depth, seam=seam, X=X, Y=Y, size=size, heading=heading, flap=flap, fore=fore, fade=fade)


def bird(x, y, size, heading, flap, fore):
    """A starling in flight: two curved wings swept back from a small body, span 2 * size.
    flap moves the wingtips (negative raises them), fore narrows the span for birds seen at an angle."""
    ch, sh = math.cos(heading), math.sin(heading)
    pts = [(-1, 0.30 + flap), (-0.45, -0.30 + flap * 0.4), (0, -0.16), (0.45, -0.30 + flap * 0.4), (1, 0.30 + flap),
           (0.42, -0.06 + flap * 0.35), (0, 0.22), (-0.42, -0.06 + flap * 0.35)]
    q = []
    for px, py in pts:
        px, py = px * fore * size, py * size
        q.append("%.2f,%.2f" % (x - px * sh - py * ch, y + px * ch - py * sh))
    return f"M{q[0]}Q{q[1]} {q[2]}Q{q[3]} {q[4]}Q{q[5]} {q[6]}Q{q[7]} {q[0]}Z"


def mix(a, b, t):
    a = [int(a[i:i + 2], 16) for i in (1, 3, 5)]
    b = [int(b[i:i + 2], 16) for i in (1, 3, 5)]
    return "#" + "".join("%02x" % round(x + (y - x) * t) for x, y in zip(a, b))


def birds(optical, mono=False):
    """SVG paths for the flock, dim birds first. Birds share one path per depth and seam step,
    which keeps the file to a few dozen elements. mono: one opaque colour, no trailing group."""
    p = dict(FLOCK, **optical)
    f = flock(p)
    groups = {}
    for i in range(len(f["X"])):
        if mono and f["fade"][i]:
            continue
        key = (round(f["depth"][i] * 10), round(f["seam"][i] * 5), round(f["fade"][i] * 6) if f["fade"][i] else 6)
        groups.setdefault(key, []).append(bird(f["X"][i], f["Y"][i], f["size"][i], f["heading"][i], f["flap"][i], f["fore"][i]))
    out = []
    for (dk, sk, fk), shapes in sorted(groups.items(), key=lambda g: (g[0][0] + 6 * g[0][1], g[0][2])):
        if mono:
            out.append(f'<path fill="#ffffff" d="{"".join(shapes)}"/>')
            continue
        k, e = (dk / 10) ** 0.9, sk / 5
        op = max(p["op_far"] + (1 - p["op_far"]) * k, e) * fk / 6
        if op >= 0.05:
            out.append(f'<path fill="{mix(mix(BIRD_FAR, BIRD_NEAR, k), SEAM, e)}" fill-opacity="{op:.3f}" d="{"".join(shapes)}"/>')
    return "".join(out), f


def sky():
    stops = "".join(f'<stop offset="{o}" stop-color="{c}"/>' for o, c in SKY_STOPS)
    cx, cy, rx, ry, col, op = GLOW
    defs = (f'<linearGradient id="sky" x1="0" y1="0" x2="0" y2="1">{stops}</linearGradient>'
            f'<radialGradient id="glow" cx="{cx}" cy="{cy}" r="{rx}" gradientUnits="userSpaceOnUse" '
            f'gradientTransform="translate({cx} {cy}) scale(1 {ry / rx:.4f}) translate({-cx} {-cy})">'
            f'<stop offset="0" stop-color="{col}" stop-opacity="{op}"/><stop offset="1" stop-color="{col}" stop-opacity="0"/>'
            f'</radialGradient>')
    return defs, '<rect width="108" height="108" fill="url(#sky)"/><rect width="108" height="108" fill="url(#glow)"/>'


def tile(flock_svg):
    defs, ground = sky()
    return (f'{HEAD}<defs>{defs}<clipPath id="tile"><rect width="108" height="108" rx="24"/></clipPath></defs>'
            f'<g clip-path="url(#tile)">{ground}<g transform="translate(54 54) scale({TILE_SCALE}) translate(-54 -54)">'
            f'{flock_svg}</g></g></svg>')


def main(out):
    os.makedirs(out, exist_ok=True)
    large, _ = birds(LARGE)
    small, f = birds(SMALL)
    mono, _ = birds(SMALL, mono=True)
    # The ribbon must stay inside the 66-unit safe circle; only the trailing group may leave it.
    core = f["fade"] == 0
    r = np.hypot(f["X"][core] - 54, f["Y"][core] - 54)
    assert np.percentile(r, 99.5) < 33, f"ribbon leaves the safe zone: r99.5={np.percentile(r, 99.5):.1f}"
    defs, ground = sky()
    files = {
        "icon.svg": tile(large),
        "icon-small.svg": tile(small),
        "foreground.svg": f"{HEAD}{small}</svg>",
        "monochrome.svg": f"{HEAD}{mono}</svg>",
        "splash.svg": (f'{HEAD}<defs>{defs}<clipPath id="c"><circle cx="54" cy="54" r="36"/></clipPath></defs>'
                       f'<g clip-path="url(#c)">{ground}{small}</g></svg>'),
    }
    for name, svg in files.items():
        with open(os.path.join(out, name), "w") as fh:
            fh.write(svg)


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit("usage: murmuration.py OUTDIR")
    main(sys.argv[1])
