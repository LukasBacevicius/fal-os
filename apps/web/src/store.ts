import { useSyncExternalStore } from 'react';
import type { CameraView, NodeView, Presence, Stats, Tool } from './types';
import type { CanvasApp } from './wasm';
import type { ChatState } from './chat';
import { emptyChat } from './chat';

export interface AssetInfo { id: string; mime: string; name: string; size: number }

export interface State {
  app: CanvasApp | null;
  error: string | null;
  loading: boolean;
  canvasName: string;
  nodes: NodeView[];
  selection: string[];
  hoverNode: string | null;
  /** Note/text node being edited in place. */
  editing: string | null;
  camera: CameraView;
  stats: Stats | null;
  tool: Tool;
  presence: Presence[];
  /** Host connection (`local` = no host found). */
  conn: import('./sync').ConnState;
  hostAddr: string | null;
  /** Other sessions (agents, other humans) on the host. */
  sessions: import('./sync').SessionInfo[];
  claims: { node: string; actor: string }[];
  jobs: Record<string, import('./sync').JobInfo>;
  chat: ChatState;
  chatOpen: boolean;
  /** Left layers panel; null = automatic (shown when the canvas has nodes). */
  outlineOpen: boolean | null;
  /** Modal overlay: command palette, the shortcut list, "connect your agent" or the project
   *  picker (⌘O). */
  overlay: 'palette' | 'help' | 'connect' | 'projects' | 'export' | 'setup' | null;
  /** Node context menu (right-click) at a screen point. */
  menu: { x: number; y: number; ids: string[]; at?: [number, number] } | null;
  /** Nodes the export panel works on (empty = every node with saved export settings). */
  exportIds: string[];
  /** Lineage view: everything outside the selection's ancestors/descendants is dimmed. */
  lineage: boolean;
  /** The notice stack (bottom of the window), oldest first. See `notify`. */
  notices: Notice[];
  /** Cards: something stopped and needs a decision (host stopped, boot stalled, …). */
  cards: Card[];
  /** Projects from the host's `/workspace` socket (newest first). */
  projects: import('./projects').ProjectInfo[];
  /** The open project; null = a host without `/workspace` (single canvas `main`). */
  project: string | null;
  /** `/workspace`: unknown until it answers or fails; `unavailable` = older host. */
  workspace: 'unknown' | 'connected' | 'unavailable';
  /** Agent activity from the host (`activity` events), newest last; expires after 30 s. */
  activity: import('./activity').ActivityEntry[];
  /** Static outlines instead of sweeps and pulses (persisted; defaults to the OS setting). */
  reduceMotion: boolean;
  /** ⌘ (Ctrl) held: tabs show their ⌘1…9 badges. */
  modHeld: boolean;
  /** The selection prompt is open. */
  promptOpen: boolean;
  /** Video node shown in the focus view (near-fullscreen player), if open. */
  focusVideo: string | null;
  /** Node shown in the canvas preview (quick look of any non-video media), if open. */
  preview: string | null;
  /** App nodes: app server, live versions, errors, sound (apps.ts). */
  apps: import('./apps').AppsInfo;
  /** App node with keyboard and mouse (click / ⏎ focuses, Esc returns to the canvas). */
  appFocus: string | null;
  /** App node whose sources are shown read-only (`[src]`). */
  appSrc: string | null;
  /** Chat panel content: the active chat, or the full history list (⌘Y). */
  chatView: 'chat' | 'history';
  /** A subagent's own transcript shown in the panel (null = the main chat). */
  subView: string | null;
  /** Composer drafts per chat ('' key = a new chat). */
  drafts: Record<string, string>;
  /** Agent that drives new messages: the Opus director, the fal Agent or Codex (⌘.). */
  driver: 'opus' | 'fal' | 'codex';
  /** A chat waiting for the inline "delete …? [y/n]" answer. */
  confirmDelete: string | null;
  /** A canvas node is being dragged over the chat panel (drop attaches it). */
  dragToChat: boolean;
  /** "Connect your agent" progress, from real host events. */
  connect: {
    registered: Record<string, { ok: boolean; path?: string; error?: string }>;
    connected: Record<string, { agentName: string; session: string }>;
    firstWrite: Record<string, string[]>;
  };
  /** "Use fal-os from your own Claude / Codex" (host `director.status`): per-target state. */
  director: DirectorStatus | null;
  /** Asset library: the left-panel tab (⌘⇧A) or the full-screen view (⌘⇧L). */
  library: 'panel' | 'full' | null;
  saved: 'idle' | 'saving' | 'saved';
}

export interface NoticeAction { label: string; run: () => void }

/** One line in the notice stack.
 *  - `ok` / `info`: fade after ~3 s (8 s with actions);
 *  - `error`: stays until dismissed ([×]);
 *  - `ongoing`: one keyed line for a condition that lasts (host offline, saves failing),
 *    updated in place, not dismissable; `resolveNotice` turns it into a short "back in sync". */
export interface Notice {
  key: string;
  text: string;
  level: 'ok' | 'info' | 'error' | 'ongoing';
  actions: NoticeAction[];
  /** Called when the user dismisses it (e.g. tell the host). */
  onDismiss?: () => void;
  at: number;
}

export interface Card { key: string; text: string; actions: NoticeAction[] }

export interface DirectorFile {
  path: string;
  what: 'mcp' | 'skill';
  state: 'connected' | 'outdated' | 'not-installed';
  connect: 'create' | 'update' | 'none';
  disconnect: 'remove' | 'none';
  problem?: string;
}
export interface DirectorTarget {
  id: 'claude-code' | 'claude-desktop' | 'codex';
  label: string;
  detected: boolean;
  transport: 'stdio' | 'http';
  state: DirectorFile['state'];
  files: DirectorFile[];
}
export interface DirectorStatus {
  targets: DirectorTarget[];
  home?: string;
  /** The MCP server name connect writes: `fal-os`, or `fal-os-dev` on a host that isn't the
   *  installed app. */
  server?: string;
  /** Why this host isn't the installed app (dev build, other FALOS_HOME, other port). */
  instance?: string | null;
  changed?: string[];
  error?: string;
  /** A connect/disconnect is in flight. */
  busy?: boolean;
}

let state: State = {
  app: null,
  error: null,
  loading: true,
  canvasName: 'main',
  nodes: [],
  selection: [],
  hoverNode: null,
  editing: null,
  camera: { x: 0, y: 0, zoom: 1, width: 0, height: 0 },
  stats: null,
  tool: 'select',
  presence: [],
  conn: 'local',
  hostAddr: null,
  sessions: [],
  claims: [],
  jobs: {},
  chat: emptyChat(),
  chatOpen: (() => {
    try {
      return localStorage.getItem('falos.chatClosed') !== '1';
    } catch {
      return true;
    }
  })(),
  overlay: null,
  menu: null,
  exportIds: [],
  lineage: false,
  notices: [],
  cards: [],
  projects: [],
  project: null,
  workspace: 'unknown',
  activity: [],
  reduceMotion: (() => {
    try {
      const v = localStorage.getItem('falos.reduceMotion');
      if (v === '1' || v === '0') return v === '1';
    } catch {
      /* private mode */
    }
    return typeof matchMedia === 'function' && matchMedia('(prefers-reduced-motion: reduce)').matches;
  })(),
  modHeld: false,
  promptOpen: false,
  focusVideo: null,
  preview: null,
  apps: { base: null, project: null, versions: {}, errors: {}, sound: {} },
  appFocus: null,
  appSrc: null,
  chatView: 'chat',
  subView: null,
  drafts: {},
  dragToChat: false,
  confirmDelete: null,
  driver: (() => {
    try {
      const v = localStorage.getItem('falos.driver');
      return v === 'fal' || v === 'codex' ? v : 'opus';
    } catch {
      return 'opus';
    }
  })(),
  connect: { registered: {}, connected: {}, firstWrite: {} },
  director: null,
  outlineOpen: (() => {
    try {
      const v = localStorage.getItem('falos.outline');
      return v === '1' ? true : v === '0' ? false : null;
    } catch {
      return null;
    }
  })(),
  library: null,
  saved: 'idle',
};

const listeners = new Set<() => void>();

export const getState = () => state;

export function setState(patch: Partial<State> | ((s: State) => Partial<State>)) {
  const p = typeof patch === 'function' ? patch(state) : patch;
  state = { ...state, ...p };
  listeners.forEach((l) => l());
}

function subscribe(l: () => void) {
  listeners.add(l);
  return () => listeners.delete(l);
}

/** Subscribes a component to one slice of the store (return stored references, not new objects). */
export function useStore<T>(select: (s: State) => T): T {
  return useSyncExternalStore(subscribe, () => select(state));
}

// ---- notices ----------------------------------------------------------------------------

const fadeTimers = new Map<string, number>();
let seq = 0;
const MAX_NOTICES = 6;

function scheduleFade(n: Notice) {
  window.clearTimeout(fadeTimers.get(n.key));
  fadeTimers.delete(n.key);
  if (n.level !== 'ok' && n.level !== 'info') return;
  const at = n.at;
  fadeTimers.set(n.key, window.setTimeout(() => {
    fadeTimers.delete(n.key);
    // Only if it wasn't updated meanwhile.
    setState((s) => ({ notices: s.notices.filter((x) => !(x.key === n.key && x.at === at)) }));
  }, n.actions.length ? 8000 : 3000));
}

/** Adds a notice, or updates the one with the same `key` in place. */
export function notify(text: string, o: { key?: string; level?: Notice['level']; actions?: NoticeAction[]; onDismiss?: () => void } = {}) {
  const n: Notice = { key: o.key ?? `n${++seq}`, text, level: o.level ?? 'ok', actions: o.actions ?? [], onDismiss: o.onDismiss, at: performance.now() + ++seq / 1000 };
  setState((s) => {
    const i = s.notices.findIndex((x) => x.key === n.key);
    let list = i >= 0 ? s.notices.map((x, j) => (j === i ? n : x)) : [...s.notices, n];
    // Too many: drop the oldest that fade anyway, then the oldest errors; never ongoing ones.
    while (list.length > MAX_NOTICES) {
      const drop = list.findIndex((x) => x.level === 'ok' || x.level === 'info');
      const k = drop >= 0 ? drop : list.findIndex((x) => x.level === 'error');
      if (k < 0) break;
      list = list.filter((_, j) => j !== k);
    }
    return { notices: list };
  });
  scheduleFade(n);
  return n.key;
}

/** An ongoing condition ended: its line turns into `text` and fades. No-op without one. */
export function resolveNotice(key: string, text = 'back in sync') {
  if (!state.notices.some((n) => n.key === key)) return;
  notify(text, { key, level: 'ok' });
}

export function dismissNotice(key: string) {
  window.clearTimeout(fadeTimers.get(key));
  fadeTimers.delete(key);
  setState((s) => ({ notices: s.notices.filter((n) => n.key !== key) }));
}

/** Drops every notice whose key starts with `prefix` (e.g. a project's host notices). */
export function clearNotices(prefix: string) {
  setState((s) => ({ notices: s.notices.filter((n) => !n.key.startsWith(prefix)) }));
}

export function showCard(key: string, text: string, actions: NoticeAction[]) {
  setState((s) => ({ cards: [...s.cards.filter((c) => c.key !== key), { key, text, actions }] }));
}

export function hideCard(key: string) {
  if (state.cards.some((c) => c.key === key)) setState((s) => ({ cards: s.cards.filter((c) => c.key !== key) }));
}

/** Copy text for [copy report]-style buttons: the async clipboard, else the old
 *  `execCommand('copy')` path (webviews that deny clipboard writes). Says how it went. */
export async function copyText(text: string, done = 'copied') {
  try {
    await navigator.clipboard.writeText(text);
    return void notify(done);
  } catch {
    /* fall back below */
  }
  const ta = document.createElement('textarea');
  ta.value = text;
  ta.setAttribute('readonly', '');
  ta.style.position = 'fixed';
  ta.style.opacity = '0';
  document.body.appendChild(ta);
  ta.select();
  let ok = false;
  try {
    ok = document.execCommand('copy');
  } catch {
    ok = false;
  }
  ta.remove();
  if (ok) notify(done);
  else notify('copy failed: the clipboard is blocked here', { level: 'error' });
}

const FAILURE = /\b(fail|failed|error|couldn'?t|can'?t|cannot|unable|denied|refused|invalid|missing|not found)\b/i;

/** A one-line note (the old single toast). Failures stay until dismissed; everything else
 *  fades. Kept for every existing caller. */
export function toast(msg: string, actions: NoticeAction[] = []) {
  notify(msg, { level: FAILURE.test(msg) ? 'error' : 'ok', actions });
}
