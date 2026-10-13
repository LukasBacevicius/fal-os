// App nodes (docs/ARCHITECTURE.md "App nodes"): live web apps on the canvas.
//
// The host serves each app folder from its own loopback origin (`hello.apps`); the canvas
// shows it in a sandboxed iframe (`allow-scripts` only: an opaque origin) over the engine's
// snapshot still. Click or ⏎ focuses an app (keyboard and mouse go to it), Esc gives them
// back. The guard script the host injects posts `{falos:1,type}` messages: `esc`, `ready`,
// `pointer`, `log`; we send `{falos:'mute',muted}` and `{falos:'focus'}`.

import { getState, setState } from './store';
import type { NodeView } from './types';
import { closePreview } from './preview';

export interface AppsInfo {
  /** App server base URL (`http://127.0.0.1:<port>`), from the host's hello. */
  base: string | null;
  project: string | null;
  /** app id → version on disk (from `app` events): a change reloads the iframe. */
  versions: Record<string, number>;
  /** app id → console errors of the current version. */
  errors: Record<string, number>;
  /** app id → sound on (default muted). */
  sound: Record<string, boolean>;
}

export const emptyApps = (): AppsInfo => ({ base: null, project: null, versions: {}, errors: {}, sound: {} });

export const isApp = (n: NodeView | undefined | null): n is NodeView => !!n && n.kind === 'app' && typeof n.props.app === 'string';

export function appId(n: NodeView): string {
  return String(n.props.app);
}

/** The app's page URL (entry, cache-busted by version), or null without an app server. */
export function appUrl(n: NodeView, version?: number): string | null {
  const { base, project } = getState().apps;
  if (!base || !project) return null;
  const entry = typeof n.props.entry === 'string' && n.props.entry ? n.props.entry : 'index.html';
  const v = version ?? appVersion(n);
  return `${base}/${encodeURIComponent(project)}/${appId(n)}/${entry}?v=${v}`;
}

/** Folder URL (file list / sources for `[src]`). */
export function appBase(n: NodeView): string | null {
  const { base, project } = getState().apps;
  return base && project ? `${base}/${encodeURIComponent(project)}/${appId(n)}/` : null;
}

export function appVersion(n: NodeView): number {
  const live = getState().apps.versions[appId(n)];
  const doc = Number(n.props.version) || 0;
  return Math.max(live ?? 0, doc);
}

/** Host hello: where apps are served. */
export function setAppsBase(base: unknown, project: unknown) {
  setState((s) => ({ apps: { ...s.apps, base: typeof base === 'string' ? base : null, project: typeof project === 'string' ? project : s.apps.project } }));
}

/** Host `app` event: files changed (reload) or errors were logged. */
export function onAppEvent(m: { app?: unknown; version?: unknown; what?: unknown; errors?: unknown }) {
  if (typeof m.app !== 'string') return;
  const id = m.app;
  const v = Number(m.version) || 0;
  setState((s) => {
    const versions = m.what === 'files' ? { ...s.apps.versions, [id]: v } : s.apps.versions;
    let errors = s.apps.errors;
    if (m.what === 'files') errors = { ...errors, [id]: 0 };
    else if (typeof m.errors === 'number' && v >= (s.apps.versions[id] ?? 0)) errors = { ...errors, [id]: m.errors };
    return { apps: { ...s.apps, versions, errors } };
  });
}

// ---- focus --------------------------------------------------------------------------------

/** Live iframes by node id (AppLayer registers the visible one per node). */
export const frames = new Map<string, HTMLIFrameElement>();

export function focusApp(id: string) {
  const n = getState().nodes.find((x) => x.id === id);
  if (!isApp(n)) return;
  const a = document.activeElement as HTMLElement | null;
  if (a && a !== document.body) a.blur();
  setState({ appFocus: id });
  getState().app?.setSelection([id]);
  // The iframe mounts live when focused even if it was a still (small / far away).
  requestAnimationFrame(() => focusFrame(id));
}

export function focusFrame(id: string) {
  const f = frames.get(id);
  if (!f || getState().appFocus !== id) return;
  f.focus();
  f.contentWindow?.postMessage({ falos: 'focus' }, '*');
}

export function blurApp() {
  const id = getState().appFocus;
  if (!id) return;
  setState({ appFocus: null });
  const f = frames.get(id);
  if (f && document.activeElement === f) f.blur();
  window.focus();
}

export function toggleSound(id: string) {
  setState((s) => ({ apps: { ...s.apps, sound: { ...s.apps.sound, [id]: !s.apps.sound[id] } } }));
  sendMute(id);
}

export function sendMute(id: string) {
  const n = getState().nodes.find((x) => x.id === id);
  if (!isApp(n)) return;
  const muted = !getState().apps.sound[appId(n)];
  for (const [key, f] of frames) if (key === id || key.startsWith(`${id}:`)) f.contentWindow?.postMessage({ falos: 'mute', muted }, '*');
}

/** Messages from app iframes (the injected guard). */
export function installAppMessages(): () => void {
  const on = (e: MessageEvent) => {
    const d = e.data;
    if (!d || d.falos !== 1) return;
    let key: string | null = null;
    for (const [k, f] of frames) if (f.contentWindow === e.source) key = k;
    if (!key) return;
    const id = key.split(':')[0];
    const inPreview = key.endsWith(':preview');
    if (d.type === 'esc') {
      if (inPreview && getState().preview === id) closePreview();
      else if (getState().appFocus === id) blurApp();
    } else if (d.type === 'ready') sendMute(id);
    else if (d.type === 'pointer' && !inPreview && getState().appFocus !== id) focusApp(id);
  };
  window.addEventListener('message', on);
  return () => window.removeEventListener('message', on);
}
