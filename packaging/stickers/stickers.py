#!/usr/bin/env python3
"""The sign-in screen's stickers: the website's die-cut Lucide icons (ISC), one in each of the
app's nine chat colors, as PNGs in assets/stickers/. Run from the repository root; needs ImageMagick."""
import pathlib, subprocess, tempfile

# Lucide icon paths, colors and tilts as on unisend.org's sticker sheet. Only closed shapes: an
# open path (an arc, a crossing slash) fills as a chord and leaves shards at the sticker's edge.
STICKERS = [
    ("voice", "#2e6ef0", -8, '<path d="M12 19v3"/><path d="M19 10v2a7 7 0 0 1-14 0v-2"/><rect x="9" y="2" width="6" height="13" rx="3"/>'),
    ("offline", "#e8710a", 6, '<path d="M12 6v6l4 2"/><circle cx="12" cy="12" r="10"/>'),
    ("search", "#00838f", -4, '<path d="m21 21-4.34-4.34"/><circle cx="11" cy="11" r="8"/>'),
    ("lock", "#734ef3", 5, '<rect width="18" height="11" x="3" y="11" rx="2" ry="2"/><path d="M7 11V7a5 5 0 0 1 10 0v4"/>'),
    ("shield", "#2e7d32", -7, '<path d="M20 13c0 5-3.5 7.5-7.66 8.95a1 1 0 0 1-.67-.01C7.5 20.5 4 18 4 13V6a1 1 0 0 1 1-1c2 0 4.5-1.2 6.24-2.72a1.17 1.17 0 0 1 1.52 0C14.51 3.81 17 5 19 5a1 1 0 0 1 1 1z"/><path d="m9 12 2 2 4-4"/>'),
    ("quiet", "#c2185b", 9, '<path d="M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9Z"/>'),
    ("palette", "#4f5bd5", -3, '<path d="M12 22a1 1 0 0 1 0-20 10 9 0 0 1 10 9 5 5 0 0 1-5 5h-2.25a1.75 1.75 0 0 0-1.4 2.8l.3.4a1.75 1.75 0 0 1-1.4 2.8z"/><circle cx="13.5" cy="6.5" r=".5"/><circle cx="17.5" cy="10.5" r=".5"/><circle cx="6.5" cy="12.5" r=".5"/><circle cx="8.5" cy="7.5" r=".5"/>'),
    ("smile", "#d32f2f", 7, '<path d="M13.267 2.08a10 10 0 108.653 8.653"/><path d="M15 10V9"/><path d="M16 5h6"/><path d="M16.472 15a6 6 0 01-8.943 0"/><path d="M19 2v6"/><path d="M9 10V9"/>'),
    ("poll", "#37474f", -6, '<path d="m9 12 2 2 4-4"/><path d="M5 7c0-1.1.9-2 2-2h10a2 2 0 0 1 2 2v12H5V7Z"/><path d="M22 19H2"/>'),
]
SIZE = 168  # 56 points at 3x

def svg(fill, rot, d):
    layer = lambda style: f'<g transform="rotate({rot} 12 12)" stroke-linecap="round" stroke-linejoin="round" {style}>{d}</g>'
    return (f'<svg xmlns="http://www.w3.org/2000/svg" width="{SIZE}" height="{SIZE}" viewBox="-6 -6 36 36">'
            + layer('fill="#f2f2f2" stroke="#f2f2f2" stroke-width="9"')
            + layer(f'fill="{fill}" stroke="{fill}" stroke-width="6"')
            + layer('fill="none" stroke="#ffffff" stroke-width="2"') + '</svg>')

out = pathlib.Path("assets/stickers")
out.mkdir(parents=True, exist_ok=True)
with tempfile.TemporaryDirectory() as tmp:
    for name, fill, rot, d in STICKERS:
        src = pathlib.Path(tmp, name + ".svg")
        src.write_text(svg(fill, rot, d))
        subprocess.run(["magick", "-background", "none", str(src), "-strip", "PNG32:" + str(out / f"{name}.png")], check=True)
