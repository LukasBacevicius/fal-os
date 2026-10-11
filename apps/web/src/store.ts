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
  overlay: 'palette' | 'help' | 'connect' | 'projects' | null;
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
  /** The ✦ selection prompt is open. */
  promptOpen: boolean;
  /** Video node shown in the focus view (near-fullscreen player), if open. */
  focusVideo: string | null;
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
  toast: string | null;
  saved: 'idle' | 'saving' | 'saved';
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
  outlineOpen: (() => {
    try {
      const v = localStorage.getItem('falos.outline');
      return v === '1' ? true : v === '0' ? false : null;
    } catch {
      return null;
    }
  })(),
  toast: null,
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

let toastTimer = 0;
export function toast(msg: string) {
  setState({ toast: msg });
  window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => setState({ toast: null }), 3200);
}
