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
  /** Modal overlay: command palette or the shortcut list. */
  overlay: 'palette' | 'help' | null;
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
