// Asset library client: `library.*` frames on the host's `/workspace` socket (see
// docs/ASSETS-V1.md §7), the library view state, and placing assets on the canvas.

import { useSyncExternalStore } from 'react';
import { getState, setState, toast } from './store';
import { setLibraryHandler, workspaceHost, workspaceSend } from './projects';
import { addPlaceholder, finishPlaceholder, frameRect, gridLayout, type GhostItem } from './dnd';
import { worldRects } from './geometry';
import { addContext } from './controller';

export interface LibItem {
  sha: string;
  kind: 'image' | 'video' | 'audio' | 'model3d' | 'other';
  mime: string | null;
  w: number | null;
  h: number | null;
  duration: number | null;
  source: 'import' | 'generate' | 'derive' | 'render';
  role: string | null;
  prompt: string | null;
  model: string | null;
  caption: string | null;
  palette: string[] | null;
  favorite: boolean;
  fal: { assetRecordId: string | null; url: string | null; synced: boolean } | null;
  projects: string[];
  thumb: string;
  thumb_lg: string;
  strip?: string;
  created: number;
  name: string | null;
  tags: string[];
  local_only: boolean;
  src: string | null;
  thumb_state: string;
  bytes?: number | null;
  /** Lineage summary from the doc model (op, recipe, parents, actor). */
  lineage: string | null;
}

/** Filter words: this project's inputs / generated / derived / finals, or everything. */
export type View = 'project' | 'inputs' | 'generated' | 'derived' | 'finals' | 'all' | 'fav' | 'shown';
export const VIEWS: View[] = ['project', 'inputs', 'generated', 'derived', 'finals', 'all', 'fav'];

export interface LibState {
  view: View;
  search: string;
  /** "similar to" this sha (from a tile), cleared by typing. */
  similar: string | null;
  items: LibItem[];
  total: number;
  /** A query is in flight (or its semantic half). */
  loading: boolean;
  selected: string[];
  cursor: number;
  /** Assets an agent asked to show (`library.show`). */
  shown: string[] | null;
  quick: boolean;
  fal: 'connected' | 'off' | 'unknown';
  writes: 'ok' | 'unavailable';
  /** ms from sending the query to the first results (perf). */
  lastQueryMs: number;
}

let st: LibState = {
  view: 'project',
  search: '',
  similar: null,
  items: [],
  total: 0,
  loading: false,
  selected: [],
  cursor: 0,
  shown: null,
  quick: false,
  fal: 'unknown',
  writes: 'ok',
  lastQueryMs: 0,
};
const subs = new Set<() => void>();
export const lib = () => st;
export function setLib(p: Partial<LibState>) {
  st = { ...st, ...p };
  subs.forEach((f) => f());
}
export function useLib<T>(sel: (s: LibState) => T): T {
  return useSyncExternalStore(
    (f) => {
      subs.add(f);
      return () => subs.delete(f);
    },
    () => sel(st),
  );
}

/** Host URL of a `/thumbs/…` or `/assets/…` path (token in the query: `<img>` can't send headers). */
export function hostUrl(path: string | null | undefined): string {
  if (!path) return '';
  if (/^https?:/.test(path)) return path;
  const h = workspaceHost();
  if (!h) return '';
  return `http://${h.addr}${path}${path.includes('?') ? '&' : '?'}token=${encodeURIComponent(h.token)}`;
}

// ---- queries --------------------------------------------------------------------------

let seq = 0;
let latest = '';
let sentAt = 0;

function buildQuery(): Record<string, unknown> {
  const { view, search, similar, shown } = st;
  const project = getState().project;
  const q: Record<string, unknown> = { limit: FIRST_PAGE };
  const scoped = view !== 'all' && view !== 'fav' && view !== 'shown';
  q.project = scoped && project ? project : '*';
  if (view === 'inputs') q.role = ['input'];
  if (view === 'generated') q.source = ['generate'];
  if (view === 'derived') q.source = ['derive', 'render'];
  if (view === 'finals') q.role = ['final'];
  if (view === 'fav') q.favorite = true;
  if (view === 'shown') q.shas = shown ?? [];
  const s = search.trim();
  if (similar) q.similar_to = similar;
  else if (/^#[0-9a-f]{3}([0-9a-f]{3})?$/i.test(s)) q.color = s;
  else if (s) q.text = s;
  return q;
}

/** The first page is small so the first screen paints fast; the rest follows at once. */
const FIRST_PAGE = 300;
let rest = '';
let lastQuery: Record<string, unknown> = {};

export function runQuery() {
  if (!getState().library) return;
  const id = `lq${++seq}`;
  latest = id;
  rest = '';
  sentAt = performance.now();
  lastQuery = buildQuery();
  if (workspaceSend({ type: 'library.query', id, query: lastQuery })) setLib({ loading: true });
}

let changeTimer = 0;
setLibraryHandler((m: any) => {
  switch (m.type) {
    case 'library.open':
      if (getState().library) runQuery();
      break;
    case 'library.results':
      if (m.id === rest && rest) {
        const have = new Set(st.items.map((i) => i.sha));
        setLib({ items: [...st.items, ...((m.items ?? []) as LibItem[]).filter((i) => !have.has(i.sha))], loading: false });
        return;
      }
      if (m.id !== latest) return;
      if (m.next && !m.partial) {
        rest = `${m.id}r`;
        workspaceSend({ type: 'library.query', id: rest, query: { ...lastQuery, cursor: m.next, limit: 10000, semantic: false } });
      }
      {
        const items = (m.items ?? []) as LibItem[];
        const keep = new Set(items.map((i) => i.sha));
        setLib({
          items,
          total: m.total ?? items.length,
          loading: !!m.partial,
          selected: st.selected.filter((s) => keep.has(s)),
          cursor: Math.min(st.cursor, Math.max(0, items.length - 1)),
          lastQueryMs: st.loading ? Math.round(performance.now() - sentAt) : st.lastQueryMs,
        });
      }
      break;
    case 'library.changed':
      window.clearTimeout(changeTimer);
      changeTimer = window.setTimeout(runQuery, 300);
      break;
    case 'library.show':
      setLib({ shown: m.shas ?? [], view: 'shown', search: '', similar: null, selected: [], cursor: 0 });
      setState({ library: 'full' });
      runQuery();
      break;
    case 'library.status':
      setLib({ fal: m.fal === 'connected' ? 'connected' : 'off', writes: m.writes === 'unavailable' ? 'unavailable' : 'ok' });
      break;
    case 'library.placed':
      placed(m);
      break;
    case 'library.uploading': {
      const n = (m.queued ?? []).length;
      const sk = (m.skipped ?? []).length;
      toast(`uploading ${n} to fal${sk ? ` · ${sk} skipped` : ''}`);
      break;
    }
    case 'error':
      if (m.id && pendingPlace.has(m.id)) {
        pendingPlace.get(m.id)!.done?.();
        pendingPlace.delete(m.id);
      }
      setLib({ loading: false });
      toast(`library: ${m.message ?? 'error'}`);
      break;
  }
});

export function setView(view: View) {
  setLib({ view, selected: [], cursor: 0, similar: null, shown: view === 'shown' ? st.shown : null });
  runQuery();
}

let searchTimer = 0;
export function setSearch(search: string) {
  setLib({ search, similar: null });
  window.clearTimeout(searchTimer);
  searchTimer = window.setTimeout(runQuery, 120);
}

export function findSimilar(sha: string) {
  setLib({ similar: sha, search: '', view: st.view === 'shown' ? 'all' : st.view, cursor: 0, selected: [] });
  runQuery();
}

export function openLibrary(mode: 'panel' | 'full') {
  const cur = getState().library;
  setState({ library: cur === mode ? null : mode });
}

// ---- writes ---------------------------------------------------------------------------

const targets = () => (st.selected.length ? st.selected : st.items[st.cursor] ? [st.items[st.cursor].sha] : []);

export function toggleFavorite() {
  const shas = targets();
  if (!shas.length) return;
  const value = !st.items.find((i) => i.sha === shas[0])?.favorite;
  workspaceSend({ type: 'library.favorite', id: `f${++seq}`, shas, value });
}

export function tagSelected() {
  const shas = targets();
  if (!shas.length) return;
  const t = window.prompt('tags (space separated; -tag removes)');
  if (!t) return;
  const words = t.split(/\s+/).filter(Boolean);
  workspaceSend({ type: 'library.tag', id: `t${++seq}`, shas, add: words.filter((w) => !w.startsWith('-')), remove: words.filter((w) => w.startsWith('-')).map((w) => w.slice(1)) });
}

export function collectSelected() {
  const shas = targets();
  if (!shas.length) return;
  const c = window.prompt('collection');
  if (c?.trim()) workspaceSend({ type: 'library.collect', id: `c${++seq}`, shas, collection: c.trim() });
}

export function toggleLocalOnly() {
  const shas = targets();
  if (!shas.length) return;
  const value = !st.items.find((i) => i.sha === shas[0])?.local_only;
  workspaceSend({ type: 'library.local_only', id: `l${++seq}`, shas, value });
}

/** Uploading user inputs to fal is explicit; the first upload in a project asks. */
export function uploadSelected() {
  const shas = targets();
  if (!shas.length) return;
  if (st.fal !== 'connected') return toast('fal is not connected');
  if (st.writes === 'unavailable') return toast('fal writes are not available');
  const project = getState().project;
  const key = `falos.uploadOk.${project}`;
  let ok = false;
  try {
    ok = localStorage.getItem(key) === '1';
  } catch {
    /* private mode */
  }
  if (!ok) {
    if (!window.confirm(`upload ${shas.length} file${shas.length > 1 ? 's' : ''} to your fal library?`)) return;
    try {
      localStorage.setItem(key, '1');
    } catch {
      /* private mode */
    }
  }
  workspaceSend({ type: 'library.upload', id: `u${++seq}`, shas, project });
}

/** World size a placed asset gets (the host's `place` rule: natural aspect, ≤ 960 wide). */
export function placedSize(item: Pick<LibItem, 'kind' | 'w' | 'h'> | undefined): [number, number] {
  if (item?.kind === 'audio') return [480, 96];
  if (item?.w && item?.h) {
    const w = Math.min(960, item.w);
    return [w, (w * item.h) / item.w];
  }
  return [512, 512];
}

interface PlaceOpts {
  /** Width to place at (the host keeps the aspect). */
  w?: number;
  /** Move into this frame once the node exists (drop into a frame). */
  frame?: string | null;
  /** Attach the placed node to the chat as a chip. */
  attach?: boolean;
  /** Called when the placement is on the canvas (or failed): finishes drop placeholders. */
  done?: () => void;
}
const pendingPlace = new Map<string, PlaceOpts>();

/** New placement of an asset on the open canvas (world top-left; centered when omitted). */
export function place(sha: string, at?: [number, number], opts: PlaceOpts = {}) {
  const { app, project, camera } = getState();
  if (!project) {
    opts.done?.();
    return toast('open a project first');
  }
  const item = st.items.find((i) => i.sha === sha);
  let x: number | undefined;
  let y: number | undefined;
  const [w, h] = placedSize(item);
  const p = at ?? (app ? (Array.from(app.screenToWorld(camera.width / 2, camera.height / 2)) as [number, number]) : null);
  if (p) {
    x = Math.round(p[0] - (at ? 0 : w / 2));
    y = Math.round(p[1] - (at ? 0 : h / 2));
  }
  const id = `p${++seq}`;
  if (opts.frame || opts.attach || opts.done) pendingPlace.set(id, opts);
  workspaceSend({ type: 'library.place', id, sha, project, x, y, w: opts.w });
}

/** Places several assets as a grid (the same layout as a drop) centered on `at` (world;
 *  the view center when omitted). ⏎ / double-click / `[place]` in the library. */
export function placeMany(shas: string[], at?: [number, number], opts: Omit<PlaceOpts, 'w'> = {}) {
  const { app, camera } = getState();
  if (!shas.length) return;
  const c = at ?? (app ? (Array.from(app.screenToWorld(camera.width / 2, camera.height / 2)) as [number, number]) : [0, 0]);
  const items = shas.map((sha) => st.items.find((i) => i.sha === sha));
  const sizes = items.map(placedSize);
  const rects = gridLayout(sizes, c, frameRect(opts.frame));
  let left = shas.length;
  const one = () => {
    if (--left === 0) opts.done?.();
  };
  shas.forEach((sha, i) => place(sha, [rects[i][0], rects[i][1]], { ...opts, w: items[i]?.kind !== 'audio' ? Math.round(rects[i][2]) : undefined, done: one }));
}

/** Runs `f(true)` once node `id` is in the local doc (placements arrive over the sync
 *  socket after the workspace answer), `f(false)` after ~3 s without it. */
function whenNode(id: string, f: (ok: boolean) => void, tries = 30) {
  if (getState().nodes.some((n) => n.id === id)) return f(true);
  if (tries <= 0) return f(false);
  window.setTimeout(() => whenNode(id, f, tries - 1), 100);
}

function placed(m: { id: string; node?: string }) {
  const opts = pendingPlace.get(m.id);
  if (!opts) return toast('placed on the canvas');
  pendingPlace.delete(m.id);
  if (!m.node) return opts.done?.();
  const node = m.node;
  whenNode(node, (ok) => {
    const { app, nodes } = getState();
    if (ok && app && opts.frame) {
      const rects = worldRects(nodes);
      const f = rects.get(opts.frame);
      const r = rects.get(node);
      if (f && r) app.apply([{ op: 'move', id: node, parent: opts.frame }, { op: 'update', id: node, props: { x: Math.round(r[0] - f[0]), y: Math.round(r[1] - f[1]) } }]);
    }
    if (ok && opts.attach) addContext([{ kind: 'node', node }]);
    opts.done?.();
  });
}

export const ASSET_MIME = 'application/x-falos-asset';

/** Ghost for a library tile being dragged: true placed size and its thumbnail. */
export function libGhost(sha: string): GhostItem {
  const it = st.items.find((i) => i.sha === sha);
  const [w, h] = placedSize(it);
  return { w, h, kind: it?.kind ?? 'image', thumb: it ? hostUrl(it.thumb) : undefined, name: it?.name ?? undefined };
}

/** Assets in a drag (newline-separated shas). */
export function draggedShas(dt: DataTransfer): string[] {
  return dt.getData(ASSET_MIME).split('\n').filter(Boolean);
}

/** Stage drop: library tiles dragged onto the canvas, laid out as a grid at the pointer (in
 *  `frame` when over one), with placeholders until the nodes arrive. True when handled. */
export function libraryDrop(e: React.DragEvent, rect: DOMRect, frame: string | null): boolean {
  const shas = draggedShas(e.dataTransfer);
  if (!shas.length) return false;
  const app = getState().app;
  if (!app) return true;
  const [wx, wy] = app.screenToWorld(e.clientX - rect.left, e.clientY - rect.top);
  const ghosts = shas.map(libGhost);
  const ph = addPlaceholder(gridLayout(ghosts.map((g) => [g.w, g.h]), [wx, wy], frameRect(frame)), ghosts.map((g) => g.thumb));
  placeMany(shas, [wx, wy], { frame, done: () => finishPlaceholder(ph, 200) });
  return true;
}

export const GLYPH: Record<string, string> = { import: '↓', generate: '∷', derive: 'ƒ', render: '▶' };
