import { describe, expect, it } from 'vitest';
import { addChips, applyEvent, applyHistory, chipFromPick, chipLabel, emptyChat, transcript } from './chat';
import { timecode } from './format';

describe('chat', () => {
  it('joins streamed text and updates tool rows in place', () => {
    let s = emptyChat();
    s = { ...s, pending: { text: 'make an ad', context: [], at: 1, chat: null } };
    s = applyEvent(s, 'c1', 1, { kind: 'user', text: 'make an ad', context: [{ kind: 'node', node: 'a1' }] });
    expect(s.pending).toBeNull();
    expect(s.active).toBe('c1');
    s = applyEvent(s, 'c1', 2, { kind: 'status', state: 'running' });
    s = applyEvent(s, 'c1', 3, { kind: 'text', agent: 'director', delta: 'On ' });
    s = applyEvent(s, 'c1', 4, { kind: 'text', agent: 'director', delta: 'it.' });
    s = applyEvent(s, 'c1', 5, { kind: 'agent', agent: 'generator-1', role: 'fal generator', state: 'started', status: 'hero shots' });
    s = applyEvent(s, 'c1', 6, { kind: 'tool', agent: 'generator-1', id: 't1', name: 'generate', summary: 'flux', state: 'running' });
    s = applyEvent(s, 'c1', 7, { kind: 'tool', agent: 'generator-1', id: 't1', name: 'generate', state: 'done', nodes: ['x1'] });
    // Duplicate seq is ignored.
    s = applyEvent(s, 'c1', 7, { kind: 'tool', agent: 'generator-1', id: 't1', name: 'generate', state: 'error' });
    const t = transcript(s.threads.c1);
    expect(t.items.map((i) => i.type)).toEqual(['user', 'text', 'agent', 'tool']);
    expect((t.items[1] as any).text).toBe('On it.');
    expect((t.items[3] as any).state).toBe('done');
    expect((t.items[3] as any).nodes).toEqual(['x1']);
    expect(t.running).toBe(true);
    expect(t.swarm).toEqual([{ agent: 'generator-1', role: 'fal generator', status: 'hero shots' }]);
    s = applyEvent(s, 'c1', 8, { kind: 'agent', agent: 'generator-1', state: 'done' });
    s = applyEvent(s, 'c1', 9, { kind: 'done', summary: 'ok', nodes: ['x1'] });
    expect(transcript(s.threads.c1).swarm).toEqual([]);
    expect(transcript(s.threads.c1).running).toBe(false);
  });

  it('history accepts bare events and envelopes', () => {
    const s = applyHistory(emptyChat(), [
      { chat: 'a', title: 'A', events: [{ kind: 'user', text: 'hi' }] },
      { chat: 'b', events: [{ seq: 4, event: { kind: 'user', text: 'yo' } }] },
    ]);
    expect(s.order).toEqual(['a', 'b']);
    expect(s.active).toBe('b');
    expect(s.threads.b.seq).toBe(4);
    expect(s.runtime).toBe('connected');
  });

  it('chip labels and picks', () => {
    expect(chipLabel({ kind: 'node', node: 'a1' }, { name: 'hero 2', kind: 'image' })).toBe('img hero-2');
    expect(chipLabel({ kind: 'spot', node: 'a1', x: 0.42, y: 0.314 }, { name: 'hero-2', kind: 'image' })).toBe('spot hero-2 0.42,0.31');
    expect(chipLabel({ kind: 'frame', node: 'v', t: 3.24, box: [0, 0, 1, 1] }, { name: 'clip-1', kind: 'video' })).toBe('frame clip-1 @3.24s box');
    expect(chipFromPick({ nodeId: 'v', region: { kind: 'point', x: 0.5, y: 0.5 }, time: 1.5 })).toEqual({ kind: 'frame', node: 'v', t: 1.5 });
    expect(chipFromPick({ nodeId: 'i', region: { kind: 'box', x: 0.1, y: 0.2, w: 0.3, h: 0.4 }, time: null })).toEqual({ kind: 'region', node: 'i', box: [0.1, 0.2, 0.3, 0.4] });
    expect(addChips([{ kind: 'node', node: 'a' }], [{ kind: 'node', node: 'a' }, { kind: 'node', node: 'b' }])).toHaveLength(2);
  });

  it('timecode', () => {
    expect(timecode(72.5)).toBe('1:12.50');
  });
});
