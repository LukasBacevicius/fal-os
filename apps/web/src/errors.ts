// Agent and generation errors as one plain sentence: the cause and one next step
// (docs/research/brilliant-b102.md §11), e.g. `fal key missing · open setup`. The raw text
// stays behind [details]. `transient` failures get one automatic retry; `wall`s hold a
// message until they clear (chatActions.ts).

export type Wall = 'fal-key' | 'credits' | 'claude-login' | 'codex-login' | 'runtime' | 'host';

export type NextStep =
  | { kind: 'setup'; label: string }
  | { kind: 'url'; label: string; url: string }
  | { kind: 'copy'; label: string; text: string }
  | { kind: 'continue'; label: string }
  | { kind: 'rerun'; label: string };

export interface PlainError {
  /** `<cause> · <next step>` */
  text: string;
  next: NextStep | null;
  /** The original message (shown under [details]). */
  raw: string;
  /** A dropped connection or a busy upstream: worth one automatic retry. */
  transient: boolean;
  /** A precondition the user has to fix (a held message waits for it). */
  wall: Wall | null;
}

export const FAL_BILLING_URL = 'https://fal.ai/dashboard/billing';

interface Rule { test: RegExp; text: string; next: NextStep | null; transient?: boolean; wall?: Wall }

const RULES: Rule[] = [
  { test: /\b(no fal key|fal key (is )?(missing|not set)|not signed in to fal|missing fal_key|FAL_KEY=)/i, text: 'fal key missing', next: { kind: 'setup', label: 'open setup' }, wall: 'fal-key' },
  { test: /\b(out of credits|no credits|insufficient (balance|credits?|funds)|exhausted (balance|credits)|payment required|402|user is locked)\b/i, text: 'out of credits', next: { kind: 'url', label: 'open fal billing', url: FAL_BILLING_URL }, wall: 'credits' },
  { test: /\bcodex\b.*(sign(ed)? in|log(ged)? ?in|login|auth)|codex login/i, text: 'Codex login expired', next: { kind: 'copy', label: 'run codex login', text: 'codex login' }, wall: 'codex-login' },
  {
    test: /claude isn'?t signed in|failed to authenticate|oauth (session|token)|not logged in|please run \/login|invalid api key|authentication_error|claude.*(login|sign in) (expired|required)/i,
    text: 'Claude login expired',
    next: { kind: 'copy', label: 'run claude auth login', text: 'claude auth login' },
    wall: 'claude-login',
  },
  { test: /no agent runtime is connected/i, text: 'agent runtime not running · the message waits for it', next: null },
  { test: /agent runtime (disconnected|not connected|exited|stopped)|runtime (is )?down/i, text: 'agent runtime stopped', next: { kind: 'continue', label: 'continue when it is back' }, transient: true, wall: 'runtime' },
  { test: /interrupted: its job stopped/i, text: 'interrupted by a host restart', next: { kind: 'rerun', label: 'rerun it' } },
  { test: /\b429\b|rate.?limit|too many requests/i, text: 'rate limited', next: { kind: 'continue', label: 'retry in a minute' }, transient: true },
  {
    test: /econnreset|etimedout|econnrefused|socket hang up|fetch failed|network (error|request failed)|connection (reset|closed|dropped|refused)|\b50[234]\b|bad gateway|service unavailable|gateway time-?out|overloaded|\b529\b|temporarily unavailable/i,
    text: 'connection dropped',
    next: { kind: 'continue', label: 'continue' },
    transient: true,
  },
  { test: /timed out after/i, text: 'timed out', next: { kind: 'rerun', label: 'rerun it' } },
  { test: /ede_diagnostic|error_during_execution|stopped mid-step|returned an error result/i, text: 'the agent stopped mid-step', next: { kind: 'continue', label: 'continue' } },
];

/** One sentence for an error message: cause and one next step. */
export function plainError(raw: string): PlainError {
  const text = String(raw ?? '').trim();
  for (const r of RULES) {
    if (r.test.test(text)) return { text: r.next ? `${r.text} · ${r.next.label}` : r.text, next: r.next, raw: text, transient: !!r.transient, wall: r.wall ?? null };
  }
  // Unknown: its first sentence, short.
  const first = (/^\s*[{[]/.test(text) ? 'the agent stopped' : text.split('\n')[0].split(/(?<=[.!?])\s/)[0]).trim() || 'something went wrong';
  return { text: first.length > 100 ? `${first.slice(0, 99)}…` : first, next: null, raw: text, transient: false, wall: null };
}
