import { describe, expect, it } from 'vitest';
import {
  actLabel, addChips, agentLine, applyEvent, applyHistory, applyList, chipFromPick, chipLabel, cleanError, emptyChat, groupTurns, removeChat,
  tabs, transcript, workSummary, type ChatEvent, type ChatState,
} from './chat';
import { timecode } from './format';

const run = (events: ChatEvent[], chat = 'c1'): ChatState => {
  let s = emptyChat();
  events.forEach((e, i) => (s = applyEvent(s, chat, i + 1, e, 1000 * i)));
  return s;
};

describe('chat', () => {
  it("an external agent's report stays visible when it keeps working after it", () => {
    const s = run([
      { kind: 'status', state: 'running' },
      { kind: 'text', agent: 'claude-1', delta: 'here is the board' },
      { kind: 'done', summary: '', nodes: [] },
      { kind: 'tool', agent: 'claude-1', id: 'x1', name: 'inbox', summary: 'waiting for you', state: 'running' },
    ], 'ext:claude-1');
    const turns = groupTurns(transcript(s.threads['ext:claude-1']).items);
    expect(turns[0].final.map((w) => (w as any).text)).toEqual(['here is the board']);
    expect(turns[1].work.map((w) => w.type)).toEqual(['act']);
  });

  it('a stopped turn ends with `stopped`, the next one with `done`', () => {
    const s = run([
      { kind: 'user', text: 'a', context: [] },
      { kind: 'turn', turn: 't1', state: 'start' } as any,
      { kind: 'status', state: 'running' },
      { kind: 'status', state: 'idle', error: 'stopped' } as any,
      { kind: 'turn', turn: 't1', state: 'end', nodes: [] } as any,
      { kind: 'user', text: 'b', context: [] },
      { kind: 'turn', turn: 't2', state: 'start' } as any,
      { kind: 'status', state: 'idle' },
      { kind: 'turn', turn: 't2', state: 'end', nodes: [] } as any,
    ]);
    const done = transcript(s.threads.c1).items.filter((i) => i.type === 'done') as any[];
    expect(done.map((d) => !!d.stopped)).toEqual([true, false]);
  });

  it('joins streamed text; subagent work goes to its own transcript with cards in the main one', () => {
    let s = emptyChat();
    s = { ...s, pending: { text: 'make an ad', context: [], at: 1, chat: null } };
    s = applyEvent(s, 'c1', 1, { kind: 'user', text: 'make an ad', context: [{ kind: 'node', node: 'a1' }] });
    expect(s.pending).toBeNull();
    expect(s.active).toBe('c1');
    s = applyEvent(s, 'c1', 2, { kind: 'status', state: 'running' });
    s = applyEvent(s, 'c1', 3, { kind: 'text', agent: 'director', delta: 'On ' });
    s = applyEvent(s, 'c1', 4, { kind: 'text', agent: 'director', delta: 'it.' });
    s = applyEvent(s, 'c1', 5, { kind: 'agent', agent: 'generator-1', role: 'fal generator', parent: 'director', state: 'started', status: 'hero shots' }, 10_000);
    s = applyEvent(s, 'c1', 6, { kind: 'tool', agent: 'generator-1', parent: 'director', id: 't1', name: 'generate', summary: 'flux', state: 'running' });
    s = applyEvent(s, 'c1', 7, { kind: 'tool', agent: 'generator-1', id: 't1', name: 'generate', state: 'done', nodes: ['x1'] });
    // Duplicate seq is ignored.
    s = applyEvent(s, 'c1', 7, { kind: 'tool', agent: 'generator-1', id: 't1', name: 'generate', state: 'error' });
    const t = transcript(s.threads.c1);
    expect(t.items.map((i) => i.type)).toEqual(['user', 'text', 'sub']);
    expect((t.items[1] as any).text).toBe('On it.');
    expect(t.running).toBe(true);
    expect(t.swarm).toEqual([{ agent: 'generator-1', role: 'fal generator', status: 'hero shots', tools: 1 }]);
    const sub = transcript(s.threads.c1, 'generator-1');
    expect(sub.items.map((i) => i.type)).toEqual(['act']);
    expect((sub.items[0] as any).calls[0]).toMatchObject({ state: 'done', nodes: ['x1'] });
    s = applyEvent(s, 'c1', 8, { kind: 'agent', agent: 'generator-1', state: 'done' }, 48_000);
    s = applyEvent(s, 'c1', 9, { kind: 'done', summary: 'On it.', nodes: ['x1'] });
    const end = transcript(s.threads.c1);
    expect(end.swarm).toEqual([]);
    expect(end.running).toBe(false);
    const card = end.items.find((i) => i.type === 'sub' && i.phase === 'end') as any;
    expect(card).toMatchObject({ tools: 1, ms: 38_000, nodes: ['x1'] });
    // No duplicate final text: the done line drops the summary when the director spoke.
    expect((end.items[end.items.length - 1] as any).summary).toBe('');
  });

  it('merges consecutive calls into one past-tense line and hides housekeeping', () => {
    const s = run([
      { kind: 'user', text: 'go' },
      { kind: 'tool', agent: 'director', id: 's', name: 'session', state: 'done' },
      { kind: 'tool', agent: 'director', id: 'r1', name: 'read', state: 'done', nodes: ['a', 'b'] },
      { kind: 'tool', agent: 'director', id: 'r2', name: 'read', state: 'done', nodes: ['c'] },
      { kind: 'tool', agent: 'director', id: 'g1', name: 'fal_model', summary: 'fal-ai/flux/schnell · cat · ~$0.01', state: 'done', nodes: ['i1'] },
      { kind: 'tool', agent: 'director', id: 'g2', name: 'fal_model', summary: 'fal-ai/flux/schnell · dog · ~$0.01', state: 'done', nodes: ['i2'] },
      { kind: 'thinking_done', agent: 'director', seconds: 12 },
    ]);
    const t = transcript(s.threads.c1);
    const acts = t.items.filter((i) => i.type === 'act') as any[];
    expect(acts).toHaveLength(2);
    expect(actLabel(acts[0], () => undefined)).toBe('looked at 3 nodes');
    expect(actLabel(acts[1], () => 'image')).toBe('generated 2 images (fal-ai/flux/schnell)');
    const turns = groupTurns(t.items);
    expect(turns).toHaveLength(1);
    expect(workSummary(turns[0].work)).toBe('thought 12s · 2 reads · 2 generations');
  });

  it('groups by turn: work, final text, done; queue, revert and errors', () => {
    const s = run([
      { kind: 'user', text: 'one' },
      { kind: 'turn', turn: 'T1', state: 'start' },
      { kind: 'text', agent: 'director', delta: 'let me look' },
      { kind: 'tool', agent: 'director', id: 'e1', name: 'edit', state: 'done', nodes: ['n1'] },
      { kind: 'queued', id: 'q1', text: 'two' },
      { kind: 'queued', id: 'q2', text: 'three' },
      { kind: 'dequeued', id: 'q2' },
      { kind: 'text', agent: 'director', delta: 'done it' },
      { kind: 'done', summary: 'done it', nodes: ['n1'] },
      { kind: 'turn', turn: 'T1', state: 'end', nodes: ['n1'] },
      { kind: 'reverted', turn: 'T1' },
      { kind: 'status', state: 'error', error: '[ede_diagnostic] {"x":1}', actions: ['continue'] },
    ]);
    const t = transcript(s.threads.c1);
    expect(t.queued).toEqual([{ id: 'q1', text: 'two' }]);
    const [turn] = groupTurns(t.items);
    expect(turn.work.map((w) => w.type)).toEqual(['text', 'act']);
    expect(turn.final.map((w) => (w as any).text)).toEqual(['done it']);
    expect(turn.done).toMatchObject({ turn: 'T1', reverted: true, nodes: ['n1'] });
    expect(turn.errors[0].actions).toEqual(['continue']);
    expect(cleanError(turn.errors[0].text)).toBe('the director stopped mid-step');
  });

  it('batch progress updates in place', () => {
    const s = run([
      { kind: 'user', text: 'pizza x200' },
      { kind: 'batch', id: 'b1', model: 'fal-ai/flux/schnell', total: 200, done: 0, failed: 0, running: 200 },
      { kind: 'batch', id: 'b1', model: 'fal-ai/flux/schnell', total: 200, done: 37, failed: 1, running: 162 },
    ]);
    const t = transcript(s.threads.c1);
    expect(t.items.filter((i) => i.type === 'batch')).toHaveLength(1);
    expect(t.batches[0]).toMatchObject({ done: 37, failed: 1, total: 200 });
  });

  it('tabs: open chats, running always, closed ones only in history; delete picks a neighbour', () => {
    let s = run([{ kind: 'user', text: 'a' }], 'a');
    s = applyEvent(s, 'b', 1, { kind: 'user', text: 'b' });
    s = applyEvent(s, 'c', 1, { kind: 'user', text: 'c' });
    s = applyList(s, [
      { chat: 'a', state: 'idle', updated: 1, open: true },
      { chat: 'b', state: 'running', updated: 2, open: false },
      { chat: 'c', state: 'idle', updated: 3, open: false },
    ]);
    s = { ...s, active: 'a' };
    expect(tabs(s)).toEqual(['a', 'b']);
    s = removeChat(s, 'a');
    expect(s.active).toBe('b');
    expect(s.order).toEqual(['b', 'c']);
  });

  it('history accepts bare events and envelopes', () => {
    const s = applyHistory(emptyChat(), [
      { chat: 'a', title: 'A', events: [{ kind: 'user', text: 'hi' }] },
      { chat: 'b', events: [{ seq: 4, ts: 5, event: { kind: 'user', text: 'yo' } }] },
    ]);
    expect(s.order).toEqual(['a', 'b']);
    expect(s.active).toBe('b');
    expect(s.threads.b.seq).toBe(4);
    expect(s.threads.b.events[0]._ts).toBe(5);
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

  it('dedupes verbs and agent names in activity lines', () => {
    const act = (name: string, summaries: string[]): any => ({
      type: 'act', key: '0', agent: 'director', verb: name === 'Bash' ? 'bash' : name === 'codex' ? 'codex' : name === 'generate' ? 'gen' : 'file',
      calls: summaries.map((summary, i) => ({ id: `${i}`, name, summary, state: 'done', nodes: [], cost: 0 })),
    });
    const k = () => undefined;
    expect(actLabel(act('Bash', ['ran Build a contact sheet']), k)).toBe('ran Build a contact sheet');
    expect(actLabel(act('Bash', ['npm test']), k)).toBe('ran npm test');
    expect(actLabel(act('Write', ['wrote FILM_SPEC.md']), k)).toBe('wrote FILM_SPEC.md');
    const mixed = act('Write', ['wrote FILM_SPEC.md', 'edited FILM_SPEC.md', 'edited src/a.ts']);
    mixed.calls[1].name = 'Edit';
    mixed.calls[2].name = 'Edit';
    expect(actLabel(mixed, k)).toBe('wrote FILM_SPEC.md, edited a.ts');
    expect(actLabel(act('Edit', ['Edit /x/y/z.md']), k)).toBe('edited z.md');
    expect(actLabel(act('codex', ['codex: fix the build']), k)).toBe('codex: fix the build');
    expect(actLabel(act('generate', ['generated 4 images (flux/schnell)']), k)).toBe('generated 4 images (flux/schnell)');
    expect(agentLine('motion', 'motion', 'motion · Build Remotion launch film')).toEqual({ role: '', status: 'Build Remotion launch film' });
    expect(agentLine('generator-2', 'fal generator', 'hero shots')).toEqual({ role: 'fal generator', status: 'hero shots' });
  });

  it('steered and subagent messages, capabilities', () => {
    const s = run([
      { kind: 'capabilities', available: ['opus'], missing: [{ name: 'codex', fix: 'npx @openai/codex login' }] },
      { kind: 'user', text: 'make a film' },
      { kind: 'status', state: 'running' },
      { kind: 'agent', agent: 'motion', role: 'motion', parent: 'director', state: 'started', status: 'writing the composition' },
      { kind: 'queued', id: 'q1', text: 'use red' },
      { kind: 'dequeued', id: 'q1', reason: 'steer' },
      { kind: 'user', text: 'use red', queued: 'q1' },
      { kind: 'user', text: 'slower cuts', agent: 'motion', steer: true },
    ]);
    const t = transcript(s.threads.c1);
    expect(t.caps?.missing).toEqual([{ name: 'codex', fix: 'npx @openai/codex login' }]);
    expect(t.queued).toEqual([]);
    const users = t.items.filter((i) => i.type === 'user') as any[];
    expect(users.map((u) => [u.text, u.delivered, u.to])).toEqual([['make a film', undefined, undefined], ['use red', 'steer', undefined], ['slower cuts', 'steer', 'motion']]);
    const sub = transcript(s.threads.c1, 'motion');
    expect(sub.items.filter((i) => i.type === 'user').map((i: any) => i.text)).toEqual(['slower cuts']);
    expect(t.agents.motion.status).toBe('writing the composition');
  });

  it('timecode', () => {
    expect(timecode(72.5)).toBe('1:12.50');
  });
});
