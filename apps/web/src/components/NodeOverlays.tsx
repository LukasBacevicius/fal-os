import { useMemo } from 'react';
import { useStore } from '../store';
import type { NodeView } from '../types';
import { sendChatMessage } from '../chatActions';
import { openPrompt } from './SelectionPrompt';

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

const GENERATING = new Set(['queued', 'running', '']);

/**
 * DOM text over engine-drawn nodes: frame titles above frames, prompt/model on a hovered
 * generating tile, `[retry]` on failed generations, claim labels, and the "add to chat"
 * button by the selection. All plain dim mono text.
 */
export function NodeOverlays() {
  const app = useStore((s) => s.app);
  const nodes = useStore((s) => s.nodes);
  const claims = useStore((s) => s.claims);
  const hover = useStore((s) => s.hoverNode);
  const selection = useStore((s) => s.selection);
  const jobs = useStore((s) => s.jobs);
  const promptOpen = useStore((s) => s.promptOpen);
  const editing = useStore((s) => s.editing);
  const camera = useStore((s) => s.camera); // re-position on camera changes
  const activity = useStore((s) => s.activity);
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

  let selBox: ReturnType<typeof screen> | null = null;
  const sel = selection.map((id) => rects.get(id)).filter((r): r is Rect => !!r);
  if (sel.length) {
    const x0 = Math.min(...sel.map((r) => r[0]));
    const y0 = Math.min(...sel.map((r) => r[1]));
    const x1 = Math.max(...sel.map((r) => r[0] + r[2]));
    const y1 = Math.max(...sel.map((r) => r[1] + r[3]));
    selBox = screen([x0, y0, x1 - x0, y1 - y0]);
  }

  // Frame titles, decluttered (brilliant-canvas-interaction.md): frames too small on screen
  // lose their title, overlapping titles keep the bigger frame's, and the hovered/selected
  // frame always keeps its own. Nested frames (e.g. a pasted Figma design's cards) show
  // theirs only when hovered or selected, like in Figma. DOM above the canvas, so titles
  // never sit under content.
  const labels: { n: NodeView; b: ReturnType<typeof screen>; pin: boolean }[] = [];
  {
    const cands = nodes
      .filter((n) => n.kind === 'frame' && n.name)
      .map((n) => ({ n, b: screen(rects.get(n.id)!), pin: n.id === hover || selection.includes(n.id) }))
      .filter((c) => visible(c.b) && (c.pin || (c.b.width >= 40 && !c.n.parent)))
      .sort((a, b) => Number(b.pin) - Number(a.pin) || b.b.width * b.b.height - a.b.width * a.b.height);
    const placed: [number, number, number, number][] = [];
    for (const c of cands) {
      // A frame running off the left edge keeps its title at the edge (sticky).
      const left = c.b.left < 0 ? 4 : c.b.left;
      if (c.b.left + c.b.width - left < 40 && !c.pin) continue;
      const w = Math.min(c.b.left + c.b.width - left, c.n.name.length * 6.7 + 4);
      const box: [number, number, number, number] = [left, c.b.top - 16, w, 14];
      const hit = placed.some((p) => p[0] < box[0] + box[2] && box[0] < p[0] + p[2] && p[1] < box[1] + box[3] && box[1] < p[1] + p[3]);
      if (hit && !c.pin) continue;
      placed.push(box);
      labels.push({ n: c.n, b: { ...c.b, left, width: c.b.left + c.b.width - left }, pin: c.pin });
    }
  }
  // An empty frame an agent is working on (claimed, or touched in the last 30 s): a render or
  // import is on its way.
  const pending = new Set<string>([...claims.map((c) => c.node), ...activity.filter((a) => a.op !== 'read').flatMap((a) => a.nodes)]);

  return (
    <div className="node-overlays">
      {labels.map(({ n, b, pin }) => (
        <div key={`f-${n.id}`} className="frame-label" style={{ transform: `translate(${b.left}px, ${b.top - 16}px)`, maxWidth: pin ? Math.max(b.width, 240) : b.width }}>
          {n.name}
        </div>
      ))}
      {nodes.map((n) => {
        const r = rects.get(n.id);
        if (!r) return null;
        if (n.kind === 'frame') {
          if (n.children.length || !pending.has(n.id)) return null;
          const b = screen(r);
          if (!visible(b) || b.width < 80 || b.height < 30) return null;
          return (
            <div key={`r-${n.id}`} className="frame-pending faint" style={{ transform: `translate(${b.left}px, ${b.top}px)`, width: b.width, height: b.height }}>
              rendering…
            </div>
          );
        }
        if (n.kind === 'generation') {
          const status = String(n.props.status ?? '');
          const failed = !GENERATING.has(status);
          if (!failed && hover !== n.id) return null;
          const b = screen(r);
          if (!visible(b) || b.width < 60) return null;
          const model = String(n.props.model ?? '');
          const prompt = String((n.props.params as any)?.prompt ?? n.name ?? '');
          const job = jobs[n.id];
          const queue = job?.status === 'queued' && job.queue_position != null ? ` · #${job.queue_position}` : '';
          return (
            <div key={`g-${n.id}`} className={`gen-info ${failed ? 'failed' : ''}`} style={{ transform: `translate(${b.left}px, ${b.top}px)`, width: b.width, height: b.height }}>
              {hover === n.id && (
                <div className="gen-text">
                  <div>{model}{failed ? '' : ` · ${status || 'queued'}${queue}`}</div>
                  {failed && n.props.error ? <div>{String(n.props.error).slice(0, 200)}</div> : <div className="gen-prompt">{prompt}</div>}
                </div>
              )}
              {failed && (
                <button
                  className="link gen-retry"
                  title="ask the agent to retry this generation"
                  onClick={() => sendChatMessage(`retry generation ${n.id}`, [{ kind: 'node', node: n.id }])}
                >
                  [retry]
                </button>
              )}
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
          <div key={`c-${c.node}`} className="claim-label" style={{ transform: `translate(${b.left + b.width}px, ${b.top - 16}px) translateX(-100%)` }}>
            @{c.actor}
          </div>
        );
      })}
      {selBox && visible(selBox) && !promptOpen && !editing && (
        <button
          className="link sparkle"
          title="ask about the selection ( / )"
          style={{ transform: `translate(${Math.min(selBox.left + selBox.width + 4, camera.width - 22)}px, ${Math.max(2, selBox.top - 18)}px)` }}
          onClick={openPrompt}
          onPointerDown={(e) => e.stopPropagation()}
        >
          [✦]
        </button>
      )}
    </div>
  );
}
