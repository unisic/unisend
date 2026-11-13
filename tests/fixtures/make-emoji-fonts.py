"""Builds the colour emoji fonts the emoji-font tests load: obvious coloured squares, so a render
shows at a glance which glyphs came from them. Needs fontTools and Pillow.

    python3 tests/fixtures/make-emoji-fonts.py tests/fixtures

writes emoji-sbix.ttf (sbix bitmaps, the format of Apple's font) and emoji-sbix.ttc (the same
face twice, as a collection). The faces also map 'a' and '1' so a render shows whether the font
ever takes over ordinary letters or digits: it must not.
"""

import io
import sys

from fontTools.fontBuilder import FontBuilder
from fontTools.feaLib.builder import addOpenTypeFeaturesFromString
from fontTools.pens.ttGlyphPen import TTGlyphPen
from fontTools.ttLib import TTCollection, newTable
from fontTools.ttLib.tables.sbixGlyph import Glyph as SbixGlyph
from fontTools.ttLib.tables.sbixStrike import Strike
from PIL import Image, ImageDraw

UPEM = 1000
PPEM = 64

# name, code point (or None for a ligature), fill
GLYPHS = [
    ("grin", 0x1F600, (230, 40, 40)),  # red
    ("heart", 0x2764, (40, 90, 230)),  # blue
    ("thumbs", 0x1F44D, (40, 180, 60)),  # green
    ("smile", 0x263A, (150, 75, 0)),  # brown: text fonts have it too
    ("man", 0x1F468, (120, 120, 120)),
    ("woman", 0x1F469, (120, 120, 120)),
    ("girl", 0x1F467, (120, 120, 120)),
    ("tone", 0x1F3FD, (120, 120, 120)),
    ("ri_p", 0x1F1F5, (120, 120, 120)),
    ("ri_l", 0x1F1F1, (120, 120, 120)),
    ("keycap", 0x20E3, (120, 120, 120)),
    ("a", ord("a"), (200, 0, 200)),  # magenta: a letter taken over
    ("one", ord("1"), (200, 0, 200)),  # magenta: a digit taken over
    ("family", None, (240, 200, 0)),  # yellow: ZWJ sequence
    ("thumbs_tone", None, (0, 200, 200)),  # cyan: skin tone
    ("flag_pl", None, (255, 255, 255)),  # white: flag
    ("keycap_one", None, (255, 140, 0)),  # orange: keycap
]


def png(fill):
    img = Image.new("RGBA", (PPEM, PPEM), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    d.rectangle([4, 4, PPEM - 5, PPEM - 5], fill=fill + (255,))
    d.line([8, PPEM - 9, PPEM - 9, 8], fill=(0, 0, 0, 255), width=5)
    out = io.BytesIO()
    img.save(out, "PNG")
    return out.getvalue()


def build(family):
    # Glyph 3 is the variation selector: GPUI skips glyph 3 of an emoji font, as Noto's VS16.
    order = [".notdef", "space", "zwj", "vs16"] + [g[0] for g in GLYPHS]
    cmap = {0x20: "space", 0x200D: "zwj", 0xFE0F: "vs16"}
    cmap.update({cp: name for name, cp, _ in GLYPHS if cp is not None})
    fb = FontBuilder(UPEM, isTTF=True)
    fb.setupGlyphOrder(order)
    fb.setupCharacterMap(cmap)
    empty = TTGlyphPen(None).glyph()
    fb.setupGlyf({name: empty for name in order})
    advance = {name: UPEM for name in order}
    advance.update(zwj=0, vs16=0)
    fb.setupHorizontalMetrics({name: (advance[name], 0) for name in order})
    fb.setupHorizontalHeader(ascent=950, descent=-250)
    fb.setupNameTable({"familyName": family, "styleName": "Regular", "psName": family.replace(" ", "")})
    fb.setupOS2(sTypoAscender=950, sTypoDescender=-250, usWinAscent=950, usWinDescent=250)
    fb.setupPost()
    addOpenTypeFeaturesFromString(
        fb.font,
        """
        feature ccmp {
            sub man zwj woman zwj girl by family;
            sub thumbs tone by thumbs_tone;
            sub ri_p ri_l by flag_pl;
            sub one vs16 keycap by keycap_one;
        } ccmp;
        """,
    )
    sbix = newTable("sbix")
    strike = Strike(ppem=PPEM, resolution=72)
    for name, _, fill in GLYPHS:
        strike.glyphs[name] = SbixGlyph(glyphName=name, graphicType="png ", imageData=png(fill), originOffsetX=0, originOffsetY=-12)
    sbix.strikes[PPEM] = strike
    fb.font["sbix"] = sbix
    return fb.font


if __name__ == "__main__":
    out = sys.argv[1] if len(sys.argv) > 1 else "."
    build("Test Squares Emoji").save(f"{out}/emoji-sbix.ttf")
    collection = TTCollection()
    collection.fonts = [build("Test Squares Emoji"), build("Test Squares Emoji UI")]
    collection.save(f"{out}/emoji-sbix.ttc")
