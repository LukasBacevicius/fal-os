import { useMemo } from 'react';
import { useStore } from '../store';
import type { NodeView } from '../types';

type Rect = [number, number, number, number];

/** World rects: doc positions are relative to the parent node (FORMAT.md). */
function worldRects(nodes: NodeView[]): Map<string, Rect> {
  const out = new Map<string, Rect>();
  // `nodes` is depth-first (parents before children).
  for (const n of nodes) {
    const p = n.parent ? out.get(n.parent) : undefined;
    out.set(n.id, [n.x + (p?.[0] ?? 0), n.y + (p?.[1] ?? 0), n.w, n.h]);
  }
  return out;
}

/**
 * DOM overlays positioned over engine-drawn nodes: frame titles (Figma-style, above the
 * frame), a shimmer + status chip on running generations, and claim badges.
 */
export function NodeOverlays() {
  const app = useStore((s) => s.app);
  const nodes = useStore((s) => s.nodes);
  const claims = useStore((s) => s.claims);
  const jobs = useStore((s) => s.jobs);
  const camera = useStore((s) => s.camera); // re-position on camera changes
  const rects = useMemo(() => worldRects(nodes), [nodes]);
  if (!app) return null;
  const screen = (r: Rect) => {
    const [x0, y0] = app.worldToScreen(r[0], r[1]);
    const [x1, y1] = app.worldToScreen(r[0] + r[2], r[1] + r[3]);
    return { left: x0, top: y0, width: x1 - x0, height: y1 - y0 };
  };
  const visible = (b: { left: number; top: number; width: number; height: number }) =>
    b.left < camera.width && b.top < camera.height && b.left + b.width > 0 && b.top + b.height > 0;
  const byId = new Map(nodes.map((n) => [n.id, n]));

  return (
    <div className="node-overlays">
      {nodes.map((n) => {
        const r = rects.get(n.id);
        if (!r) return null;
        if (n.kind === 'frame' && n.name) {
          const b = screen(r);
          if (!visible(b) || b.width < 40) return null;
          return (
            <div key={`f-${n.id}`} className="frame-label" style={{ transform: `translate(${b.left}px, ${b.top - 20}px)`, maxWidth: b.width }}>
              {n.name}
            </div>
          );
        }
        if (n.kind === 'generation') {
          const status = String(n.props.status ?? 'queued');
          if (status !== 'queued' && status !== 'running') return null;
          const b = screen(r);
          if (!visible(b)) return null;
          const job = jobs[n.id];
          const pos = job?.status === 'queued' && job.queue_position != null ? ` · #${job.queue_position} in queue` : '';
          return (
            <div key={`g-${n.id}`} className={`gen-shimmer ${status}`} style={{ transform: `translate(${b.left}px, ${b.top}px)`, width: b.width, height: b.height }}>
              {b.width > 120 && <span className="gen-chip">{status === 'running' ? 'Generating' : 'Queued'}{pos}</span>}
            </div>
          );
        }
        return null;
      })}
      {claims.map((c) => {
        const r = rects.get(c.node);
        if (!r || !byId.has(c.node)) return null;
        const b = screen(r);
        if (!visible(b)) return null;
        return (
          <div key={`c-${c.node}`} className="claim-badge" style={{ transform: `translate(${b.left + b.width}px, ${b.top}px) translate(-100%, -100%)`, background: c.color }}>
            @{c.actor} is working here
          </div>
        );
      })}
    </div>
  );
}
