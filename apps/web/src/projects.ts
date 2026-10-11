// Projects (one project = one canvas = one fal Agent project, many chats): the host's
// `/workspace?token=…` socket lists them and takes create / rename / duplicate / delete /
// touch. The open project is in the URL (`?project=<id>`) and remembered locally; each
// project remembers its camera. A host without `/workspace` falls back to the single canvas
// (`/sync/main`), and everything here stays quiet.

import { getState, setState, toast } from './store';
import type { HostInfo } from './host';
import { discoverHost, explicitHost } from './host';
import { switchProject } from './sync';
import type { CanvasApp } from './wasm';
import type { NodeView } from './types';
import { worldRects } from './geometry';

export interface ProjectInfo {
  id: string;
  name: string;
  updated: number;
  /** Running chats. */
  running: number;
  /** The linked fal Agent project, if any. */
  fal_project: string | null;
  thumb: string | null;
}

const LAST_KEY = 'falos.project';

/** Projects newest first. */
export const byRecent = (list: ProjectInfo[]) => [...list].sort((a, b) => (b.updated ?? 0) - (a.updated ?? 0));

export function urlProject(): string | null {
  return new URLSearchParams(location.search).get('project');
}

export function lastProject(): string | null {
  try {
    return localStorage.getItem(LAST_KEY);
  } catch {
    return null;
  }
}

/** Puts the project in the URL (reload keeps it) and remembers it (also in the Tauri shell,
 *  whose webview keeps localStorage). */
export function rememberProject(id: string | null) {
  const u = new URL(location.href);
  if (id) u.searchParams.set('project', id);
  else u.searchParams.delete('project');
  if (u.href !== location.href) history.replaceState(history.state, '', u);
  try {
    if (id) localStorage.setItem(LAST_KEY, id);
  } catch {
    /* private mode */
  }
}

// ---- per-project camera ----------------------------------------------------------------

export interface Cam { x: number; y: number; zoom: number }
const cams = new Map<string, Cam>();
let camTimer = 0;

export function saveCamera(key: string, c: { x: number; y: number; zoom: number }) {
  if (!Number.isFinite(c.x) || !Number.isFinite(c.y) || !(c.zoom > 0)) return;
  cams.set(key, { x: c.x, y: c.y, zoom: c.zoom });
  window.clearTimeout(camTimer);
  camTimer = window.setTimeout(() => {
    try {
      for (const [k, v] of cams) localStorage.setItem(`falos.camera.${k}`, JSON.stringify(v));
    } catch {
      /* private mode */
    }
  }, 400);
}

export function loadCamera(key: string): Cam | null {
  const m = cams.get(key);
  if (m) return m;
  try {
    const v = JSON.parse(localStorage.getItem(`falos.camera.${key}`) ?? 'null');
    if (v && Number.isFinite(v.x) && Number.isFinite(v.y) && v.zoom > 0) return v as Cam;
  } catch {
    /* private mode / bad JSON */
  }
  return null;
}

/** Loads `bytes` and brings back the project's camera; fits the content when there is no
 *  saved camera or the saved one would show none of it (e.g. saved while empty). */
export function loadScene(app: CanvasApp, bytes: Uint8Array, key: string) {
  const cam = loadCamera(key);
  app.loadSnapshot(bytes, !cam);
  if (!cam) return;
  app.setCamera(cam.x, cam.y, cam.zoom);
  const nodes = app.nodes() as NodeView[];
  if (!nodes.length) return;
  const c = app.camera() as { width: number; height: number };
  const v = [cam.x, cam.y, c.width / cam.zoom, c.height / cam.zoom];
  const seen = [...worldRects(nodes).values()].some((r) => r[0] < v[0] + v[2] && r[0] + r[2] > v[0] && r[1] < v[1] + v[3] && r[1] + r[3] > v[1]);
  if (!seen) app.fitAll();
}

// ---- the /workspace socket ---------------------------------------------------------------

class Workspace {
  private ws: WebSocket | null = null;
  private timer = 0;
  private retry = 0;
  private everOpened = false;
  private gotList = false;
  private listWaiters: ((list: ProjectInfo[] | null) => void)[] = [];
  private createWaiters: ((id: string | null) => void)[] = [];
  /** Projects seen in a list: one that disappears while open was deleted elsewhere. */
  private seen = new Set<string>();

  constructor(private host: HostInfo) {}

  start() {
    this.connect();
  }

  private connect() {
    window.clearTimeout(this.timer);
    let ws: WebSocket;
    try {
      ws = new WebSocket(`ws://${this.host.addr}/workspace?${new URLSearchParams({ token: this.host.token })}`);
    } catch {
      return this.failed();
    }
    this.ws = ws;
    let opened = false;
    ws.onopen = () => {
      opened = true;
      this.everOpened = true;
      this.retry = 0;
      setState({ workspace: 'connected' });
    };
    ws.onmessage = (e) => {
      if (typeof e.data !== 'string') return;
      try {
        this.onMessage(JSON.parse(e.data));
      } catch (err) {
        console.warn('workspace: bad frame', err);
      }
    };
    ws.onclose = () => {
      if (this.ws !== ws) return;
      this.ws = null;
      if (!opened && !this.everOpened) return this.failed();
      // Was working: reconnect (a restarted host may have moved).
      const delay = Math.min(10_000, 500 * 2 ** this.retry) * (0.75 + Math.random() * 0.5);
      this.retry = Math.min(this.retry + 1, 6);
      this.timer = window.setTimeout(() => void this.reconnect(), delay);
    };
  }

  /** Never answered: an older host (single canvas). Check again now and then. */
  private failed() {
    if (getState().workspace !== 'connected') setState({ workspace: 'unavailable' });
    this.flushList(null);
    this.flushCreate(null);
    this.timer = window.setTimeout(() => void this.reconnect(), 30_000);
  }

  private async reconnect() {
    if (!explicitHost()) {
      const h = await discoverHost();
      if (h) this.host = h;
    }
    this.connect();
  }

  private flushList(list: ProjectInfo[] | null) {
    const w = this.listWaiters;
    this.listWaiters = [];
    w.forEach((f) => f(list));
  }

  private flushCreate(id: string | null) {
    const w = this.createWaiters;
    this.createWaiters = [];
    w.forEach((f) => f(id));
  }

  private onMessage(m: any) {
    switch (m?.type) {
      case 'projects': {
        const list = (Array.isArray(m.projects) ? m.projects : [])
          .filter((p: any) => p && typeof p.id === 'string')
          .map((p: any): ProjectInfo => ({
            id: p.id,
            name: typeof p.name === 'string' && p.name ? p.name : 'untitled',
            updated: typeof p.updated === 'number' ? p.updated : 0,
            running: typeof p.running === 'number' ? p.running : Array.isArray(p.running) ? p.running.length : p.running ? 1 : 0,
            fal_project: typeof p.fal_project === 'string' ? p.fal_project : null,
            thumb: typeof p.thumb === 'string' ? p.thumb : null,
          }));
        this.gotList = true;
        const cur = getState().project;
        const gone = cur && this.seen.has(cur) && !list.some((p: ProjectInfo) => p.id === cur);
        for (const p of list) this.seen.add(p.id);
        setState({ projects: list, workspace: 'connected' });
        this.flushList(list);
        if (gone) {
          // The open project was deleted (here or elsewhere): open the most recent other one.
          const next = byRecent(list)[0];
          if (next) switchProject(next.id);
          else void createProject('untitled');
        }
        break;
      }
      case 'project.created':
        if (typeof m.id === 'string') {
          this.seen.add(m.id);
          const w = this.createWaiters.shift();
          if (w) w(m.id);
        }
        break;
      case 'error':
        toast(`workspace: ${String(m.message ?? 'error')}`);
        // A failed create must not hang its caller.
        if (/create/i.test(String(m.message ?? ''))) this.createWaiters.shift()?.(null);
        break;
    }
  }

  /** The first project list (null if the host has no `/workspace` or it takes too long). */
  firstList(timeout: number): Promise<ProjectInfo[] | null> {
    if (this.gotList) return Promise.resolve(getState().projects);
    if (getState().workspace === 'unavailable') return Promise.resolve(null);
    return new Promise((resolve) => {
      const t = window.setTimeout(() => {
        this.listWaiters = this.listWaiters.filter((f) => f !== done);
        resolve(null);
      }, timeout);
      const done = (l: ProjectInfo[] | null) => {
        window.clearTimeout(t);
        resolve(l);
      };
      this.listWaiters.push(done);
    });
  }

  send(m: unknown): boolean {
    if (this.ws?.readyState !== WebSocket.OPEN) return false;
    this.ws.send(JSON.stringify(m));
    return true;
  }

  /** `project.create` → the new id from `project.created` (null on failure / timeout). */
  create(name: string, timeout = 5000): Promise<string | null> {
    if (!this.send({ type: 'project.create', name })) return Promise.resolve(null);
    return new Promise((resolve) => {
      const t = window.setTimeout(() => {
        this.createWaiters = this.createWaiters.filter((f) => f !== done);
        resolve(null);
      }, timeout);
      const done = (id: string | null) => {
        window.clearTimeout(t);
        resolve(id);
      };
      this.createWaiters.push(done);
    });
  }
}

let ws: Workspace | null = null;

/** Starts the workspace socket and picks the project to open: the URL's, else the last one,
 *  else the most recent (an empty workspace gets a first project). Null = no `/workspace`
 *  (older host): use the single canvas. */
export async function bootProject(host: HostInfo): Promise<string | null> {
  if (!ws) {
    ws = new Workspace(host);
    ws.start();
  }
  const list = await ws.firstList(2000);
  if (!list) return null;
  const want = urlProject() ?? lastProject();
  if (want && list.some((p) => p.id === want)) return want;
  const recent = byRecent(list)[0];
  if (recent) return recent.id;
  return await ws.create('untitled');
}

const need = () => {
  if (!ws || getState().workspace !== 'connected') {
    toast('projects need a newer fal-os host');
    return null;
  }
  return ws;
};

/** Creates a project and opens it: an empty canvas with the composer focused. */
export async function createProject(name: string): Promise<string | null> {
  const w = need();
  if (!w) return null;
  const id = await w.create(name.trim() || 'untitled');
  if (!id) {
    toast("couldn't create the project");
    return null;
  }
  switchProject(id);
  setState({ chatOpen: true, chatView: 'chat', subView: null });
  requestAnimationFrame(() => window.dispatchEvent(new Event('falos:focus-chat')));
  return id;
}

export function renameProject(id: string, name: string) {
  const v = name.trim();
  if (!v) return;
  // Optimistic: the next `projects` list confirms it.
  if (need()?.send({ type: 'project.rename', id, name: v }))
    setState((s) => ({ projects: s.projects.map((p) => (p.id === id ? { ...p, name: v } : p)) }));
}

export function duplicateProject(id: string) {
  const name = getState().projects.find((p) => p.id === id)?.name ?? 'project';
  if (need()?.send({ type: 'project.duplicate', id })) toast(`duplicating '${name}'`);
}

/** Moves a project to the trash (the host keeps it there). */
export function deleteProject(id: string) {
  const name = getState().projects.find((p) => p.id === id)?.name ?? 'project';
  if (!need()?.send({ type: 'project.delete', id })) return;
  toast(`moved '${name}' to the trash`);
}

export function touchProject(id: string) {
  ws?.send({ type: 'project.touch', id });
}

/** Opens a project from the picker / palette (and marks it recently used). */
export function openProject(id: string) {
  if (id === getState().project) return;
  switchProject(id);
}

export const currentProject = () => {
  const s = getState();
  return s.projects.find((p) => p.id === s.project) ?? null;
};
