# Islet brand assets

The logo, icons, file icons and social images for Islet. Design record and rules: [`docs/brand.md`](../docs/brand.md).

`build/build.py` generates every file below, so the assets can be rebuilt after a change. The SVG files are the source; the PNG, ICO and ICNS files are exports of them. The wordmark in every logo SVG is outlined to paths, so no font is needed to display them.

## Rebuild

Needs Python 3.8+ and, for the PNG exports, Chrome, Chromium or Edge (found automatically; set `ISLET_CHROME` to override). No other tools or packages.

```
python brand/build/build.py              # everything
python brand/build/build.py --svg-only   # SVGs and the sprite only, no browser
```

PowerShell: `python brand\build\build.py`. The SVGs rebuild byte for byte; PNGs can differ by a few bytes between browser versions.

The outlined letters come from `build/wordmark.json`. To change the wordmark, a file-icon label or the descriptor line, edit `build/outline_fonts.py`, run it (needs `pip install fonttools uharfbuzz`; Outfit is in `fonts/`), then rebuild.

## Sprite

`icon/sprite.svg` holds the logos, the icons and the file icons as `<symbol>` elements, so a page loads one file:

```html
<svg width="32" height="32"><use href="sprite.svg#islet-icon"/></svg>
```

Symbol ids are the file names without the extension: `islet-mark`, `islet-horizontal-dark`, `islet-icon`, `islet-toml`, `islet-lock-dark`, and so on.

## Layout

| folder | contents |
|---|---|
| `logo/` | `islet-mark`, `islet-horizontal`, `islet-stacked`, each as colour (`.svg`, for light backgrounds), `-dark` (for dark backgrounds), `-black` and `-white` (one colour). PNGs on a transparent background: mark 1024, horizontal 2000 wide, stacked 1200 wide. |
| `icon/` | App icon `islet-icon.svg` (dark tile) and `islet-icon-light.svg` (sand tile); PNG ladder 16 to 1024; `sprite.svg`; `favicon.svg`, `favicon.ico` (16, 32, 48), `islet.ico` (16 to 256), `islet.icns` (16 to 1024); `apple-touch-icon-180.png`, `android-chrome-192.png`, `android-chrome-512.png`, `maskable-512.png`, `mstile-150.png`; round avatars `avatar-512.png` and `avatar-light-512.png`. |
| `editor/` | `vscode-icon-256.png` and `file-icons/`: icons for `islet.toml`, `islet.lock` and `monorepo.toml`, each with a `-dark` variant for dark themes, as SVG and 128px PNG. |
| `spec/` | Spec sheets: mark construction on the 128 grid, how to draw the cube and the trio step by step (`cube-construction`), the hexagon shell step by step (`shell-construction`), the file icons, one overview sheet and one step-by-step sheet each (`file-icon-tile`, `file-icon-toml`, `file-icon-lock`, `file-icon-monorepo` and the `-steps` versions), app icon corner radius and safe areas, lockup clear space and minimum sizes. Generated, with Outfit embedded. |
| `fonts/` | Outfit (variable font) and its SIL OFL licence. Embedded in the spec sheets; the logo outlines do not need it. |
| `previews/` | Exploration boards from the design process (round 9 to the palette). Not generated. |
| `social/` | `github-social-1280x640.png`, `og-1200x630.png`, `x-header-1500x500.png`, `linkedin-1584x396.png`, `readme-banner-1280x320.png`. |

## Quick rules

- Name: lowercase `islet` in the logo, `Islet` in prose, `islet` for the command.
- Colours: Shell Teal `#0F9AA6`, Gold `#FFC83D`, Sunburn `#FF6F3C`, Green `#12A150`, Ink `#0E3440`, Sand `#FFF1C9`, Deep `#08262D`. Use Ink, not teal, for text on white.
- Clear space: x = 28 units of the 128-unit mark on every side. Minimum size: mark 24px (use the icon tile below that), horizontal lockup 140px wide. `spec/lockup-clear-space.png` draws both.
- Do not stretch, rotate, recolour, outline, add effects to, or put the logo on a low-contrast background.
- Font: wordmark and text in Outfit Bold and Regular (SIL Open Font License), the same text font Pelago uses.
- Editor extensions use the icons only. They never change the user's theme or workbench colours.
- No tagline is set yet. The social images carry the descriptor "The package manager for Pelago."; the README banner has no text beyond the name.
