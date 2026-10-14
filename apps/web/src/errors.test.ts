import { describe, expect, it } from 'vitest';
import { plainError } from './errors';

describe('plain errors', () => {
  it('names the cause and one next step', () => {
    const cases: [string, string, string | null][] = [
      ['no fal key: put `FAL_KEY=…` in ~/.fal-os/fal.env', 'fal key missing · open setup', 'fal-key'],
      ['User is locked. Reason: Exhausted balance. Top up your balance at fal.ai/dashboard/billing.', 'out of credits · open fal billing', 'credits'],
      ["Claude isn't signed in (the login expired): run `claude` in a terminal and /login, then continue", 'Claude login expired · run claude auth login', 'claude-login'],
      ["Codex isn't signed in: run `npx codex login`", 'Codex login expired · run codex login', 'codex-login'],
      ['agent runtime disconnected', 'agent runtime stopped · continue when it is back', 'runtime'],
      ['interrupted: its job stopped (host restart or crash) · rerun it', 'interrupted by a host restart · rerun it', null],
      ['the director stopped mid-step', 'the agent stopped mid-step · continue', null],
    ];
    for (const [raw, text, wall] of cases) {
      const e = plainError(raw);
      expect(e.text).toBe(text);
      expect(e.wall).toBe(wall);
      expect(e.raw).toBe(raw);
    }
  });

  it('marks dropped connections and busy upstreams as transient', () => {
    for (const raw of ['fal queue submit: error sending request: connection refused', 'socket hang up', '503 Service Unavailable', 'HTTP 429 Too Many Requests', 'API Error: 529 overloaded']) {
      expect(plainError(raw).transient, raw).toBe(true);
    }
    expect(plainError('no fal key').transient).toBe(false);
  });

  it('keeps unknown errors to their first sentence', () => {
    expect(plainError('Something odd happened. Here is a lot more detail.').text).toBe('Something odd happened.');
    expect(plainError('{"type":"error","error":{}}').text).toBe('the agent stopped');
    expect(plainError('x'.repeat(300)).text.length).toBe(100);
  });
});
