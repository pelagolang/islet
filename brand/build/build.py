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
    ("islet-yaml", "one cube: a package"),
    ("islet-lock", "a cube with a shackle: locked"),
    ("monorepo-yaml", "a cube of blocks: many packages"),
)


def rounded_cube(cx, cy, r, tones, rho):
    """One cube with its outer outline rounded (clipped), like the trio in the mark."""
    _uid[0] += 1
    cid = f"islet-rc-{_uid[0]}"
    pts = [(cx + r * math.cos(math.radians(-90 + 60 * k)), cy + r * math.sin(math.radians(-90 + 60 * k))) for k in range(6)]
    return f'<clipPath id="{cid}"><path d="{rpath(pts, rho)}"/></clipPath><g clip-path="url(#{cid})">' + cube(cx, cy, r, *tones) + "</g>"


def block_cube(pal, s=12, cx=64, base=66):
    """A cube made of 27 small cubes (3 x 3 x 3), drawn back to front with a small gap between blocks."""
    out = ""
    for _, x, y, z in sorted((x + y + z, x, y, z) for x in range(3) for y in range(3) for z in range(3)):
        out += cube(cx + (x - y) * K * s, base + (x + y) * 0.5 * s - z * s, s * 0.88, *tone(pal["gold"]))
    return out


def file_icon(name, dark):
    """A rounded hexagon tile (the shell) with the cubes inside. No page outline: the shape is Islet's own."""
    pal = DARK  # the cubes always use the bright palette; the tile changes with the theme
    fill = "#0F2F37" if dark else DEEP
    tile = (
        f'<path d="{rpath([(64 + 54 * math.cos(math.radians(-90 + 60 * k)), 64 + 54 * math.sin(math.radians(-90 + 60 * k))) for k in range(6)], 14)}" '
        f'fill="{fill}" stroke="{pal["shell"]}" stroke-width="9" stroke-linejoin="round"/>'
    )
    if name == "islet-yaml":
        glyph = rounded_cube(64, 66, 28, tone(pal["gold"]), 12)
    elif name == "islet-lock":
        glyph = (
            rounded_cube(64, 64, 27, tone(pal["gold"]), 12)
            + f'<circle cx="91" cy="92" r="17" fill="{pal["sun"]}" stroke="{fill}" stroke-width="5"/>'
            + f'<path d="M85.5 90V86.5a5.5 5.5 0 0 1 11 0V90" fill="none" stroke="{pal["ink"]}" stroke-width="3.2" stroke-linecap="round"/>'
            + f'<rect x="83" y="89" width="16" height="12.5" rx="3.2" fill="{pal["ink"]}"/>'
            + f'<circle cx="91" cy="94.2" r="2" fill="{pal["sun"]}"/><rect x="90" y="95" width="2" height="3.6" rx="1" fill="{pal["sun"]}"/>'
        )
    else:
        glyph = block_cube(pal)
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


def spec_shell():
    """How to draw the hexagon shell, step by step, then how the trio sits inside it."""
    S = 2.9
    colw = 345
    xs = [48 + i * colw for i in range(5)]
    cy0 = 470
    body = ""

    def panel(i, title, text, ox, y=150):
        out = label(ox, y, f"{i}. {title}", 17, weight=700, fill=LIGHT["shell"])
        t, _ = wrap(text, ox, y + 24, 13, 38)
        return out + t

    R = SHELL_R
    P = lambda cx, k, rr=R: (cx + rr * S * math.cos(math.radians(-90 + 60 * k)), cy0 + rr * S * math.sin(math.radians(-90 + 60 * k)))
    names = ["V1", "V2", "V3", "V4", "V5", "V6"]

    # 1 circle and six points
    cx = xs[0] + 150
    body += panel(1, "A circle, six points", "Draw a circle of radius 52 around the centre of the 128 grid (64, 64). Mark six points on it, 60 degrees apart, starting at the top.", xs[0])
    body += f'<circle cx="{cx}" cy="{cy0}" r="{R * S}" fill="none" stroke="{GUIDE}" stroke-width="1.2" stroke-dasharray="4 4"/>'
    body += line(cx, cy0 - R * S - 12, cx, cy0 + R * S + 12, GUIDE, 1, "3 5") + line(cx - R * S - 12, cy0, cx + R * S + 12, cy0, GUIDE, 1, "3 5")
    for k in range(6):
        x, y = P(cx, k)
        dx = 12 if x >= cx else -12
        body += f'<circle cx="{x:.1f}" cy="{y:.1f}" r="3.5" fill="{GUIDE}"/>' + label(x + dx, y + (-8 if k in (0, 1, 5) else 18), names[k], 12, "start" if dx > 0 else "end", GUIDE, 700)
    body += dim(cx, cy0 + R * S + 40, cx + R * S, cy0 + R * S + 40, "r 52", 1, 12)

    # 2 sharp hexagon with the coordinates
    cx = xs[1] + 185
    body += panel(2, "Join them", "Join the six points into a hexagon with a point on top. It is regular: every side is 52 long.", xs[1])
    body += f'<path d="M{"L".join("%.1f %.1f" % P(cx, k) for k in range(6))}Z" fill="none" stroke="{GUIDE}" stroke-width="2"/>'
    coords = [(64, 12), (109.0, 38), (109.0, 90), (64, 116), (19.0, 90), (19.0, 38)]
    for k, (ux, uy) in enumerate(coords):
        x, y = P(cx, k)
        dx = 12 if x >= cx else -12
        body += f'<circle cx="{x:.1f}" cy="{y:.1f}" r="3.5" fill="{GUIDE}"/>' + label(x + dx, y + (-8 if k in (0, 1, 5) else 18), f"({ux:g}, {uy:g})", 12, "start" if dx > 0 else "end", INK, 600)

    # 3 one corner, zoomed
    ox = xs[2]
    Z = 6.5
    vx, vy = ox + 150, cy0 - 40  # the top vertex, zoomed
    ex1, ey1 = vx + math.cos(math.radians(30)) * 18 * Z * 0.9, vy + math.sin(math.radians(30)) * 18 * Z * 0.9
    ex0, ey0 = vx - math.cos(math.radians(30)) * 18 * Z * 0.9, ey1
    rho = 14
    a = (vx - math.cos(math.radians(30)) * rho * Z, vy + math.sin(math.radians(30)) * rho * Z)
    b = (vx + math.cos(math.radians(30)) * rho * Z, a[1])
    body += panel(3, "Round each corner", "At every corner, measure 14 units back along each side (points A and B). Replace the corner with a curve from A to B that uses the corner as its control point.", ox)
    body += f'<path d="M{ex0:.1f} {ey0:.1f}L{vx:.1f} {vy:.1f}L{ex1:.1f} {ey1:.1f}" fill="none" stroke="{MUTED_LIGHT}" stroke-width="1.5" stroke-dasharray="5 4"/>'
    body += f'<path d="M{a[0]:.1f} {a[1]:.1f}Q{vx:.1f} {vy:.1f} {b[0]:.1f} {b[1]:.1f}" fill="none" stroke="{GUIDE}" stroke-width="3"/>'
    body += line(a[0], a[1], vx, vy, GUIDE, 1, "3 3") + line(b[0], b[1], vx, vy, GUIDE, 1, "3 3")
    body += f'<circle cx="{vx:.1f}" cy="{vy:.1f}" r="4" fill="{GUIDE}"/>' + label(vx, vy - 14, "corner (control point)", 12, "middle", GUIDE, 700)
    body += f'<circle cx="{a[0]:.1f}" cy="{a[1]:.1f}" r="4" fill="{GUIDE}"/>' + label(a[0] - 8, a[1] + 20, "A", 13, "end", GUIDE, 700)
    body += f'<circle cx="{b[0]:.1f}" cy="{b[1]:.1f}" r="4" fill="{GUIDE}"/>' + label(b[0] + 8, b[1] + 20, "B", 13, "start", GUIDE, 700)
    body += label(vx, vy + 150, "A and B are 14 from the corner", 12, "middle", GUIDE, 600)

    # 4 the rounded outline and the stroke
    cx = xs[3] + 150
    pts = [P(cx, k) for k in range(6)]
    body += panel(4, "Give the line a width", "The rounded hexagon is the centre line. Stroke it 11 wide with round joins: that is 5.5 on each side, so the shell reaches 46.5 to 57.5 from the centre.", xs[3])
    body += f'<path d="{rpath(pts, rho * S)}" fill="none" stroke="{LIGHT["shell"]}" stroke-width="{SHELL_W * S}" stroke-linejoin="round" opacity="0.85"/>'
    body += f'<path d="{rpath(pts, rho * S)}" fill="none" stroke="{GUIDE}" stroke-width="1.5"/>'
    body += f'<circle cx="{cx}" cy="{cy0}" r="{(R + SHELL_W / 2) * S}" fill="none" stroke="{GUIDE}" stroke-width="1" stroke-dasharray="2 4"/>'
    body += f'<circle cx="{cx}" cy="{cy0}" r="{(R - SHELL_W / 2) * S}" fill="none" stroke="{GUIDE}" stroke-width="1" stroke-dasharray="2 4"/>'
    body += dim(cx + (R - SHELL_W / 2) * S, cy0 + R * S + 40, cx + (R + SHELL_W / 2) * S, cy0 + R * S + 40, "11", 1, 12)

    # 5 trio inside
    ox = xs[4]
    cx = ox + 150
    body += panel(5, "Put the cubes inside", "Centre the three-cube group on x = 64 and y = 66, two units below the shell centre. The clear gap to the inside of the shell is about the width of a cube edge.", ox)
    body += f'<g transform="translate({cx - 64 * S} {cy0 - 64 * S}) scale({S})">{mark_body(LIGHT)}</g>'
    body += line(cx - R * S - 8, cy0 + 2 * S, cx + R * S + 8, cy0 + 2 * S, GUIDE, 1, "3 5") + label(cx + R * S + 10, cy0 + 2 * S + 4, "y 66", 12, "start", GUIDE, 700)
    body += line(cx, cy0 - R * S - 8, cx, cy0 + R * S + 8, GUIDE, 1, "3 5")

    # the recipe
    body += label(48, 790, "Recipe", 20, weight=700, fill=INK)
    code = [
        "points   (64,12) (109,38) (109,90) (64,116) (19,90) (19,38)",
        "corners  at each point P: A = 14 back toward the previous point, B = 14 on toward the next;  M A  Q P  B",
        "stroke   #0F9AA6 (dark: #4FD6D0), width 11, fill none, round joins",
    ]
    body += '<rect x="48" y="810" width="1100" height="112" rx="6" fill="#0E3440"/>'
    for i, ln in enumerate(code):
        body += f'<text x="64" y="{842 + i * 28}" font-family="Consolas, Menlo, monospace" font-size="15" fill="#FFF1C9">{ln}</text>'
    body += label(48, 954, "The finished path is in brand/logo/islet-mark.svg. Six curves joined by straight lines; build.py draws it in rpath().", 14, fill=INK)
    body += f'<svg x="1250" y="740" width="260" height="260" viewBox="0 0 128 128">{mark_body(LIGHT)}</svg>'
    return spec_head(1800, "Shell construction", "How to draw the hexagon shell around the cubes. Pink: construction. All numbers are on the 128 grid.", 1040) + body + "</svg>"


def _icon_spec(title, sub, art, guides, notes, extra_rows=()):
    """One file-icon spec sheet: the drawing on the 128 grid at 5x with guides, notes on the right."""
    S, ox, oy = 5, 70, 140
    body = grid(ox, oy, S) + f'<g transform="translate({ox} {oy}) scale({S})">{art}</g>' + guides(ox, oy, S)
    right = ox + 128 * S + 60
    y = 180
    for head, text in notes:
        body += label(right, y, head, 15, weight=700, fill=LIGHT["shell"])
        t, ly = wrap(text, right, y + 22, 14, 46)
        body += t
        y = ly + 38
    for r, h in extra_rows:
        body += r(right, y + 14)
        y += h
    return spec_head(right + 520, title, sub, max(int(y) + 40, 820)) + body + "</svg>"


def _tile_fill(dark=False):
    return "#0F2F37" if dark else DEEP


def spec_file_tile():
    def guides(ox, oy, S):
        px = lambda u: ox + u * S
        py = lambda u: oy + u * S
        g = ""
        for rr, d in ((54, "6 4"), (49.5, "2 4"), (58.5, "2 4")):
            g += f'<circle cx="{px(64)}" cy="{py(64)}" r="{rr * S}" fill="none" stroke="{GUIDE}" stroke-width="1.2" stroke-dasharray="{d}"/>'
        g += line(px(64), py(2), px(64), py(126), GUIDE, 1, "3 5") + line(px(2), py(64), px(126), py(64), GUIDE, 1, "3 5")
        g += dim(px(64), py(4), px(64 + 54 * 1), py(4), "", 1) if False else ""
        g += dim(px(64), py(122), px(118), py(122), "R 54", 1, 12)
        g += dim(px(114), py(64), px(123), py(64), "", 1, 12) if False else ""
        return g

    notes = [
        ("What it is", "The tile every file icon sits on: a rounded hexagon with a point on top, the same shape as the logo's shell, filled and outlined. It replaces the page outline other icon sets use."),
        ("Shape", "Regular hexagon, radius 54 to the centre of the line, centred on (64, 64). Corners rounded with a radius of 14, built as on the shell sheet: a curve starting 14 back from each corner."),
        ("Line", "Stroke 9 wide with round joins, so the line runs from 49.5 to 58.5 from the centre. Colour: the dark-background shell teal #4FD6D0 on both themes."),
        ("Fill", "Light theme: Deep #08262D. Dark theme: #0F2F37, a little lighter than the editor's dark page so the tile still reads."),
        ("Room inside", "The inner area is about 99 wide. Glyphs stay within a circle of radius 36 around (64, 66) so they never touch the line."),
        ("Differences from the logo shell", "Radius 54 and stroke 9, against 52 and 11 for the logo, so the glyph has more room at 16 px."),
    ]
    art = f'<path d="{rpath([(64 + 54 * math.cos(math.radians(-90 + 60 * k)), 64 + 54 * math.sin(math.radians(-90 + 60 * k))) for k in range(6)], 14)}" fill="{DEEP}" stroke="{DARK["shell"]}" stroke-width="9" stroke-linejoin="round"/>'
    return _icon_spec("File icon tile", "The hexagon every file icon sits on. Pink: construction lines on the 128 grid.", art, guides, notes)


def spec_file_yaml():
    r, cy, rho = 28, 66, 12

    def guides(ox, oy, S):
        px = lambda u: ox + u * S
        py = lambda u: oy + u * S
        g = f'<circle cx="{px(64)}" cy="{py(cy)}" r="{r * S}" fill="none" stroke="{GUIDE}" stroke-width="1.2" stroke-dasharray="4 4"/>'
        g += line(px(64), py(2), px(64), py(126), GUIDE, 1, "3 5") + line(px(2), py(cy), px(126), py(cy), GUIDE, 1, "3 5")
        g += f'<circle cx="{px(64)}" cy="{py(cy)}" r="3.5" fill="{GUIDE}"/>' + label(px(64) + 8, py(cy) + 18, "(64, 66)", 12, "start", GUIDE, 700)
        g += dim(px(64 + K * r), py(cy + r + 8), px(64), py(cy + r + 8), "", 1) if False else ""
        g += dim(px(64 - K * r), py(104), px(64 + K * r), py(104), "2 x 0.866 r = 48.5", 1, 12)
        g += dim(px(104), py(cy - r), px(104), py(cy + r), "2 r = 56", 1, 12)
        return g

    notes = [
        ("What it is", "islet.yaml: one package, drawn as one cube. It is the single cube of the mark, on its own, with its outer outline rounded."),
        ("Cube", "Isometric cube of edge r = 28, centred on (64, 66). Drawn as on the cube sheet: a hexagon of radius 28 split into top, left and right faces by three lines from the centre."),
        ("Colour", "Gold #FFD15C. Top face mixed 25 % toward white, left face as is, right face mixed 22 % toward black. No outlines."),
        ("Rounding", "Only the outer outline is rounded, with a radius of 12 (0.43 of the edge, the same ratio as the logo's cubes: 7 on 16). The cube is clipped to a rounded hexagon of radius 28; the edges between faces stay crisp."),
        ("Position", "Two units below the tile's centre, like the trio in the logo. The glyph stays inside the radius-36 circle."),
    ]
    return _icon_spec("File icon: islet.yaml", "One rounded cube on the tile. Pink: construction.", file_icon("islet-yaml", False), guides, notes)


def spec_file_lock():
    def guides(ox, oy, S):
        px = lambda u: ox + u * S
        py = lambda u: oy + u * S
        g = f'<circle cx="{px(91)}" cy="{py(92)}" r="{17 * S}" fill="none" stroke="{GUIDE}" stroke-width="1.2" stroke-dasharray="4 4"/>'
        g += f'<circle cx="{px(91)}" cy="{py(92)}" r="{22 * S}" fill="none" stroke="{GUIDE}" stroke-width="1" stroke-dasharray="2 4"/>'
        g += f'<circle cx="{px(64)}" cy="{py(64)}" r="{27 * S}" fill="none" stroke="{GUIDE}" stroke-width="1.2" stroke-dasharray="4 4"/>'
        g += f'<circle cx="{px(91)}" cy="{py(92)}" r="3.5" fill="{GUIDE}"/>' + label(px(91) + 8, py(92) + 52, "(91, 92)", 12, "start", GUIDE, 700)
        g += f'<circle cx="{px(64)}" cy="{py(64)}" r="3.5" fill="{GUIDE}"/>' + label(px(64) - 8, py(64) - 10, "(64, 64)", 12, "end", GUIDE, 700)
        g += dim(px(74), py(112), px(108), py(112), "badge 34", 1, 12)
        return g

    def zoom(right, y):
        # the padlock drawn large with its numbers
        Z = 9
        ox, oy = right, y + 10
        out = label(right, y - 8, "The padlock, drawn 9 times larger", 15, weight=700, fill=LIGHT["shell"])
        pal = DARK
        out += f'<rect x="{ox - 10}" y="{oy}" width="260" height="200" rx="8" fill="{pal["sun"]}"/>'
        out += f'<g transform="translate({ox + 130 - 91 * Z} {oy + 100 - 92 * Z}) scale({Z})">'
        out += f'<path d="M85.5 90V86.5a5.5 5.5 0 0 1 11 0V90" fill="none" stroke="{pal["ink"]}" stroke-width="3.2" stroke-linecap="round"/><rect x="83" y="89" width="16" height="12.5" rx="3.2" fill="{pal["ink"]}"/><circle cx="91" cy="94.2" r="2" fill="{pal["sun"]}"/><rect x="90" y="95" width="2" height="3.6" rx="1" fill="{pal["sun"]}"/></g>'
        return out

    notes = [
        ("What it is", "islet.lock: a package that is pinned. The rounded cube of islet.yaml with a padlock badge, so the two files read as a pair."),
        ("Cube", "As the islet.yaml cube but edge 27 and centred on the tile centre (64, 64), so the lock file never pushes the cube off centre. Gold #FFD15C, rounded outline radius 12."),
        ("Badge", "A circle of radius 17 centred on (91, 92) in Sunburn #FF8556. It overlaps the cube's lower right corner."),
        ("Ring", "A 5 wide ring in the tile colour around the badge cuts it away from the cube. The ring is the badge's stroke, so it is drawn with the circle."),
        ("Padlock", "Shackle: a path from (85.5, 90) up 3.5, a half circle of radius 5.5 and back down to (96.5, 90), stroke 3.2, round caps, Sand #FFF1C9. Body: a 16 x 12.5 rounded rectangle at (83, 89), corner radius 3.2, Sand. Keyhole: a circle of radius 2 at (91, 94.2) and a 2 x 3.6 slot below it, both in Sunburn."),
        ("Why a badge", "The cube carries the meaning 'a package' for every Islet file; the badge says what this file does to it."),
    ]
    return _icon_spec("File icon: islet.lock", "The rounded cube with a padlock badge. Pink: construction.", file_icon("islet-lock", False), guides, notes, ((zoom, 250),))


def spec_file_monorepo():
    s, cx, base = 12, 64, 66

    def guides(ox, oy, S):
        px = lambda u: ox + u * S
        py = lambda u: oy + u * S
        g = ""
        # centres of the 27 blocks
        for x in range(3):
            for y in range(3):
                for z in range(3):
                    g += f'<circle cx="{px(cx + (x - y) * K * s):.1f}" cy="{py(base + (x + y) * 0.5 * s - z * s):.1f}" r="2" fill="{GUIDE}" opacity="0.7"/>'
        g += f'<circle cx="{px(64)}" cy="{py(66)}" r="{36 * S}" fill="none" stroke="{GUIDE}" stroke-width="1.2" stroke-dasharray="4 4"/>'
        # the three axis steps from block (0,0,0)
        o = (cx, base + 0)
        for (dx, dy, name) in ((K * s, 0.5 * s, "+x"), (-K * s, 0.5 * s, "+y"), (0, -s, "+z")):
            x0, y0 = px(o[0]), py(o[1])
            g += line(x0, y0, px(o[0] + dx * 1), py(o[1] + dy * 1), "#12A150", 3)
            g += label(px(o[0] + dx * 1.45), py(o[1] + dy * 1.45) + 5, name, 13, "middle", "#0b7a3b", 700)
        return g

    def steps(right, y):
        out = label(right, y, "Block positions", 15, weight=700, fill=LIGHT["shell"])
        rows = ["block (x, y, z), each 0, 1 or 2", "centre x = 64 + (x - y) x 10.39", "centre y = 66 + (x + y) x 6 - z x 12", "drawn edge = 0.88 x 12 = 10.56", "order: back to front, lowest x + y + z first"]
        for i, t in enumerate(rows):
            out += label(right, y + 26 + i * 22, t, 14, fill=INK)
        return out

    notes = [
        ("What it is", "monorepo.yaml: many packages in one repository, drawn as a cube built of 27 small cubes (3 x 3 x 3), each one a package."),
        ("Block", "Each block is a cube as on the cube sheet, with an edge of 12 on the grid. It is drawn at 0.88 of that (10.56) so a thin gap of the tile colour shows between neighbours."),
        ("Grid", "A block at (x, y, z) sits 12 x 0.866 = 10.39 to the right and 6 down for each step in x, 10.39 to the left and 6 down for each step in y, and 12 up for each step in z. The middle block (1, 1, 1) is on the glyph centre (64, 66)."),
        ("Order", "Draw back to front: sort the blocks by x + y + z, lowest first, so nearer blocks cover farther ones."),
        ("Colour", "All blocks Gold #FFD15C with the usual three faces. One colour keeps it quiet next to the other two icons; the number of blocks carries the meaning."),
        ("Size", "The whole cube is a hexagon of radius 36 (3 blocks). It fills the same space as the single cube on the other two icons."),
    ]
    return _icon_spec("File icon: monorepo.yaml", "A cube of 27 blocks on the tile. Pink: construction; green arrows: one step along each axis.", file_icon("monorepo-yaml", False), guides, notes, ((steps, 150),))


# ---------------------------------------------------------------- step-by-step file icon sheets

def _gl(x1, y1, x2, y2, dash=None, w=1.5, col=None):
    d = f' stroke-dasharray="{dash}"' if dash else ""
    return f'<line x1="{x1:.2f}" y1="{y1:.2f}" x2="{x2:.2f}" y2="{y2:.2f}" stroke="{col or GUIDE}" stroke-width="{w}" vector-effect="non-scaling-stroke"{d}/>'


def _gc(x, y, r, dash="4 4", w=1.3, col=None):
    d = f' stroke-dasharray="{dash}"' if dash else ""
    return f'<circle cx="{x:.2f}" cy="{y:.2f}" r="{r:.2f}" fill="none" stroke="{col or GUIDE}" stroke-width="{w}" vector-effect="non-scaling-stroke"{d}/>'


def _gp(path, w=1.6, col=None, dash=None, fill="none"):
    d = f' stroke-dasharray="{dash}"' if dash else ""
    return f'<path d="{path}" fill="{fill}" stroke="{col or GUIDE}" stroke-width="{w}" vector-effect="non-scaling-stroke" stroke-linejoin="round"{d}/>'


def _gd(x, y, u):
    return f'<circle cx="{x:.2f}" cy="{y:.2f}" r="{3.5 * u:.2f}" fill="{GUIDE}"/>'


def _gt(x, y, text, u, anchor="start", size=12, col=None, weight=700):
    return (f'<text x="{x:.2f}" y="{y:.2f}" font-family="{FONT}" font-size="{size * u:.2f}" font-weight="{weight}" '
            f'text-anchor="{anchor}" fill="{col or GUIDE}">{text}</text>')


def _hex_pts(cx, cy, r):
    return [(cx + r * math.cos(math.radians(-90 + 60 * k)), cy + r * math.sin(math.radians(-90 + 60 * k))) for k in range(6)]


def _poly_d(pts):
    return "M" + "L".join(f"{x:.2f} {y:.2f}" for x, y in pts) + "Z"


def _ab_points(pts, rho):
    """The points A and B (rho back along each side) of every corner of a polygon."""
    out = []
    n = len(pts)
    for i in range(n):
        p0, p1, p2 = pts[i - 1], pts[i], pts[(i + 1) % n]
        for q in (p0, p2):
            L = math.hypot(q[0] - p1[0], q[1] - p1[1])
            out.append((p1[0] + (q[0] - p1[0]) * rho / L, p1[1] + (q[1] - p1[1]) * rho / L))
    return out


def step_sheet(title, sub, panels, cols=4):
    """Panels of (title, text, view, art): `view` is (x, y, width) of the square of the 128 grid shown, `art(u)` draws it."""
    pw, box = 345, 295
    top = 150
    rowh = 130 + box + 40
    rows = (len(panels) + cols - 1) // cols
    width = 48 + cols * pw
    body = ""
    for i, (ptitle, text, view, art) in enumerate(panels):
        ox = 48 + (i % cols) * pw
        oy = top + (i // cols) * rowh
        body += label(ox, oy, f"{i + 1}. {ptitle}", 17, weight=700, fill=LIGHT["shell"])
        t, _ = wrap(text, ox, oy + 24, 13, 40)
        body += t
        by = oy + 118
        vx, vy, vw = view
        u = vw / box
        body += f'<rect x="{ox}" y="{by}" width="{box}" height="{box}" fill="#ffffff" stroke="#c9d6df"/>'
        body += f'<svg x="{ox}" y="{by}" width="{box}" height="{box}" viewBox="{vx} {vy} {vw} {vw}">{art(u)}</svg>'
    return spec_head(width, title, sub, top + rows * rowh + 20) + body + "</svg>"


_TILE_PATH = None


def _tile_d():
    return rpath(_hex_pts(64, 64, 54), 14)


def _tile(dark=False):
    return f'<path d="{_tile_d()}" fill="{_tile_fill(dark)}" stroke="{DARK["shell"]}" stroke-width="9" stroke-linejoin="round"/>'


def _cube_guides(cx, cy, r, u, spokes=True, labels=True):
    pts = _hex_pts(cx, cy, r)
    out = _gc(cx, cy, r) + _gp(_poly_d(pts))
    if spokes:
        for k in (5, 1, 3):
            out += _gl(cx, cy, pts[k][0], pts[k][1], None, 2.4)
    names = ["T", "RT", "RB", "B", "LB", "LT"]
    for k, (x, y) in enumerate(pts):
        out += _gd(x, y, u)
        if labels:
            dx = 6 * u if x >= cx else -6 * u
            out += _gt(x + dx, y + (-5 * u if k in (0, 1, 5) else 14 * u), names[k], u, "start" if dx > 0 else "end")
    return out


def steps_tile():
    pts = _hex_pts(64, 64, 54)
    panels = [
        ("A circle, six points", "Draw a circle of radius 54 around the centre (64, 64). Mark six points on it, 60 degrees apart, starting at the top. This is the shell's recipe with a slightly bigger radius.", (0, 0, 128),
         lambda u: _gc(64, 64, 54) + _gl(64, 4, 64, 124, "3 5", 1) + _gl(4, 64, 124, 64, "3 5", 1) + "".join(_gd(x, y, u) for x, y in pts) + _gt(64 + 4 * u, 8, "top", u) ),
        ("Join them", "Join the six points into a regular hexagon with a point on top. Every side is 54 long. The corners are (64, 10), (110.8, 37), (110.8, 91), (64, 118), (17.2, 91) and (17.2, 37).", (0, 0, 128),
         lambda u: _gp(_poly_d(pts), 2) + "".join(_gd(x, y, u) for x, y in pts)),
        ("Mark the corner points", "At every corner measure 14 units back along each of the two sides. That gives twelve points, two per corner (A and B).", (0, 0, 128),
         lambda u: _gp(_poly_d(pts), 1.5, dash="5 4") + "".join(_gd(x, y, u) for x, y in _ab_points(pts, 14)) + _gt(64, 8, "A and B are 14 from each corner", u, "middle")),
        ("Round the corners", "Replace each corner with a curve from A to B that uses the corner as its control point. The six curves are joined by the straight pieces between them.", (0, 0, 128),
         lambda u: _gp(_poly_d(pts), 1.2, dash="5 4", col=MUTED_LIGHT) + _gp(_tile_d(), 2.4) + "".join(_gd(x, y, u) for x, y in _ab_points(pts, 14))),
        ("Give the line a width", "The rounded hexagon is the centre line. Stroke it 9 wide, with round joins, in the dark-background teal #4FD6D0: that is 4.5 each side, 49.5 to 58.5 from the centre.", (0, 0, 128),
         lambda u: f'<path d="{_tile_d()}" fill="none" stroke="{DARK["shell"]}" stroke-width="9" stroke-linejoin="round"/>' + _gp(_tile_d(), 1.2) + _gc(64, 64, 49.5, "2 4", 1) + _gc(64, 64, 58.5, "2 4", 1)),
        ("Fill it", "Fill the inside with Deep #08262D on light themes and #0F2F37 on dark themes. Fill and stroke are one path, so the fill reaches the middle of the line.", (0, 0, 128),
         lambda u: _tile(False)),
        ("Check the room inside", "Glyphs stay inside a circle of radius 36 around (64, 66). The inner area is 99 wide, so a glyph at radius 36 leaves about 13 units to the line.", (0, 0, 128),
         lambda u: _tile(False) + _gc(64, 66, 36, "4 4", 1.6, "#FFC83D") + _gl(64, 4, 64, 124, "3 5", 1, "#FFC83D") + _gl(4, 66, 124, 66, "3 5", 1, "#FFC83D") + _gt(64 + 38 * 0.2, 66 - 38, "r 36", u, "start", 12, "#FFC83D")),
        ("Both themes", "The same shape on both. Only the fill changes, so the tile reads on a white or on a dark editor background.", (0, 0, 128),
         lambda u: f'<g transform="translate(2 34) scale(0.47)">{_tile(False)}</g><g transform="translate(66 34) scale(0.47)">{_tile(True)}</g>' ),
    ]
    return step_sheet("File icon tile: construction", "How to draw the hexagon tile, step by step. Pink: construction.", panels)


def steps_yaml():
    r, cx, cy, rho = 28, 64, 66, 12
    pts = _hex_pts(cx, cy, r)
    t0, t1, t2 = tone(DARK["gold"])
    P = {n: pts[i] for i, n in enumerate(["T", "RT", "RB", "B", "LB", "LT"])}
    faces = lambda: poly([P["T"], P["RT"], (cx, cy), P["LT"]], t0) + poly([P["LT"], (cx, cy), P["B"], P["LB"]], t1) + poly([(cx, cy), P["RT"], P["RB"], P["B"]], t2)
    view = (cx - 40, cy - 40, 80)
    panels = [
        ("A hexagon, point up", "Draw a circle of radius 28 around (64, 66) and mark six points on it, 60 degrees apart, starting at the top: T, RT, RB, B, LB, LT. Join them.", view,
         lambda u: _cube_guides(cx, cy, r, u, spokes=False)),
        ("Three spokes", "Join the centre to LT, RT and B. The hexagon splits into three rhombi: top, left and right.", view,
         lambda u: _cube_guides(cx, cy, r, u)),
        ("Fill the faces", "Top: gold #FFD15C mixed 25 % toward white. Left: gold itself. Right: gold mixed 22 % toward black. No outlines.", view,
         lambda u: faces()),
        ("Mark the corner points", "At each of the six corners measure 12 units back along both sides. That is 0.43 of the edge, the same ratio as the logo's cubes (7 on 16).", view,
         lambda u: faces() + _gp(_poly_d(pts), 1.4, dash="5 4") + "".join(_gd(x, y, u) for x, y in _ab_points(pts, rho))),
        ("Round the outline", "Draw a curve from A to B at each corner, using the corner as the control point. The result is the rounded outline.", view,
         lambda u: _gp(_poly_d(pts), 1.2, dash="5 4", col=MUTED_LIGHT) + _gp(rpath(pts, rho), 2.4) + "".join(_gd(x, y, u) for x, y in _ab_points(pts, rho))),
        ("Clip the faces to it", "Draw the three faces, then clip them to the rounded outline. Only the outside is rounded; the lines between faces stay straight and crisp.", view,
         lambda u: rounded_cube(cx, cy, r, (t0, t1, t2), rho) + _gp(rpath(pts, rho), 1.2, dash="4 4")),
        ("Put it on the tile", "Centre it on (64, 66), two units below the tile centre, like the trio in the logo. It sits inside the radius-36 circle.", (0, 0, 128),
         lambda u: _tile(False) + rounded_cube(cx, cy, r, (t0, t1, t2), rho) + _gc(64, 66, 36, "4 4", 1.4, "#FFC83D") + _gl(64, 4, 64, 124, "3 5", 1, "#FFC83D") + _gl(4, 66, 124, 66, "3 5", 1, "#FFC83D")),
        ("Done", "islet.yaml. Light theme on the left, dark theme on the right; only the tile fill changes.", (0, 0, 128),
         lambda u: f'<g transform="translate(2 34) scale(0.47)">{file_icon("islet-yaml", False)}</g><g transform="translate(66 34) scale(0.47)">{file_icon("islet-yaml", True)}</g>'),
    ]
    return step_sheet("File icon islet.yaml: construction", "The rounded cube, step by step. Pink: construction.", panels)


def steps_lock():
    pal = DARK
    cube = lambda: rounded_cube(64, 64, 27, tone(pal["gold"]), 12)
    fill = _tile_fill(False)
    badge_view = (68, 68, 48)
    shackle_d = "M85.5 90V86.5a5.5 5.5 0 0 1 11 0V90"
    badge = lambda: f'<circle cx="91" cy="92" r="17" fill="{pal["sun"]}" stroke="{fill}" stroke-width="5"/>'
    shack = lambda: f'<path d="{shackle_d}" fill="none" stroke="{pal["ink"]}" stroke-width="3.2" stroke-linecap="round"/>'
    body = lambda: f'<rect x="83" y="89" width="16" height="12.5" rx="3.2" fill="{pal["ink"]}"/>'
    key = lambda: f'<circle cx="91" cy="94.2" r="2" fill="{pal["sun"]}"/><rect x="90" y="95" width="2" height="3.6" rx="1" fill="{pal["sun"]}"/>'
    panels = [
        ("The cube, centred", "Draw the rounded gold cube as for islet.yaml, but with edge 27 and centred on the tile centre (64, 64). The badge goes on top of it, so the cube never moves.", (0, 0, 128),
         lambda u: _tile(False) + cube() + _gl(64, 4, 64, 124, "3 5", 1) + _gl(4, 64, 124, 64, "3 5", 1) + _gd(64, 64, u) + _gt(64 + 5 * u, 64 - 5 * u, "(64, 64)", u)),
        ("Place the badge circle", "Put a circle of radius 17 centred on (91, 92), lower right of the cube. It overlaps the cube's corner but stays inside the tile.", (0, 0, 128),
         lambda u: _tile(False) + cube() + _gc(91, 92, 17, "4 4", 1.6) + _gd(91, 92, u) + _gt(91 - 4 * u, 92 + 32, "(91, 92) r 17", u, "middle")),
        ("Fill and ring", "Fill it Sunburn #FF8556 and give it a 5 wide stroke in the tile colour. The stroke is the ring that cuts the badge away from the cube.", (0, 0, 128),
         lambda u: _tile(False) + cube() + badge() + _gc(91, 92, 19.5, "2 4", 1)),
        ("Shackle", "In the badge, draw the shackle from (85.5, 90) up 3.5, a half circle of radius 5.5 centred on (91, 86.5) and down to (96.5, 90). Stroke 3.2, round caps, Sand #FFF1C9.", badge_view,
         lambda u: badge() + shack() + _gc(91, 86.5, 5.5, "3 3", 1.4) + "".join(_gd(x, y, u * 0.8) for x, y in ((85.5, 90), (96.5, 90), (85.5, 86.5), (96.5, 86.5), (91, 86.5)))),
        ("Body", "Below it draw a rounded rectangle 16 wide and 12.5 tall with its top left at (83, 89) and corner radius 3.2, Sand. The legs end 1 unit inside the top of the body, so the shackle looks joined to it.", badge_view,
         lambda u: badge() + shack() + body() + _gp("M83 89h16v12.5H83z", 1.2, dash="3 3")),
        ("Keyhole", "In the body cut a keyhole in Sunburn: a circle of radius 2 centred on (91, 94.2) and a slot 2 wide and 3.6 tall with its top left at (90, 95).", badge_view,
         lambda u: badge() + shack() + body() + key() + _gc(91, 94.2, 2, None, 1) ),
        ("The finished badge", "Badge, shackle, body and keyhole together. Everything is on the 128 grid, so it scales with the tile.", badge_view,
         lambda u: badge() + shack() + body() + key()),
        ("Done", "islet.lock. Light theme on the left, dark theme on the right; only the tile fill (and the ring, which follows it) changes.", (0, 0, 128),
         lambda u: f'<g transform="translate(2 34) scale(0.47)">{file_icon("islet-lock", False)}</g><g transform="translate(66 34) scale(0.47)">{file_icon("islet-lock", True)}</g>'),
    ]
    return step_sheet("File icon islet.lock: construction", "The rounded cube with its padlock badge, step by step. Pink: construction.", panels)


def steps_monorepo():
    s, cx, base = 12, 64, 66
    pal = DARK
    cen = lambda x, y, z: (cx + (x - y) * K * s, base + (x + y) * 0.5 * s - z * s)
    blk = lambda x, y, z: cube(*cen(x, y, z), s * 0.88, *tone(pal["gold"]))
    stack = lambda zs: "".join(blk(x, y, z) for _, x, y, z in sorted((x + y + z, x, y, z) for z in zs for x in range(3) for y in range(3)))
    view = (cx - 40, base - 40, 80)
    c0 = cen(0, 0, 0)
    panels = [
        ("One block", "Start with one cube, edge 12, as on the cube sheet: a hexagon of radius 12 split into three faces. Its centre is (64, 66).", (cx - 20, base - 20, 40),
         lambda u: cube(cx, base, s, *tone(pal["gold"])) + _cube_guides(cx, base, s, u, spokes=True, labels=False)),
        ("Leave a gap", "Draw the block at 0.88 of its size, an edge of 10.56. The thin ring of tile colour left around it becomes the gap between neighbours.", (cx - 20, base - 20, 40),
         lambda u: cube(cx, base, s * 0.88, *tone(pal["gold"])) + _gp(_poly_d(_hex_pts(cx, base, s)), 1.4, dash="4 3")),
        ("Three steps", "Moving one block along x goes 10.39 right and 6 down. Along y it goes 10.39 left and 6 down. Along z it goes 12 up. Draw these three steps from block (0, 0, 0).", (cx - 30, base - 30, 60),
         lambda u: blk(0, 0, 0) + blk(1, 0, 0) + blk(0, 1, 0) + blk(0, 0, 1) + "".join(_gl(c0[0], c0[1], c0[0] + dx, c0[1] + dy, None, 2.4, "#0b7a3b") + _gt(c0[0] + dx * 1.35, c0[1] + dy * 1.35 + 2 * u, n, u, "middle", 14, "#0b7a3b") for dx, dy, n in ((K * s, 0.5 * s, "+x"), (-K * s, 0.5 * s, "+y"), (0, -s, "+z")))),
        ("One layer", "Nine blocks, x and y from 0 to 2, at z = 0. Draw them back to front: the ones with the smallest x + y first.", (cx - 40, base - 16, 80),
         lambda u: stack([0])),
        ("Two layers", "Add a second layer at z = 1: the same nine positions, 12 higher.", (cx - 40, base - 26, 80),
         lambda u: stack([0, 1])),
        ("Three layers", "A third layer at z = 2 completes the 3 x 3 x 3 cube of 27 blocks. Draw order overall: sort by x + y + z, lowest first, so nearer blocks cover farther ones.", (cx - 40, base - 42, 80),
         lambda u: stack([0, 1, 2]) + _gp(_poly_d(_hex_pts(cx, base, 3 * s)), 1.4, dash="4 4")),
        ("Put it on the tile", "The whole cube is a hexagon of radius 36 (3 blocks), centred on (64, 66), the same space as the single cube on the other two icons.", (0, 0, 128),
         lambda u: _tile(False) + stack([0, 1, 2]) + _gc(64, 66, 36, "4 4", 1.4, "#FFC83D")),
        ("Done", "monorepo.yaml. Light theme on the left, dark theme on the right; only the tile fill changes.", (0, 0, 128),
         lambda u: f'<g transform="translate(2 34) scale(0.47)">{file_icon("monorepo-yaml", False)}</g><g transform="translate(66 34) scale(0.47)">{file_icon("monorepo-yaml", True)}</g>'),
    ]
    return step_sheet("File icon monorepo.yaml: construction", "The block cube, step by step. Pink: construction; green arrows: one step along each axis.", panels)


def specs():
    return {"spec/mark-construction.svg": spec_construction(), "spec/icon-construction.svg": spec_icon(), "spec/lockup-clear-space.svg": spec_lockup(), "spec/cube-construction.svg": spec_cube(), "spec/shell-construction.svg": spec_shell(), "spec/file-icon-tile.svg": spec_file_tile(), "spec/file-icon-yaml.svg": spec_file_yaml(), "spec/file-icon-lock.svg": spec_file_lock(), "spec/file-icon-monorepo.svg": spec_file_monorepo(), "spec/file-icon-tile-steps.svg": steps_tile(), "spec/file-icon-yaml-steps.svg": steps_yaml(), "spec/file-icon-lock-steps.svg": steps_lock(), "spec/file-icon-monorepo-steps.svg": steps_monorepo()}


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
