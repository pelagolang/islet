# Islet brand

Decided by Dennis on 2026-11-08, chosen by feel over about 25 boards of options. Assets and rules: [`brand/`](../brand/README.md).

## The mark

Three isometric cubes in a thick rounded hexagon. The cubes read as packages and the hexagon as the store that holds them, but no colour or part carries a fixed meaning. The mark is crisp and geometric on purpose: Dennis rejected pictures of islands, suns and any organic shape.

- Wordmark: lowercase `islet`, Outfit Bold 700, outlined to paths in every logo file.
- Geometry and construction: [`brand/spec/mark-construction.png`](../brand/spec/mark-construction.png).
- Locked geometry: shell radius 52, stroke 11, corner radius 14; cubes of edge 16; only the outer outline of the trio is rounded.

## Colour

| name | light | dark | use |
|---|---|---|---|
| Shell Teal | `#0F9AA6` | `#4FD6D0` | the hexagon shell, links, accents |
| Gold | `#FFC83D` | `#FFD15C` | top cube |
| Sunburn | `#FF6F3C` | `#FF8556` | bottom left cube |
| Green | `#12A150` | `#3DD68C` | bottom right cube |
| Ink | `#0E3440` | | text on light |
| Sand | `#FFF1C9` | | text on dark, light app tile |
| Deep | `#08262D` | | dark background and dark app tile |

Cube faces are the colour lightened 25 % (top), the colour (left) and the colour darkened 22 % (right). Palette chosen from the hybrid of two shortlisted sets (bright cubes, teal shell); see `brand/previews/21-palettes.png` and `23-p2-p6-hybrid.png`.

## Rules

- Do not stretch, rotate, recolour, outline or add effects to the logo, and keep it off low-contrast backgrounds.
- Clear space x = 28 units of the 128-unit mark. Minimum: mark 24px, horizontal lockup 140px wide. Below 24px use the app icon tile.
- Never use Pelago's marks (the monoline P, the dot and the wave), and never reuse the exact Pelago teal values.
- Editor extensions and tools use the icons only. They never change the user's theme.

## What was tried and dropped

Written down so nobody proposes them again.

- Anything that looked like Pelago: teal monoline marks, waves. Rejected in round 1.
- Island and sun pictures, striped sunsets (the look of generated music-app logos), two overlapping circles (a payment-card logo), and every blob or organic shape.
- A single cube in a hexagon shell: it looks like the webpack logo.
- Rounding every cube (only the outer outline of the trio is rounded).
- Wordmarks in Bungee, pixel, voxel, beach and script faces. Outfit Bold won; heavier weights were tried and dropped.

## Open

- **Tagline.** None is set. The social images carry the descriptor "The package manager for Pelago."; the README banner has none.
- **Look-alike check.** The shape (hexagon plus cubes) has not been checked with a reverse image search. Run the final mark through Google Lens and TinEye before it appears on a website or a release.
- **Colour overlap.** Shell Teal is close to Pelago's teal family. This was accepted; the cubes are what set Islet apart.
- **GitHub social preview.** `brand/social/github-social-1280x640.png` has to be uploaded by hand in the repository settings; the API does not allow it.
