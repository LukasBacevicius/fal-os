// Canvas actions behind the keyboard: duplicate, group/ungroup into frames, copy/paste,
// reading-order selection cycling, zoom. Everything goes through `CanvasApp.apply` (one
// transaction = one undo step, attributed to the local actor) or the camera API.

import { getState, toast } from './store';
import { worldRects, type Rect } from './geometry';
import { importFiles } from './controller';
import type { NodeView } from './types';

const ID_CHARS = 'abcdefghijklmnopqrstuvwxyz0123456789';
const newId = () => Array.from(crypto.getRandomValues(new Uint8Array(6)), (b) => ID_CHARS[b % 36]).join('');

/** Selected nodes without the ones whose ancestor is also selected (subtrees go along). */
function selectedRoots(nodes: NodeView[], sel: string[]): NodeView[] {
  const byId = new Map(nodes.map((n) => [n.id, n]));
  const set = new Set(sel);
  const covered = (n: NodeView): boolean => {
    let p = n.parent;
    while (p) {
      if (set.has(p)) return true;
      p = byId.get(p)?.parent ?? null;
    }
    return false;
  };
  return nodes.filter((n) => set.has(n.id) && !covered(n));
}

/** Subtree rows in depth-first order (parents first). */
function subtree(nodes: NodeView[], root: NodeView): NodeView[] {
  const kids = new Map<string, NodeView[]>();
  for (const n of nodes) if (n.parent) kids.set(n.parent, [...(kids.get(n.parent) ?? []), n]);
  const out: NodeView[] = [];
  const walk = (n: NodeView) => {
    out.push(n);
    for (const k of kids.get(n.id) ?? []) walk(k);
  };
  walk(root);
  return out;
}

interface Clip { format: 'falos-nodes'; nodes: { id: string; parent: string | null; kind: string; name: string; text: string | null; props: Record<string, unknown>; x: number; y: number; w: number; h: number }[] }

function clipOf(sel: string[]): Clip | null {
  const { nodes } = getState();
  const roots = selectedRoots(nodes, sel);
  if (!roots.length) return null;
  const rects = worldRects(nodes);
  const rows: Clip['nodes'] = [];
  for (const r of roots) {
    for (const n of subtree(nodes, r)) {
      const top = n.id === r.id;
      const wr = rects.get(n.id)!;
      // Roots carry world positions (they become top-level), children stay parent-relative.
      rows.push({ id: n.id, parent: top ? null : n.parent, kind: n.kind, name: n.name, text: n.text, props: n.props, x: top ? wr[0] : n.x, y: top ? wr[1] : n.y, w: n.w, h: n.h });
    }
  }
  return { format: 'falos-nodes', nodes: rows };
}

/** Recreates clipboard rows with fresh ids, offset by `d` (roots only). Returns new root ids. */
function pasteClip(clip: Clip, d: [number, number]): string[] {
  const { app } = getState();
  if (!app) return [];
  const ids = new Map(clip.nodes.map((n) => [n.id, newId()]));
  const roots: string[] = [];
  const cmds = clip.nodes.map((n) => {
    const root = !n.parent || !ids.has(n.parent);
    if (root) roots.push(ids.get(n.id)!);
    const props = { ...n.props, x: Math.round(n.x + (root ? d[0] : 0)), y: Math.round(n.y + (root ? d[1] : 0)), w: n.w, h: n.h };
    return { op: 'create', id: ids.get(n.id), kind: n.kind, parent: root ? null : ids.get(n.parent!), name: n.name, props, text: n.text ?? undefined };
  });
  try {
    app.apply(cmds);
    app.setSelection(roots);
  } catch (e) {
    toast(`paste failed: ${e}`);
    return [];
  }
  return roots;
}

export function duplicate() {
  const clip = clipOf(getState().selection);
  if (clip) pasteClip(clip, [24, 24]);
}

/** ⌘C (copy event): the selected subtrees as JSON text on the clipboard. */
export function copy(e: ClipboardEvent): boolean {
  const clip = clipOf(getState().selection);
  if (!clip || !e.clipboardData) return false;
  e.clipboardData.setData('text/plain', JSON.stringify(clip));
  e.preventDefault();
  lastClip = clip;
  return true;
}

let lastClip: Clip | null = null;

/** Paste: files/images become media nodes (uploaded to the host like drops), copied nodes
 *  are recreated at the cursor, plain text becomes a note. */
export async function paste(e: ClipboardEvent, at: [number, number] | null) {
  const { app } = getState();
  if (!app) return;
  const dt = e.clipboardData;
  const files = dt ? Array.from(dt.files) : [];
  if (files.length) {
    e.preventDefault();
    await importFiles(files, at ?? undefined);
    return;
  }
  const text = dt?.getData('text/plain') ?? '';
  let clip: Clip | null = null;
  try {
    const v = JSON.parse(text);
    if (v?.format === 'falos-nodes') clip = v;
  } catch {
    /* not ours */
  }
  if (!clip && !text && lastClip) clip = lastClip;
  e.preventDefault();
  if (clip) {
    const c = getState().camera;
    const p = at ?? [c.width / 2, c.height / 2];
    const [wx, wy] = app.screenToWorld(p[0], p[1]);
    const minX = Math.min(...clip.nodes.filter((n) => !n.parent || !clip!.nodes.some((m) => m.id === n.parent)).map((n) => n.x));
    const minY = Math.min(...clip.nodes.filter((n) => !n.parent || !clip!.nodes.some((m) => m.id === n.parent)).map((n) => n.y));
    pasteClip(clip, [wx - minX, wy - minY]);
  } else if (text.trim()) {
    const c = getState().camera;
    const p = at ?? [c.width / 2, c.height / 2];
    const [x, y] = app.screenToWorld(p[0], p[1]);
    const r = app.apply({ op: 'create', kind: 'note', props: { x: Math.round(x), y: Math.round(y), w: 280, h: 160 }, text: text.slice(0, 20_000) }) as { created: string[] };
    if (r.created?.[0]) app.setSelection([r.created[0]]);
  }
}

/** ⌘G: wraps the selection (siblings of one parent) in a new group fitted to it; the
 *  children keep their world positions. */
export function groupIntoFrame() {
  wrapSelection('group');
}

/** ⌘⌥G: wraps the selection in a titled frame with a little padding. */
export function frameSelection() {
  wrapSelection('frame');
}

function wrapSelection(kind: 'group' | 'frame') {
  const { app, nodes, selection } = getState();
  if (!app || !selection.length) return;
  const roots = selectedRoots(nodes, selection);
  const parent = roots[0].parent;
  if (roots.some((n) => n.parent !== parent)) {
    toast('group: select nodes with the same parent');
    return;
  }
  const rects = worldRects(nodes);
  const rs = roots.map((n) => rects.get(n.id)!);
  const pad = kind === 'frame' ? 24 : 0;
  const x0 = Math.min(...rs.map((r) => r[0])) - pad, y0 = Math.min(...rs.map((r) => r[1])) - pad;
  const x1 = Math.max(...rs.map((r) => r[0] + r[2])) + pad, y1 = Math.max(...rs.map((r) => r[1] + r[3])) + pad;
  const po: Rect = parent ? rects.get(parent)! : [0, 0, 0, 0];
  const id = newId();
  const cmds: unknown[] = [{ op: 'create', id, kind, parent, name: kind, props: { x: Math.round(x0 - po[0]), y: Math.round(y0 - po[1]), w: Math.round(x1 - x0), h: Math.round(y1 - y0) } }];
  roots.forEach((n, i) => {
    const r = rs[i];
    cmds.push({ op: 'move', id: n.id, parent: id });
    cmds.push({ op: 'update', id: n.id, props: { x: Math.round(r[0] - x0), y: Math.round(r[1] - y0) } });
  });
  app.apply(cmds);
  app.setSelection([id]);
}

/** ⌘⇧G: dissolves selected frames, keeping their children in place. */
export function ungroup() {
  const { app, nodes, selection } = getState();
  if (!app) return;
  const rects = worldRects(nodes);
  const frames = nodes.filter((n) => selection.includes(n.id) && (n.kind === 'frame' || n.kind === 'group'));
  if (!frames.length) return;
  const cmds: unknown[] = [];
  const kept: string[] = [];
  for (const f of frames) {
    const po: Rect = f.parent ? rects.get(f.parent)! : [0, 0, 0, 0];
    for (const k of nodes.filter((n) => n.parent === f.id)) {
      const r = rects.get(k.id)!;
      cmds.push({ op: 'move', id: k.id, parent: f.parent });
      cmds.push({ op: 'update', id: k.id, props: { x: Math.round(r[0] - po[0]), y: Math.round(r[1] - po[1]) } });
      kept.push(k.id);
    }
    cmds.push({ op: 'delete', id: f.id });
  }
  app.apply(cmds);
  app.setSelection(kept);
}

/** Drawable nodes in reading order: rows top to bottom (a row = overlapping vertical
 *  extents), left to right within a row. Frames come before their contents. */
export function readingOrder(nodes: NodeView[]): string[] {
  const rects = worldRects(nodes);
  const items = nodes.filter((n) => n.kind !== 'group' && n.kind !== 'connector').map((n) => ({ id: n.id, r: rects.get(n.id)!, depth: n.depth }));
  items.sort((a, b) => a.r[1] - b.r[1]);
  const rows: (typeof items)[] = [];
  for (const it of items) {
    const row = rows[rows.length - 1];
    const bottom = row ? Math.min(...row.map((x) => x.r[1] + x.r[3])) : -Infinity;
    if (row && it.r[1] < bottom - Math.min(it.r[3], 40) * 0.5) row.push(it);
    else rows.push([it]);
  }
  return rows.flatMap((row) => row.sort((a, b) => a.r[0] - b.r[0] || a.depth - b.depth).map((x) => x.id));
}

/** Tab / ⇧Tab: selects the next/previous node in reading order and brings it into view. */
export function cycleSelection(dir: 1 | -1) {
  const { app, nodes, selection } = getState();
  if (!app || !nodes.length) return;
  const order = readingOrder(nodes);
  const cur = selection.length ? order.indexOf(selection[selection.length - 1]) : -1;
  const next = order[(cur + dir + order.length) % order.length] ?? order[0];
  app.setSelection([next]);
  app.focusNode(next);
}

export function zoomBy(f: number) {
  const { app, camera } = getState();
  app?.zoomTo(Math.max(0.02, Math.min(64, camera.zoom * f)));
}
