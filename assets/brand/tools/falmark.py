"""The fal mark as geometry: SVG path -> polygons -> coarse pixel grid.

Tiny, dependency-light (Pillow only) on purpose: the mark is one even-odd path of M/C/H/V/L/Z
commands, so we flatten it ourselves instead of pulling in an SVG renderer.
"""
import re
import xml.etree.ElementTree as ET

from PIL import Image, ImageChops, ImageDraw

_TOKEN = re.compile(r'[MmCcLlHhVvZz]|-?(?:\d+\.?\d*|\.\d+)(?:[eE][-+]?\d+)?')


def load_path(svg_path):
    """Subpaths (lists of (x, y) points, cubic curves flattened) of the SVG's first <path>."""
    root = ET.parse(svg_path).getroot()
    d = next(el.get('d') for el in root.iter() if el.tag.endswith('path'))
    toks = _TOKEN.findall(d)
    subpaths, cur, cmd, i = [], [], None, 0
    x = y = 0.0

    def num():
        nonlocal i
        v = float(toks[i])
        i += 1
        return v

    while i < len(toks):
        if toks[i].isalpha():
            cmd = toks[i]
            i += 1
            if cmd in 'Zz':
                if cur:
                    subpaths.append(cur)
                cur = []
                continue
        rel = cmd.islower()
        c = cmd.upper()
        ox, oy = (x, y) if rel else (0.0, 0.0)
        if c == 'M':
            if cur:
                subpaths.append(cur)
            x, y = ox + num(), oy + num()
            cur = [(x, y)]
            cmd = 'l' if rel else 'L'
        elif c == 'L':
            x, y = ox + num(), oy + num()
            cur.append((x, y))
        elif c == 'H':
            x = ox + num()
            cur.append((x, y))
        elif c == 'V':
            y = oy + num()
            cur.append((x, y))
        elif c == 'C':
            p0 = (x, y)
            p1 = (ox + num(), oy + num())
            p2 = (ox + num(), oy + num())
            p3 = (ox + num(), oy + num())
            for k in range(1, 25):
                t = k / 24
                mt = 1 - t
                cur.append((
                    mt ** 3 * p0[0] + 3 * mt * mt * t * p1[0] + 3 * mt * t * t * p2[0] + t ** 3 * p3[0],
                    mt ** 3 * p0[1] + 3 * mt * mt * t * p1[1] + 3 * mt * t * t * p2[1] + t ** 3 * p3[1],
                ))
            x, y = p3
        else:
            raise ValueError(f'unsupported path command {cmd}')
    if cur:
        subpaths.append(cur)
    return subpaths


def bbox(subpaths):
    xs = [p[0] for s in subpaths for p in s]
    ys = [p[1] for s in subpaths for p in s]
    return min(xs), min(ys), max(xs), max(ys)


def render_mask(subpaths, size, pad=0.0):
    """Even-odd mask (L, 0/255) of the mark centred in a size x size square with `pad` margin
    (fraction of size) on each side."""
    x0, y0, x1, y1 = bbox(subpaths)
    side = max(x1 - x0, y1 - y0)
    inner = size * (1 - 2 * pad)
    s = inner / side
    offx = size * pad + (inner - (x1 - x0) * s) / 2
    offy = size * pad + (inner - (y1 - y0) * s) / 2
    mask = Image.new('L', (size, size), 0)
    for sp in subpaths:
        m = Image.new('L', (size, size), 0)
        ImageDraw.Draw(m).polygon([((px - x0) * s + offx, (py - y0) * s + offy) for px, py in sp], fill=255)
        mask = ImageChops.logical_xor(mask.convert('1'), m.convert('1')).convert('L')
    return mask


def coverage_grid(subpaths, n, oversample=48):
    """n x n list of per-cell coverage in [0, 1] (mark scaled to fill the grid)."""
    size = n * oversample
    m = render_mask(subpaths, size)
    small = m.resize((n, n), Image.BOX)
    return [[small.getpixel((x, y)) / 255 for x in range(n)] for y in range(n)]


def pixel_grid(subpaths, n, threshold=0.5):
    """n x n booleans, forced 4-fold symmetric (the mark is): a cell is lit when the mean
    coverage of it and its mirror images reaches `threshold`."""
    g = coverage_grid(subpaths, n)
    out = []
    for y in range(n):
        row = []
        for x in range(n):
            vals = [g[y][x], g[y][n - 1 - x], g[n - 1 - y][x], g[n - 1 - y][n - 1 - x],
                    g[x][y], g[n - 1 - x][y], g[x][n - 1 - y], g[n - 1 - x][n - 1 - y]]
            row.append(sum(vals) / len(vals) >= threshold)
        out.append(row)
    return out
