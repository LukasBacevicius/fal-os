import { describe, expect, it } from 'vitest';
import { lineageGraph, lineageOf, matchesFilter, parentRefs, sourceOf, validScale } from './out';
import { filesOf, transcript } from './chat';
import type { NodeView } from './types';

const node = (id: string, asset: string | null, props: Record<string, unknown> = {}): NodeView => ({
  id, kind: 'image', name: id, x: 0, y: 0, w: 10, h: 10, parent: null, depth: 0, children: [], createdBy: 'a', updatedBy: 'a', text: null,
  props: asset ? { asset, ...props } : props,
});

describe('provenance', () => {
  it('reads old and new lineage parents', () => {
    expect(parentRefs({ parents: ['sha256:a'] })).toEqual([{ asset: 'sha256:a', role: 'input' }]);
    expect(parentRefs({ parents: [{ asset: 'sha256:b', node: 'n1', role: 'mask' }] })).toEqual([{ asset: 'sha256:b', node: 'n1', role: 'mask' }]);
    expect(parentRefs(undefined)).toEqual([]);
  });

  it('classifies sources', () => {
    expect(sourceOf({ id: 'x' })).toBe('import');
    expect(sourceOf({ lineage: { model: 'import' } })).toBe('import');
    expect(sourceOf({ lineage: { model: 'fal-ai/flux' } })).toBe('generate');
    expect(sourceOf({ source: 'derive' })).toBe('derive');
    expect(sourceOf({ lineage: { kind: 'render' } })).toBe('render');
    expect(sourceOf(null)).toBe(null);
  });

  it('walks ancestors and descendants', () => {
    // a → b (by asset) → c (by node ref); d unrelated
    const nodes = [node('a', 'sha256:a'), node('b', 'sha256:b'), node('c', 'sha256:c'), node('d', 'sha256:d')];
    const recs: Record<string, any> = {
      b: { lineage: { parents: ['sha256:a'] } },
      c: { lineage: { parents: [{ asset: 'sha256:old', node: 'b' }] } },
    };
    const g = lineageGraph(nodes, (id) => recs[id] ?? null);
    expect([...lineageOf(['b'], g)].sort()).toEqual(['a', 'b', 'c']);
    expect([...lineageOf(['d'], g)]).toEqual(['d']);
  });

  it('filters', () => {
    expect(matchesFilter(node('a', 'sha256:a'), 'inputs', 'import')).toBe(true);
    expect(matchesFilter(node('a', 'sha256:a', { role: 'final' }), 'inputs', 'import')).toBe(false);
    expect(matchesFilter(node('a', 'sha256:a', { role: 'final' }), 'finals', 'generate')).toBe(true);
    expect(matchesFilter(node('a', 'sha256:a'), 'derived', 'render')).toBe(true);
  });

  it('accepts Paper scale grammar', () => {
    for (const s of ['', '2x', '0.5x', '1080p', '512w', '512h']) expect(validScale(s)).toBe(true);
    for (const s of ['2', 'x2', '10q']) expect(validScale(s)).toBe(false);
  });
});

describe('files event', () => {
  it('becomes file cards in the final section', () => {
    const files = filesOf({ files: [{ path: '/p/exports/a_reel.mp4', bytes: 10, preset: 'reel', node: 'n1' }, { nope: 1 }] });
    expect(files).toEqual([{ path: '/p/exports/a_reel.mp4', name: 'a_reel.mp4', node: 'n1', preset: 'reel', w: undefined, h: undefined, bytes: 10, sha256: undefined }]);
    const t = transcript({ chat: 'c', seq: 0, events: [{ kind: 'user', text: 'export' }, { kind: 'files', files: [{ path: '/x/y.png' }], dir: '/x' }] as any });
    expect(t.items.map((i) => i.type)).toEqual(['user', 'files']);
  });
});
