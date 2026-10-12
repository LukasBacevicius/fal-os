// Small readers for the text formats the previews show: .cube LUTs, SRT / WebVTT cues,
// OpenTimelineIO, plus a test chart to try a LUT on.

// ---- LUTs ----------------------------------------------------------------------------------

export interface Lut {
  title: string;
  /** 3D: N per axis (red fastest); 1D LUTs are expanded to 3D. */
  size: number;
  data: Float32Array;
  min: [number, number, number];
  max: [number, number, number];
}

/** Parses an Adobe / Resolve `.cube` (3D, or 1D expanded to 3D). */
export function parseCube(text: string): Lut {
  let title = '';
  let size3 = 0;
  let size1 = 0;
  let min: [number, number, number] = [0, 0, 0];
  let max: [number, number, number] = [1, 1, 1];
  const vals: number[] = [];
  for (const raw of text.split(/\r?\n/)) {
    const l = raw.trim();
    if (!l || l.startsWith('#')) continue;
    const kw = /^([A-Z_0-9]+)\s+(.*)$/.exec(l);
    if (kw && /[A-Z]/.test(kw[1][0])) {
      const [, k, v] = kw;
      if (k === 'TITLE') title = v.replace(/^"|"$/g, '');
      else if (k === 'LUT_3D_SIZE') size3 = Number(v);
      else if (k === 'LUT_1D_SIZE') size1 = Number(v);
      else if (k === 'DOMAIN_MIN') min = v.split(/\s+/).map(Number) as [number, number, number];
      else if (k === 'DOMAIN_MAX') max = v.split(/\s+/).map(Number) as [number, number, number];
      continue;
    }
    const p = l.split(/\s+/);
    if (p.length >= 3) vals.push(Number(p[0]), Number(p[1]), Number(p[2]));
  }
  if (size3 > 1 && vals.length >= size3 ** 3 * 3) return { title, size: size3, data: Float32Array.from(vals.slice(0, size3 ** 3 * 3)), min, max };
  if (size1 > 1 && vals.length >= size1 * 3) {
    // 1D: per-channel curves; sampled into a 17³ cube.
    const n = 17;
    const data = new Float32Array(n ** 3 * 3);
    const curve = (c: number, x: number) => {
      const f = Math.min(size1 - 1, Math.max(0, x * (size1 - 1)));
      const i = Math.floor(f);
      const j = Math.min(size1 - 1, i + 1);
      return vals[i * 3 + c] * (1 - (f - i)) + vals[j * 3 + c] * (f - i);
    };
    for (let b = 0; b < n; b++) for (let g = 0; g < n; g++) for (let r = 0; r < n; r++) {
      const o = (r + g * n + b * n * n) * 3;
      data[o] = curve(0, r / (n - 1));
      data[o + 1] = curve(1, g / (n - 1));
      data[o + 2] = curve(2, b / (n - 1));
    }
    return { title, size: n, data, min, max };
  }
  throw new Error('not a .cube LUT');
}

/** Applies a LUT to RGBA pixels in place (trilinear). */
export function applyLut(lut: Lut, px: Uint8ClampedArray) {
  const { size: n, data, min, max } = lut;
  const s = n - 1;
  const at = (r: number, g: number, b: number, c: number) => data[(r + g * n + b * n * n) * 3 + c];
  for (let i = 0; i < px.length; i += 4) {
    const v = [0, 1, 2].map((c) => Math.min(1, Math.max(0, (px[i + c] / 255 - min[c]) / (max[c] - min[c] || 1))) * s);
    const r0 = Math.floor(v[0]), g0 = Math.floor(v[1]), b0 = Math.floor(v[2]);
    const r1 = Math.min(s, r0 + 1), g1 = Math.min(s, g0 + 1), b1 = Math.min(s, b0 + 1);
    const fr = v[0] - r0, fg = v[1] - g0, fb = v[2] - b0;
    for (let c = 0; c < 3; c++) {
      const c00 = at(r0, g0, b0, c) * (1 - fr) + at(r1, g0, b0, c) * fr;
      const c10 = at(r0, g1, b0, c) * (1 - fr) + at(r1, g1, b0, c) * fr;
      const c01 = at(r0, g0, b1, c) * (1 - fr) + at(r1, g0, b1, c) * fr;
      const c11 = at(r0, g1, b1, c) * (1 - fr) + at(r1, g1, b1, c) * fr;
      const out = (c00 * (1 - fg) + c10 * fg) * (1 - fb) + (c01 * (1 - fg) + c11 * fg) * fb;
      px[i + c] = Math.round(out * 255);
    }
  }
}

/** A test chart: a hue × saturation field over a gray ramp and a few reference swatches
 *  (skin, sky, foliage). */
export function drawChart(g: CanvasRenderingContext2D | OffscreenCanvasRenderingContext2D, w: number, h: number) {
  const top = Math.round(h * 0.62);
  for (let x = 0; x < w; x++) {
    const hue = (x / w) * 360;
    const grad = g.createLinearGradient(0, 0, 0, top);
    grad.addColorStop(0, `hsl(${hue} 100% 50%)`);
    grad.addColorStop(0.55, `hsl(${hue} 60% 55%)`);
    grad.addColorStop(1, `hsl(${hue} 10% 30%)`);
    g.fillStyle = grad;
    g.fillRect(x, 0, 1, top);
  }
  const rampH = Math.round((h - top) * 0.5);
  const steps = 16;
  for (let i = 0; i < steps; i++) {
    const v = Math.round((i / (steps - 1)) * 255);
    g.fillStyle = `rgb(${v},${v},${v})`;
    g.fillRect(Math.floor((i * w) / steps), top, Math.ceil(w / steps), rampH);
  }
  const sw = ['#c58c6e', '#e0b39a', '#8d5a3b', '#6f95c8', '#4f7a3a', '#d8c35a', '#a33a2f', '#2e2e2e'];
  sw.forEach((c, i) => {
    g.fillStyle = c;
    g.fillRect(Math.floor((i * w) / sw.length), top + rampH, Math.ceil(w / sw.length), h - top - rampH);
  });
}

// ---- subtitles -----------------------------------------------------------------------------

export interface Cue { start: number; end: number; text: string }

const TS = /((?:\d+:)?\d{1,2}:\d{2}(?:[.,]\d{1,3})?)\s*-->\s*((?:\d+:)?\d{1,2}:\d{2}(?:[.,]\d{1,3})?)/;

function secs(t: string): number {
  const p = t.replace(',', '.').split(':').map(Number);
  return p.reduce((a, v) => a * 60 + v, 0);
}

/** SRT or WebVTT cues (styling tags stripped). */
export function parseSubs(text: string): Cue[] {
  const out: Cue[] = [];
  for (const block of text.replace(/\r/g, '').split(/\n{2,}/)) {
    const lines = block.split('\n');
    const i = lines.findIndex((l) => TS.test(l));
    if (i < 0) continue;
    const m = TS.exec(lines[i])!;
    const body = lines.slice(i + 1).join('\n').replace(/<[^>]+>/g, '').replace(/\{\\[^}]*\}/g, '').trim();
    out.push({ start: secs(m[1]), end: secs(m[2]), text: body });
  }
  return out;
}

/** `83.5` → `01:23.500` (hours when needed). */
export function cueTime(t: number): string {
  const h = Math.floor(t / 3600), m = Math.floor(t / 60) % 60, s = t % 60;
  const ss = s.toFixed(3).padStart(6, '0');
  return h ? `${h}:${String(m).padStart(2, '0')}:${ss}` : `${String(m).padStart(2, '0')}:${ss}`;
}

// ---- OpenTimelineIO ------------------------------------------------------------------------

export interface OtioTrack { name: string; kind: string; items: { name: string; kind: string; dur: number }[] }

const rt = (v: any) => (v && typeof v.value === 'number' && v.rate ? v.value / v.rate : 0);

/** Tracks and their clips / gaps of an OTIO timeline (null if it isn't one). */
export function parseOtio(json: any): { name: string; rate: number; tracks: OtioTrack[] } | null {
  if (!json || typeof json !== 'object' || !String(json.OTIO_SCHEMA ?? '').startsWith('Timeline')) return null;
  const stack = json.tracks;
  const tracks: OtioTrack[] = (stack?.children ?? []).map((t: any) => ({
    name: t.name || '',
    kind: t.kind || 'Video',
    items: (t.children ?? []).map((c: any) => ({
      name: c.name || '',
      kind: String(c.OTIO_SCHEMA ?? '').split('.')[0],
      dur: rt(c.source_range?.duration),
    })),
  }));
  const first = (stack?.children ?? []).flatMap((t: any) => t.children ?? []).find((c: any) => c?.source_range?.duration?.rate);
  return { name: json.name || '', rate: first?.source_range?.duration?.rate ?? 0, tracks };
}
