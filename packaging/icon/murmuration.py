#!/usr/bin/env python3
"""Generates the Murmur icon: a murmuration of starlings, one ribbon of birds that twists over
itself once, on an evening sky built around violet #8006da.

Usage: murmuration.py OUTDIR
Writes the SVG masters render.sh rasterises (all on a 108-unit canvas, the Android adaptive size):
  icon.svg         desktop tile, many small birds, for 256 px and up
  icon-small.svg   desktop tile, fewer larger birds so the flock holds together at 16 to 192 px
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
SKY_STOPS = [(0, "#8a12e0"), (0.5, "#8006da"), (1, "#3c0080")]
GLOW = (54, 104, 72, 26, "#d65aa8", 0.42)  # cx, cy, rx, ry, colour, opacity: warm light low on the horizon
BIRD_FAR, BIRD_NEAR = "#b47aee", "#fffbff"
HALF_PI = math.pi / 2

# Optical sizes: the flock is the same ribbon, drawn with more, finer birds when there are pixels for them.
LARGE = dict(n=7000, size=0.42, snear=1.25, op_far=0.55)
SMALL = dict(n=5200, size=0.70, snear=1.30, op_far=0.58)
FLOCK = dict(seed=3, thick=0.45, hjit=0.28, strays=80, strayreach=5.0, focal=90.0, sfar=0.7,
             ripple=(0.3, 1.6), fit=26.0, offset=(-1.5, 1.5))
TILE_SCALE = 1.4  # a desktop tile has no launcher mask eating its edges, so the flock is drawn larger


def ribbon(k=13, bulge=10, pinch=0.42, wfar=11, wnear=15, tw_end=-HALF_PI):
    """Control points (x, y, z, twist, half width) of the flock's centre line. It rises from the far
    lower left to the near upper right; twist 0 shows the ribbon edge-on (a dense crease), pi/2 face-on."""
    pts = []
    for i in range(k):
        s = i / (k - 1)
        sm = 1 / (1 + math.exp(-(s - pinch) * 12))
        pts.append((-32 + 64 * s, 16 - 30 * s - bulge * math.sin(math.pi * s), -10 + 20 * s,
                    HALF_PI + (tw_end - HALF_PI) * sm, (wfar + (wnear - wfar) * sm) * math.sin(math.pi * s) ** 0.75))
    return np.array(pts)


def spline(cp, u):
    """Catmull-Rom through the control points, u in [0, 1]."""
    u = np.clip(np.asarray(u, float), 0, 1)
    k = len(cp) - 1
    s = u * k
    i = np.clip(np.floor(s).astype(int), 0, k - 1)
    f = (s - i)[..., None]
    P = np.vstack([cp[:1] * 2 - cp[1:2], cp, cp[-1:] * 2 - cp[-2:-1]])
    p0, p1, p2, p3 = P[i], P[i + 1], P[i + 2], P[i + 3]
    return 0.5 * (2 * p1 + (p2 - p0) * f + (2 * p0 - 5 * p1 + 4 * p2 - p3) * f ** 2 + (3 * p1 - p0 - 3 * p2 + p3) * f ** 3)


def rotate(v, axis, ang):
    c, s = np.cos(ang)[:, None], np.sin(ang)[:, None]
    return v * c + np.cross(axis, v) * s + axis * np.sum(axis * v, 1)[:, None] * (1 - c)


def flock(p):
    """Birds spread evenly over the ribbon's area in 3D, then projected: where the ribbon turns
    edge-on they pile up into a bright crease, the way real murmurations show their folds."""
    rng = np.random.default_rng(p["seed"])
    cp = ribbon()
    center = lambda u: spline(cp, u)[..., :3]
    width = lambda u: np.maximum(spline(cp, u)[..., 4], 0)
    uu = np.linspace(0, 1, 2001)
    speed = np.linalg.norm(np.gradient(center(uu), uu, axis=0), axis=1)
    cdf = np.cumsum(width(uu) * speed)
    cdf /= cdf[-1]
    m = p["n"] * 3
    u = np.interp(rng.random(m), cdf, uu)
    v = rng.uniform(-1, 1, m)
    amp, freq = p["ripple"]  # slow density waves across the sheet
    keep = rng.random(m) < 1 - amp * (0.5 + 0.5 * np.sin(2 * math.pi * (freq * u + 0.35 * v) + 1.3))
    u, v = u[keep][:p["n"]], v[keep][:p["n"]]
    n = len(u)
    th = rng.normal(0, 1, n) * p["thick"]

    eps = 1e-3
    t = center(u + eps) - center(u - eps)
    t /= np.linalg.norm(t, axis=1)[:, None]
    ref = np.tile([0.0, 0.0, 1.0], (n, 1))
    w0 = ref - t * np.sum(ref * t, 1)[:, None]
    w0 /= np.linalg.norm(w0, axis=1)[:, None]
    w = rotate(w0, t, spline(cp, u)[:, 3])
    pos = center(u) + w * (v * width(u))[:, None] + np.cross(t, w) * th[:, None]

    # strays: a thinning trail beyond each tip, along the direction of travel
    fade = [np.zeros(n)]
    heads = [t]
    places = [pos]
    for u0, sgn, k in ((0.0, -1, p["strays"] // 2), (1.0, 1, p["strays"] - p["strays"] // 2)):
        tip = center(np.array([u0]))[0]
        ui = np.clip(u0 - sgn * 2 * eps, 0, 1)
        tt = center(np.array([ui + eps]))[0] - center(np.array([ui - eps]))[0]
        tt /= np.linalg.norm(tt)
        d = rng.exponential(p["strayreach"], k)
        jit = rng.normal(0, 1, (k, 3)) * (1 + 0.45 * d)[:, None] * np.array([1, 1, 0.6])
        places.append(tip + sgn * tt * d[:, None] + jit)
        heads.append(np.tile(sgn * tt, (k, 1)))
        fade.append(np.exp(-d / (2.2 * p["strayreach"])))
    pos, t, fade = np.concatenate(places), np.concatenate(heads), np.concatenate(fade)

    sc = p["focal"] / (p["focal"] - pos[:, 2])
    X, Y = 54 + pos[:, 0] * sc, 54 + pos[:, 1] * sc
    heading = np.arctan2(t[:, 1], t[:, 0]) + rng.normal(0, p["hjit"], len(X))
    core = fade == 0
    zmin, zmax = pos[core, 2].min(), pos[core, 2].max()
    depth = np.clip((pos[:, 2] - zmin) / (zmax - zmin), 0, 1)
    size = p["size"] * sc * (p["sfar"] + (p["snear"] - p["sfar"]) * depth) * rng.uniform(0.75, 1.2, len(X))
    # centre the core's robust bounding box and scale it to the target half extent
    x0, x1 = np.percentile(X[core], [0.5, 99.5])
    y0, y1 = np.percentile(Y[core], [0.5, 99.5])
    k = p["fit"] * 2 / max(x1 - x0, y1 - y0)
    X = 54 + p["offset"][0] + (X - (x0 + x1) / 2) * k
    Y = 54 + p["offset"][1] + (Y - (y0 + y1) / 2) * k
    size *= k
    flap = rng.uniform(-0.35, 0.5, len(X))
    fore = rng.uniform(0.45, 1.0, len(X))
    return dict(depth=depth, X=X, Y=Y, size=size, heading=heading, flap=flap, fore=fore, fade=fade)


def bird(x, y, size, heading, flap, fore):
    """A starling in flight: two curved wings swept back from a small body, span 2 * size.
    flap moves the wingtips (negative raises them), fore narrows the span for birds seen at an angle."""
    ch, sh = math.cos(heading), math.sin(heading)
    pts = [(-1, 0.30 + flap), (-0.45, -0.30 + flap * 0.4), (0, -0.16), (0.45, -0.30 + flap * 0.4), (1, 0.30 + flap),
           (0.42, -0.06 + flap * 0.35), (0, 0.22), (-0.42, -0.06 + flap * 0.35)]
    q = []
    for px, py in pts:
        px, py = px * fore * size, py * size
        q.append("%.1f,%.1f" % (x - px * sh - py * ch, y + px * ch - py * sh))
    return f"M{q[0]}Q{q[1]} {q[2]}Q{q[3]} {q[4]}Q{q[5]} {q[6]}Q{q[7]} {q[0]}Z"


def mix(a, b, t):
    a = [int(a[i:i + 2], 16) for i in (1, 3, 5)]
    b = [int(b[i:i + 2], 16) for i in (1, 3, 5)]
    return "#" + "".join("%02x" % round(x + (y - x) * t) for x, y in zip(a, b))


def birds(optical, mono=False, buckets=14):
    """SVG paths for the flock, far birds first. Birds share one path per depth band and brightness
    step, which keeps the file to a few dozen elements. mono: one opaque colour, no strays."""
    p = dict(FLOCK, **optical)
    f = flock(p)
    step = np.random.default_rng(p["seed"] + 1).integers(0, 3, len(f["X"]))
    groups, strays = {}, []
    for i in np.argsort(f["depth"]):
        shape = bird(f["X"][i], f["Y"][i], f["size"][i], f["heading"][i], f["flap"][i], f["fore"][i])
        if f["fade"][i]:
            strays.append((i, shape))
        else:
            b = min(buckets - 1, int(f["depth"][i] * buckets))
            groups.setdefault((b, step[i]), []).append(shape)
    out = []
    for (b, j), shapes in sorted(groups.items()):
        k = ((b + 0.5) / buckets) ** 0.8
        if mono:
            out.append(f'<path fill="#ffffff" d="{"".join(shapes)}"/>')
            continue
        op = min(1.0, (p["op_far"] + (1 - p["op_far"]) * k) * (1 + 0.12 * (j - 1)))
        out.append(f'<path fill="{mix(BIRD_FAR, BIRD_NEAR, k)}" fill-opacity="{op:.3f}" d="{"".join(shapes)}"/>')
    if not mono:
        for i, shape in strays:
            k = f["depth"][i] ** 0.8
            op = (p["op_far"] + (1 - p["op_far"]) * k) * f["fade"][i]
            if op >= 0.06:
                out.append(f'<path fill="{mix(BIRD_FAR, BIRD_NEAR, k)}" fill-opacity="{op:.3f}" d="{shape}"/>')
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
    # The dense core must stay inside the 72-unit circle a launcher mask leaves visible.
    core = f["fade"] == 0
    r = np.hypot(f["X"][core] - 54, f["Y"][core] - 54)
    assert np.percentile(r, 99) < 36, f"flock leaves the masked area: r99={np.percentile(r, 99):.1f}"
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
