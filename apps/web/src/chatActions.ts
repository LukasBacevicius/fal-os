// Chat actions shared by the panel, the tabs, the selection prompt, the palette and keys.

import { getState, setState, toast } from './store';
import { hostSync } from './sync';
import { isExternal, selectChat, tabs, type Chip } from './chat';

/** Sends `text` with `context` to `chat` (null = start a new chat). While the chat runs the
 *  host queues it. Returns false (with a toast) when there is no host. */
export function sendTo(chat: string | null, text: string, context: Chip[]): boolean {
  const s = getState();
  const sync = hostSync();
  if (!sync || s.conn !== 'connected') {
    toast('chat needs a fal-os host');
    return false;
  }
  if (!sync.sendChat(text, context, chat)) {
    toast('not connected to the host');
    return false;
  }
  setState((st) => ({ chatOpen: true, chatView: 'chat', subView: null, chat: chat === null ? selectChat(st.chat, null) : st.chat }));
  return true;
}

/** Sends a message on the active chat with the pending chips (plus `extra`). */
export function sendChatMessage(text: string, extra: Chip[] = []): boolean {
  const s = getState();
  // An external agent's tab (Claude Code, Codex, …): the message goes to its inbox.
  const active = s.chat.active;
  if (!sendTo(active, text, [...s.chat.chips, ...extra])) return false;
  setState((st) => ({ chat: { ...st.chat, chips: [] } }));
  return true;
}

/** Shows a chat in the panel (opening its tab if it was closed). */
export function openChat(id: string, sub: string | null = null) {
  const s = getState();
  if (s.chat.meta[id]?.open === false) hostSync()?.openChat(id);
  setState((st) => ({ chatOpen: true, chatView: 'chat', subView: sub, chat: selectChat(st.chat, id) }));
}

/** Closes a tab (the chat stays in the history). Running chats always keep their tab. */
export function closeTab(id: string) {
  const s = getState();
  const list = tabs(s.chat);
  const i = list.indexOf(id);
  hostSync()?.closeChat(id);
  setState((st) => {
    const rest = list.filter((x) => x !== id);
    const active = st.chat.active === id ? rest[Math.min(i, rest.length - 1)] ?? null : st.chat.active;
    return { chat: { ...st.chat, closed: { ...st.chat.closed, [id]: true }, active }, subView: st.chat.active === id ? null : st.subView };
  });
}

export function askDelete(id: string) {
  setState({ confirmDelete: id });
}

export function confirmDelete(yes: boolean) {
  const id = getState().confirmDelete;
  setState({ confirmDelete: null });
  if (yes && id) hostSync()?.deleteChat(id);
}

export function persistTabOrder(order: string[]) {
  try {
    localStorage.setItem('falos.tabOrder', JSON.stringify(order));
  } catch {
    /* private mode */
  }
  setState((s) => ({ chat: { ...s.chat, tabOrder: order } }));
}

export function toggleHistory() {
  setState((s) => ({ chatOpen: true, chatView: s.chatOpen && s.chatView === 'history' ? 'chat' : 'history' }));
}

/** "Continue in my agent": copies `cd <project> && claude "continue this fal OS project"`
 *  (or codex) with the native clipboard; `open` also runs it in Terminal. */
export function continueInAgent(agent: 'claude' | 'codex', open = false): boolean {
  if (!hostSync()?.continueInAgent(agent, open)) {
    toast('needs a fal OS host');
    return false;
  }
  return true;
}

export const DRIVERS = ['opus', 'fal', 'codex'] as const;

/** ⌘. / the composer toggle: which agent drives the next message. */
export function setDriver(d?: (typeof DRIVERS)[number]) {
  const next = d ?? DRIVERS[(DRIVERS.indexOf(getState().driver) + 1) % DRIVERS.length];
  try {
    localStorage.setItem('falos.driver', next);
  } catch {
    /* private mode */
  }
  setState({ driver: next });
}

/** Steering: deliver `text` to the running turn of the active chat now (⌘⏎), or to one of its
 *  subagents (`agent`, from the subagent view). Returns false (with a toast) when it can't. */
export function steerText(text: string, agent: string | null = null): boolean {
  const s = getState();
  const chat = s.chat.active;
  const sync = hostSync();
  if (!chat || !sync || s.conn !== 'connected') {
    toast('chat needs a fal-os host');
    return false;
  }
  // External agents read their inbox between steps: steering is a plain send.
  if (isExternal(chat)) return sendTo(chat, text, []);
  if (!sync.steerChat(chat, { text, agent })) {
    toast('not connected to the host');
    return false;
  }
  return true;
}

/** `[send now]` on a queued message: deliver it to the running turn right away. */
export function steerQueued(id: string): boolean {
  const chat = getState().chat.active;
  if (!chat) return false;
  return !!hostSync()?.steerChat(chat, { id });
}
