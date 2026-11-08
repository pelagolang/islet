#!/usr/bin/env python3
"""Turn the brand text into outlined paths and write `wordmark.json`.

The logo SVGs must not depend on an installed font, so `build.py` reads glyph outlines from
`wordmark.json` instead of from a font. Run this script only when the wordmark, a file-icon label or the
descriptor changes. It needs `pip install fonttools uharfbuzz` and Outfit (SIL Open Font License; a copy is
in `../fonts/`):

    python brand/build/outline_fonts.py

Spacing comes from HarfBuzz, so kerning is whatever the font defines. The wordmark is lowercase "islet" in
Outfit Bold (700).
"""
import json
from pathlib import Path

import uharfbuzz as hb
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen
from fontTools.ttLib import TTFont
from fontTools.varLib import instancer

HERE = Path(__file__).resolve().parent
OUTFIT = HERE.parent / "fonts" / "Outfit-VariableFont_wght.ttf"

DESCRIPTOR = "The package manager for Pelago."
RUNS = {  # key: (weight, text)
    "islet": (700, "islet"),
    "descriptor": (400, DESCRIPTOR),
}


def outline(weight, text):
    font = instancer.instantiateVariableFont(TTFont(OUTFIT), {"wght": weight})
    tmp = HERE / f".outfit-{weight}.ttf"
    font.save(tmp)
    try:
        face = hb.Face(tmp.read_bytes())
    finally:
        tmp.unlink()
    hbfont = hb.Font(face)
    buf = hb.Buffer()
    buf.add_str(text)
    buf.guess_segment_properties()
    hb.shape(hbfont, buf, {"kern": True, "liga": False})
    glyphs = font.getGlyphSet()
    order = font.getGlyphOrder()
    x = 0
    parts = []
    for info, pos in zip(buf.glyph_infos, buf.glyph_positions):
        name = order[info.codepoint]
        pen = SVGPathPen(glyphs, ntos=lambda v: ("%.1f" % v).rstrip("0").rstrip("."))
        glyphs[name].draw(TransformPen(pen, (1, 0, 0, -1, x + pos.x_offset, -pos.y_offset)))
        cmd = pen.getCommands()
        if cmd:
            parts.append(cmd)
        x += pos.x_advance
    return {"d": "".join(parts), "adv": x, "upm": font["head"].unitsPerEm, "text": text}


def main():
    data = {key: outline(w, t) for key, (w, t) in RUNS.items()}
    (HERE / "wordmark.json").write_text(json.dumps(data, separators=(",", ":")) + "\n")
    print("wordmark.json written:", {k: round(v["adv"]) for k, v in data.items()})


if __name__ == "__main__":
    main()
