#!/usr/bin/env python3
"""Unisend icon candidates: writes the SVG layers of each candidate and its preview sheets.

Usage: make.py            (run from anywhere; writes next to this file)
Per candidate directory (candidate-a, candidate-b, candidate-c):
  foreground.svg   Android adaptive foreground, 108-unit canvas, mark inside the 66-unit safe circle
  background.svg   Android adaptive background, flat brand violet
  monochrome.svg   Android 13+ themed-icon layer: the foreground as one-colour silhouette
  icon.svg         desktop tile (rounded square, mark drawn larger: no launcher mask eats the edge)
  splash.svg       Android splash: background and mark inside the 72-unit circle
  preview.png      masks, sizes, wallpapers, themed icon, desktop sizes
and sheet.png with all three side by side.
Same canvas and file roles as packaging/icon/murmuration.py, so packaging/icon/render.sh can
rasterise the chosen one. Needs ImageMagick 7 with librsvg, python3 with Pillow and numpy.
"""
import os
import subprocess
import tempfile

import numpy as np
from PIL import Image, ImageDraw, ImageFont

HERE = os.path.dirname(os.path.abspath(__file__))
HEAD = '<svg xmlns="http://www.w3.org/2000/svg" width="108" height="108" viewBox="0 0 108 108">'
VIOLET, LIME, WHITE = "#8006da", "#c6f24e", "#ffffff"
TILE_SCALE = 1.4  # desktop tile: no mask, so the mark is drawn larger, as in murmuration.py
SAFE_R = 33  # radius of the 66-unit safe circle
B_SCALE = 0.86  # B's square tail corner sits on the diagonal, so the whole U is drawn smaller

# A: one stroke. Down the left arm, round the bottom, up the right arm, out as a send arrow.
# The arrowhead is the lime moment of sending; the monochrome layer draws it in the same colour.
A = ('<g transform="translate(54 54) scale(0.92) translate(-54 -54)" fill="none" stroke-width="11" '
     'stroke-linecap="round" stroke-linejoin="round"><path d="M35,40 V62 A14,14 0 0 0 63,62 V31" stroke="{c}"/>'
     '<path d="M53.5,39.5 L63,30 L72.5,39.5" stroke="{h}"/></g>')


def b_mark(left, right):
    """B: two speech-bubble halves that join into one U. The left half keeps its bubble tail,
    the right half is round; a thin slit at the bottom shows they are two."""
    cy, ro, ri, g, top, h = 55.5, 23.5, 10.5, 3.0, 35.0, 6.5
    xl, xr = 54 - (ro + ri) / 2, 54 + (ro + ri) / 2
    lo, li = xl - h, xl + h
    ro_out, ri_out = xr + h, xr - h
    yo = cy + (ro ** 2 - (g / 2) ** 2) ** 0.5
    yi = cy + (ri ** 2 - (g / 2) ** 2) ** 0.5
    bottom = cy + ro
    gl, gr = 54 - g / 2, 54 + g / 2
    # the bubble tail: the outer corner runs to a point just past the square corner
    left_d = (f"M{lo},{top} V{bottom - 8} Q{lo},{bottom - 1} {lo - 3},{bottom + 2.5} Q{lo + 3},{bottom} {lo + 9},{bottom} H{gl} V{yi:.3f} "
              f"A{ri},{ri} 0 0 1 {li},{cy} V{top} A{h},{h} 0 0 0 {lo},{top} Z")
    right_d = (f"M{gr},{yo:.3f} A{ro},{ro} 0 0 0 {ro_out},{cy} V{top} A{h},{h} 0 0 0 {ri_out},{top} "
               f"V{cy} A{ri},{ri} 0 0 1 {gr},{yi:.3f} Z")
    return (f'<g transform="translate(54 54) scale({B_SCALE}) translate(-54 -54)" stroke-linejoin="round">'
            f'<path d="{left_d}" fill="{left}"/><path d="{right_d}" fill="{right}"/></g>')


def c_mark(frame, dot):
    """C: the Matrix bracket turned a quarter into a U, holding one message about to leave."""
    return (f'<path d="M37,33 V69.5 H71 V33" fill="none" stroke="{frame}" stroke-width="11.5" '
            f'stroke-linejoin="miter"/><circle cx="54" cy="41" r="7" fill="{dot}"/>')


CANDIDATES = {
    "candidate-a": dict(fg=A.format(c=WHITE, h=LIME), mono=A.format(c=WHITE, h=WHITE), title="A  One stroke, sent"),
    "candidate-b": dict(fg=b_mark("#d9b3ff", WHITE), mono=b_mark(WHITE, WHITE), title="B  Two halves, one U"),
    "candidate-c": dict(fg=c_mark(WHITE, LIME), mono=c_mark(WHITE, WHITE), title="C  Bracket U, one message"),
}


def svgs(c):
    bg = f'<rect width="108" height="108" fill="{VIOLET}"/>'
    big = f'<g transform="translate(54 54) scale({TILE_SCALE}) translate(-54 -54)">{c["fg"]}</g>'
    return {
        "foreground.svg": f"{HEAD}{c['fg']}</svg>",
        "background.svg": f"{HEAD}{bg}</svg>",
        "monochrome.svg": f"{HEAD}{c['mono']}</svg>",
        "icon.svg": (f'{HEAD}<defs><clipPath id="tile"><rect width="108" height="108" rx="24"/></clipPath></defs>'
                     f'<g clip-path="url(#tile)">{bg}{big}</g></svg>'),
        "splash.svg": (f'{HEAD}<defs><clipPath id="c"><circle cx="54" cy="54" r="36"/></clipPath></defs>'
                       f'<g clip-path="url(#c)">{bg}</g>{c["fg"]}</svg>'),
    }


def render(path, px):
    with tempfile.NamedTemporaryFile(suffix=".png") as t:
        subprocess.run(["magick", "-density", "1200", "-background", "none", path, "-resize", f"{px}x{px}",
                        f"PNG32:{t.name}"], check=True)
        return Image.open(t.name).convert("RGBA").copy()


def mask(shape, s):
    """Launcher masks on an s-px square: circle, Samsung-like squircle, rounded square."""
    m = Image.new("L", (s * 4, s * 4), 0)
    d = ImageDraw.Draw(m)
    S = s * 4
    if shape == "circle":
        d.ellipse([0, 0, S - 1, S - 1], fill=255)
    elif shape == "squircle":
        t = np.linspace(0, 2 * np.pi, 720)
        n = 5.0
        x = np.sign(np.cos(t)) * np.abs(np.cos(t)) ** (2 / n)
        y = np.sign(np.sin(t)) * np.abs(np.sin(t)) ** (2 / n)
        d.polygon([((xx + 1) * (S - 1) / 2, (yy + 1) * (S - 1) / 2) for xx, yy in zip(x, y)], fill=255)
    else:
        d.rounded_rectangle([0, 0, S - 1, S - 1], radius=S * 0.22, fill=255)
    return m.resize((s, s), Image.LANCZOS)


def launcher(layers, shape, s, mono=None):
    """What a launcher shows: the centre 72 of the 108-unit layers, cut by the mask."""
    big = layers["composite"] if mono is None else mono
    full = int(round(s * 108 / 72))
    im = big.resize((full, full), Image.LANCZOS)
    off = (full - s) // 2
    im = im.crop((off, off, off + s, off + s))
    out = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    out.paste(im, (0, 0), mask(shape, s))
    return out


def themed(layers, s, dark):
    bgc, fgc = ((0x2b, 0x22, 0x3f), (0xd8, 0xc4, 0xff)) if dark else ((0xea, 0xde, 0xff), (0x2a, 0x10, 0x5c))
    a = layers["mono"].split()[3]
    im = Image.new("RGBA", a.size, bgc + (255,))
    im.paste(Image.new("RGBA", a.size, fgc + (255,)), (0, 0), a)
    return launcher(layers, "circle", s, mono=im)


def placeholder(s, colour, glyph):
    """Neutral stand-in app icons for scale: flat colour, one plain shape, no brand."""
    S = s * 4
    im = Image.new("RGBA", (S, S), colour + (255,))
    d = ImageDraw.Draw(im)
    c, r = S / 2, S * 0.17
    if glyph == 0:
        d.ellipse([c - r, c - r, c + r, c + r], fill="white")
    elif glyph == 1:
        d.rounded_rectangle([c - r, c - r, c + r, c + r], radius=r * 0.3, fill="white")
    elif glyph == 2:
        d.polygon([(c, c - r * 1.1), (c + r * 1.1, c + r * 0.8), (c - r * 1.1, c + r * 0.8)], fill="white")
    else:
        d.rectangle([c - r * 1.2, c - r * 0.25, c + r * 1.2, c + r * 0.25], fill="white")
    im = im.resize((s, s), Image.LANCZOS)
    out = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    out.paste(im, (0, 0), mask("circle", s))
    return out


FONT = "/usr/share/fonts/rsms-inter-fonts/Inter-SemiBold.ttf"
PANEL, INK, DIM = (0x16, 0x14, 0x1c), (0xee, 0xea, 0xf4), (0x8d, 0x87, 0x99)


def preview(name, c, d):
    layers = {k: render(os.path.join(d, f), 1080) for k, f in
              (("fg", "foreground.svg"), ("bg", "background.svg"), ("mono", "monochrome.svg"))}
    layers["composite"] = Image.alpha_composite(layers["bg"], layers["fg"])

    # safe-zone check on the drawn pixels, in 108-unit layer coordinates
    a = np.asarray(layers["fg"].split()[3]) > 127
    ys, xs = np.nonzero(a)
    r = np.hypot((xs + 0.5) / 10 - 54, (ys + 0.5) / 10 - 54).max()
    assert r <= SAFE_R, f"{name}: mark leaves the safe circle, r={r:.2f}"
    cx, cy = (xs.min() + xs.max()) / 20, (ys.min() + ys.max()) / 20

    W, H = 1500, 1250
    sheet = Image.new("RGBA", (W, H), PANEL + (255,))
    dr = ImageDraw.Draw(sheet)
    f_big, f_small = ImageFont.truetype(FONT, 30), ImageFont.truetype(FONT, 16)
    dr.text((40, 30), c["title"], font=f_big, fill=INK)
    dr.text((40, 72), f"mark radius {r:.1f} of 33 safe, box centre ({cx:.1f}, {cy:.1f})", font=f_small, fill=DIM)

    # layer view: full 108 layer with the 72 viewport and 66 safe circle drawn on top
    lv = layers["composite"].resize((324, 324), Image.LANCZOS)
    g = ImageDraw.Draw(lv)
    g.rectangle([54, 54, 270, 270], outline=(255, 255, 255, 90))
    g.ellipse([63, 63, 261, 261], outline=(198, 242, 78, 160), width=2)
    sheet.paste(lv, (40, 110), lv)
    dr.text((40, 442), "108 layer, 72 viewport, 66 safe", font=f_small, fill=DIM)

    # masks by size: 96 / 48 / 24 per shape, 192 for squircle and circle beside them
    for shape, yy in (("circle", 110), ("squircle", 220), ("rounded", 330)):
        x = 410
        for s in (96, 48, 24):
            im = launcher(layers, shape, s)
            sheet.paste(im, (x, yy + (96 - s) // 2), im)
            x += s + 28
        dr.text((x, yy + 40), shape, font=f_small, fill=DIM)
    big = launcher(layers, "squircle", 192)
    sheet.paste(big, (800, 110), big)
    bigc = launcher(layers, "circle", 192)
    sheet.paste(bigc, (1020, 110), bigc)
    for i, (shape, s) in enumerate((("squircle", 192), ("circle", 192))):
        dr.text((800 + i * 220, 312), f"{shape} {s}", font=f_small, fill=DIM)
    for i, dark in enumerate((False, True)):
        t = themed(layers, 96, dark)
        sheet.paste(t, (1250, 110 + i * 120), t)
    dr.text((1250, 352), "themed (mono)", font=f_small, fill=DIM)
    dr.text((1250, 372), "light / dark", font=f_small, fill=DIM)

    # home screens: dark and light wallpaper, icon among neutral placeholders, at about 1 cm
    y = 480
    for i, (wall, label_c, size) in enumerate((((0x1c, 0x1f, 0x2a), (230, 230, 235), 112),
                                                ((0xe9, 0xe4, 0xdb), (40, 40, 48), 112))):
        px = 40 + i * 720
        dr.rounded_rectangle([px, y, px + 680, y + 330], radius=28, fill=wall + (255,))
        cols = [(0x5b, 0x7c, 0x99), (0x6e, 0x8f, 0x5e), (0xc0, 0x7a, 0x4a), (0x8a, 0x8a, 0x95)]
        slots = [(0, 0), (1, 0), (2, 0), (3, 0), (0, 1), (1, 1), (2, 1), (3, 1)]
        for k, (cx_, cy_) in enumerate(slots):
            ix, iy = px + 50 + cx_ * 160, y + 30 + cy_ * 150
            if (cx_, cy_) == (1, 0):
                im = launcher(layers, "squircle", size)
            elif (cx_, cy_) == (2, 1):
                im = themed(layers, size, dark=(i == 0))
            else:
                im = placeholder(size, cols[k % 4], k % 4)
            sheet.paste(im, (ix, iy), im)
            lab = "Unisend" if (cx_, cy_) in ((1, 0), (2, 1)) else "App"
            tw = dr.textlength(lab, font=f_small)
            dr.text((ix + (size - tw) / 2, iy + size + 6), lab, font=f_small, fill=label_c)
    dr.text((40, y + 340), "home screen at ~1 cm (S23 ~ 165 px/cm, shown 112 px); top: squircle, bottom: themed",
            font=f_small, fill=DIM)

    # desktop tile sizes, 16 px also shown 8x without smoothing
    y = 870
    tile = render(os.path.join(d, "icon.svg"), 1024)
    x = 40
    for s in (128, 64, 48, 32, 24, 16):
        for j, bgc in enumerate(((0x2a, 0x2a, 0x30), (0xef, 0xef, 0xf2))):
            box = Image.new("RGBA", (s + 16, s + 16), bgc + (255,))
            im = tile.resize((s, s), Image.LANCZOS)
            box.paste(im, (8, 8), im)
            sheet.paste(box, (x, y + j * 150 + (128 - s) // 2))
        x += s + 40
    t16 = tile.resize((16, 16), Image.LANCZOS)
    for j, bgc in enumerate(((0x2a, 0x2a, 0x30), (0xef, 0xef, 0xf2))):
        box = Image.new("RGBA", (16, 16), bgc + (255,))
        box.alpha_composite(t16)
        sheet.paste(box.resize((128, 128), Image.NEAREST), (x + 20, y + j * 150))
    dr.text((x + 20, y + 300), "16 px at 8x", font=f_small, fill=DIM)
    dr.text((40, y + 300), "desktop tile 128 / 64 / 48 / 32 / 24 / 16 px on dark and light panel", font=f_small, fill=DIM)

    out = sheet.convert("RGB")
    out.save(os.path.join(d, "preview.png"), optimize=True)
    return out, r


def main():
    previews = []
    for name, c in CANDIDATES.items():
        d = os.path.join(HERE, name)
        os.makedirs(d, exist_ok=True)
        for fname, body in svgs(c).items():
            with open(os.path.join(d, fname), "w") as f:
                f.write(body + "\n")
        im, r = preview(name, c, d)
        print(f"{name}: mark radius {r:.2f} (safe {SAFE_R})")
        previews.append(im)
    w = previews[0].width // 2
    sheet = Image.new("RGB", (w * 3 + 80, previews[0].height // 2 + 40), PANEL)
    for i, p in enumerate(previews):
        sheet.paste(p.resize((w, p.height // 2), Image.LANCZOS), (20 + i * (w + 20), 20))
    sheet.save(os.path.join(HERE, "sheet.png"), optimize=True)


if __name__ == "__main__":
    main()
