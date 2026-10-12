// Drag and drop onto the canvas, from Finder (files) and from the asset library (tiles):
// the drag session (what is being dragged, sized ghosts), the grid the drop lands in (the
// same layout as the engine's `grid_layout`, so ghosts and placed nodes line up), the frame
// under the pointer (drop into frames), drop placeholders that show until the media is in,
// and per-asset upload progress to the host. Rendered by components/DropLayer.tsx; all of it
// is lightweight DOM transforms over the canvas (no React work per pointer move).

import { useSyncExternalStore } from 'react';
import { getState } from './store';
import { worldRects, type Rect } from './geometry';

/** One dragged item, world size (true size for library assets, an estimate for files). */
export interface GhostItem { w: number; h: number; kind: string; thumb?: string; name?: string }

export interface DragSession {
  source: 'files' | 'library';
  items: GhostItem[];
  /** Library: the asset shas, in `items` order. */
  shas?: string[];
}

/** A drop waiting for its media: world rects, progress (files done / total). */
export interface Placeholder { id: number; rects: Rect[]; thumbs: (string | undefined)[]; done: number; total: number; leaving: boolean }

interface DndState {
  session: DragSession | null;
  placeholders: Placeholder[];
  /** Asset id → upload progress to the host (0..1). */
  uploads: Record<string, number>;
}

let st: DndState = { session: null, placeholders: [], uploads: {} };
const subs = new Set<() => void>();
export const dnd = () => st;
function set(p: Partial<DndState>) {
  st = { ...st, ...p };
  subs.forEach((f) => f());
}
export function useDnd<T>(sel: (s: DndState) => T): T {
  return useSyncExternalStore(
    (f) => {
      subs.add(f);
      return () => subs.delete(f);
    },
    () => sel(st),
  );
}

// ---- layout -----------------------------------------------------------------------------

export const GRID_GAP = 40;

const GRID_PAD = 24;

/** Mirror of `grid_layout` in crates/client/src/web/mod.rs: ceil(sqrt(n)) columns, rows
 *  top-aligned, the block centered on `at` (world). Into a `frame` (world rect) the block
 *  is scaled down to fit inside it (with a margin) and kept within it. */
export function gridLayout(sizes: [number, number][], at: [number, number], frame?: Rect | null): Rect[] {
  const n = sizes.length;
  if (!n) return [];
  const cols = Math.ceil(Math.sqrt(n));
  const rows: [number, number][][] = [];
  for (let i = 0; i < n; i += cols) rows.push(sizes.slice(i, i + cols));
  const block = (k: number) => {
    const rowW = rows.map((r) => r.reduce((a, s) => a + s[0] * k, 0) + GRID_GAP * k * (r.length - 1));
    const rowH = rows.map((r) => Math.max(...r.map((s) => s[1] * k)));
    return { rowH, w: Math.max(...rowW), h: rowH.reduce((a, b) => a + b, 0) + GRID_GAP * k * (rows.length - 1) };
  };
  const b1 = block(1);
  const k = frame ? Math.min(1, Math.max(0.05, Math.min((frame[2] - 2 * GRID_PAD) / b1.w, (frame[3] - 2 * GRID_PAD) / b1.h))) : 1;
  const b = block(k);
  const c: [number, number] = [at[0], at[1]];
  if (frame) {
    const clamp = (v: number, lo: number, hi: number) => (lo > hi ? (lo + hi) / 2 : Math.min(hi, Math.max(lo, v)));
    c[0] = clamp(c[0], frame[0] + GRID_PAD + b.w / 2, frame[0] + frame[2] - GRID_PAD - b.w / 2);
    c[1] = clamp(c[1], frame[1] + GRID_PAD + b.h / 2, frame[1] + frame[3] - GRID_PAD - b.h / 2);
  }
  const out: Rect[] = [];
  let y = c[1] - b.h / 2;
  rows.forEach((row, r) => {
    let x = c[0] - b.w / 2;
    for (const s of row) {
      out.push([x, y, s[0] * k, s[1] * k]);
      x += (s[0] + GRID_GAP) * k;
    }
    y += b.rowH[r] + GRID_GAP * k;
  });
  return out;
}

/** World rect of a frame by id (for fitting a drop into it). */
export function frameRect(id: string | null | undefined): Rect | null {
  if (!id) return null;
  return worldRects(getState().nodes).get(id) ?? null;
}

/** World size of a file before it is probed (the engine sizes media at natural aspect,
 *  longest side ≤ 640; these are its card sizes and typical media aspects). */
export function fileGhost(type: string, name = ''): GhostItem {
  const t = type.toLowerCase();
  const ext = name.split('.').pop()?.toLowerCase() ?? '';
  if (t.startsWith('image/')) return { w: 480, h: 360, kind: 'image', name };
  if (t.startsWith('video/')) return { w: 640, h: 360, kind: 'video', name };
  if (t.startsWith('audio/')) return { w: 360, h: 150, kind: 'audio', name };
  if (t.startsWith('text/') || ['md', 'txt', 'json'].includes(ext)) return { w: 320, h: 240, kind: 'text', name };
  if (t === 'application/pdf' || ext === 'pdf') return { w: 260, h: 200, kind: 'pdf', name };
  if (['glb', 'gltf', 'obj', 'fbx', 'usdz'].includes(ext)) return { w: 280, h: 220, kind: '3d', name };
  return { w: 280, h: 180, kind: 'file', name };
}

/** Ghosts for a Finder drag: `items` carries kinds and mime types during dragover (names
 *  and bytes only on drop). */
export function filesSession(dt: DataTransfer): DragSession {
  const items = Array.from(dt.items ?? []).filter((i) => i.kind === 'file');
  const ghosts = items.length ? items.map((i) => fileGhost(i.type)) : Array.from(dt.files).map((f) => fileGhost(f.type, f.name));
  return { source: 'files', items: ghosts.length ? ghosts : [fileGhost('')] };
}

// ---- the drag session ---------------------------------------------------------------------

export function startDrag(s: DragSession) {
  set({ session: s });
}

/** Clears the session and the frame highlight. */
export function endDrag() {
  getState().app?.setDropTarget(null);
  if (st.session) set({ session: null });
}

/** Rects (world) the session's items would land in with the pointer at CSS point `p`
 *  (relative to the canvas). */
export function ghostRects(items: GhostItem[], p: [number, number], frame: string | null = null): Rect[] {
  const app = getState().app;
  if (!app) return [];
  const [wx, wy] = app.screenToWorld(p[0], p[1]);
  return gridLayout(items.map((i) => [i.w, i.h]), [wx, wy], frameRect(frame));
}

// ---- placeholders -----------------------------------------------------------------------

let phSeq = 0;
let importing: number | null = null;

export function addPlaceholder(rects: Rect[], thumbs: (string | undefined)[] = []): number {
  const id = ++phSeq;
  set({ placeholders: [...st.placeholders, { id, rects, thumbs, done: 0, total: rects.length, leaving: false }] });
  return id;
}

/** Fades a placeholder out (200 ms crossfade with the media appearing under it). */
export function finishPlaceholder(id: number, delay = 120) {
  window.setTimeout(() => {
    set({ placeholders: st.placeholders.map((p) => (p.id === id ? { ...p, leaving: true, done: p.total } : p)) });
    window.setTimeout(() => set({ placeholders: st.placeholders.filter((p) => p.id !== id) }), 220);
  }, delay);
}

/** The placeholder that receives the engine's `import {done, total}` events. */
export function trackImport(id: number | null) {
  importing = id;
}

export function importProgress(done: number) {
  if (importing == null) return;
  set({ placeholders: st.placeholders.map((p) => (p.id === importing ? { ...p, done } : p)) });
}

// ---- uploads to the host ----------------------------------------------------------------

let uploadFlush = 0;
let pendingUploads: Record<string, number | null> = {};

/** Upload progress of an asset (0..1); `null` = finished (or failed). Batched per frame. */
export function setUpload(id: string, v: number | null) {
  pendingUploads[id] = v;
  if (uploadFlush) return;
  uploadFlush = requestAnimationFrame(() => {
    uploadFlush = 0;
    const next = { ...st.uploads };
    for (const [k, val] of Object.entries(pendingUploads)) {
      if (val == null) delete next[k];
      else next[k] = val;
    }
    pendingUploads = {};
    set({ uploads: next });
  });
}
