// Canvas card art: the engine draws nodes without a raster of their own (3D, PDF, fonts, LUTs,
// subtitles, …) as cards, and asks the page for richer art first (crates/client media.rs
// `cardSource` → `globalThis.falosCardArt(spec)`). Each card keeps the plain look: the
// `[kind] name` label row in mono, then the content: a 3D poster, the PDF's first page, a
// font specimen, a LUT's before / after, the first subtitle cues.

import { previewKind, hostPath } from './preview';
import { applyLut, cueTime, drawChart, parseCube, parseSubs } from './formats';
import { loadFont, loadModelViewer } from './components/Preview';

interface Spec {
  kind: string;
  title?: string;
  mime?: string;
  url?: string;
  asset?: string;
  aspect?: number;
  w?: number;
  theme?: string;
}

const MONO = 'ui-monospace, "SF Mono", Menlo, monospace';
const THEMES = {
  dark: { panel: '#0f0f0f', text: '#e6e6e6', dim: '#8a8a8a', faint: '#4a4a4a' },
  light: { panel: '#f7f7f7', text: '#0a0a0a', dim: '#6e6e6e', faint: '#b5b5b5' },
};

type Ctx = OffscreenCanvasRenderingContext2D | CanvasRenderingContext2D;

function card(spec: Spec, tag: string) {
  const W = 768;
  const H = Math.max(64, Math.min(2048, Math.round(W / Math.max(0.2, spec.aspect || 1.5))));
  const c = typeof OffscreenCanvas !== 'undefined' ? new OffscreenCanvas(W, H) : Object.assign(document.createElement('canvas'), { width: W, height: H });
  const g = c.getContext('2d') as Ctx;
  const th = spec.theme === 'light' ? THEMES.light : THEMES.dark;
  const k = W / Math.max(40, Number(spec.w) || 320);
  const pad = 10 * k, fs = 12 * k;
  g.fillStyle = th.panel;
  g.fillRect(0, 0, W, H);
  g.textBaseline = 'alphabetic';
  g.font = `400 ${fs}px ${MONO}`;
  const label = `[${tag}] `;
  g.fillStyle = th.dim;
  g.fillText(label, pad, pad + fs);
  const lw = g.measureText(label).width;
  g.fillStyle = th.text;
  g.fillText(clip(g, spec.title || '', W - pad * 2 - lw), pad + lw, pad + fs);
  // Content box under the label row.
  const box = { x: pad, y: pad + fs * 1.8, w: W - pad * 2, h: Math.max(1, H - pad * 2.8 - fs * 1.8 + pad) };
  return { c, g, W, H, k, pad, fs, th, box };
}

function clip(g: Ctx, text: string, maxW: number): string {
  let t = String(text);
  if (g.measureText(t).width <= maxW) return t;
  while (t.length > 1 && g.measureText(t + '…').width > maxW) t = t.slice(0, -1);
  return t + '…';
}

function contain(g: Ctx, img: CanvasImageSource & { width: number; height: number }, b: { x: number; y: number; w: number; h: number }) {
  const s = Math.min(b.w / img.width, b.h / img.height);
  const w = img.width * s, h = img.height * s;
  g.drawImage(img, b.x + (b.w - w) / 2, b.y + (b.h - h) / 2, w, h);
}

async function done(c: OffscreenCanvas | HTMLCanvasElement, W: number, H: number) {
  return { bitmap: await createImageBitmap(c), width: W, height: H };
}

const hexOf = (spec: Spec) => (spec.asset ?? '').replace(/^sha256:/, '');

// ---- 3D posters ----------------------------------------------------------------------------

const posters = new Map<string, Promise<Blob | null>>();
let queue: Promise<unknown> = Promise.resolve();
const CACHE = 'falos-posters-v1';

/** A still of a glTF / GLB model rendered by <model-viewer> (cached by asset id, also across
 *  reloads in the Cache API). One render at a time. */
export function modelPoster(url: string, id: string): Promise<Blob | null> {
  let p = posters.get(id);
  if (p) return p;
  p = (async () => {
    const key = `https://falos.local/poster/${encodeURIComponent(id)}`;
    try {
      const hit = await (await caches.open(CACHE)).match(key);
      if (hit) return await hit.blob();
    } catch {
      /* no Cache API (insecure context) */
    }
    const run = queue.then(() => renderModel(url));
    queue = run.catch(() => {});
    const blob = await run;
    if (blob) {
      try {
        await (await caches.open(CACHE)).put(key, new Response(blob, { headers: { 'content-type': blob.type } }));
      } catch {
        /* quota / no Cache API */
      }
    }
    return blob;
  })().catch(() => null);
  posters.set(id, p);
  return p;
}

async function renderModel(url: string): Promise<Blob | null> {
  await loadModelViewer();
  const mv = document.createElement('model-viewer') as HTMLElement & { toBlob(o?: object): Promise<Blob> };
  // On screen but invisible: model-viewer only renders when it intersects the viewport.
  Object.assign(mv.style, { position: 'fixed', left: '0', top: '0', width: '640px', height: '640px', opacity: '0', pointerEvents: 'none', zIndex: '-1' });
  mv.setAttribute('environment-image', 'neutral');
  mv.setAttribute('shadow-intensity', '0');
  mv.setAttribute('interaction-prompt', 'none');
  mv.setAttribute('src', url);
  document.body.appendChild(mv);
  try {
    await new Promise<void>((res, rej) => {
      const t = window.setTimeout(() => rej(new Error('model load timed out')), 30000);
      mv.addEventListener('load', () => (window.clearTimeout(t), res()), { once: true });
      mv.addEventListener('error', () => (window.clearTimeout(t), rej(new Error('model failed to load'))), { once: true });
    });
    await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
    return await mv.toBlob({ mimeType: 'image/png', idealAspect: false });
  } catch {
    return null;
  } finally {
    mv.remove();
  }
}

// ---- kinds ---------------------------------------------------------------------------------

async function imageCard(spec: Spec, tag: string, blob: Blob | null) {
  if (!blob) return null;
  const img = await createImageBitmap(blob);
  const { c, g, W, H, box } = card(spec, tag);
  contain(g, img, box);
  img.close();
  return done(c, W, H);
}

async function pdfCard(spec: Spec) {
  const u = spec.url && hostPath(spec.url, `/thumbs/pdf/${hexOf(spec)}/1?w=1600`);
  if (!u) return null;
  const r = await fetch(u);
  return r.ok ? imageCard(spec, 'pdf', await r.blob()) : null;
}

async function fontCard(spec: Spec) {
  const family = await loadFont(spec.url!);
  const { c, g, W, H, th, box, k } = card(spec, 'font');
  const f = (px: number) => `400 ${px}px "${family}"`;
  const big = Math.min(box.h * 0.62, box.w * 0.34);
  g.fillStyle = th.text;
  g.font = f(big);
  g.fillText('Aa', box.x, box.y + big * 0.92);
  const tx = box.x + Math.min(box.w * 0.42, g.measureText('Aa').width + 16 * k);
  const lines = ['ABCDEFGHIJKLM', 'NOPQRSTUVWXYZ', 'abcdefghijklm', 'nopqrstuvwxyz', '0123456789'];
  const lh = Math.min(box.h / (lines.length + 0.5), 22 * k);
  g.font = f(lh * 0.8);
  lines.forEach((l, i) => g.fillText(clip(g, l, box.x + box.w - tx), tx, box.y + lh * (i + 0.9)));
  return done(c, W, H);
}

async function lutCard(spec: Spec) {
  const r = await fetch(spec.url!);
  if (!r.ok) return null;
  const lut = parseCube(await r.text());
  const { c, g, W, H, th, box, k } = card(spec, 'lut');
  const half = Math.floor((box.w - 6 * k) / 2);
  const ch = Math.max(8, box.h - 14 * k);
  const tile = typeof OffscreenCanvas !== 'undefined' ? new OffscreenCanvas(half, ch) : Object.assign(document.createElement('canvas'), { width: half, height: ch });
  const tg = tile.getContext('2d', { willReadFrequently: true }) as Ctx;
  drawChart(tg, half, ch);
  g.drawImage(tile, box.x, box.y);
  const d = tg.getImageData(0, 0, half, ch);
  applyLut(lut, d.data);
  tg.putImageData(d, 0, 0);
  g.drawImage(tile, box.x + box.w - half, box.y);
  g.fillStyle = th.dim;
  g.font = `400 ${10 * k}px ${MONO}`;
  g.fillText('before', box.x, box.y + ch + 11 * k);
  g.fillText('after', box.x + box.w - half, box.y + ch + 11 * k);
  return done(c, W, H);
}

async function subsCard(spec: Spec) {
  const r = await fetch(spec.url!);
  if (!r.ok) return null;
  const cues = parseSubs(await r.text());
  if (!cues.length) return null;
  const { c, g, W, H, th, box, k } = card(spec, 'subs');
  const fs = 11 * k, lh = fs * 1.5;
  g.font = `400 ${fs}px ${MONO}`;
  const n = Math.max(1, Math.floor(box.h / lh));
  cues.slice(0, n).forEach((cue, i) => {
    const y = box.y + lh * (i + 0.8);
    const t = cueTime(cue.start).replace(/\.\d+$/, '');
    g.fillStyle = th.dim;
    g.fillText(t, box.x, y);
    const tw = g.measureText(t + '  ').width;
    g.fillStyle = th.text;
    g.fillText(clip(g, cue.text.replace(/\n/g, ' '), box.w - tw), box.x + tw, y);
  });
  return done(c, W, H);
}

/** `globalThis.falosCardArt`: richer card art, or null for the plain card. */
async function cardArt(spec: Spec) {
  if (!spec.url) return null;
  switch (previewKind(spec.mime ?? '', spec.title ?? '', spec.kind)) {
    case 'model3d':
      return imageCard(spec, '3d', await modelPoster(spec.url, spec.asset || spec.url));
    case 'pdf':
      return pdfCard(spec);
    case 'font':
      return fontCard(spec);
    case 'lut':
      return lutCard(spec);
    case 'subs':
      return subsCard(spec);
    default:
      return null;
  }
}

export function installCardArt() {
  (globalThis as any).falosCardArt = cardArt;
}
