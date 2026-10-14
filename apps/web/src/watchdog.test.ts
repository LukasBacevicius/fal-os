import { describe, expect, it } from 'vitest';
import { reconcileJobs, sweepMarkers, DONE_JOB_MS, UNKNOWN_SESSION_MS, type Markers } from './watchdog';
import { ACTIVITY_TTL_MS, type ActivityEntry } from './activity';
import type { NodeView } from './types';

const node = (id: string, kind: NodeView['kind'], props: Record<string, unknown> = {}): NodeView => ({
  id, kind, name: id, x: 0, y: 0, w: 10, h: 10, parent: null, depth: 0, children: [], createdBy: 'a', updatedBy: 'a', text: null, props,
});
const act = (session: string, recv: number): ActivityEntry => ({ session, agent: 'a', chat: null, color: '#888', op: 'write', nodes: ['n'], at: 0, recv });
const empty: Markers = { activity: [], jobs: {}, claims: [] };

describe('marker watchdog', () => {
  it('expires activity by TTL and by its session going away', () => {
    const m = { ...empty, activity: [act('s1', 0), act('gone', 0), act('old', -ACTIVITY_TTL_MS), act('new', 9_000)] };
    const out = sweepMarkers(m, { now: 10_000, nodes: [], live: new Set(['s1', 'old']) });
    // `gone` isn't a live session (and is past the grace), `old` timed out, `new` is too
    // recent to judge.
    expect(out.activity?.map((a) => a.session)).toEqual(['s1', 'new']);
    expect(sweepMarkers({ ...empty, activity: [act('x', 10_000 - UNKNOWN_SESSION_MS)] }, { now: 10_000, nodes: [], live: new Set() }).activity).toEqual([]);
    // No host: only the TTL applies.
    expect(sweepMarkers({ ...empty, activity: [act('x', 0)] }, { now: 1_000, nodes: [], live: null })).toEqual({});
  });

  it('drops job progress whose tile stopped generating or is gone', () => {
    const jobs = {
      run: { node: 'run', model: 'm', status: 'running' },
      failedTile: { node: 'failedTile', model: 'm', status: 'running' },
      converted: { node: 'converted', model: 'm', status: 'running' },
      deleted: { node: 'deleted', model: 'm', status: 'queued' },
      fin: { node: 'fin', model: 'm', status: 'done', seen: 0 },
    };
    const nodes = [node('run', 'generation', { status: 'running' }), node('failedTile', 'generation', { status: 'failed' }), node('converted', 'image'), node('fin', 'image')];
    const out = sweepMarkers({ ...empty, jobs }, { now: 1_000, nodes, live: null });
    expect(Object.keys(out.jobs ?? {})).toEqual(['run', 'fin']);
    // Terminal entries go after a minute.
    const later = sweepMarkers({ ...empty, jobs: { fin: jobs.fin } }, { now: DONE_JOB_MS, nodes, live: null });
    expect(later.jobs).toEqual({});
  });

  it('drops claims on nodes that are gone', () => {
    const out = sweepMarkers({ ...empty, claims: [{ node: 'a', actor: 'x' }, { node: 'b', actor: 'x' }] }, { now: 0, nodes: [node('a', 'image')], live: null });
    expect(out.claims).toEqual([{ node: 'a', actor: 'x' }]);
  });

  it('keeps only the jobs the host still runs after a reconnect', () => {
    const jobs = { a: { node: 'a', model: 'm', status: 'running' }, b: { node: 'b', model: 'm', status: 'queued' }, c: { node: 'c', model: 'm', status: 'failed' } };
    const out = reconcileJobs(jobs, [{ node: 'b', status: 'running' }, { node: 'd', status: 'queued' }]);
    expect(out).toEqual({ b: { node: 'b', model: 'm', status: 'running' }, c: jobs.c, d: { node: 'd', model: '', status: 'queued' } });
  });
});
