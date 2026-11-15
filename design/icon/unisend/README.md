# Unisend icon candidates

## Shipped: B, refined

The owner chose B. Its master is now `packaging/icon/unisend.py`; `packaging/icon/render.sh` writes
every shipped file from it, and `final.py` here draws `final.png` from those files. Changes from the
candidate below, same idea and colours:

- Drawn at 0.84 instead of 0.86 and nudged 1 unit right and 0.5 up, which also balances the tail's
  weight: mark radius 30.8 of 33, about 2 units clear of the safe circle under any launcher mask.
- Gap 3.5 units instead of 3, so it stays open on a 96 px launcher icon.
- `icon-small.svg` for desktop sizes 16 to 48 px and the mdpi legacy icon: the U redrawn on a 16 px
  grid, arms 3 px, a 1 px gap on a whole pixel column (2 px at 32, 3 px at 48), and a one-point tail,
  so the two halves stay apart where the fine gap would blur shut.
- The Android adaptive layers and the splash are vector drawables instead of bitmaps.

`candidate-*/` and `make.py` below are the record of the choice and still draw the original B.

Three candidates for the Unisend app icon. `make.py` writes
every file below; run `python3 design/icon/unisend/make.py` (needs ImageMagick 7 with its librsvg
delegate, which this machine has as RSVG 2.63.2, plus Pillow and numpy). It fails if a mark leaves
the 66-unit safe circle.

Each `candidate-*/` holds the same file roles as the former `packaging/icon/murmuration.py`, all on the
108-unit Android adaptive canvas: `foreground.svg`, `background.svg` (flat `#8006da`),
`monochrome.svg` (themed icon, Android 13+), `icon.svg` (desktop tile, rounded square, mark drawn
1.4 times larger) and `splash.svg`. `preview.png` shows the layer with its 72 viewport and 66 safe
circle, circle, squircle and rounded square masks at 192/96/48/24 px, themed light and dark, a home
screen on dark and light wallpaper beside neutral placeholder icons, and the desktop tile from 128
down to 16 px (16 px also blown up 8 times). `sheet.png` puts the three previews side by side.

## A: One stroke, sent

A single round-capped stroke draws the U: down the left arm, round the bottom, up the right arm,
which leaves the letter as an arrow pointing up, the send gesture of every modern composer. The
arrowhead is the only lime in the icon, so the eye lands on the moment of sending. It is one thick
line (11 units, about 7 px on a 48 px launcher icon) with one turn and one point, so it survives
every mask and still reads as "U, going up" at 24 px and on the 16 px tray tile. The monochrome
layer is the same stroke in one colour. Mark radius 28.6 of 33.

## B: Two halves, one U

The U is made of two speech-bubble halves meeting at the bottom: the left half, in lilac, ends in a
bubble tail at its outer corner; the right half, in white, is round. A thin slit between them keeps
them two pieces that close into one letter, "uni" plus "send". At 192 and 96 px the tail and the
slit read clearly. Below 48 px the slit and the tail fade and what remains is a two-tone U, still
recognisable but no longer telling the story. It also sits closest to the safe-circle edge
(radius 32.7 of 33) because the tail corner lies on the diagonal.

## C: Bracket U, one message

The Matrix bracket turned a quarter turn becomes a square U, and it holds one lime dot: a single
message in the protocol's own bracket. Flat ends and mitred corners make it the most geometric of
the three. It is the strongest at the smallest sizes (the 16 px tile is a clean white U with a lime
pixel cluster) because every edge is horizontal or vertical and lands on the pixel grid. The risk is
the reading: some will see a box or a bin holding a ball, and the dot can look like an unread badge.

## Recommendation

A. It says the name in one gesture (a U that sends), it is the only one that keeps its whole meaning
at launcher size, uses the lime accent exactly once, and leaves the most margin inside every mask.
C is the alternative if the owner wants the icon to nod to Matrix rather than to sending.
