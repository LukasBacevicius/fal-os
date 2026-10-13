// Agent activity on the canvas (docs/UX-V1.md §4). The host sends one `activity` event per
// agent read / write / edit / delete with node ids; entries expire after 30 s or when the
// agent finishes (`activity.clear`). Each event plays a one-shot effect in the engine overlay
// (a 1 px outline in the chat's color that holds, then steps down), and the frames/groups
// agents are working in get a steady outline. Agents never move the camera.

import { getState, setState } from './store';
import { worldRects, type Rect } from './geometry';
import type { NodeView } from './types';

export type ActivityOp = 'read' | 'write' | 'edit' | 'delete';

export interface ActivityEntry {
  session: string;
  agent: string;
  chat: string | null;
  color: string;
  op: ActivityOp;
  nodes: string[];
  /** Host timestamp (ms). */
  at: number;
  /** Local arrival (ms, performance clock): drives expiry. */
  recv: number;
}

export const ACTIVITY_TTL_MS = 30_000;

/** Last known world rects of nodes removed recently, so delete ghosts have a place even when
 *  the doc update arrives before the activity event. */
const graveyard = new Map<string, { rect: Rect; at: number }>();
let lastRects = new Map<string, Rect>();

/** Called on every node list refresh. */
export function trackNodes(nodes: NodeView[]) {
  const rects = worldRects(nodes);
  const now = performance.now();
  for (const [id, r] of lastRects) if (!rects.has(id)) graveyard.set(id, { rect: r, at: now });
  for (const [id, g] of graveyard) if (now - g.at > 10_000 || rects.has(id)) graveyard.delete(id);
  lastRects = rects;
}

/** Host `activity` event. */
export function onActivity(m: any) {
  const op = m.op as ActivityOp;
  if (!['read', 'write', 'edit', 'delete'].includes(op) || !Array.isArray(m.nodes)) return;
  const nodes = (m.nodes as unknown[]).filter((x): x is string => typeof x === 'string');
  const color = typeof m.color === 'string' && m.color ? m.color : '#8a8a8a';
  const e: ActivityEntry = {
    session: String(m.session ?? ''), agent: String(m.agent ?? ''), chat: typeof m.chat === 'string' ? m.chat : null,
    color, op, nodes, at: typeof m.at === 'number' ? m.at : Date.now(), recv: performance.now(),
  };
  const app = getState().app;
  if (app) {
    const rects: Record<string, Rect> = {};
    for (const id of nodes) {
      const r = lastRects.get(id) ?? graveyard.get(id)?.rect;
      if (r) rects[id] = r;
    }
    app.activity({ op, nodes, color, rects });
  }
  setState((s) => ({ activity: [...s.activity.filter((a) => performance.now() - a.recv < ACTIVITY_TTL_MS), e].slice(-200) }));
  schedule();
}

/** Host `activity.clear`: a session finished (some or all of its nodes). */
export function onActivityClear(m: any) {
  const session = String(m.session ?? '');
  const nodes: string[] | null = Array.isArray(m.nodes) ? m.nodes : null;
  setState((s) => ({
    activity: s.activity
      .map((a) => (a.session !== session ? a : nodes ? { ...a, nodes: a.nodes.filter((n) => !nodes.includes(n)) } : null))
      .filter((a): a is ActivityEntry => !!a && a.nodes.length > 0),
  }));
}

let timer = 0;
function schedule() {
  if (timer) return;
  timer = window.setInterval(() => {
    const now = performance.now();
    const { activity } = getState();
    const live = activity.filter((a) => now - a.recv < ACTIVITY_TTL_MS);
    if (live.length !== activity.length) setState({ activity: live });
    if (!live.length) {
      window.clearInterval(timer);
      timer = 0;
    }
  }, 1000);
}

/** The frame or group each active agent session is working in: the nearest container of
 *  the nodes in its latest write/edit (or read), in its chat's color. */
export function workingSet(activity: ActivityEntry[], nodes: NodeView[]): { node: string; color: string }[] {
  const byId = new Map(nodes.map((n) => [n.id, n]));
  const container = (id: string): string | null => {
    for (let n = byId.get(id); n; n = n.parent ? byId.get(n.parent) : undefined) if (n.kind === 'frame' || n.kind === 'group') return n.id;
    return null;
  };
  const latest = new Map<string, ActivityEntry>();
  for (const a of activity) if (a.op !== 'delete') latest.set(a.session, a);
  const out = new Map<string, string>();
  for (const a of latest.values()) {
    for (const id of a.nodes) {
      const c = container(id);
      if (c) {
        out.set(c, a.color);
        break;
      }
    }
  }
  return [...out].map(([node, color]) => ({ node, color }));
}

/** Node id → chat color for nodes agents touched recently (layers dots). */
export function activityColors(activity: ActivityEntry[]): Map<string, string> {
  const m = new Map<string, string>();
  for (const a of activity) for (const n of a.nodes) m.set(n, a.color);
  return m;
}

export function setReduceMotion(on: boolean) {
  try {
    localStorage.setItem('falos.reduceMotion', on ? '1' : '0');
  } catch {
    /* private mode */
  }
  setState({ reduceMotion: on });
  getState().app?.setReduceMotion(on);
}
