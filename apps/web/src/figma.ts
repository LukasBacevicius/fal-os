// Native Figma paste (⌘C in Figma, ⌘V on the canvas). The wasm client decodes the clipboard's
// scene (crates/figma) into a plan; this module turns the plan into one transaction (one undo
// step) of frames, groups, text and image placeholders at the paste point, selects it, then
// streams in the pixels: Figma images are downloaded by the host (`POST /import/url`; the
// signed S3 URLs send no CORS headers) and vector art is rendered to PNG (raster.ts), with
// bounded concurrency and a progress toast. Fill-ins are untracked transactions, so undoing
// the paste removes everything at once.

import { figmaPlan } from './wasm/pkg/falos_client';
import { getState, toast } from './store';
import { hostSync } from './sync';
import * as persist from './persist';
import { renderRaster } from './raster';
import { planCommands, type FigmaPlan } from './figmaPlan';

export { isFigmaScene } from './figmaPlan';

const CONCURRENCY = 8;

/** Runs `jobs` with at most `n` in flight. */
async function pool<T>(items: T[], n: number, job: (t: T) => Promise<void>) {
  let next = 0;
  await Promise.all(
    Array.from({ length: Math.min(n, items.length) }, async () => {
      while (next < items.length) await job(items[next++]);
    }),
  );
}

function deferred<T>() {
  let resolve!: (v: T) => void;
  const promise = new Promise<T>((r) => (resolve = r));
  return { promise, resolve };
}

async function sha256Id(blob: Blob): Promise<string> {
  const d = new Uint8Array(await crypto.subtle.digest('SHA-256', await blob.arrayBuffer()));
  return 'sha256:' + Array.from(d, (b) => b.toString(16).padStart(2, '0')).join('');
}

const ID_CHARS = 'abcdefghijklmnopqrstuvwxyz0123456789';
const newId = () => Array.from(crypto.getRandomValues(new Uint8Array(6)), (b) => ID_CHARS[b % 36]).join('');

/** Untracked prop updates, batched (one transaction per ~250 ms: every transaction re-reads
 *  the node list and re-renders the panels, which is what limits a big paste) and skipping
 *  nodes that are gone (e.g. the paste was undone meanwhile). */
function updater() {
  let queue: { id: string; props: Record<string, unknown> }[] = [];
  let timer = 0;
  const flush = () => {
    timer = 0;
    const { app, nodes } = getState();
    const live = new Set(nodes.map((n) => n.id));
    const batch = queue.filter((u) => live.has(u.id));
    queue = [];
    if (!app || !batch.length) return;
    try {
      app.applyUntracked(batch.map((u) => ({ op: 'update', id: u.id, props: u.props })));
    } catch (e) {
      console.warn('figma fill-in failed', e);
    }
  };
  return {
    push(id: string, props: Record<string, unknown>) {
      queue.push({ id, props });
      timer ||= window.setTimeout(flush, 250);
    },
    flush() {
      window.clearTimeout(timer);
      flush();
    },
  };
}

function registerLocal(id: string, blob: Blob, mime: string, name: string) {
  const { app, conn } = getState();
  app?.registerAsset(id, blob, mime, name);
  // Local-only canvases keep their assets in IndexedDB (a host keeps them on disk).
  if (conn === 'local') persist.saveAsset({ id, blob, mime, name }).catch(console.warn);
}

/** Downloads a Figma image: through the host when connected, else directly (works only for
 *  CORS-enabled URLs). Resolves to the asset id and a bitmap for rasters that draw it. */
async function fetchImage(url: string, sha1: string, name: string): Promise<{ id: string; blob: Blob }> {
  const sync = hostSync();
  if (sync?.connected) {
    // Figma names images by SHA-1: the host verifies it and skips known ones.
    const a = await sync.importUrl(url, { sha1 });
    sync.markUploaded(a.id);
    const blob = await sync.assetBlob(a.id);
    registerLocal(a.id, blob, a.mime || blob.type, name);
    return { id: a.id, blob };
  }
  // No host: only CORS-enabled URLs work (Figma's don't), so say what would fix it.
  const r = await fetch(url).catch(() => {
    throw new Error('blocked by CORS: Figma images download through the fal-os host');
  });
  if (!r.ok) throw new Error(`download ${r.status}`);
  const blob = await r.blob();
  const id = await sha256Id(blob);
  registerLocal(id, blob, blob.type, name);
  return { id, blob };
}

/** Imports Figma clipboard HTML at screen point `at` (CSS px; null = view center). */
export async function importFigma(html: string, at: [number, number] | null): Promise<boolean> {
  const { app, camera } = getState();
  if (!app) return false;
  const t0 = performance.now();
  let plan: FigmaPlan;
  try {
    plan = JSON.parse(figmaPlan(html));
  } catch (e) {
    toast(`figma paste failed: ${e}`);
    return true;
  }
  if (!plan.nodes.length) {
    toast('figma paste: nothing visible to import');
    return true;
  }
  const p = at ?? [camera.width / 2, camera.height / 2];
  const [wx, wy] = app.screenToWorld(p[0], p[1]);
  // Centered on the paste point, on whole canvas units.
  const origin: [number, number] = [Math.round(wx - plan.size[0] / 2), Math.round(wy - plan.size[1] / 2)];
  const { cmds, ids, roots } = planCommands(plan, origin, newId);
  try {
    app.apply(cmds);
  } catch (e) {
    toast(`figma paste failed: ${e}`);
    return true;
  }
  app.setSelection(roots);
  const tStructure = performance.now();
  console.info(`figma paste: ${plan.nodes.length} nodes in ${Math.round(tStructure - t0)} ms`, plan.stats, plan.warnings);

  // ---- stream in the pixels ----
  const fill = updater();
  const byImage = new Map<string, string[]>();
  plan.nodes.forEach((n, i) => {
    if (n.kind === 'image' && n.image) byImage.set(n.image, [...(byImage.get(n.image) ?? []), ids[i]]);
  });
  // Every image any raster draws, too.
  for (const n of plan.nodes) for (const h of n.raster?.images ?? []) if (!byImage.has(h)) byImage.set(h, []);
  // Identical rasters (repeated icons) render once.
  const byRaster = new Map<string, number[]>();
  plan.nodes.forEach((n, i) => {
    if (n.kind !== 'raster' || !n.raster) return;
    const key = `${n.w}x${n.h}:${JSON.stringify(n.raster.ops)}`;
    byRaster.set(key, [...(byRaster.get(key) ?? []), i]);
  });
  const total = byImage.size + byRaster.size;
  let done = 0, failed = 0;
  const progress = () => {
    if (done < total) toast(`importing from figma ${done}/${total}`);
  };
  progress();

  const imageJobs = [...byImage.entries()];
  const images = new Map(imageJobs.map(([hash]) => [hash, deferred<{ id: string; blob: Blob } | null>()]));
  const bitmaps = new Map<string, Promise<ImageBitmap | null>>();
  const bitmap = (hash: string) => {
    let b = bitmaps.get(hash);
    if (!b) {
      b = images.get(hash)!.promise.then((img) => (img ? createImageBitmap(img.blob).catch(() => null) : null));
      bitmaps.set(hash, b);
    }
    return b;
  };

  const runImage = async ([hash, nodes]: [string, string[]]) => {
    const url = plan.images[hash];
    try {
      if (!url) throw new Error('no download URL');
      const img = await fetchImage(url, hash, `figma-${hash.slice(0, 8)}`);
      for (const id of nodes) fill.push(id, { asset: img.id });
      images.get(hash)!.resolve(img);
    } catch (e) {
      failed++;
      for (const id of nodes) fill.push(id, { error: `figma image: ${String(e).slice(0, 160)}` });
      images.get(hash)!.resolve(null);
    }
    done++;
    progress();
  };

  const runRaster = async ([, idx]: [string, number[]]) => {
    const n = plan.nodes[idx[0]];
    try {
      const imgs = new Map<string, ImageBitmap>();
      for (const h of n.raster!.images) {
        const b = await bitmap(h);
        if (b) imgs.set(h, b);
      }
      const blob = await renderRaster(n.raster!, n.w, n.h, imgs);
      const id = await sha256Id(blob);
      registerLocal(id, blob, 'image/png', n.name);
      for (const i of idx) fill.push(ids[i], { asset: id });
    } catch (e) {
      failed++;
      for (const i of idx) fill.push(ids[i], { error: `render: ${String(e).slice(0, 160)}` });
    }
    done++;
    progress();
  };

  // Images first (rasters may wait on them), but both pools run together.
  await Promise.all([pool(imageJobs, CONCURRENCY, runImage), pool([...byRaster.entries()], 4, runRaster)]);
  fill.flush();
  const secs = ((performance.now() - t0) / 1000).toFixed(1);
  console.info(`figma paste: pixels done in ${secs} s (${byImage.size} images, ${byRaster.size} rasters, ${failed} failed)`);
  toast(failed ? `imported from figma — ${failed} of ${total} images failed` : `imported from figma: ${plan.nodes.length} layers, ${byImage.size} images`);
  return true;
}
