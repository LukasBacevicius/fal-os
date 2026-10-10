// Glue between the wasm CanvasApp and the UI store: event handling, local persistence,
// file import helpers.

import { getState, setState, toast, type AssetInfo } from './store';
import * as persist from './persist';
import type { CanvasApp } from './wasm';
import type { AnnotationView, NodeView } from './types';
import { hostSync } from './sync';

export const CANVAS_ID = 'main';

let saveTimer = 0;
let booting = false;

function refresh(app: CanvasApp, what: { nodes?: boolean; annotations?: boolean; assets?: boolean }) {
  const patch: Parameters<typeof setState>[0] = {};
  if (what.nodes) {
    patch.nodes = app.nodes() as NodeView[];
    hostSync()?.syncAssets(patch.nodes);
  }
  if (what.annotations || what.nodes) patch.annotations = app.annotations() as AnnotationView[];
  if (what.assets) patch.assets = app.assets() as AssetInfo[];
  setState(patch);
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
        refresh(app, { nodes: payload.nodes, annotations: payload.annotations, assets: payload.assets });
        scheduleSave(app);
        break;
      case 'hover':
        setState({ hoverNode: payload.node, hoverAnnotation: payload.annotation });
        break;
      case 'camera':
        setState({ camera: payload });
        break;
      case 'draft':
        setState({ draft: payload });
        break;
      case 'draft-move':
        setState((s) => (s.draft ? { draft: { ...s.draft, screen: payload.screen, rect: payload.rect } } : {}));
        break;
      case 'annotation-select':
        setState({ selectedAnnotation: payload.id });
        break;
      case 'tool':
        setState({ tool: payload });
        break;
      case 'asset': {
        const blob = app.assetBlob(payload.id);
        if (blob && !booting) persist.saveAsset({ id: payload.id, blob, mime: payload.mime, name: payload.name }).catch(console.warn);
        void hostSync()?.upload(payload.id);
        setState({ assets: app.assets() as AssetInfo[] });
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
  refresh(app, { nodes: true, annotations: true, assets: true });
  setState({ canvasName: app.canvasName(), camera: app.camera() });
}

export async function importFiles(files: File[], at?: [number, number]) {
  const { app, camera } = getState();
  if (!app || !files.length) return [];
  const [x, y] = at ?? [camera.width / 2, camera.height / 2];
  try {
    return (await app.importFiles(files, x, y)) as string[];
  } catch (e) {
    toast(`Import failed: ${e}`);
    return [];
  }
}

/** Loads the generated sample media (`pnpm samples`) as if dropped on the canvas. */
export async function loadSamples() {
  try {
    const list: string[] = await (await fetch('samples/manifest.json')).json();
    const files = await Promise.all(
      list.map(async (name) => {
        const blob = await (await fetch(`samples/${name}`)).blob();
        return new File([blob], name, { type: blob.type });
      }),
    );
    return await importFiles(files);
  } catch {
    toast('No samples found — run `pnpm --dir apps/web samples`');
    return [];
  }
}

export async function exportCanvas() {
  const { app } = getState();
  if (!app) return;
  const assets = (app.assets() as AssetInfo[]).map((a) => ({ ...a, blob: app.assetBlob(a.id)! })).filter((a) => a.blob);
  const blob = await persist.exportBundle(app.exportSnapshot(), assets);
  const url = URL.createObjectURL(blob);
  const link = document.createElement('a');
  link.href = url;
  link.download = `${getState().canvasName || 'canvas'}.falos.json`;
  link.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

export async function importCanvas(file: File) {
  const { app } = getState();
  if (!app) return;
  try {
    const b = await persist.importBundle(file);
    for (const a of b.assets) {
      app.registerAsset(a.id, a.blob, a.mime, a.name);
      await persist.saveAsset(a);
    }
    app.loadSnapshot(b.snapshot, true);
    toast(`Opened ${file.name}`);
  } catch (e) {
    toast(`Could not open: ${e}`);
  }
}

export async function newCanvas() {
  const { app } = getState();
  if (!app) return;
  await persist.clearAll(CANVAS_ID);
  window.location.reload();
}
