// WebGPU device loss: never lose the canvas to a GPU problem. When the engine reports
// `device_lost`, the last frame stays on screen (a 2D copy over the canvas) with a quiet
// "restoring canvas…", the app is recreated on a new device, and the scene, camera and
// selection come back. After 3 losses in 30 minutes it stops trying and shows a card.

import { getState, notify, showCard } from './store';

const WINDOW_MS = 30 * 60_000;
const MAX_LOSSES = 3;

const losses: number[] = [];
let recreate: ((view: { camera: { x: number; y: number; zoom: number }; selection: string[]; snapshot: Uint8Array | null }) => Promise<void>) | null = null;
let busy = false;

/** The stage registers how to build a new app on the same canvas element. */
export function onRecreate(f: typeof recreate) {
  recreate = f;
}

/** A copy of what the canvas shows now, laid over it (best effort). */
function freezeFrame(canvas: HTMLCanvasElement): HTMLElement[] {
  const out: HTMLElement[] = [];
  try {
    const c = document.createElement('canvas');
    c.width = canvas.width;
    c.height = canvas.height;
    c.className = 'gpu-frame';
    c.style.width = canvas.style.width;
    c.style.height = canvas.style.height;
    c.getContext('2d')?.drawImage(canvas, 0, 0);
    canvas.parentElement?.appendChild(c);
    out.push(c);
  } catch {
    /* the canvas keeps its last presented frame anyway */
  }
  const note = document.createElement('div');
  note.className = 'gpu-note';
  note.textContent = 'restoring canvas…';
  canvas.parentElement?.appendChild(note);
  out.push(note);
  return out;
}

export async function deviceLost(reason: string) {
  if (busy) return;
  const s = getState();
  const old = s.app;
  const canvas = document.querySelector<HTMLCanvasElement>('.stage-canvas');
  if (!old || !canvas || !recreate) return;
  busy = true;
  console.warn('WebGPU device lost:', reason);
  const now = Date.now();
  losses.push(now);
  while (losses.length && now - losses[0] > WINDOW_MS) losses.shift();
  const frozen = freezeFrame(canvas);
  if (losses.length > MAX_LOSSES) {
    frozen.at(-1)?.remove();
    showCard('gpu', `the canvas lost its GPU ${losses.length} times in 30 minutes · rendering paused`, [
      { label: 'restart rendering', run: () => location.reload() },
    ]);
    busy = false;
    return;
  }
  old.stop();
  const cam = old.camera() as { x: number; y: number; zoom: number };
  let snapshot: Uint8Array | null = null;
  try {
    snapshot = old.exportSnapshot();
  } catch {
    /* the host has it */
  }
  try {
    await recreate({ camera: { x: cam.x, y: cam.y, zoom: cam.zoom }, selection: s.selection, snapshot });
    // One frame from the new device before the copy goes away: no flash.
    await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
    notify('canvas restored', { key: 'gpu', level: 'ok' });
  } catch (e) {
    showCard('gpu', `couldn't restore the canvas: ${e}`, [{ label: 'reload', run: () => location.reload() }]);
  } finally {
    for (const el of frozen) el.remove();
    busy = false;
  }
}
