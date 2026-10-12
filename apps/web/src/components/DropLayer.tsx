// Drop feedback over the canvas (see dnd.ts): while files or library tiles are dragged over
// it, the canvas dims and sized ghosts (true size at the current zoom, laid out as the grid
// the drop will create) follow the pointer with a mono "drop to add N files" label; the
// frame under the pointer is highlighted by the engine. After a drop, placeholder tiles
// (sparse pixel dots, a 1px progress line) hold the spot until the media is in, then fade.
// Pointer moves only write transforms to existing elements (no React render per move).

import { memo, useEffect, useMemo, useRef, useState } from 'react';
import { getState, useStore } from '../store';
import { dnd, endDrag, ghostRects, useDnd, type DragSession } from '../dnd';
import { worldRects, type Rect } from '../geometry';

type Point = [number, number];

// ---- imperative pointer feed (Stage → DropLayer) ----------------------------------------

let feed: ((p: Point | null, frame: string | null) => void) | null = null;
let overCanvas = false;

/** The pointer (CSS px relative to the canvas) and the frame under it during a drag over
 *  the canvas; `null` when it leaves. */
export function dropOver(p: Point | null, frame: string | null = null) {
  overCanvas = !!p;
  getState().app?.setDropTarget(p ? frame : null);
  feed?.(p, frame);
  thumbFeed?.();
}

function screenRect(r: Rect): Rect {
  const app = getState().app!;
  const [x0, y0] = app.worldToScreen(r[0], r[1]);
  const [x1, y1] = app.worldToScreen(r[0] + r[2], r[1] + r[3]);
  return [x0, y0, x1 - x0, y1 - y0];
}

function label(s: DragSession, frame: string | null): string {
  const n = s.items.length;
  const what = s.source === 'files' ? `${n} file${n === 1 ? '' : 's'}` : `${n} asset${n === 1 ? '' : 's'}`;
  const f = frame ? getState().nodes.find((x) => x.id === frame) : null;
  return `drop to add ${what}${f ? ` into ${f.name || 'frame'}` : ''}`;
}

// ---- pixel dots -------------------------------------------------------------------------

/** Deterministic sparse dots for a tile (UI-SPEC generating state: 2–3 px gray squares). */
function dots(seed: number, n = 12): { x: number; y: number; s: number; d: number }[] {
  let h = seed * 2654435761 + 1013904223;
  const rnd = () => ((h = (h * 1664525 + 1013904223) >>> 0) / 4294967296);
  return Array.from({ length: n }, () => ({ x: rnd() * 100, y: rnd() * 100, s: rnd() < 0.5 ? 2 : 3, d: rnd() * 2.4 }));
}

const Dots = memo(function Dots({ seed }: { seed: number }) {
  const ds = useMemo(() => dots(seed), [seed]);
  return (
    <>
      {ds.map((d, i) => (
        <i key={i} className="ph-dot" style={{ left: `${d.x}%`, top: `${d.y}%`, width: d.s, height: d.s, animationDelay: `${d.d}s` }} />
      ))}
    </>
  );
});

// ---- the layer --------------------------------------------------------------------------

export function DropLayer() {
  const session = useDnd((s) => s.session);
  const placeholders = useDnd((s) => s.placeholders);
  const reduce = useStore((s) => s.reduceMotion);
  useStore((s) => s.camera); // placeholders follow the camera
  const [over, setOver] = useState(false);
  const ghosts = useRef<(HTMLDivElement | null)[]>([]);
  const tag = useRef<HTMLDivElement>(null);
  const repaint = useRef<() => void>(() => {});

  useEffect(() => {
    let raf = 0;
    let last: { p: Point | null; frame: string | null } = { p: null, frame: null };
    repaint.current = () => {
      if (!raf) raf = requestAnimationFrame(paint);
    };
    const paint = () => {
      raf = 0;
      const s = dnd().session;
      const { p, frame } = last;
      if (!s || !p || !getState().app) return;
      const rects = ghostRects(s.items, p, frame);
      rects.forEach((r, i) => {
        const el = ghosts.current[i];
        if (!el) return;
        const [x, y, w, h] = screenRect(r);
        el.style.transform = `translate(${x}px, ${y}px)`;
        el.style.width = `${w}px`;
        el.style.height = `${h}px`;
      });
      const t = tag.current;
      if (t) {
        t.textContent = label(s, frame);
        // Next to the pointer, flipped to stay inside the canvas.
        const { width, height } = getState().camera;
        const x = p[0] + 14 + t.offsetWidth > width - 4 ? p[0] - 14 - t.offsetWidth : p[0] + 14;
        const y = p[1] + 16 + t.offsetHeight > height - 4 ? p[1] - 12 - t.offsetHeight : p[1] + 16;
        t.style.transform = `translate(${Math.max(4, x)}px, ${y}px)`;
      }
    };
    feed = (p, frame) => {
      last = { p, frame };
      setOver(!!p);
      if (!raf) raf = requestAnimationFrame(paint);
    };
    return () => {
      feed = null;
      cancelAnimationFrame(raf);
    };
  }, []);

  // Ghost elements that just mounted (a new session, back over the canvas) get placed.
  useEffect(() => {
    if (session && over) repaint.current();
  }, [session, over]);

  const app = getState().app;
  return (
    <div className={`drop-layer ${reduce ? 'still' : ''}`}>
      {session && over && <div className="drop-veil" />}
      {session && over && session.items.map((g, i) => (
        <div key={i} ref={(el) => (ghosts.current[i] = el)} className="drop-ghost">
          {g.thumb ? <img src={g.thumb} alt="" draggable={false} /> : <span className="faint">[{g.kind}]</span>}
        </div>
      ))}
      {session && over && <div ref={tag} className="drop-tag" />}
      {app && placeholders.map((ph) => ph.rects.map((r, i) => {
        const [x, y, w, h] = screenRect(r);
        const done = i < ph.done;
        return (
          <div key={`${ph.id}-${i}`} className={`drop-ph ${ph.leaving ? 'out' : ''}`} style={{ transform: `translate(${x}px, ${y}px)`, width: w, height: h }}>
            {ph.thumbs[i] ? <img src={ph.thumbs[i]} alt="" draggable={false} /> : <Dots seed={ph.id * 31 + i} />}
            <div className={`ph-bar ${done ? 'full' : ''}`} />
          </div>
        );
      }))}
      <UploadBars />
    </div>
  );
}

/** A 1px line under nodes whose bytes are still uploading to the host. */
function UploadBars() {
  const uploads = useDnd((s) => s.uploads);
  const nodes = useStore((s) => s.nodes);
  useStore((s) => s.camera);
  const ids = Object.keys(uploads);
  const rects = useMemo(() => (ids.length ? worldRects(nodes) : null), [nodes, ids.length]);
  if (!ids.length || !rects || !getState().app) return null;
  return (
    <>
      {nodes.filter((n) => typeof n.props.asset === 'string' && uploads[n.props.asset as string] != null).map((n) => {
        const r = rects.get(n.id);
        if (!r) return null;
        const [x, y, w, h] = screenRect(r);
        return (
          <div key={`u-${n.id}`} className="upload-bar" title="uploading to the host" style={{ transform: `translate(${x}px, ${y + h - 1}px)`, width: w }}>
            <div style={{ width: `${Math.round(uploads[n.props.asset as string] * 100)}%` }} />
          </div>
        );
      })}
    </>
  );
}

// ---- the dragged thumbnail outside the canvas -------------------------------------------

let thumbFeed: (() => void) | null = null;

/** Library drags carry a blank native drag image; this small thumbnail follows the pointer
 *  instead (over the library, the chat…) and hands over to the true-size ghosts on the
 *  canvas. Also ends any drag session when the drag ends anywhere. */
export function DragThumb() {
  const session = useDnd((s) => s.session);
  const el = useRef<HTMLDivElement>(null);
  const at = useRef<Point | null>(null);

  useEffect(() => {
    const move = (e: DragEvent) => {
      at.current = [e.clientX, e.clientY];
      thumbFeed?.();
    };
    const end = () => endDrag();
    const leave = (e: DragEvent) => {
      // A Finder drag leaving the window.
      if (!e.relatedTarget && dnd().session?.source === 'files') endDrag();
    };
    document.addEventListener('dragover', move, true);
    document.addEventListener('drop', end, true);
    document.addEventListener('dragend', end, true);
    document.addEventListener('dragleave', leave, true);
    thumbFeed = () => {
      const e = el.current;
      if (!e) return;
      const p = at.current;
      e.style.opacity = p && !overCanvas ? '1' : '0';
      if (p) e.style.transform = `translate(${p[0] + 10}px, ${p[1] + 12}px)`;
    };
    return () => {
      document.removeEventListener('dragover', move, true);
      document.removeEventListener('drop', end, true);
      document.removeEventListener('dragend', end, true);
      document.removeEventListener('dragleave', leave, true);
      thumbFeed = null;
    };
  }, []);

  if (session?.source !== 'library') return null;
  const first = session.items[0];
  const n = session.items.length;
  return (
    <div ref={el} className="drag-thumb" style={{ opacity: 0 }}>
      {first?.thumb ? <img src={first.thumb} alt="" draggable={false} /> : <span className="faint">[{first?.kind}]</span>}
      {n > 1 && <span className="drag-count">{n}</span>}
    </div>
  );
}

/** A transparent drag image, so the browser doesn't draw its own translucent tile copy. */
export const BLANK_DRAG = (() => {
  if (typeof Image === 'undefined') return null as unknown as HTMLImageElement; // tests (node)
  const img = new Image();
  img.src = 'data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7';
  return img;
})();
