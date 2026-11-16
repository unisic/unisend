#!/usr/bin/env python3
"""Generates the Unisend icon: a U made of two speech-bubble halves on violet #8006da. The left
half is lilac and ends in a bubble tail at its outer corner, the right half is white and round,
and a narrow gap keeps them two pieces.

Usage: unisend.py OUTDIR
Writes the masters render.sh rasterises and copies, all on the 108-unit Android adaptive canvas:
  icon.svg          desktop tile, the full mark, for 64 px and up
  icon-small.svg    desktop tile for 16 to 48 px: the mark redrawn on a 16 px grid, wider gap,
                    one-point tail, so the two halves stay apart once the fine gap would blur shut
  foreground.svg    Android adaptive foreground: the mark inside the 66-unit safe circle
  monochrome.svg    Android themed-icon layer: the same mark in one colour
  splash.svg        Android splash: violet disc (the 72-unit circle the splash shows) and the mark
  ic_launcher_background.xml, ic_launcher_foreground.xml, ic_launcher_monochrome.xml,
  splash_icon.xml   the same layers as Android vector drawables, for res/drawable
  Unisend.icon/     macOS 26+ Icon Composer document: each half a Liquid Glass layer on a 1024
                    canvas over a near-black violet fill; the system draws its mask, glass and modes
No dependencies beyond python3.
"""
import json
import os
import sys

VIOLET, LILAC, WHITE = "#8006da", "#d9b3ff", "#ffffff"
HEAD = '<svg xmlns="http://www.w3.org/2000/svg" width="108" height="108" viewBox="0 0 108 108">'
TILE_RX = 24
# The tail corner lies on the diagonal, the first point a circle mask reaches: the mark is drawn at
# 0.84 and nudged up and right (which also balances the tail's weight), leaving it about 2 units
# inside the 33-unit safe radius.
LAYER = dict(scale=0.84, dx=1.0, dy=-0.5)
TILE_SCALE = 1.4  # a desktop tile has no launcher mask eating its edges, so the mark is drawn larger
SMALL_PX = 108 / 16  # icon-small is drawn on a 16 px grid so its edges land on whole pixels at 16, 32, 48
MAC_K = 1024 / 108 * 1.15  # the mark spans about 55% of the Icon Composer canvas
MAC_LEFT, MAC_FILL = "#b066ff", ("#2a1840", "#120a1c")


def num(v):
    return f"{v:.3f}".rstrip("0").rstrip(".")


def path(cmds, tf):
    """Path data from (letter, numbers) pairs; tf maps a point, and scales arc radii by k."""
    fn, k = tf
    out = []
    for c, *a in cmds:
        if c == "A":
            rx, ry, rot, large, sweep, x, y = a
            px, py = fn(x, y)
            out.append(f"A{num(rx * k)},{num(ry * k)} {rot} {large} {sweep} {num(px)},{num(py)}")
        elif c == "Z":
            out.append("Z")
        else:
            pts = [fn(a[i], a[i + 1]) for i in range(0, len(a), 2)]
            out.append(c + " ".join(f"{num(x)},{num(y)}" for x, y in pts))
    return "".join(out)


def mark(tf):
    """The full mark in the candidate's units: arms 13 wide, bowl radii 23.5 and 10.5, gap 3.5."""
    cy, ro, ri, g, top, h = 55.5, 23.5, 10.5, 3.5, 35.0, 6.5
    xl, xr = 54 - (ro + ri) / 2, 54 + (ro + ri) / 2
    lo, li = xl - h, xl + h
    yo = cy + (ro ** 2 - (g / 2) ** 2) ** 0.5
    yi = cy + (ri ** 2 - (g / 2) ** 2) ** 0.5
    bottom = cy + ro
    gl, gr = 54 - g / 2, 54 + g / 2
    left = [("M", lo, top), ("L", lo, bottom - 8), ("Q", lo, bottom - 1, lo - 3, bottom + 2.5),
            ("Q", lo + 3, bottom, lo + 9, bottom), ("L", gl, bottom), ("L", gl, yi),
            ("A", ri, ri, 0, 0, 1, li, cy), ("L", li, top), ("A", h, h, 0, 0, 0, lo, top), ("Z",)]
    right = [("M", gr, yo), ("A", ro, ro, 0, 0, 0, xr + h, cy), ("L", xr + h, top),
             ("A", h, h, 0, 0, 0, xr - h, top), ("L", xr - h, cy), ("A", ri, ri, 0, 0, 1, gr, yi), ("Z",)]
    return path(left, tf), path(right, tf)


def mark_small():
    """The mark on a 16 px grid: arms 3 px, gap 1 px on whole columns, U offset half a pixel right
    so the gap is not split across two columns, tail cut to one point."""
    cx, cy, ro, ri, cap, top, bottom = 8.5, 7.5, 5.5, 2.5, 1.5, 4.5, 13
    gl, gr = 8, 9
    yo = cy + (ro ** 2 - (gr - cx) ** 2) ** 0.5
    yi = cy + (ri ** 2 - (cx - gl) ** 2) ** 0.5
    lo, li, rin, rout = cx - ro, cx - ri, cx + ri, cx + ro
    left = [("M", lo, top), ("L", lo, 11.25), ("Q", lo, 13, 1.75, 14.25), ("Q", 3.5, 13, 5.25, bottom),
            ("L", gl, bottom), ("L", gl, yi), ("A", ri, ri, 0, 0, 1, li, cy), ("L", li, top),
            ("A", cap, cap, 0, 0, 0, lo, top), ("Z",)]
    right = [("M", gr, yo), ("A", ro, ro, 0, 0, 0, rout, cy), ("L", rout, top),
             ("A", cap, cap, 0, 0, 0, rin, top), ("L", rin, cy), ("A", ri, ri, 0, 0, 1, gr, yi), ("Z",)]
    tf = (lambda x, y: (x * SMALL_PX, y * SMALL_PX), SMALL_PX)
    return path(left, tf), path(right, tf)


def around_centre(scale, dx=0.0, dy=0.0):
    return (lambda x, y: (54 + scale * (x - 54) + dx, 54 + scale * (y - 54) + dy), scale)


def svg_paths(paths, colours):
    return "".join(f'<path d="{d}" fill="{c}"/>' for d, c in zip(paths, colours))


def vector(paths, colours):
    body = "".join(f'\n    <path android:fillColor="#FF{c[1:].upper()}" android:pathData="{d}" />'
                   for d, c in zip(paths, colours))
    return ('<?xml version="1.0" encoding="utf-8"?>\n'
            '<!-- Generated by packaging/icon/unisend.py; edit that and run packaging/icon/render.sh. -->\n'
            '<vector xmlns:android="http://schemas.android.com/apk/res/android"\n'
            '    android:width="108dp"\n    android:height="108dp"\n'
            f'    android:viewportWidth="108"\n    android:viewportHeight="108">{body}\n</vector>\n')


def srgb(hex_colour):
    return "srgb:" + ",".join(f"{int(hex_colour[i:i + 2], 16) / 255:.5f}" for i in (1, 3, 5)) + ",1.00000"


def mac_icon():
    """The Icon Composer document: icon.json and one layer SVG per half, each in its own group."""
    left, right = mark((lambda x, y: (512 + MAC_K * (x - 54), 512 + MAC_K * (y - 55)), MAC_K))
    head = '<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">'
    groups = [{"layers": [{"image-name": f"{n}.svg", "name": n, "glass": True}],
               "shadow": {"kind": "layer-color", "opacity": 0.5}, "specular": True,
               "translucency": {"enabled": True, "value": 0.4}} for n in ("right", "left")]
    doc = {"fill": {"linear-gradient": [srgb(c) for c in MAC_FILL],
                    "orientation": {"start": {"x": 0.5, "y": 0}, "stop": {"x": 0.5, "y": 0.7}}},
           "groups": groups, "supported-platforms": {"squares": ["macOS"]}}
    return {
        "Unisend.icon/icon.json": json.dumps(doc, indent=2) + "\n",
        "Unisend.icon/Assets/left.svg": f'{head}<path d="{left}" fill="{MAC_LEFT}"/></svg>\n',
        "Unisend.icon/Assets/right.svg": f'{head}<path d="{right}" fill="{WHITE}"/></svg>\n',
    }


def files():
    s = LAYER["scale"]
    layer = mark(around_centre(s, LAYER["dx"], LAYER["dy"]))
    tile = mark(around_centre(s * TILE_SCALE, LAYER["dx"] * TILE_SCALE, LAYER["dy"] * TILE_SCALE))
    square, disc = "M0,0H108V108H0Z", "M18,54A36,36 0 1 0 90,54A36,36 0 1 0 18,54Z"
    two, mono = (LILAC, WHITE), (WHITE, WHITE)

    def tile_svg(paths):
        return (f'{HEAD}<defs><clipPath id="tile"><rect width="108" height="108" rx="{TILE_RX}"/></clipPath></defs>'
                f'<g clip-path="url(#tile)"><rect width="108" height="108" fill="{VIOLET}"/>{svg_paths(paths, two)}</g></svg>\n')

    return {
        "icon.svg": tile_svg(tile),
        "icon-small.svg": tile_svg(mark_small()),
        "foreground.svg": f"{HEAD}{svg_paths(layer, two)}</svg>\n",
        "monochrome.svg": f"{HEAD}{svg_paths(layer, mono)}</svg>\n",
        "splash.svg": f'{HEAD}<path d="{disc}" fill="{VIOLET}"/>{svg_paths(layer, two)}</svg>\n',
        "ic_launcher_background.xml": vector([square], [VIOLET]),
        "ic_launcher_foreground.xml": vector(layer, two),
        "ic_launcher_monochrome.xml": vector(layer, mono),
        "splash_icon.xml": vector((disc, *layer), (VIOLET, *two)),
        **mac_icon(),
    }


def main():
    if len(sys.argv) != 2:
        sys.exit("usage: unisend.py OUTDIR")
    for name, body in files().items():
        out = os.path.join(sys.argv[1], name)
        os.makedirs(os.path.dirname(out), exist_ok=True)
        with open(out, "w") as f:
            f.write(body)


if __name__ == "__main__":
    main()
