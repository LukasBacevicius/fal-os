// Glue between the wasm CanvasApp and the UI store: event handling, local persistence, file
// import, chat context from the canvas.

import { getState, setState, toast } from './store';
import * as persist from './persist';
import type { CanvasApp } from './wasm';
import type { NodeView, Pick } from './types';
import { hostSync } from './sync';
import { addChips, chipFromPick, type Chip } from './chat';
import { trackNodes } from './activity';
import { worldRects } from './geometry';

export const CANVAS_ID = 'main';

let saveTimer = 0;
let booting = false;

function refreshNodes(app: CanvasApp) {
  const nodes = app.nodes() as NodeView[];
  hostSync()?.syncAssets(nodes);
  trackNodes(nodes);
  setState({ nodes });
}

function scheduleSave(app: CanvasApp) {
  // With a host, the host owns the canvas on disk; IndexedDB keeps the local-only canvas.
  if (booting || getState().conn !== 'local') return;
  setState({ saved: 'saving' });
  window.clearTimeout(saveTimer);
  saveTimer = window.setTimeout(async () => {
    try {
      await persist.saveSnapshot(CANVAS_ID, app.exportSnapshot());
      setState({ saved: 'saved' });
    } catch (e) {
      console.warn('save failed', e);
      setState({ saved: 'idle' });
    }
  }, 400);
}

export function attach(app: CanvasApp) {
  app.on((type: string, payload: any) => {
    switch (type) {
      case 'selection':
        setState({ selection: payload as string[] });
        hostSync()?.sendSelection(payload as string[]);
        break;
      case 'doc':
        if (payload.nodes) refreshNodes(app);
        scheduleSave(app);
        break;
      case 'hover':
        setState({ hoverNode: payload.node });
        break;
      case 'camera':
        setState({ camera: payload });
        break;
      case 'pick':
        addContext([chipFromPick(payload as Pick)]);
        break;
      case 'tool':
        setState({ tool: payload });
        break;
      case 'asset': {
        const blob = app.assetBlob(payload.id);
        if (blob && !booting) persist.saveAsset({ id: payload.id, blob, mime: payload.mime, name: payload.name }).catch(console.warn);
        void hostSync()?.upload(payload.id);
        break;
      }
      case 'stats':
        setState({ stats: payload });
        break;
      case 'error':
        toast(payload.message);
        break;
    }
  });
}

/** Restores the canvas and its assets from IndexedDB. With a host, only the asset cache is
 *  used (the host's snapshot replaces the canvas). */
export async function restore(app: CanvasApp, hosted = false) {
  booting = true;
  try {
    const [snapshot, assets] = await Promise.all([hosted ? undefined : persist.loadSnapshot(CANVAS_ID), persist.loadAssets()]);
    for (const a of assets) app.registerAsset(a.id, a.blob, a.mime, a.name);
    if (snapshot) app.loadSnapshot(snapshot, true);
  } catch (e) {
    console.warn('restore failed', e);
  } finally {
    booting = false;
  }
  refreshNodes(app);
  setState({ canvasName: app.canvasName(), camera: app.camera() });
}

export async function importFiles(files: File[], at?: [number, number]) {
  const { app, camera } = getState();
  if (!app || !files.length) return [];
  const [x, y] = at ?? [camera.width / 2, camera.height / 2];
  try {
    return (await app.importFiles(files, x, y)) as string[];
  } catch (e) {
    toast(`import failed: ${e}`);
    return [];
  }
}

/** A screen point (CSS px) near the middle of the view where a ~`size` px box covers no
 *  node, so attached files land in empty space; falls back to the center. */
export function emptySpot(size = 260): [number, number] {
  const { app, camera, nodes } = getState();
  const c: [number, number] = [camera.width / 2, camera.height / 2];
  if (!app) return c;
  const rects = [...worldRects(nodes).values()];
  const free = (sx: number, sy: number) => {
    const [x0, y0] = app.screenToWorld(sx - size / 2, sy - size / 2);
    const [x1, y1] = app.screenToWorld(sx + size / 2, sy + size / 2);
    return !rects.some((r) => r[0] < x1 && r[0] + r[2] > x0 && r[1] < y1 && r[1] + r[3] > y0);
  };
  const step = size / 2;
  for (let ring = 0; ring < 6; ring++) {
    for (let dx = -ring; dx <= ring; dx++) {
      for (let dy = -ring; dy <= ring; dy++) {
        if (Math.max(Math.abs(dx), Math.abs(dy)) !== ring) continue;
        const p: [number, number] = [c[0] + dx * step, c[1] + dy * step];
        if (p[0] < size / 2 || p[1] < size / 2 || p[0] > camera.width - size / 2 || p[1] > camera.height - size / 2) continue;
        if (free(p[0], p[1])) return p;
      }
    }
  }
  return c;
}

/** Files dropped or pasted on the chat: imported as canvas nodes in empty space near the
 *  view (uploaded to the host like any import) and attached as chips. */
export async function attachFiles(files: File[]) {
  if (!files.length) return;
  const ids = await importFiles(files, emptySpot());
  addContext(ids.map((node) => ({ kind: 'node', node })));
}

// ---- chat context ----------------------------------------------------------------------

export function addContext(chips: Chip[]) {
  if (!chips.length) return;
  setState((s) => ({ chat: { ...s.chat, chips: addChips(s.chat.chips, chips) } }));
  window.dispatchEvent(new Event('falos:focus-chat'));
}

/** ⌘L / "add to chat": the selected nodes as node chips. */
export function addSelectionToChat() {
  const sel = getState().selection;
  addContext(sel.map((node) => ({ kind: 'node', node })));
}

/** Pans to a node and selects it (node ids in chat). */
export function focusNode(id: string) {
  const { app } = getState();
  if (!app) return;
  app.focusNode(id);
  app.setSelection([id]);
}
