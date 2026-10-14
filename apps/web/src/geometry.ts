import type { CameraView, NodeView } from './types';

export type Rect = [number, number, number, number];

/** World rects: doc positions are relative to the parent node (FORMAT.md). `nodes` is
 *  depth-first (parents before children). */
export function worldRects(nodes: NodeView[]): Map<string, Rect> {
  const out = new Map<string, Rect>();
  for (const n of nodes) {
    const p = n.parent ? out.get(n.parent) : undefined;
    out.set(n.id, [n.x + (p?.[0] ?? 0), n.y + (p?.[1] ?? 0), n.w, n.h]);
  }
  return out;
}

/** Whether a world rect intersects the camera's view. */
export function inView(r: Rect, c: CameraView, margin = 0): boolean {
  const w = c.width / c.zoom, h = c.height / c.zoom;
  return r[0] < c.x + w + margin && r[1] < c.y + h + margin && r[0] + r[2] > c.x - margin && r[1] + r[3] > c.y - margin;
}

/** The offset for the next ⌘D: 24, 24, or, when the copies of the last ⌘D are still selected
 *  (`last`) and were moved, the offset from that source to them again ("repeat last offset":
 *  ⌘D, move, ⌘D ⌘D lays out a row). `now`: the selection's box. */
export function duplicateOffset(sel: string[], last: { copies: string[]; from: Rect } | null, now: Rect | null): [number, number] {
  const same = !!last && last.copies.length === sel.length && last.copies.every((id) => sel.includes(id));
  if (same && now && last) {
    const d: [number, number] = [now[0] - last.from[0], now[1] - last.from[1]];
    if (d[0] || d[1]) return d;
  }
  return [24, 24];
}
