// Renders the draw ops of a Figma import raster (crates/figma `Raster`) into a PNG with
// Canvas 2D: SVG paths (Path2D), solid / gradient / image paints, centerline strokes, drop
// shadows, opacity + clip layers and boolean operations.

export type Mat = [number, number, number, number, number, number];

export type Paint =
  | { type: 'solid'; color: string; blend?: string }
  | { type: 'gradient'; kind: string; stops: [number, string][]; m: Mat; opacity: number; blend?: string }
  | { type: 'image'; hash: string; m: Mat; opacity: number; blend?: string };

export type Op =
  | { op: 'fill'; path: string; evenodd: boolean; m: Mat; paint: Paint }
  | { op: 'stroke'; path: string; m: Mat; width: number; align: string; cap: string; join: string; dash?: number[]; paint: Paint }
  | { op: 'shadow'; path: string; evenodd: boolean; m: Mat; color: string; dx: number; dy: number; blur: number }
  | { op: 'push'; opacity: number; clip?: string; clipM: Mat }
  | { op: 'pop' }
  | { op: 'bool'; mode: string; parts: { path: string; evenodd: boolean; m: Mat }[]; paints: Paint[]; size: [number, number]; m: Mat };

export interface RasterSpec { ops: Op[]; images: string[] }

type Ctx = OffscreenCanvasRenderingContext2D;

/** Pixels per canvas unit: 2× for sharp zoom-ins, capped so the long side stays ≤ 4096 px. */
export function rasterScale(w: number, h: number): number {
  return Math.max(0.05, Math.min(2, 4096 / Math.max(w, h, 1), Math.sqrt(16e6 / Math.max(1, w * h))));
}

// `a ∘ b`: apply b first.
function mul(a: Mat, b: Mat): Mat {
  return [a[0] * b[0] + a[1] * b[3], a[0] * b[1] + a[1] * b[4], a[0] * b[2] + a[1] * b[5] + a[2], a[3] * b[0] + a[4] * b[3], a[3] * b[1] + a[4] * b[4], a[3] * b[2] + a[4] * b[5] + a[5]];
}

// Our matrices are [a c e; b d f] rows; canvas wants (a, b, c, d, e, f).
function setM(g: Ctx, m: Mat) {
  g.setTransform(m[0], m[3], m[1], m[4], m[2], m[5]);
}
function addM(g: Ctx, m: Mat) {
  g.transform(m[0], m[3], m[1], m[4], m[2], m[5]);
}

const BLEND: Record<string, GlobalCompositeOperation> = {
  multiply: 'multiply', screen: 'screen', overlay: 'overlay', darken: 'darken', lighten: 'lighten', 'color-dodge': 'color-dodge',
  'color-burn': 'color-burn', 'hard-light': 'hard-light', 'soft-light': 'soft-light', difference: 'difference', exclusion: 'exclusion',
  hue: 'hue', saturation: 'saturation', color: 'color', luminosity: 'luminosity',
};

function gradient(g: Ctx, p: Extract<Paint, { type: 'gradient' }>): CanvasGradient {
  let gr: CanvasGradient;
  if (p.kind === 'linear') gr = g.createLinearGradient(0, 0.5, 1, 0.5);
  else if (p.kind === 'angular' && 'createConicGradient' in g) gr = g.createConicGradient(0, 0.5, 0.5);
  else gr = g.createRadialGradient(0.5, 0.5, 0, 0.5, 0.5, 0.5);
  for (const [at, c] of p.stops) gr.addColorStop(Math.min(1, Math.max(0, at)), c);
  return gr;
}

/** Fills `path` (in the coordinates `m` maps to device pixels) with `paint`. */
function paintPath(g: Ctx, path: Path2D, rule: CanvasFillRule, m: Mat, paint: Paint, images: Map<string, ImageBitmap>) {
  g.save();
  setM(g, m);
  if (paint.blend && BLEND[paint.blend]) g.globalCompositeOperation = BLEND[paint.blend];
  if (paint.type === 'solid') {
    g.fillStyle = paint.color;
    g.fill(path, rule);
  } else if (paint.type === 'gradient') {
    g.clip(path, rule);
    addM(g, paint.m);
    g.globalAlpha *= paint.opacity;
    g.fillStyle = gradient(g, paint);
    g.fillRect(-50, -50, 101, 101);
  } else {
    const img = images.get(paint.hash);
    if (img) {
      g.clip(path, rule);
      addM(g, paint.m);
      g.globalAlpha *= paint.opacity;
      g.imageSmoothingQuality = 'high';
      g.drawImage(img, 0, 0, 1, 1);
    }
  }
  g.restore();
}

/** A solid color for stroking with a non-solid paint (strokes are rarely gradients). */
function strokeStyle(p: Paint): string {
  if (p.type === 'solid') return p.color;
  if (p.type === 'gradient') return p.stops[0]?.[1] ?? '#000';
  return '#888';
}

function layer(w: number, h: number): Ctx {
  return new OffscreenCanvas(w, h).getContext('2d')!;
}

/** Renders `spec` for a node of `w`×`h` canvas units. `images`: Figma SHA-1 → bitmap. */
export async function renderRaster(spec: RasterSpec, w: number, h: number, images: Map<string, ImageBitmap>): Promise<Blob> {
  const s = rasterScale(w, h);
  const W = Math.max(1, Math.ceil(w * s)), H = Math.max(1, Math.ceil(h * s));
  const base: Mat = [s, 0, 0, 0, s, 0];
  const root = layer(W, H);
  const stack: { g: Ctx; op: Extract<Op, { op: 'push' }> }[] = [];
  let g = root;
  const rule = (evenodd: boolean): CanvasFillRule => (evenodd ? 'evenodd' : 'nonzero');
  for (const op of spec.ops) {
    switch (op.op) {
      case 'fill':
        paintPath(g, new Path2D(op.path), rule(op.evenodd), mul(base, op.m), op.paint, images);
        break;
      case 'stroke': {
        const path = new Path2D(op.path);
        g.save();
        setM(g, mul(base, op.m));
        g.lineCap = op.cap as CanvasLineCap;
        g.lineJoin = op.join as CanvasLineJoin;
        if (op.dash?.length) g.setLineDash(op.dash);
        g.strokeStyle = strokeStyle(op.paint);
        if (op.paint.type !== 'solid') g.globalAlpha *= op.paint.opacity;
        if (op.align === 'inside') {
          g.clip(path);
          g.lineWidth = op.width * 2;
        } else if (op.align === 'outside') {
          const outer = new Path2D();
          outer.rect(-1e5, -1e5, 2e5, 2e5);
          outer.addPath(path);
          g.clip(outer, 'evenodd');
          g.lineWidth = op.width * 2;
        } else {
          g.lineWidth = op.width;
        }
        g.stroke(path);
        g.restore();
        break;
      }
      case 'shadow': {
        // Draw the shape far off-canvas and let its shadow land in place: only the shadow shows.
        const far = W * 2 + 1000;
        g.save();
        setM(g, mul([1, 0, -far, 0, 1, 0], mul(base, op.m)));
        g.shadowColor = op.color;
        g.shadowBlur = op.blur * s;
        g.shadowOffsetX = far + op.dx * s;
        g.shadowOffsetY = op.dy * s;
        g.fillStyle = '#000';
        g.fill(new Path2D(op.path), rule(op.evenodd));
        g.restore();
        break;
      }
      case 'push':
        stack.push({ g, op });
        g = layer(W, H);
        break;
      case 'pop': {
        const top = stack.pop();
        if (!top) break;
        const parent = top.g;
        parent.save();
        if (top.op.clip) {
          setM(parent, mul(base, top.op.clipM));
          parent.clip(new Path2D(top.op.clip));
          parent.setTransform(1, 0, 0, 1, 0, 0);
        }
        parent.globalAlpha = top.op.opacity;
        parent.drawImage(g.canvas, 0, 0);
        parent.restore();
        g = parent;
        break;
      }
      case 'bool': {
        // Mask from the parts (composite ops do the boolean), then the paints through it.
        const mask = layer(W, H);
        const comp: Record<string, GlobalCompositeOperation> = { union: 'source-over', subtract: 'destination-out', intersect: 'destination-in', xor: 'xor' };
        op.parts.forEach((p, i) => {
          setM(mask, mul(base, p.m));
          mask.globalCompositeOperation = i === 0 ? 'source-over' : comp[op.mode] ?? 'source-over';
          mask.fillStyle = '#000';
          mask.fill(new Path2D(p.path), rule(p.evenodd));
        });
        const paints = layer(W, H);
        const rect = new Path2D();
        rect.rect(0, 0, op.size[0], op.size[1]);
        for (const p of op.paints) paintPath(paints, rect, 'nonzero', mul(base, op.m), p, images);
        mask.setTransform(1, 0, 0, 1, 0, 0);
        mask.globalCompositeOperation = 'source-in';
        mask.drawImage(paints.canvas, 0, 0);
        g.drawImage(mask.canvas, 0, 0);
        break;
      }
    }
  }
  while (stack.length) {
    // Unbalanced push (shouldn't happen): flatten.
    const top = stack.pop()!;
    top.g.drawImage(g.canvas, 0, 0);
    g = top.g;
  }
  return await (root.canvas as OffscreenCanvas).convertToBlob({ type: 'image/png' });
}
