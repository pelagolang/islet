#!/usr/bin/env python3
"""Build every Islet brand asset from the drawings in this file.

    python brand/build/build.py             # SVGs, sprite, PNGs, ICO, ICNS, social images, spec sheets
    python brand/build/build.py --svg-only  # only the SVG files and the sprite (no browser needed)

Needs Python 3.8+ and, for the PNG files, a Chromium-based browser (Chrome, Chromium or Edge). It finds the
browser itself; set ISLET_CHROME to its path to override. Nothing else is required: the PNG crop, the ICO
and the ICNS are written by this script, and the wordmark outlines come from `wordmark.json` (made by
`outline_fonts.py`, which is only needed when the wordmark, a file-icon label or the descriptor changes).

The SVG files are the source of truth. Everything else is an export of them. Running the script rewrites the
files under `brand/` in place. The SVGs come out byte-identical; the PNGs can differ in a few bytes between
browser versions, so judge them by eye, not by `git diff`.
"""
import argparse
import base64
import glob
import json
import math
import os
import shutil
import struct
import subprocess
import sys
import tempfile
import zlib
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

HERE = Path(__file__).resolve().parent
BRAND = HERE.parent

# Palette (docs/brand.md). Light is for light backgrounds, dark for dark ones.
LIGHT = dict(shell="#0F9AA6", gold="#FFC83D", sun="#FF6F3C", green="#12A150", ink="#0E3440", paper="#FFFFFF", sand="#FFF1C9")
DARK = dict(shell="#4FD6D0", gold="#FFD15C", sun="#FF8556", green="#3DD68C", ink="#FFF1C9", paper="#08262D", sand="#FFF1C9")
DEEP = "#08262D"   # dark tile and dark page
SAND = "#FFF1C9"
WHITE = "#FFFFFF"
MUTED_LIGHT = "#4F7480"
MUTED_DARK = "#9CC4C4"

WORD = json.load(open(HERE / "wordmark.json"))
SVG_OPEN = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="%s">'


# ---------------------------------------------------------------- the mark

K = 0.866  # sqrt(3) / 2: the width of an isometric cube of edge 1, half-width


def mix(h, t, to_white):
    h = h.lstrip("#")
    r, g, b = (int(h[i:i + 2], 16) for i in (0, 2, 4))
    tr, tg, tb = (255, 255, 255) if to_white else (0, 0, 0)
    f = lambda a, b2: int(a + (b2 - a) * t)
    return "#%02X%02X%02X" % (f(r, tr), f(g, tg), f(b, tb))


def tone(c):
    """Top, left and right face of a cube: lightened 25 %, as is, darkened 22 %."""
    return (mix(c, 0.25, True), c, mix(c, 0.22, False))


def poly(pts, fill):
    op = ""
    if isinstance(fill, tuple):
        fill, o = fill
        op = f' fill-opacity="{o}"'
    return f'<polygon points="{" ".join(f"{x:.1f},{y:.1f}" for x, y in pts)}" fill="{fill}"{op}/>'


def cube(cx, cy, r, top, left, right):
    T = (cx, cy - r); RT = (cx + K * r, cy - r / 2); C = (cx, cy); LT = (cx - K * r, cy - r / 2)
    RB = (cx + K * r, cy + r / 2); B = (cx, cy + r); LB = (cx - K * r, cy + r / 2)
    return poly([T, RT, C, LT], top) + poly([LT, C, B, LB], left) + poly([C, RT, RB, B], right)


def rpath(pts, rho):
    """A closed polygon with every corner rounded by a quadratic curve that starts rho from the corner."""
    n = len(pts)
    d = ""
    for i in range(n):
        p0, p1, p2 = pts[i - 1], pts[i], pts[(i + 1) % n]

        def toward(a, b, dist):
            length = math.hypot(b[0] - a[0], b[1] - a[1])
            return (a[0] + (b[0] - a[0]) * dist / length, a[1] + (b[1] - a[1]) * dist / length)

        a = toward(p1, p0, rho)
        b = toward(p1, p2, rho)
        d += ("M" if i == 0 else "L") + f"{a[0]:.1f} {a[1]:.1f}Q{p1[0]:.1f} {p1[1]:.1f} {b[0]:.1f} {b[1]:.1f}"
    return d + "Z"


def hex_shell(cx, cy, r, colour, width, rho):
    pts = [(cx + r * math.cos(math.radians(-90 + 60 * k)), cy + r * math.sin(math.radians(-90 + 60 * k))) for k in range(6)]
    return f'<path d="{rpath(pts, rho)}" fill="none" stroke="{colour}" stroke-width="{width}" stroke-linejoin="round"/>'


# Geometry of the locked mark (128 x 128 units). See brand/README.md and spec/mark-construction.svg.
SHELL_R, SHELL_W, SHELL_RHO = 52, 11, 14
CUBE_R, TRIO_RHO, TRIO_CY = 16, 7, 66
CUBES = (  # centres: top, bottom left, bottom right
    (64, TRIO_CY - 1.5 * CUBE_R * 0.5),
    (64 - K * CUBE_R, TRIO_CY - 1.5 * CUBE_R * 0.5 + 1.5 * CUBE_R),
    (64 + K * CUBE_R, TRIO_CY - 1.5 * CUBE_R * 0.5 + 1.5 * CUBE_R),
)

_uid = [0]


def trio(tones):
    """The three cubes, clipped to the outline of their union so only the outer outline is rounded."""
    _uid[0] += 1
    cid = f"islet-trio-{_uid[0]}"
    r = CUBE_R
    (c0, cbl, cbr) = CUBES

    def v(c, i):
        a = math.radians(-90 + 60 * i)
        return (c[0] + r * math.cos(a), c[1] + r * math.sin(a))

    outline = [v(c0, 0), v(c0, 1), v(cbr, 0), v(cbr, 1), v(cbr, 2), v(cbr, 3), v(cbr, 4), v(cbl, 3), v(cbl, 4), v(cbl, 5), v(cbl, 0), v(c0, 5)]
    return (
        f'<clipPath id="{cid}"><path d="{rpath(outline, TRIO_RHO)}"/></clipPath><g clip-path="url(#{cid})">'
        + cube(*c0, r, *tones[0]) + cube(*cbl, r, *tones[1]) + cube(*cbr, r, *tones[2]) + "</g>"
    )


def mark_body(pal):
    """The mark. `pal` is LIGHT or DARK, or a one-colour string (black or white) for the mono marks."""
    if isinstance(pal, str):
        faces = ((pal, 0.55), (pal, 0.8), (pal, 1))
        return hex_shell(64, 64, SHELL_R, pal, SHELL_W, SHELL_RHO) + trio((faces, faces, faces))
    return hex_shell(64, 64, SHELL_R, pal["shell"], SHELL_W, SHELL_RHO) + trio((tone(pal["gold"]), tone(pal["sun"]), tone(pal["green"])))


def text_path(key, fg, x, baseline, size):
    """An outlined text run from wordmark.json, `size` units high (the font size)."""
    w = WORD[key]
    s = size / w["upm"]
    return (f'<path transform="translate({x:.2f} {baseline:.2f}) scale({s:.5f})" fill="{fg}" d="{w["d"]}"/>', w["adv"] * s)


NAME_SIZE = 90
NAME_X = 134
NAME_BASE = 97

VARIANTS = {  # file suffix: (palette or mono colour, name colour)
    "": (LIGHT, LIGHT["ink"]),
    "-dark": (DARK, DARK["ink"]),
    "-black": ("#000000", "#000000"),
    "-white": (WHITE, WHITE),
}


def lockups():
    out = {}
    for suf, (pal, name_fg) in VARIANTS.items():
        out[f"logo/islet-mark{suf}.svg"] = SVG_OPEN % "0 0 128 128" + mark_body(pal) + "</svg>"
        name, adv = text_path("islet", name_fg, NAME_X, NAME_BASE, NAME_SIZE)
        out[f"logo/islet-horizontal{suf}.svg"] = SVG_OPEN % f"0 0 {NAME_X + adv + 6:.0f} 128" + mark_body(pal) + name + "</svg>"
        size = 60
        sw = WORD["islet"]["adv"] * size / WORD["islet"]["upm"]
        width = max(sw, 128) + 24
        name, _ = text_path("islet", name_fg, (width - sw) / 2, 186, size)
        out[f"logo/islet-stacked{suf}.svg"] = (
            SVG_OPEN % f"0 0 {width:.0f} 214"
            + f'<g transform="translate({(width - 128) / 2:.1f} 0)">{mark_body(pal)}</g>' + name + "</svg>"
        )
    return out


# ---------------------------------------------------------------- app icons

def badge(pal, bg, radius=28, scale=0.84, flat=False):
    o = (128 - 128 * scale) / 2
    rx = "" if flat else f' rx="{radius}"'
    return f'<rect width="128" height="128"{rx} fill="{bg}"/><g transform="translate({o:.1f} {o:.1f}) scale({scale})">{mark_body(pal)}</g>'


def icons():
    box = "0 0 128 128"
    clip = '<clipPath id="islet-avatar-clip"><circle cx="64" cy="64" r="64"/></clipPath><g clip-path="url(#islet-avatar-clip)">'
    return {
        "icon/islet-icon.svg": SVG_OPEN % box + badge(DARK, DEEP) + "</svg>",
        "icon/islet-icon-light.svg": SVG_OPEN % box + badge(LIGHT, SAND) + "</svg>",
        "icon/favicon.svg": SVG_OPEN % box + badge(DARK, DEEP, scale=0.92) + "</svg>",
        "icon/apple-touch-icon.svg": SVG_OPEN % box + badge(DARK, DEEP, flat=True, scale=0.74) + "</svg>",
        "icon/maskable.svg": SVG_OPEN % box + badge(DARK, DEEP, flat=True, scale=0.6) + "</svg>",
        "icon/avatar.svg": SVG_OPEN % box + clip + badge(DARK, DEEP, flat=True, scale=0.7) + "</g></svg>",
        "icon/avatar-light.svg": SVG_OPEN % box + clip + badge(LIGHT, SAND, flat=True, scale=0.7) + "</g></svg>",
    }


# ---------------------------------------------------------------- file icons

FILES = (  # file name, what it holds
    ("islet-toml", "one cube: a package"),
    ("islet-lock", "a cube with a shackle: locked"),
    ("monorepo-toml", "three cubes: many packages"),
)


def file_icon(name, dark):
    """A rounded hexagon tile (the shell) with the cubes inside. No page outline: the shape is Islet's own."""
    pal = DARK  # the cubes always use the bright palette; the tile changes with the theme
    fill = "#0F2F37" if dark else DEEP
    tile = (
        f'<path d="{rpath([(64 + 54 * math.cos(math.radians(-90 + 60 * k)), 64 + 54 * math.sin(math.radians(-90 + 60 * k))) for k in range(6)], 14)}" '
        f'fill="{fill}" stroke="{pal["shell"]}" stroke-width="9" stroke-linejoin="round"/>'
    )
    if name == "islet-toml":
        glyph = cube(64, 66, 27, *tone(pal["gold"]))
    elif name == "islet-lock":
        shackle = (f'<path d="M55 62V44a9 9 0 0 1 18 0V62" fill="none" stroke="{pal["ink"]}" stroke-width="7" '
                   'stroke-linecap="round" stroke-linejoin="round"/>')
        glyph = shackle + cube(64, 78, 22, *tone(pal["sun"]))
    else:
        glyph = f'<g transform="translate(64 66) scale(1.3) translate(-64 -66)">{trio((tone(pal["gold"]), tone(pal["sun"]), tone(pal["green"])))}</g>'
    return tile + glyph


def file_icons():
    out = {}
    for dark, suf in ((False, ""), (True, "-dark")):
        for name, _ in FILES:
            out[f"editor/file-icons/{name}{suf}.svg"] = SVG_OPEN % "0 0 128 128" + file_icon(name, dark) + "</svg>"
    return out


# ---------------------------------------------------------------- social images

def social_svg(width, height, dark, descriptor):
    pal = DARK if dark else LIGHT
    bg = DEEP if dark else WHITE
    m = height * 0.38  # lockup height
    name, adv = text_path("islet", pal["ink"], NAME_X, NAME_BASE, NAME_SIZE)
    vw = NAME_X + adv + 6
    lock_w = vw * m / 128
    tag_size = height * 0.07
    gap = height * 0.06
    block = m + (gap + tag_size if descriptor else 0)
    top = (height - block) / 2
    ring = height * 1.25
    ghost = (
        f'<svg x="{width * 0.80:.1f}" y="{height * 0.5 - ring / 2:.1f}" width="{ring:.1f}" height="{ring:.1f}" viewBox="0 0 128 128" opacity="{0.10 if dark else 0.08}">'
        + hex_shell(64, 64, SHELL_R, pal["shell"], SHELL_W, SHELL_RHO) + "</svg>"
    )
    body = f'<svg x="{(width - lock_w) / 2:.1f}" y="{top:.1f}" width="{lock_w:.1f}" height="{m:.1f}" viewBox="0 0 {vw:.0f} 128">{mark_body(pal)}{name}</svg>'
    if descriptor:
        tag, tw = text_path("descriptor", MUTED_DARK if dark else MUTED_LIGHT, 0, 0, tag_size)
        tag = tag.replace("translate(0.00 0.00)", f"translate({(width - tw) / 2:.2f} {top + m + gap + tag_size * 0.74:.2f})")
        body += tag
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width} {height}">'
        f'<rect width="{width}" height="{height}" fill="{bg}"/>' + ghost + body + "</svg>"
    )


SOCIAL = (  # name, width, height, dark, has the descriptor line
    ("github-social-1280x640", 1280, 640, True, True),
    ("og-1200x630", 1200, 630, False, True),
    ("x-header-1500x500", 1500, 500, True, True),
    ("linkedin-1584x396", 1584, 396, False, True),
    ("readme-banner-1280x320", 1280, 320, True, False),
)


def social():
    return {f"social/{n}.svg": social_svg(w, h, d, t) for n, w, h, d, t in SOCIAL}


def sprite(svgs):
    """One SVG with a <symbol> per logo, icon and file icon: `<use href="sprite.svg#islet-icon"/>`."""
    order = [k for k in svgs if k.startswith(("logo/", "icon/islet-icon", "editor/file-icons/"))]
    symbols = []
    for key in order:
        src = svgs[key]
        vb = src.split('viewBox="')[1].split('"')[0]
        inner = src.split(">", 1)[1].rsplit("</svg>", 1)[0]
        symbols.append(f'<symbol id="{Path(key).stem}" viewBox="{vb}">{inner}</symbol>')
    return '<svg xmlns="http://www.w3.org/2000/svg" style="display:none">' + "".join(symbols) + "</svg>"


# ---------------------------------------------------------------- PNG, ICO, ICNS

def find_chrome():
    env = os.environ.get("ISLET_CHROME")
    if env:
        return env
    for name in ("google-chrome", "chromium", "chromium-browser", "chrome", "msedge"):
        found = shutil.which(name)
        if found:
            return found
    candidates = [
        r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
    ] + glob.glob("/opt/pw-browsers/chromium-*/chrome-linux/chrome")
    for c in candidates:
        if os.path.exists(c):
            return c
    sys.exit("No Chrome, Chromium or Edge found. Set ISLET_CHROME to the browser executable, or use --svg-only.")


def read_png(path):
    data = Path(path).read_bytes()
    pos, idat, header = 8, b"", None
    while pos < len(data):
        n, kind = struct.unpack(">I4s", data[pos:pos + 8])
        chunk = data[pos + 8:pos + 8 + n]
        if kind == b"IHDR":
            header = struct.unpack(">IIBBBBB", chunk)
        elif kind == b"IDAT":
            idat += chunk
        pos += 12 + n
    w, h, depth, ctype, _, _, interlace = header
    assert depth == 8 and ctype in (2, 6) and interlace == 0, "unsupported PNG from the browser"
    bpp = 4 if ctype == 6 else 3
    raw = zlib.decompress(idat)
    stride = w * bpp
    rows, prev = [], bytearray(stride)
    for y in range(h):
        ft = raw[y * (stride + 1)]
        row = bytearray(raw[y * (stride + 1) + 1:(y + 1) * (stride + 1)])
        for i in range(stride):
            a = row[i - bpp] if i >= bpp else 0
            b = prev[i]
            c = prev[i - bpp] if i >= bpp else 0
            if ft == 1:
                row[i] = (row[i] + a) & 255
            elif ft == 2:
                row[i] = (row[i] + b) & 255
            elif ft == 3:
                row[i] = (row[i] + (a + b) // 2) & 255
            elif ft == 4:
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                pr = a if pa <= pb and pa <= pc else (b if pb <= pc else c)
                row[i] = (row[i] + pr) & 255
        rows.append(row)
        prev = row
    return w, h, bpp, rows


def write_png(path, w, h, bpp, rows):
    def chunk(kind, payload):
        return struct.pack(">I", len(payload)) + kind + payload + struct.pack(">I", zlib.crc32(kind + payload))

    raw = b"".join(b"\x00" + bytes(r) for r in rows)
    ihdr = struct.pack(">IIBBBBB", w, h, 8, 6 if bpp == 4 else 2, 0, 0, 0)
    Path(path).write_bytes(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))


def crop_png(path, width, height):
    w, h, bpp, rows = read_png(path)
    write_png(path, width, height, bpp, [r[: width * bpp] for r in rows[:height]])


def svg_size(path):
    vb = Path(path).read_text().split('viewBox="')[1].split('"')[0].split()
    return float(vb[2]), float(vb[3])


def export_png(chrome, svg, out, width, transparent=True):
    vw, vh = svg_size(svg)
    height = round(width * vh / vw)
    with tempfile.TemporaryDirectory() as tmp:
        page = Path(tmp) / "page.html"
        page.write_text(
            f'<html><body style="margin:0"><img src="{Path(svg).resolve().as_uri()}" width="{width}" height="{height}" style="display:block"></body></html>'
        )
        cmd = [
            chrome, "--headless", "--no-sandbox", "--disable-gpu", "--hide-scrollbars",
            f"--user-data-dir={Path(tmp) / 'profile'}",
            "--default-background-color=" + ("00000000" if transparent else "ffffffff"),
            f"--screenshot={out}", f"--window-size={width},{height + 150}", page.resolve().as_uri(),
        ]
        for attempt in range(3):  # headless Chrome occasionally dies on a busy machine; try again
            r = subprocess.run(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
            if r.returncode == 0 and os.path.exists(out):
                break
            if attempt == 2:
                sys.exit(f"{chrome} failed (exit {r.returncode}) on {svg}:\n{r.stderr[-1500:]}")
    crop_png(out, width, height)


def write_ico(path, pngs):
    """A .ico that stores one PNG per size (supported since Windows Vista)."""
    entries, blobs, offset = b"", b"", 6 + 16 * len(pngs)
    for size, png in pngs:
        entries += struct.pack("<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32, len(png), offset)
        blobs += png
        offset += len(png)
    Path(path).write_bytes(struct.pack("<HHH", 0, 1, len(pngs)) + entries + blobs)


ICNS_TYPES = (  # PNG-compressed icon types: (type, pixel size)
    ("icp4", 16), ("icp5", 32), ("icp6", 64), ("ic07", 128), ("ic13", 256), ("ic08", 256),
    ("ic14", 512), ("ic09", 512), ("ic10", 1024), ("ic11", 32), ("ic12", 64),
)


def write_icns(path, png_for_size):
    parts = b""
    for kind, size in ICNS_TYPES:
        png = png_for_size[size]
        parts += kind.encode() + struct.pack(">I", len(png) + 8) + png
    Path(path).write_bytes(b"icns" + struct.pack(">I", len(parts) + 8) + parts)


# ---------------------------------------------------------------- design specs

INK = "#0E3440"
GUIDE = "#e0457b"   # construction lines
GRID = "#c9d6df"
FONT = "Outfit, sans-serif"


def label(x, y, text, size=13, anchor="start", fill=INK, weight=400):
    return (
        f'<text x="{x:.1f}" y="{y:.1f}" font-family="{FONT}" font-size="{size}" font-weight="{weight}" '
        f'text-anchor="{anchor}" fill="{fill}">{text}</text>'
    )


def line(x1, y1, x2, y2, colour=GUIDE, w=1.5, dash=None):
    d = f' stroke-dasharray="{dash}"' if dash else ""
    return f'<line x1="{x1:.1f}" y1="{y1:.1f}" x2="{x2:.1f}" y2="{y2:.1f}" stroke="{colour}" stroke-width="{w}"{d}/>'


def dim(x1, y1, x2, y2, text, side=1, size=13):
    """A dimension line with end ticks and a label (pixel coordinates). side picks the label side."""
    horizontal = abs(y1 - y2) < 0.01
    out = line(x1, y1, x2, y2, GUIDE, 1.5)
    if horizontal:
        out += line(x1, y1 - 5, x1, y1 + 5) + line(x2, y2 - 5, x2, y2 + 5)
        out += label((x1 + x2) / 2, y1 + side * 18, text, size, "middle", GUIDE, 600)
    else:
        out += line(x1 - 5, y1, x1 + 5, y1) + line(x2 - 5, y2, x2 + 5, y2)
        out += label(x1 + side * 10, (y1 + y2) / 2 + 5, text, size, "start" if side > 0 else "end", GUIDE, 600)
    return out


def grid(ox, oy, S, n=128, step=8):
    out = ""
    for i in range(0, n + 1, step):
        major = i % 32 == 0
        c, w = ("#9fb3c1", 1.2) if major else (GRID, 0.8)
        out += line(ox + i * S, oy, ox + i * S, oy + n * S, c, w) + line(ox, oy + i * S, ox + n * S, oy + i * S, c, w)
    return out


def font_face():
    """Outfit, embedded so the spec sheets look the same on every machine (SIL OFL, brand/fonts)."""
    data = base64.b64encode((BRAND / "fonts/Outfit-VariableFont_wght.ttf").read_bytes()).decode()
    return (
        "<style>@font-face{font-family:Outfit;font-weight:100 900;"
        f"src:url(data:font/ttf;base64,{data}) format('truetype')}}</style>"
    )


def spec_head(width, title, sub, height=900):
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width} {height}">'
        + font_face() +
        f'<rect width="{width}" height="{height}" fill="#ffffff"/>'
        + label(48, 62, title, 30, weight=700)
        + label(48, 90, sub, 15, fill="#58708a")
    )


def spec_head(width, title, sub, height=900):
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width} {height}">'
        + font_face() +
        f'<rect width="{width}" height="{height}" fill="#ffffff"/>'
        + label(48, 62, title, 30, weight=700)
        + label(48, 90, sub, 15, fill=MUTED_LIGHT)
    )


def wrap(text, right, y, size=14, limit=40):
    out, line_, ly = "", "", y
    for w in text.split():
        if len(line_ + w) > limit:
            out += label(right, ly, line_, size, fill=INK)
            line_, ly = "", ly + size + 6
        line_ += w + " "
    return out + label(right, ly, line_, size, fill=INK), ly


def spec_construction():
    S, ox, oy = 5.6, 70, 150
    px = lambda u: ox + u * S
    py = lambda u: oy + u * S
    body = grid(ox, oy, S)
    body += f'<g transform="translate({ox} {oy}) scale({S})" opacity="0.85">{mark_body(LIGHT)}</g>'
    circ = lambda x, y, r, d="2 4", w=1.2: f'<circle cx="{px(x):.1f}" cy="{py(y):.1f}" r="{r * S:.1f}" fill="none" stroke="{GUIDE}" stroke-width="{w}" stroke-dasharray="{d}"/>'
    # the shell: centre-line circle (r 52) and the two edges (46.5 and 57.5)
    body += circ(64, 64, SHELL_R, "6 4", 1.4) + circ(64, 64, SHELL_R - SHELL_W / 2) + circ(64, 64, SHELL_R + SHELL_W / 2)
    # cube circles and centres
    for (cx, cy) in CUBES:
        body += circ(cx, cy, CUBE_R, "3 3") + f'<circle cx="{px(cx):.1f}" cy="{py(cy):.1f}" r="3" fill="{GUIDE}"/>'
    body += line(px(64), py(6), px(64), py(122), GUIDE, 1, "3 5") + line(px(6), py(64), px(122), py(64), GUIDE, 1, "3 5")
    body += line(px(10), py(TRIO_CY), px(118), py(TRIO_CY), GUIDE, 1, "3 5")
    body += dim(px(64), py(-3), px(116), py(-3), "r 52", -1, 12)
    body += dim(px(6.5), py(133), px(18), py(133), "11", 1, 12)
    body += label(px(64), py(CUBES[0][1]) - 6, "cube edge 16", 12, "middle", GUIDE, 700)
    right = 70 + 128 * S + 100
    notes = [
        ("Unit", "A 128 x 128 grid. The shell stroke is 11 units; everything else is derived from the cube edge, 16."),
        ("Shell", "A hexagon with a point on top: radius 52 to the centre of the stroke (46.5 to 57.5), stroke 11, corners rounded with a radius of 14, round joins."),
        ("Cubes", "Three isometric cubes of edge 16 that touch edge to edge. The trio is centred on x = 64 and y = 66, two units below the shell centre; it is 55.4 wide and 78 tall before the outline is rounded."),
        ("Faces", "Top face lightened 25 %, left face the colour itself, right face darkened 22 %. Gold on top, sunburn bottom left, green bottom right."),
        ("Rounding", "Only the outer outline of the trio is rounded (radius 7). The three cubes are clipped to that outline, so the edges where cubes meet stay crisp."),
        ("Colour", "Shell Teal #0F9AA6 on light, #4FD6D0 on dark. Gold, Sunburn and Green below. One-colour marks use the shell colour and three face opacities."),
    ]
    y = 190
    for head, text in notes:
        body += label(right, y, head, 15, weight=700, fill=LIGHT["shell"])
        t, ly = wrap(text, right, y + 22, 14, 44)
        body += t
        y = ly + 40
    return spec_head(right + 420, "Mark construction", "Grid of 128 units. Pink lines are construction lines; the mark is drawn over them.", max(int(y) + 40, 940)) + body + "</svg>"


def spec_icon():
    S, ox, oy = 5.6, 70, 150
    px = lambda u: ox + u * S
    py = lambda u: oy + u * S
    body = f'<g transform="translate({ox} {oy}) scale({S})">{badge(DARK, DEEP)}</g>'
    body += grid(ox, oy, S, step=16)
    body += f'<circle cx="{px(28)}" cy="{py(28)}" r="{28 * S}" fill="none" stroke="{GUIDE}" stroke-width="1.5" stroke-dasharray="4 4"/>'
    body += line(px(28), py(0), px(28), py(28), GUIDE, 1.5) + line(px(0), py(28), px(28), py(28), GUIDE, 1.5)
    body += label(px(28) + 8, py(14) + 5, "28", 14, "start", GUIDE, 700)
    o = 128 * 0.08
    body += f'<rect x="{px(o)}" y="{py(o)}" width="{128 * 0.84 * S}" height="{128 * 0.84 * S}" fill="none" stroke="{DARK["shell"]}" stroke-width="1.8" stroke-dasharray="6 4"/>'
    body += dim(px(0), py(135), px(o), py(135), "10.2", 1, 12)
    body += dim(px(o), py(135), px(128 - o), py(135), "mark at 84 % = 107.5", 1, 12)
    body += f'<circle cx="{px(64)}" cy="{py(64)}" r="{64 * S}" fill="none" stroke="{GUIDE}" stroke-width="1" stroke-dasharray="2 5"/>'
    body += line(px(64), py(0), px(64), py(128), GUIDE, 1, "3 5") + line(px(0), py(64), px(128), py(64), GUIDE, 1, "3 5")
    right = 70 + 128 * S + 90
    body += label(right, 190, "App icon", 18, weight=700, fill=LIGHT["shell"])
    for i, t in enumerate(("Rounded square, corner radius 28 of 128 (21.9 %).",
                           "The mark fills 84 % and is centred.",
                           "Dark tile #08262D with the dark-background mark.",
                           "Light: sand tile #FFF1C9 with the light mark.")):
        body += label(right, 218 + i * 22, t, 13)
    variants = (
        ("Apple touch", badge(DARK, DEEP, flat=True, scale=0.74), "square, mark 74 %, the OS rounds it", False),
        ("Maskable", badge(DARK, DEEP, flat=True, scale=0.6), "mark 60 %, inside the 80 % safe circle", True),
        ("Avatar", '<clipPath id="spec-av"><circle cx="64" cy="64" r="64"/></clipPath><g clip-path="url(#spec-av)">' + badge(DARK, DEEP, flat=True, scale=0.7) + "</g>", "circle crop, mark 70 %", False),
    )
    vs = 160
    for i, (name, art, note, safe) in enumerate(variants):
        x, y = right + i * (vs + 60), 400
        body += f'<g transform="translate({x} {y}) scale({vs / 128})">{art}</g>'
        if safe:
            body += f'<circle cx="{x + vs / 2}" cy="{y + vs / 2}" r="{vs * 0.4}" fill="none" stroke="{DARK["shell"]}" stroke-width="1.8" stroke-dasharray="5 4"/>'
        body += label(x, y + vs + 26, name, 14, weight=700)
        for j, part in enumerate(note.split(", ")):
            body += label(x, y + vs + 46 + j * 16, part, 11, fill=MUTED_LIGHT)
    width = right + 3 * (vs + 60)
    return spec_head(width, "App icon construction", "Corner radius, safe area and the variants. Pink: construction. Dashed teal: safe area.", 960) + body + "</svg>"


def spec_lockup():
    k = 3.4
    ox, oy = 150, 300
    name, adv = text_path("islet", INK, NAME_X, NAME_BASE, NAME_SIZE)
    width_u = NAME_X + adv + 6
    x = 28
    px = lambda u: ox + u * k
    py = lambda u: oy + u * k
    body = f'<g transform="translate({ox} {oy}) scale({k})">{mark_body(LIGHT)}{name}</g>'
    body += f'<rect x="{px(0)}" y="{py(0)}" width="{width_u * k}" height="{128 * k}" fill="none" stroke="{GUIDE}" stroke-width="1.2"/>'
    body += f'<rect x="{px(-x)}" y="{py(-x)}" width="{(width_u + 2 * x) * k}" height="{(128 + 2 * x) * k}" fill="none" stroke="{LIGHT["shell"]}" stroke-width="1.8" stroke-dasharray="7 5"/>'
    for (a, b) in ((-x, -x), (width_u, -x), (-x, 128), (width_u, 128)):
        body += f'<rect x="{px(a)}" y="{py(b)}" width="{x * k}" height="{x * k}" fill="{LIGHT["shell"]}" opacity="0.2"/>'
    body += dim(px(-x), py(-x) - 20, px(0), py(-x) - 20, "x", -1)
    body += label(px(width_u / 2), py(-x) - 30, "x = 28 units, the same as the icon corner radius", 13, "middle", GUIDE, 600)
    body += dim(px(122), py(112), px(NAME_X), py(112), "12", 1, 12)
    body += dim(px(width_u) + 48 + x * k, py(0), px(width_u) + 48 + x * k, py(128), "128", 1)
    y = oy + (128 + x) * k + 70
    body += label(70, y - 18, "Smallest sizes", 18, weight=700, fill=LIGHT["shell"])
    sizes = ((16, "mark 16 px"), (24, "mark 24 px"), (32, "icon 32 px"), (64, "icon 64 px"))
    cx = 70
    for s, text in sizes:
        art = mark_body(LIGHT) if "mark" in text else badge(DARK, DEEP)
        body += f'<svg x="{cx}" y="{y}" width="{s}" height="{s}" viewBox="0 0 128 128">{art}</svg>'
        body += label(cx, y + 64 + 24, text, 12, fill=MUTED_LIGHT)
        cx += max(s, 80) + 40
    lw = 140
    body += f'<svg x="{cx + 20}" y="{y}" width="{lw}" height="{lw * 128 / width_u:.1f}" viewBox="0 0 {width_u:.0f} 128">{mark_body(LIGHT)}{name}</svg>'
    body += label(cx + 20, y + 64 + 24, "lockup, 140 px wide is the minimum", 12, fill=MUTED_LIGHT)
    body += label(70, y + 130, "Under 24 px prefer the icon tile to the bare mark: the cube faces blur together below that.", 13)
    return spec_head(int(px(width_u) + x * k + 140), "Lockup, clear space and minimum size", "Keep x clear on every side. Pink: artwork bounds. Dashed teal: clear space.", int(y + 190)) + body + "</svg>"


def spec_cube():
    """How to draw the cube, step by step, then how the trio is put together and rounded."""
    r = 16
    S = 6.5
    pw, ph = 330, 330
    steps = []

    def panel(i, title, text, art, ox, y=150):
        out = label(ox, y, f"{i}. {title}", 17, weight=700, fill=LIGHT["shell"])
        t, _ = wrap(text, ox, y + 24, 13, 38)
        return out + t + art

    def pts_of(cx, cy, rr):
        return {"T": (cx, cy - rr), "RT": (cx + K * rr, cy - rr / 2), "RB": (cx + K * rr, cy + rr / 2), "B": (cx, cy + rr),
                "LB": (cx - K * rr, cy + rr / 2), "LT": (cx - K * rr, cy - rr / 2), "C": (cx, cy)}

    def px(cx, cy, d):  # a unit-space point to pixels around the panel centre
        return (cx + d[0] * S, cy + d[1] * S)

    def hexpts(cx, cy, rr):
        P = pts_of(cx, cy, rr)
        return [P["T"], P["RT"], P["RB"], P["B"], P["LB"], P["LT"]]

    def path(ptlist, fill="none", stroke=GUIDE, w=1.6):
        d = "M" + "L".join(f"{x:.1f} {y:.1f}" for x, y in ptlist) + "Z"
        return f'<path d="{d}" fill="{fill}" stroke="{stroke}" stroke-width="{w}" stroke-linejoin="round"/>'

    def dot(p, name, dx=8, dy=-8, anchor="start"):
        return f'<circle cx="{p[0]:.1f}" cy="{p[1]:.1f}" r="3.5" fill="{GUIDE}"/>' + label(p[0] + dx, p[1] + dy, name, 12, anchor, GUIDE, 700)

    width = 1650
    colx = [48 + i * 320 for i in range(5)]
    cy0 = 470
    body = ""
    # 1: the circle and the hexagon
    cx = colx[0] + 140
    P = pts_of(cx, cy0, r * S)
    body += f'<circle cx="{cx}" cy="{cy0}" r="{r * S}" fill="none" stroke="{GUIDE}" stroke-width="1.2" stroke-dasharray="4 4"/>'
    body += path(hexpts(cx, cy0, r * S)) + dot(P["T"], "T", 10, -8) + dot(P["RT"], "RT", 10, -6) + dot(P["RB"], "RB", 10, 16) + dot(P["B"], "B", 10, 18) + dot(P["LB"], "LB", -10, 16, "end") + dot(P["LT"], "LT", -10, -6, "end")
    body += line(cx, cy0 - r * S - 14, cx, cy0 + r * S + 14, GUIDE, 1, "3 5")
    body += dim(cx + K * r * S + 40, cy0 - r * S, cx + K * r * S + 40, cy0 + r * S, "2r", 1, 12)
    body = panel(1, "A hexagon, point up", "Draw a circle of radius r (the cube edge). Mark six points on it, 60 degrees apart, starting at the top: T, RT, RB, B, LB, LT. Join them. This is the cube's outline.", "", colx[0]) + body
    # 2: spokes
    cx = colx[1] + 140
    P = pts_of(cx, cy0, r * S)
    art = path(hexpts(cx, cy0, r * S), stroke=MUTED_LIGHT, w=1.2)
    for k in ("LT", "RT", "B"):
        art += line(*P["C"], *P[k], GUIDE, 2.2)
    art += dot(P["C"], "C (centre)", 12, 22)
    art += label(cx, cy0 + r * S + 48, "three spokes: C to LT, RT and B", 12, "middle", GUIDE, 600)
    body += panel(2, "Three spokes from the centre", "Put a point C in the middle and draw lines from it to LT, RT and B. The hexagon is now split into three rhombi: top, left and right. Each has two sides of length r.", art, colx[1])
    # 3: faces
    cx = colx[2] + 140
    P = pts_of(cx, cy0, r * S)
    t0, t1, t2 = tone(LIGHT["gold"])
    art = poly([P["T"], P["RT"], P["C"], P["LT"]], t0) + poly([P["LT"], P["C"], P["B"], P["LB"]], t1) + poly([P["C"], P["RT"], P["RB"], P["B"]], t2)
    art += label(cx, cy0 - r * S * 0.55, "top", 13, "middle", INK, 700) + label(cx - 36, cy0 + 34, "left", 13, "middle", INK, 700) + label(cx + 38, cy0 + 34, "right", 13, "middle", INK, 700)
    art += label(cx, cy0 + r * S + 48, "no outlines: faces only", 12, "middle", GUIDE, 600)
    body += panel(3, "Fill the three faces", "Top face: the colour mixed 25 % toward white. Left face: the colour itself. Right face: the colour mixed 22 % toward black. Light comes from the top left, and there are no outlines.", art, colx[2])
    # 4: coordinates table
    ox = colx[3]
    rows = [("T", "0, -r"), ("RT", "0.866 r, -0.5 r"), ("RB", "0.866 r, 0.5 r"), ("B", "0, r"), ("LB", "-0.866 r, 0.5 r"), ("LT", "-0.866 r, -0.5 r"), ("C", "0, 0")]
    art = ""
    for i, (n, v) in enumerate(rows):
        yy = 250 + i * 34
        art += label(ox, yy, n, 14, weight=700, fill=GUIDE) + label(ox + 50, yy, f"({v})", 14, fill=INK)
    art += label(ox, 250 + 7 * 34 + 10, "Origin at C, x to the right, y down.", 12, fill=MUTED_LIGHT)
    art += label(ox, 250 + 7 * 34 + 30, "0.866 is the square root of 3, over 2.", 12, fill=MUTED_LIGHT)
    art += label(ox, 250 + 7 * 34 + 50, "Faces: top T-RT-C-LT, left LT-C-B-LB,", 12, fill=MUTED_LIGHT)
    art += label(ox, 250 + 7 * 34 + 68, "right C-RT-RB-B.", 12, fill=MUTED_LIGHT)
    body += label(ox, 150, "4. The numbers", 17, weight=700, fill=LIGHT["shell"])
    t, _ = wrap("The same drawing as coordinates, for any cube of edge r. In the mark r = 16, so a cube is 27.7 units wide and 32 tall.", ox, 174, 13, 38)
    body += t + art
    # 5: the finished cube at three sizes
    ox = colx[4]
    body += label(ox, 150, "5. Check it", 17, weight=700, fill=LIGHT["shell"])
    t, _ = wrap("The finished cube at the sizes the mark uses. The three rhombi must meet at one point with equal angles, and all six outer edges must be the same length.", ox, 174, 13, 38)
    body += t
    for i, rr in enumerate((46, 28, 16)):
        cxx = ox + 70 + i * 0 + (0 if i == 0 else 0)
    body += f'<g transform="translate({ox + 70} {cy0})">{cube(0, 0, 70, t0, t1, t2)}</g>'
    body += f'<g transform="translate({ox + 70 + 130} {cy0 + 40})">{cube(0, 0, 30, t0, t1, t2)}</g>'
    body += f'<g transform="translate({ox + 70 + 130} {cy0 - 60})">{cube(0, 0, 14, t0, t1, t2)}</g>'
    body += label(ox + 70, cy0 + 100, "r 70", 12, "middle", MUTED_LIGHT) + label(ox + 200, cy0 + 90, "r 30", 12, "middle", MUTED_LIGHT) + label(ox + 200, cy0 - 36, "r 14", 12, "middle", MUTED_LIGHT)
    # second row: the trio
    y2 = 760
    body += label(48, y2 - 60, "Putting three cubes together", 22, weight=700, fill=INK)
    S2 = 3.2
    cx = 48 + 150
    cyy = y2 + 250
    c0 = (cx, cyy - 0.75 * r * S2)
    cbl = (cx - K * r * S2, cyy + 0.75 * r * S2)
    cbr = (cx + K * r * S2, cyy + 0.75 * r * S2)
    art = ""
    for c, tn in ((c0, tone(LIGHT["gold"])), (cbl, tone(LIGHT["sun"])), (cbr, tone(LIGHT["green"]))):
        art += cube(*c, r * S2, *tn)
    for c in (c0, cbl, cbr):
        art += path(hexpts(*c, r * S2), stroke=GUIDE, w=1.2) + f'<circle cx="{c[0]:.1f}" cy="{c[1]:.1f}" r="3.5" fill="{GUIDE}"/>'
    art += dim(cbl[0], cbl[1] + r * S2 + 40, cbr[0], cbr[1] + r * S2 + 40, "2 x 0.866 r", 1, 12)
    art += dim(cbr[0] + K * r * S2 + 40, c0[1], cbr[0] + K * r * S2 + 40, cbr[1], "1.5 r", 1, 12)
    body += panel("6", "Place three cubes edge to edge", "Cube 1 on top, cube 2 bottom left, cube 3 bottom right, all the same r. The bottom two sit 0.866 r either side of the centre line and 1.5 r below the top cube, so each shares an edge with the other two.", art, 48, y2 - 20)
    # outline and rounding
    ox = 48 + 330
    cx2 = ox + 150
    c0b = (cx2, cyy - 0.75 * r * S2); cblb = (cx2 - K * r * S2, cyy + 0.75 * r * S2); cbrb = (cx2 + K * r * S2, cyy + 0.75 * r * S2)

    def vv(c, i):
        a = math.radians(-90 + 60 * i)
        return (c[0] + r * S2 * math.cos(a), c[1] + r * S2 * math.sin(a))

    outline = [vv(c0b, 0), vv(c0b, 1), vv(cbrb, 0), vv(cbrb, 1), vv(cbrb, 2), vv(cbrb, 3), vv(cbrb, 4), vv(cblb, 3), vv(cblb, 4), vv(cblb, 5), vv(cblb, 0), vv(c0b, 5)]
    art2 = f'<path d="{rpath(outline, 0.01)}" fill="none" stroke="{GUIDE}" stroke-width="2"/>'
    for i, pt in enumerate(outline):
        art2 += f'<circle cx="{pt[0]:.1f}" cy="{pt[1]:.1f}" r="3.5" fill="{GUIDE}"/>'
    body += panel("7", "Trace the outline", "Walk round the outside of the group: 12 corners. The notches where two cubes meet are the concave corners; the six tips are the convex ones.", art2, ox, y2 - 20)
    ox += 330
    cx3 = ox + 150
    shift = cx3 - cx2
    pp = [(x + shift, y) for x, y in outline]
    art3 = f'<path d="{rpath(pp, TRIO_RHO * S2)}" fill="{LIGHT["shell"]}" fill-opacity=".18" stroke="{GUIDE}" stroke-width="2"/>'
    for (x, y), (x2, y2_) in zip(pp, pp[1:] + pp[:1]):
        pass
    art3 += label(cx3, cyy + r * S2 * 2 + 30, f"corner radius {TRIO_RHO} units", 12, "middle", GUIDE, 600)
    body += panel("8", "Round the outer corners", "Replace each of the 12 corners with a curve that starts 7 units before the corner and ends 7 units after it, with the corner as the control point (a quadratic curve).", art3, ox, y2 - 20)
    ox += 330
    cx4 = ox + 150
    sh = cx4 - cx2
    clip_id = "spec-trio-clip"
    ppp = [(x + sh, y) for x, y in outline]
    art4 = f'<clipPath id="{clip_id}"><path d="{rpath(ppp, TRIO_RHO * S2)}"/></clipPath><g clip-path="url(#{clip_id})">'
    for c, tn in ((c0b, tone(LIGHT["gold"])), (cblb, tone(LIGHT["sun"])), (cbrb, tone(LIGHT["green"]))):
        art4 += cube(c[0] + sh, c[1], r * S2, *tn)
    art4 += "</g>"
    body += panel("9", "Clip the cubes to it", "Draw the three cubes, then clip them to the rounded outline. Only the outside is rounded; the edges where cubes meet stay crisp. Then put the whole group inside the shell (spec: mark construction).", art4, ox, y2 - 20)
    ox += 330
    cx5 = ox + 150
    art5 = f'<svg x="{ox}" y="{y2 + 90}" width="300" height="300" viewBox="0 0 128 128">{mark_body(LIGHT)}</svg>'
    body += label(ox, y2 - 20, "Done: the mark", 17, weight=700, fill=LIGHT["shell"]) + art5
    return spec_head(width, "Cube construction", "How to draw one cube, then the group of three. Pink: construction. Unit r is the cube edge (16 in the mark).", 1180) + body + "</svg>"


def specs():
    return {"spec/mark-construction.svg": spec_construction(), "spec/icon-construction.svg": spec_icon(), "spec/lockup-clear-space.svg": spec_lockup(), "spec/cube-construction.svg": spec_cube()}


# ---------------------------------------------------------------- main

def write_all(files):
    for rel, text in files.items():
        p = BRAND / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text, newline="\n")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--svg-only", action="store_true", help="write the SVG files and the sprite, skip every export")
    args = ap.parse_args()

    svgs = {}
    for part in (lockups(), icons(), file_icons(), social(), specs()):
        svgs.update(part)
    write_all(svgs)
    write_all({"icon/sprite.svg": sprite(svgs)})
    print(f"{len(svgs) + 1} SVG files written")
    if args.svg_only:
        return

    chrome = find_chrome()
    jobs = []

    def job(src, out, width, transparent=True):
        jobs.append((str(BRAND / src), str(BRAND / out), width, transparent))

    for suf in VARIANTS:
        job(f"logo/islet-mark{suf}.svg", f"logo/islet-mark{suf}-1024.png", 1024)
        job(f"logo/islet-horizontal{suf}.svg", f"logo/islet-horizontal{suf}-2000.png", 2000)
        job(f"logo/islet-stacked{suf}.svg", f"logo/islet-stacked{suf}-1200.png", 1200)
    for s in (16, 24, 32, 48, 64, 96, 128, 192, 256, 512, 1024):
        job("icon/islet-icon.svg", f"icon/islet-icon-{s}.png", s)
    for s in (128, 512):
        job("icon/islet-icon-light.svg", f"icon/islet-icon-light-{s}.png", s)
    job("icon/apple-touch-icon.svg", "icon/apple-touch-icon-180.png", 180)
    job("icon/apple-touch-icon.svg", "icon/mstile-150.png", 150)
    job("icon/islet-icon.svg", "icon/android-chrome-192.png", 192)
    job("icon/islet-icon.svg", "icon/android-chrome-512.png", 512)
    job("icon/maskable.svg", "icon/maskable-512.png", 512)
    job("icon/avatar.svg", "icon/avatar-512.png", 512)
    job("icon/avatar-light.svg", "icon/avatar-light-512.png", 512)
    job("icon/islet-icon.svg", "editor/vscode-icon-256.png", 256)
    for f in sorted(glob.glob(str(BRAND / "editor/file-icons/*.svg"))):
        rel = os.path.relpath(f, BRAND).replace(os.sep, "/")
        job(rel, rel[:-4] + "-128.png", 128)
    for n, w, h, d, t in SOCIAL:
        job(f"social/{n}.svg", f"social/{n}.png", w, transparent=False)
    for f in sorted(glob.glob(str(BRAND / "spec/*.svg"))):
        rel = os.path.relpath(f, BRAND).replace(os.sep, "/")
        job(rel, rel[:-4] + ".png", 1600, transparent=False)

    with ThreadPoolExecutor(1 if sys.platform == "darwin" else 4) as pool:  # parallel headless Chrome is flaky on macOS
        for _ in pool.map(lambda a: export_png(chrome, *a), jobs):
            pass
    print(f"{len(jobs)} PNG files written")

    icon = lambda s: (BRAND / f"icon/islet-icon-{s}.png").read_bytes()
    write_ico(BRAND / "icon/favicon.ico", [(s, icon(s)) for s in (16, 32, 48)])
    write_ico(BRAND / "icon/islet.ico", [(s, icon(s)) for s in (16, 32, 48, 64, 128, 256)])
    write_icns(BRAND / "icon/islet.icns", {s: icon(s) for s in (16, 32, 64, 128, 256, 512, 1024)})
    print("favicon.ico, islet.ico, islet.icns written")


if __name__ == "__main__":
    main()
