// Agent activity can't linger (docs/research/brilliant-b102.md §9, "indicator watchdog").
// Every marker an agent or a job puts on screen has an owner and an expiry:
//
// | marker                         | owner             | cleared when                                   |
// |--------------------------------|-------------------|------------------------------------------------|
// | activity outline / working set | agent session     | `activity.clear`, 30 s TTL, session gone        |
// | agent cursor                   | agent session     | `session_closed`, 10 s idle fade, socket lost   |
// | claim outline                  | agent session     | host `claims`, node gone, socket lost           |
// | job progress (LED tile hover)  | generation job    | terminal status, node no longer generating,     |
// |                                |                   | not in the host's `hello.jobs` on (re)connect   |
// | chat tab `*`                   | chat run          | host `chat.list` (sent on every connect); not   |
// |                                |                   | shown while the host is unreachable             |
// | LED generating tile            | the host's job    | host watchdog: no live job for 60 s → `failed`  |
//
// `sweepMarkers` is the pure part (tested); `installWatchdog` runs it every 2 s.

import { getState, setState, type State } from './store';
import { ACTIVITY_TTL_MS, type ActivityEntry } from './activity';
import type { JobInfo } from './sync';
import type { NodeView } from './types';

export const ACTIVE_JOB = new Set(['queued', 'running', 'IN_QUEUE', 'IN_PROGRESS']);
/** Terminal job entries are kept this long (the failed tile's hover still names the model). */
export const DONE_JOB_MS = 60_000;
/** Activity from a session not in the host's session list survives this long. */
export const UNKNOWN_SESSION_MS = 5_000;

export interface Markers {
  activity: ActivityEntry[];
  jobs: Record<string, JobInfo>;
  claims: { node: string; actor: string }[];
}

export interface SweepContext {
  /** performance.now() */
  now: number;
  nodes: NodeView[];
  /** Live session ids on the host (null: not known, e.g. no host). */
  live: Set<string> | null;
}

/** The markers that should go: returns only the parts that changed. */
export function sweepMarkers(m: Markers, c: SweepContext): Partial<Markers> {
  const out: Partial<Markers> = {};
  const byId = new Map(c.nodes.map((n) => [n.id, n]));
  // A session the UI hasn't heard of yet gets a few seconds (its presence may be on the way).
  const owned = (a: ActivityEntry) => !c.live || !a.session || c.live.has(a.session) || c.now - a.recv < UNKNOWN_SESSION_MS;
  const activity = m.activity.filter((a) => c.now - a.recv < ACTIVITY_TTL_MS && owned(a));
  if (activity.length !== m.activity.length) out.activity = activity;
  let jobsChanged = false;
  const jobs: Markers['jobs'] = {};
  for (const [node, j] of Object.entries(m.jobs)) {
    const n = byId.get(node);
    const generating = n?.kind === 'generation' && n.props.status !== 'failed';
    const keep = ACTIVE_JOB.has(j.status) ? generating : !!n && (j.seen === undefined || c.now - j.seen < DONE_JOB_MS);
    if (keep) jobs[node] = j;
    else jobsChanged = true;
  }
  if (jobsChanged) out.jobs = jobs;
  const claims = m.claims.filter((cl) => byId.has(cl.node));
  if (claims.length !== m.claims.length) out.claims = claims;
  return out;
}

/** The host's jobs on (re)connect: active entries it doesn't list are over. */
export function reconcileJobs(jobs: Markers['jobs'], live: { node: string; status: string }[]): Markers['jobs'] {
  const now = new Map(live.map((j) => [j.node, j.status]));
  const out: Markers['jobs'] = {};
  for (const [node, j] of Object.entries(jobs)) {
    if (!ACTIVE_JOB.has(j.status)) out[node] = j;
    else if (now.has(node)) out[node] = { ...j, status: now.get(node)! };
  }
  for (const [node, status] of now) if (!out[node]) out[node] = { node, model: '', status };
  return out;
}

function liveSessions(s: State): Set<string> | null {
  if (s.conn !== 'connected') return null;
  return new Set(s.sessions.map((x) => x.id));
}

let timer = 0;

/** Sweeps stale markers every 2 s (cheap: only changed parts are written). */
export function installWatchdog(): () => void {
  window.clearInterval(timer);
  timer = window.setInterval(() => {
    const s = getState();
    const patch = sweepMarkers({ activity: s.activity, jobs: s.jobs, claims: s.claims }, { now: performance.now(), nodes: s.nodes, live: liveSessions(s) });
    if (!Object.keys(patch).length) return;
    if (patch.claims) s.app?.setClaims(patch.claims);
    setState(patch);
  }, 2_000);
  return () => window.clearInterval(timer);
}
