// Glue between the wasm CanvasApp and the UI store: event handling, local persistence, file
// import, chat context from the canvas.

import { getState, setState, toast } from './store';
import * as persist from './persist';
import type { CanvasApp } from './wasm';
import type { NodeView, Pick } from './types';
import { hostSync } from './sync';
import { addChips, chipFromPick, type Chip } from './chat';

export const CANVAS_ID = 'main';

let saveTimer = 0;
let booting = false;

function refreshNodes(app: CanvasApp) {
  const nodes = app.nodes() as NodeView[];
  hostSync()?.syncAssets(nodes);
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
