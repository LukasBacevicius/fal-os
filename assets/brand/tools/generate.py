#!/usr/bin/env python3
"""Generates every fal-os brand asset from the fal mark SVG + the hand-checked LED grid.

    python3 assets/brand/tools/generate.py                  # all outputs, default repo paths
    python3 assets/brand/tools/generate.py --auto 13        # print the auto grid for n=13
    python3 assets/brand/tools/generate.py --preview DIR    # also write review sheets to DIR

Outputs (paths overridable):
  assets/brand/        fal-mark-pixel.svg (flat mono), icon-{lit,dim}-1024.png,
                       falos-mark.ttf / .woff2 (U+E000 pixel mark, U+E001 smooth mark)
  apps/web/public/     favicon-{lit,dim}.svg, favicon-{lit,dim}-32.png
  apps/web/src/brand/  falos-mark.woff2 (copy the UI loads via @font-face)
  apps/desktop/src-tauri/icons/  icon.png (1024 lit), icon-dim.png, icon.icns (16-1024 @1x/@2x)

Needs Pillow + numpy; the font needs fontTools (+ brotli for woff2).
"""
import argparse
import os
import shutil
import subprocess
import sys
import tempfile

import numpy as np
from PIL import Image, ImageDraw, ImageFilter

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from falmark import bbox, load_path, pixel_grid  # noqa: E402

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), '..', '..', '..'))

# LED look.
GAP = 0.09        # inset of each lit pixel, fraction of a cell (per side)
RADIUS = 0.24     # pixel corner radius, fraction of a cell
DIM = 0.28        # idle brightness (app icon)
FAVICON_DIM = 0.62  # idle favicon / UI mark opacity: softer than lit, still prominent
BG_TOP, BG_BOTTOM = (34, 34, 37), (11, 11, 12)


def read_grid(path):
    rows = [l.strip() for l in open(path) if l.strip() and set(l.strip()) <= {'#', '.'}]  # other lines = notes
    g = [[c == '#' for c in r] for r in rows]
    n = len(g)
    assert all(len(r) == n for r in g), 'grid must be square'
    return g


def cells(grid):
    return [(x, y) for y, r in enumerate(grid) for x, v in enumerate(r) if v]


# ---- raster helpers ---------------------------------------------------------------------------

def cell_mask(grid, size, box, gap=GAP, radius=RADIUS, every=False, ss=4):
    """L mask of the grid's cells (every cell when `every`) inside box=(x, y, side), supersampled."""
    n = len(grid)
    bx, by, side = (v * ss for v in box)
    m = Image.new('L', (size * ss, size * ss), 0)
    d = ImageDraw.Draw(m)
    c = side / n
    pts = [(x, y) for y in range(n) for x in range(n)] if every else cells(grid)
    for x, y in pts:
        d.rounded_rectangle([bx + (x + gap) * c, by + (y + gap) * c, bx + (x + 1 - gap) * c, by + (y + 1 - gap) * c],
                            radius=max(0, radius * c), fill=255)
    return m.resize((size, size), Image.LANCZOS)


def squircle_mask(size, x0, y0, side, ss=4):
    """macOS tile: rounded square with ~continuous corners (each corner a superellipse quadrant,
    radius 22.4% of the side, the Big Sur grid)."""
    r = 0.25 * side * ss * 1.25  # superellipse corners need a larger box for the same visual radius
    x0, y0, s = x0 * ss, y0 * ss, side * ss
    pts = []
    for cx, cy, a0 in ((x0 + s - r, y0 + r, -90), (x0 + s - r, y0 + s - r, 0), (x0 + r, y0 + s - r, 90), (x0 + r, y0 + r, 180)):
        for k in range(91):
            t = np.radians(a0 + k)
            ct, st = np.cos(t), np.sin(t)
            pts.append((cx + r * np.sign(ct) * abs(ct) ** 0.4, cy + r * np.sign(st) * abs(st) ** 0.4))
    m = Image.new('L', (size * ss, size * ss), 0)
    ImageDraw.Draw(m).polygon(pts, fill=255)
    return m.resize((size, size), Image.LANCZOS)


def rounded_mask(size, radius_frac, ss=4):
    m = Image.new('L', (size * ss, size * ss), 0)
    ImageDraw.Draw(m).rounded_rectangle([0, 0, size * ss - 1, size * ss - 1], radius=radius_frac * size * ss, fill=255)
    return m.resize((size, size), Image.LANCZOS)


def arr(img):
    return np.asarray(img, dtype=np.float32) / 255.0


def blur(mask, r):
    return arr(mask.filter(ImageFilter.GaussianBlur(r))) if r > 0.3 else arr(mask)


def pad_grid(grid, k=1):
    n = len(grid) + 2 * k
    return [[False] * n for _ in range(k)] + [[False] * k + list(r) + [False] * k for r in grid] + [[False] * n for _ in range(k)]


def tile_icon(grid, size, lit=True, *, margin=100 / 1024, glyph=None, favicon=False):
    """The app icon: dark tile, LED glyph always lit. The LED grid fills the tile: the mark plus one
    ring of unlit LEDs. `favicon`: full-bleed rounded square, no shadow."""
    lit = True  # the mark is always on; "working" is shown in the UI, not by dimming the icon
    grid = pad_grid(grid)
    small = size <= 64
    if glyph is None:
        glyph = 0.94 if small else 0.88
    if favicon:
        tx = ty = 0.0
        tside = float(size)
        tmask = rounded_mask(size, 0.22)
    else:
        tside = size * (1 - 2 * margin)
        tx = ty = size * margin
        tmask = squircle_mask(size, tx, ty, tside)
    ta = arr(tmask)[..., None]

    # Background: vertical gradient, faint top sheen.
    yy = np.linspace(0, 1, size, dtype=np.float32)[:, None, None]
    yy = np.clip((yy * size - ty) / tside, 0, 1)
    top, bot = np.array(BG_TOP, np.float32) / 255, np.array(BG_BOTTOM, np.float32) / 255
    rgb = top * (1 - yy) + bot * yy
    rgb = np.broadcast_to(rgb, (size, size, 3)).copy()

    gside = tside * glyph
    box = (tx + (tside - gside) / 2, ty + (tside - gside) / 2, gside)
    cellpx = gside / len(grid)
    gap = GAP if cellpx >= 6 else 0.05 if cellpx >= 2 else 0.0
    m = cell_mask(grid, size, box, gap=gap)
    ma = arr(m)[..., None]
    if not small:
        off = arr(cell_mask(grid, size, box, gap=gap, every=True))[..., None] * (1 - ma)
        rgb = rgb + (0.03 if lit else 0.02) * off
    if lit:
        # Bloom: tight halo + wide soft glow (additive), slightly cool-white core.
        glow = (0.55 * blur(m, cellpx * 0.35) + 0.38 * blur(m, cellpx * 1.3) + 0.16 * blur(m, cellpx * 3.6))[..., None]
        if small:
            glow = (0.35 * blur(m, cellpx * 0.6))[..., None]
        rgb = rgb + glow * np.array([0.92, 0.95, 1.0], np.float32)
        rgb = rgb * (1 - ma) + np.array([1.0, 1.0, 1.0], np.float32) * ma
    else:
        rgb = rgb * (1 - ma) + DIM * ma
    rgb = np.clip(rgb, 0, 1) * ta  # inside the tile only

    out = np.zeros((size, size, 4), np.float32)
    out[..., :3] = rgb
    out[..., 3:] = ta
    img = Image.fromarray((out * 255 + 0.5).astype(np.uint8))
    # premultiplied -> straight alpha
    img = Image.fromarray(unpremultiply(np.asarray(img)))
    if not favicon and size >= 64:
        shadow = Image.new('RGBA', (size, size), (0, 0, 0, 0))
        sm = tmask.filter(ImageFilter.GaussianBlur(size * 0.018))
        sm = Image.fromarray((arr(sm) * 0.45 * 255).astype(np.uint8))
        shadow.paste((0, 0, 0, 255), (0, int(size * 0.012)), sm)
        img = Image.alpha_composite(shadow, img)
    return img


def unpremultiply(a):
    a = a.astype(np.float32)
    al = a[..., 3:4] / 255
    rgb = np.where(al > 0, a[..., :3] / np.maximum(al, 1e-6), 0)
    return np.concatenate([np.clip(rgb, 0, 255), a[..., 3:4]], -1).astype(np.uint8)


# ---- vector outputs ---------------------------------------------------------------------------

def pixel_svg(grid, color='currentColor', gap=GAP, radius=RADIUS):
    n = len(grid)
    s = 1 - 2 * gap
    rects = ''.join(f'<rect x="{x + gap:.3g}" y="{y + gap:.3g}" width="{s:.3g}" height="{s:.3g}" rx="{radius:.3g}"/>'
                    for x, y in cells(grid))
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {n} {n}" fill="{color}">{rects}</svg>\n')


def favicon_svg(grid, lit):
    """Tab icon: just the LED mark on a transparent background (like fal.ai's own favicon).
    White on dark tab bars, near-black on light ones. Lit = full + glow, idle = clearly visible
    but softer (FAVICON_DIM), never washed out."""
    n = len(grid)
    pad = 0.4
    v = n + 2 * pad
    g = 0.07
    s = 1 - 2 * g
    rects = ''.join(f'<rect x="{x + pad + g:.3g}" y="{y + pad + g:.3g}" width="{s:.3g}" height="{s:.3g}" rx=".2"/>'
                    for x, y in cells(grid))
    style = ('<style>.m{fill:#fff}.g{fill:#fff}'
             '@media (prefers-color-scheme: light){.m{fill:#111}.g{display:none}}</style>')
    glow = ('<filter id="b" x="-20%" y="-20%" width="140%" height="140%"><feGaussianBlur stdDeviation=".5"/></filter>'
            f'<g class="g" opacity=".7" filter="url(#b)">{rects}</g>') if lit else ''
    lit = True
    glow = glow or ('<filter id="b" x="-20%" y="-20%" width="140%" height="140%"><feGaussianBlur stdDeviation=".5"/></filter>'
            f'<g class="g" opacity=".7" filter="url(#b)">{rects}</g>')
    op = ''
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {v:.3g} {v:.3g}">{style}'
            f'{glow}<g class="m"{op}>{rects}</g></svg>\n')


def favicon_png(grid, size, lit):
    """PNG fallback of favicon_svg (transparent, white mark; idle at FAVICON_DIM alpha)."""
    n = len(grid)
    scale = 8
    big = size * scale
    pad = 0.4
    cell = big / (n + 2 * pad)
    im = Image.new('RGBA', (big, big), (0, 0, 0, 0))
    d = ImageDraw.Draw(im)
    a = 255  # always lit
    for x, y in cells(grid):
        x0 = (x + pad + 0.07) * cell
        y0 = (y + pad + 0.07) * cell
        d.rounded_rectangle([x0, y0, x0 + cell * 0.86, y0 + cell * 0.86], radius=cell * 0.2, fill=(255, 255, 255, a))
    return im.resize((size, size), Image.LANCZOS)


def build_font(grid, subpaths, ttf_path, woff2_path):
    from fontTools.fontBuilder import FontBuilder
    from fontTools.pens.ttGlyphPen import TTGlyphPen

    upm, asc, desc = 1000, 800, -200
    # The glyph: an 860-unit square centred on ~the lowercase middle so it sits with mono text.
    side, cy, adv = 860, 330, 1000
    x0, y0 = (adv - side) / 2, cy - side / 2

    def rect(pen, ax, ay, bx, by):  # clockwise in y-up = TrueType outer contour
        pen.moveTo((ax, ay)); pen.lineTo((ax, by)); pen.lineTo((bx, by)); pen.lineTo((bx, ay)); pen.closePath()

    n = len(grid)
    c = side / n
    g = 0.06
    pen = TTGlyphPen(None)
    for x, y in cells(grid):
        ax, bx = x0 + (x + g) * c, x0 + (x + 1 - g) * c
        top = y0 + side - y * c  # grid rows go down
        rect(pen, round(ax), round(top - (1 - g) * c), round(bx), round(top - g * c))
    pixel = pen.glyph()

    # Smooth mark: flattened path, outer clockwise, hole counter-clockwise (nonzero fill).
    bx0, by0, bx1, by1 = bbox(subpaths)
    k = side / max(bx1 - bx0, by1 - by0)
    pen = TTGlyphPen(None)
    for i, sp in enumerate(subpaths):
        pts = [(round(x0 + (px - bx0) * k), round(y0 + side - (py - by0) * k)) for px, py in sp]
        area = sum(a[0] * b[1] - b[0] * a[1] for a, b in zip(pts, pts[1:] + pts[:1]))
        want_cw = i == 0
        if (area < 0) != want_cw:
            pts.reverse()
        dedup = [p for j, p in enumerate(pts) if p != pts[j - 1]]
        pen.moveTo(dedup[0])
        for p in dedup[1:]:
            pen.lineTo(p)
        pen.closePath()
    smooth = pen.glyph()

    empty = TTGlyphPen(None).glyph()
    fb = FontBuilder(upm, isTTF=True)
    order = ['.notdef', 'space', 'falmark', 'falmark.smooth']
    fb.setupGlyphOrder(order)
    fb.setupCharacterMap({0x20: 'space', 0xE000: 'falmark', 0xE001: 'falmark.smooth'})
    fb.setupGlyf({'.notdef': empty, 'space': empty, 'falmark': pixel, 'falmark.smooth': smooth})
    fb.setupHorizontalMetrics({gn: (adv, 0) for gn in order})
    fb.setupHorizontalHeader(ascent=asc, descent=desc)
    fb.setupNameTable({'familyName': 'falos-mark', 'styleName': 'Regular', 'uniqueFontIdentifier': 'falos-mark-1',
                       'fullName': 'falos-mark', 'psName': 'falos-mark', 'version': 'Version 1.000'})
    fb.setupOS2(sTypoAscender=asc, sTypoDescender=desc, usWinAscent=asc, usWinDescent=-desc)
    fb.setupPost()
    fb.setupHead(unitsPerEm=upm)
    fb.save(ttf_path)
    from fontTools.ttLib import TTFont
    f = TTFont(ttf_path)
    f.flavor = 'woff2'
    f.save(woff2_path)


def write_icns(grid, path):
    with tempfile.TemporaryDirectory() as tmp:
        iconset = os.path.join(tmp, 'icon.iconset')
        os.mkdir(iconset)
        for s in (16, 32, 128, 256, 512):
            tile_icon(grid, s, True).save(os.path.join(iconset, f'icon_{s}x{s}.png'))
            tile_icon(grid, s * 2, True).save(os.path.join(iconset, f'icon_{s}x{s}@2x.png'))
        if shutil.which('iconutil'):
            subprocess.run(['iconutil', '-c', 'icns', iconset, '-o', path], check=True)
        else:
            Image.open(os.path.join(iconset, 'icon_512x512@2x.png')).save(path, sizes=[(s, s) for s in (16, 32, 64, 128, 256, 512, 1024)])


def preview(grid, out_dir, brand):
    os.makedirs(out_dir, exist_ok=True)
    bg = (24, 24, 24, 255)
    for lit in (True, False):
        tag = 'lit' if lit else 'dim'
        big = tile_icon(grid, 1024, lit)
        sheet = Image.new('RGBA', (1024 + 300, 1024), bg)
        sheet.alpha_composite(big)
        x = 1044
        for s in (16, 32, 64, 128):
            sheet.alpha_composite(tile_icon(grid, s, lit), (x, 40))
            sheet.alpha_composite(tile_icon(grid, s, lit, favicon=True), (x, 240))
            x += s + 12
        # 32px icon and favicon at 8x nearest so the actual pixels can be judged
        sheet.alpha_composite(tile_icon(grid, 32, lit).resize((256, 256), Image.NEAREST), (1044, 420))
        sheet.alpha_composite(tile_icon(grid, 32, lit, favicon=True).resize((256, 256), Image.NEAREST), (1044, 700))
        sheet.save(os.path.join(out_dir, f'brand-icon-{tag}.png'))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--svg', default=os.path.join(ROOT, 'assets/brand/fal-mark.svg'))
    ap.add_argument('--grid', default=os.path.join(ROOT, 'assets/brand/fal-mark.grid'))
    ap.add_argument('--brand', default=os.path.join(ROOT, 'assets/brand'))
    ap.add_argument('--web-public', default=os.path.join(ROOT, 'apps/web/public'))
    ap.add_argument('--web-src', default=os.path.join(ROOT, 'apps/web/src/brand'))
    ap.add_argument('--icons', default=os.path.join(ROOT, 'apps/desktop/src-tauri/icons'))
    ap.add_argument('--auto', type=int, help='print the auto-rasterized grid for this size and exit')
    ap.add_argument('--threshold', type=float, default=0.5)
    ap.add_argument('--preview', help='also write review sheets (brand-*.png) here')
    a = ap.parse_args()

    subpaths = load_path(a.svg)
    if a.auto:
        for row in pixel_grid(subpaths, a.auto, a.threshold):
            print(''.join('#' if v else '.' for v in row))
        return
    grid = read_grid(a.grid)
    auto = pixel_grid(subpaths, len(grid), a.threshold)
    if auto != grid:
        print(f'note: {a.grid} is hand-tuned (differs from --auto {len(grid)})')

    for d in (a.brand, a.web_public, a.web_src, a.icons):
        os.makedirs(d, exist_ok=True)
    open(os.path.join(a.brand, 'fal-mark-pixel.svg'), 'w').write(pixel_svg(grid))
    for lit in (True, False):
        tag = 'lit' if lit else 'dim'
        tile_icon(grid, 1024, lit).save(os.path.join(a.brand, f'icon-{tag}-1024.png'), optimize=True)
        open(os.path.join(a.web_public, f'favicon-{tag}.svg'), 'w').write(favicon_svg(grid, lit))
        favicon_png(grid, 32, lit).save(os.path.join(a.web_public, f'favicon-{tag}-32.png'), optimize=True)
    # Dock icons (runtime swap) + bundle icon. 512 keeps the binary small; the Dock tile tops out near it.
    tile_icon(grid, 1024, True).save(os.path.join(a.icons, 'icon.png'), optimize=True)
    tile_icon(grid, 512, True).save(os.path.join(a.icons, 'dock-lit.png'), optimize=True)
    tile_icon(grid, 512, False).save(os.path.join(a.icons, 'dock-dim.png'), optimize=True)
    write_icns(grid, os.path.join(a.icons, 'icon.icns'))
    ttf, woff2 = os.path.join(a.brand, 'falos-mark.ttf'), os.path.join(a.brand, 'falos-mark.woff2')
    build_font(grid, subpaths, ttf, woff2)
    shutil.copyfile(woff2, os.path.join(a.web_src, 'falos-mark.woff2'))
    if a.preview:
        preview(grid, a.preview, a.brand)
    print('ok')


if __name__ == '__main__':
    main()
