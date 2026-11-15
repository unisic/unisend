#!/usr/bin/env python3
"""Preview sheet of the shipped Unisend icon: writes final.png next to this file.

Reads what packaging/icon/render.sh installed (legacy mipmaps, hicolor PNGs) and renders the
adaptive layers from packaging/icon/unisend.py, the same paths as the vector drawables in
android/app/src/main/res/drawable. Fails if the mark leaves the 66-unit safe circle.
Needs ImageMagick 7 with librsvg, python3 with Pillow and numpy.
"""
import os
import subprocess
import sys
import tempfile

import numpy as np
from PIL import Image, ImageDraw, ImageFont

from make import DIM, FONT, INK, PANEL, SAFE_R, launcher, render, themed

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.normpath(os.path.join(HERE, "..", "..", ".."))
RES = os.path.join(ROOT, "android/app/src/main/res")
HICOLOR = os.path.join(ROOT, "packaging/linux/icons/hicolor")


def main():
    with tempfile.TemporaryDirectory() as svg:
        subprocess.run([sys.executable, os.path.join(ROOT, "packaging/icon/unisend.py"), svg], check=True)
        layers = {k: render(os.path.join(svg, f), 1080) for k, f in
                  (("fg", "foreground.svg"), ("mono", "monochrome.svg"), ("splash", "splash.svg"))}
    layers["bg"] = Image.new("RGBA", layers["fg"].size, (0x80, 0x06, 0xda, 255))
    layers["composite"] = Image.alpha_composite(layers["bg"], layers["fg"])

    a = np.asarray(layers["fg"].split()[3]) > 127
    ys, xs = np.nonzero(a)
    r = np.hypot((xs + 0.5) / 10 - 54, (ys + 0.5) / 10 - 54).max()
    assert r <= SAFE_R - 1.5, f"mark too close to the safe circle, r={r:.2f}"

    W, H = 1500, 1100
    sheet = Image.new("RGBA", (W, H), PANEL + (255,))
    dr = ImageDraw.Draw(sheet)
    f_big, f_small = ImageFont.truetype(FONT, 30), ImageFont.truetype(FONT, 16)
    dr.text((40, 30), "Unisend icon, as shipped", font=f_big, fill=INK)
    dr.text((40, 72), f"mark radius {r:.1f} of 33 safe", font=f_small, fill=DIM)

    lv = layers["composite"].resize((324, 324), Image.LANCZOS)
    g = ImageDraw.Draw(lv)
    g.rectangle([54, 54, 270, 270], outline=(255, 255, 255, 90))
    g.ellipse([63, 63, 261, 261], outline=(198, 242, 78, 160), width=2)
    sheet.paste(lv, (40, 110), lv)
    dr.text((40, 442), "adaptive layer: 108, 72 viewport, 66 safe", font=f_small, fill=DIM)

    # launcher sizes: S23 xxxhdpi launcher icons are about 150 to 190 px
    x = 400
    for shape, yy in (("squircle", 110), ("circle", 310)):
        xx = x
        for s in (160, 96, 48):
            im = launcher(layers, shape, s)
            sheet.paste(im, (xx, yy + (160 - s) // 2), im)
            xx += s + 24
        dr.text((x, yy + 164), f"{shape} 160 / 96 / 48", font=f_small, fill=DIM)
    x = 820
    for i, dark in enumerate((False, True)):
        t = themed(layers, 120, dark)
        sheet.paste(t, (x + i * 140, 110), t)
    dr.text((x, 236), "themed light / dark", font=f_small, fill=DIM)
    sp = layers["splash"].resize((160, 160), Image.LANCZOS)
    surf = Image.new("RGBA", (180, 180), (0x16, 0x14, 0x1c, 255))
    surf.paste(sp, (10, 10), sp)
    sheet.paste(surf, (x, 290))
    dr.text((x, 474), "splash", font=f_small, fill=DIM)

    # legacy mipmaps at their own pixel size, square tile then round
    x = 40
    for n in ("xxxhdpi", "xxhdpi", "xhdpi", "hdpi", "mdpi"):
        for name in ("ic_launcher", "ic_launcher_round"):
            im = Image.open(os.path.join(RES, f"mipmap-{n}", f"{name}.png")).convert("RGBA")
            sheet.paste(im, (x, 510 + 192 - im.height), im)
            x += im.width + 16
    dr.text((40, 712), "legacy mipmaps 192 / 144 / 96 / 72 / 48 px, tile and round", font=f_small, fill=DIM)

    # desktop hicolor PNGs on dark and light panels, 32, 24 and 16 also enlarged
    y = 750
    x = 40
    for s in (128, 64, 48, 32, 24, 16):
        im = Image.open(os.path.join(HICOLOR, f"{s}x{s}", "apps", "dev.gpui.beeper.png")).convert("RGBA")
        for j, bgc in enumerate(((0x2a, 0x2a, 0x30), (0xef, 0xef, 0xf2))):
            box = Image.new("RGBA", (s + 16, s + 16), bgc + (255,))
            box.paste(im, (8, 8), im)
            sheet.paste(box, (x, y + j * 150 + (128 - s) // 2))
        x += s + 40
    for s in (32, 24, 16):
        im = Image.open(os.path.join(HICOLOR, f"{s}x{s}", "apps", "dev.gpui.beeper.png")).convert("RGBA")
        box = Image.new("RGBA", (s, s), (0x2a, 0x2a, 0x30, 255))
        box.alpha_composite(im)
        z = 128 // s
        big = box.resize((s * z, s * z), Image.NEAREST)
        sheet.paste(big, (x + 10, y), big)
        x += s * z + 24
    dr.text((40, y + 300), "desktop hicolor 128 / 64 / 48 / 32 / 24 / 16 px (48 and below: icon-small);"
            " right: 32, 24, 16 enlarged", font=f_small, fill=DIM)

    sheet.convert("RGB").save(os.path.join(HERE, "final.png"), optimize=True)
    print(f"final.png: mark radius {r:.2f} (safe {SAFE_R})")


if __name__ == "__main__":
    main()
