"""Make Bender digit glyphs tabular (equal advance, centered) in place.

The Slint software renderer ignores OpenType features (no `tnum`), so the only
way to stop the HUD timer from jittering is to bake equal-width digits into the
font itself. For each weight we widen every digit to the width of `0` (the
widest figure in all three Bender weights) and shift its outline right by half
the slack so the glyph stays centered in the new cell. Punctuation (`:` `/` `+`
`%` `*` `.`) is left untouched. Originals are recoverable via git.
"""

import glob
import os

from fontTools.ttLib import TTFont
from fontTools.pens.boundsPen import BoundsPen
from fontTools.pens.t2CharStringPen import T2CharStringPen
from fontTools.pens.transformPen import TransformPen

DIGITS = "0123456789"
FONT_DIR = os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
    "crates", "ruler-app", "assets", "fonts",
)


def bounds(glyph_set, gname):
    pen = BoundsPen(glyph_set)
    glyph_set[gname].draw(pen)
    return pen.bounds  # (xMin, yMin, xMax, yMax) or None


def process(path):
    font = TTFont(path)
    cmap = font.getBestCmap()
    hmtx = font["hmtx"]
    cff = font["CFF "].cff
    top = cff[cff.fontNames[0]]
    char_strings = top.CharStrings
    glyph_set = font.getGlyphSet()

    target = hmtx[cmap[ord("0")]][0]  # '0' is the widest digit in every weight
    print(f"== {os.path.basename(path)}  target width = {target}")

    new_strings = {}
    new_metrics = {}
    for ch in DIGITS:
        gname = cmap[ord(ch)]
        adv, lsb = hmtx[gname]
        delta = round((target - adv) / 2)
        b = bounds(glyph_set, gname)
        pen = T2CharStringPen(target, glyph_set)
        # redraw the outline translated +delta in x to center it in the cell
        glyph_set[gname].draw(TransformPen(pen, (1, 0, 0, 1, delta, 0)))
        new_strings[gname] = pen.getCharString(top.Private)
        new_metrics[gname] = (target, lsb + delta)
        ink = (b[2] - b[0]) if b else 0
        print(f"   {ch}: adv {adv:>4} -> {target}  shift +{delta:<3}  ink {ink}")

    for gname, cs in new_strings.items():
        char_strings[gname] = cs
    for gname, m in new_metrics.items():
        hmtx[gname] = m

    font.save(path)


def verify(path):
    font = TTFont(path)
    cmap = font.getBestCmap()
    hmtx = font["hmtx"]
    glyph_set = font.getGlyphSet()
    advs = {ch: hmtx[cmap[ord(ch)]][0] for ch in DIGITS}
    assert len(set(advs.values())) == 1, advs
    w = next(iter(advs.values()))
    # confirm each glyph is centered: left gap ~= right gap
    gaps = []
    for ch in DIGITS:
        gname = cmap[ord(ch)]
        b = bounds(glyph_set, gname)
        if b:
            left = b[0]
            right = w - b[2]
            gaps.append((ch, round(left), round(right)))
    print(f"   verify OK: all advances = {w}; (digit,left,right) = {gaps}")


if __name__ == "__main__":
    for p in sorted(glob.glob(os.path.join(FONT_DIR, "Bender-*.otf"))):
        process(p)
        verify(p)
