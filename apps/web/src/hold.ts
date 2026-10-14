// Hold, don't drop (docs/research/brilliant-b102.md §12), and one automatic retry.
//
// - A message that hits a wall (host unreachable, agent runtime down, fal key missing, a
//   login expired, out of credits) is held, with one line saying why, and sent by itself
//   when the wall clears. [send now] forces it, [cancel] puts it back in the composer.
// - A run that fails transiently (a dropped connection, a busy upstream) continues once by
//   itself; the error line says so.

import { getState, setState, notify } from './store';
import { draftKey, isExternal, type Chip } from './chat';
import { plainError, type Wall } from './errors';
import { latestCaps, reasonOf } from './chatActions';
import { hostSync } from './sync';

export interface Held {
  chat: string | null;
  text: string;
  context: Chip[];
  wall: Wall;
  at: number;
}

/** Walls that clear by themselves without a probe (the next host frame tells). */
const SELF_CLEARING: Wall[] = ['host', 'runtime'];

/** Why a send can't go through right now, or null. */
export function wallFor(s: ReturnType<typeof getState>, chat: string | null): Wall | null {
  if (s.conn === 'connecting' || s.conn === 'reconnecting') return 'host';
  if (s.conn !== 'connected') return null; // no host at all: not a wall, a missing feature
  if (chat && isExternal(chat)) return null; // its inbox keeps the message anyway
  if (s.chat.runtime === 'missing') return 'runtime';
  const reason = reasonOf(latestCaps(s), s.driver);
  return reason ? plainError(reason).wall : null;
}

/** Whether the held message's wall is still up. Host and runtime walls clear by themselves;
 *  a key, a login or credits clear when the setup flow says so (or the user sends anyway). */
export function wallUp(s: ReturnType<typeof getState>, h: Held): boolean {
  switch (h.wall) {
    case 'host':
      return s.conn !== 'connected';
    case 'runtime':
      return s.conn !== 'connected' || s.chat.runtime === 'missing';
    default:
      return !cleared.has(h.wall);
  }
}

/** Walls the user says are fixed (setup saved a key, a sign-in finished). */
const cleared = new Set<Wall>();

export function hold(h: Omit<Held, 'at'>) {
  cleared.delete(h.wall);
  setState({ held: { ...h, at: Date.now() } });
}

/** Sends the held message now (the wall cleared, or [send now]). */
export function releaseHeld(said?: string) {
  const h = getState().held;
  if (!h) return;
  setState({ held: null });
  const sync = hostSync();
  if (!sync || !sync.sendChat(h.text, h.context, h.chat)) {
    // Still nowhere to go: back to the composer, never lost.
    restoreDraft(h);
    return;
  }
  if (said) notify(said, { key: 'held', level: 'ok' });
}

/** [cancel]: the held message back into its composer. */
export function cancelHeld() {
  const h = getState().held;
  if (!h) return;
  setState({ held: null });
  restoreDraft(h);
}

function restoreDraft(h: Held) {
  setState((s) => {
    const key = draftKey(h.chat, s.project);
    const cur = s.drafts[key] ?? '';
    return { drafts: { ...s.drafts, [key]: cur.trim() ? `${cur}\n${h.text}` : h.text }, chat: { ...s.chat, chips: [...h.context, ...s.chat.chips] } };
  });
}

/** Setup saved a key or a sign-in finished: walls of that kind are down. */
export function wallCleared(w: Wall) {
  cleared.add(w);
  check();
}

const DONE_TEXT: Record<Wall, string> = {
  host: 'host is back · sent the held message',
  runtime: 'agent runtime is back · sent the held message',
  'fal-key': 'fal key set · sent the held message',
  credits: 'sent the held message',
  'claude-login': 'signed in · sent the held message',
  'codex-login': 'signed in · sent the held message',
};

function check() {
  const s = getState();
  const h = s.held;
  if (!h) return;
  if (!wallUp(s, h)) releaseHeld(DONE_TEXT[h.wall]);
}

// ---- one automatic retry -----------------------------------------------------------------

/** chat → the error text that was retried automatically (the error line says so). */
const retried = new Map<string, string>();
/** The last message each chat sent: a wall that answers it puts it on hold. */
const lastSent = new Map<string, { text: string; context: Chip[]; at: number }>();

export const autoRetried = (chat: string, text: string) => retried.get(chat) === text;

/** A message went out (a new user turn): it may retry once again. */
export function sent(chat: string | null, text: string, context: Chip[]) {
  if (chat) retried.delete(chat);
  lastSent.set(chat ?? '', { text, context, at: Date.now() });
}

/** A live `status: error` event: retry once if transient; hold the message if it hit a wall. */
export function onRunError(chat: string, ev: { error?: unknown; actions?: unknown; _ts?: number }, ts: number) {
  if (Date.now() - ts > 30_000) return; // history replay, not a live failure
  const raw = String(ev.error ?? '');
  const pe = plainError(raw);
  const canContinue = Array.isArray(ev.actions) && ev.actions.includes('continue');
  if (pe.transient && canContinue && !retried.has(chat)) {
    retried.set(chat, raw);
    window.setTimeout(() => hostSync()?.continueChat(chat), 1_500);
    return;
  }
  if (pe.wall && !SELF_CLEARING.includes(pe.wall) && !getState().held) {
    const last = lastSent.get(chat) ?? lastSent.get('');
    if (last && Date.now() - last.at < 120_000) {
      lastSent.delete(chat);
      hold({ chat, text: last.text, context: last.context, wall: pe.wall });
    }
  }
}

let timer = 0;

/** Watches the held message's wall (1 s). */
export function installHold(): () => void {
  window.clearInterval(timer);
  timer = window.setInterval(check, 1_000);
  const onSetup = (e: Event) => {
    const d = (e as CustomEvent<{ falKey?: boolean; claude?: boolean }>).detail ?? {};
    if (d.falKey) wallCleared('fal-key');
    if (d.claude) wallCleared('claude-login');
  };
  window.addEventListener('falos:setup-changed', onSetup);
  return () => {
    window.clearInterval(timer);
    window.removeEventListener('falos:setup-changed', onSetup);
  };
}
