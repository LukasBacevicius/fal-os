// Video playback UI glue: the canvas transport for the selected video, the focus view, and
// their keys. Canvas videos are the engine's hidden muted <video> elements (crates/engine
// web_video.rs), driven through `videoState / videoPlay / videoSeek / videoMute`. The focus view
// is a real <video> with sound on the same asset URL (the host streams it with byte ranges).
//
// Keys (arrows stay nudge on the canvas, so seeking uses J/L and , .):
//   canvas, one video selected: space (tap) or K play/pause · J / L −5 s / +5 s ·
//     , / . one frame back / forward · M sound · ⏎ or double-click focus view
//   focus view: ← / → previous / next video · space or K · J / L · , . · M · esc closes

import { getState, setState } from './store';
import { worldRects } from './geometry';
import type { NodeView } from './types';
import { audioMute, audioPause, audioSeekBy, audioToggle } from './audio';
import { openPreview, previewable } from './preview';

export interface VideoState {
  time: number;
  duration: number;
  ready: boolean;
  muted: boolean;
  /** Paused by the user (held on a frame). */
  paused: boolean;
  /** The element is actually playing (hovered / selected, under the decoder cap). */
  playing: boolean;
  url: string | null;
  width: number;
  height: number;
}

export const FRAME = 1 / 30;

/** `7.4` → `0:07`; `3725` → `1:02:05`. */
export function clock(t: number): string {
  if (!Number.isFinite(t) || t < 0) t = 0;
  const s = Math.floor(t % 60), m = Math.floor(t / 60) % 60, h = Math.floor(t / 3600);
  const ss = String(s).padStart(2, '0');
  return h ? `${h}:${String(m).padStart(2, '0')}:${ss}` : `${m}:${ss}`;
}

export function videoState(id: string): VideoState | null {
  return (getState().app?.videoState(id) as VideoState | null) ?? null;
}

/** The single selected video node, if any. */
export function selectedVideo(): string | null {
  const { selection, nodes } = getState();
  if (selection.length !== 1) return null;
  const n = nodes.find((x) => x.id === selection[0]);
  return n?.kind === 'video' ? n.id : null;
}

/** The single selected audio node with a playable asset, if any. */
export function selectedAudio(): { id: string; url: string } | null {
  const { selection, nodes, app } = getState();
  if (selection.length !== 1 || !app) return null;
  const n = nodes.find((x) => x.id === selection[0]);
  const url = n?.kind === 'audio' && typeof n.props.asset === 'string' ? app.assetUrl(n.props.asset) : null;
  return n && url ? { id: n.id, url } : null;
}

/** Video nodes in reading order (rows top to bottom, left to right within a row). */
export function videosInOrder(nodes: NodeView[] = getState().nodes): string[] {
  const rects = worldRects(nodes);
  return nodes
    .filter((n) => n.kind === 'video' && n.props.asset)
    .map((n) => ({ id: n.id, r: rects.get(n.id)! }))
    .sort((a, b) => {
      const sameRow = Math.abs(a.r[1] - b.r[1]) < Math.min(a.r[3], b.r[3]) / 2;
      return sameRow ? a.r[0] - b.r[0] : a.r[1] - b.r[1];
    })
    .map((v) => v.id);
}

// ---- canvas transport ------------------------------------------------------------------

export function togglePlay(id: string) {
  const app = getState().app;
  const s = videoState(id);
  if (!app || !s) return;
  app.videoPlay(id, s.paused);
}

export function seekBy(id: string, dt: number) {
  const app = getState().app;
  const s = videoState(id);
  if (!app || !s) return;
  // Frame steps pause first, like an editor.
  if (Math.abs(dt) < 1 && !s.paused) app.videoPlay(id, false);
  app.videoSeek(id, s.time + dt);
}

export function toggleMute(id: string) {
  const app = getState().app;
  const s = videoState(id);
  if (app && s) app.videoMute(id, !s.muted);
}

// Space: a short tap toggles play; holding it (and dragging) still pans the canvas.
let spaceDown: { at: number; moved: boolean } | null = null;
export function spacePressed() {
  spaceDown = { at: performance.now(), moved: false };
}
export function pointerPressed() {
  if (spaceDown) spaceDown.moved = true;
}
/** A space tap (not a pan): toggles the selected video or audio, or opens the preview of
 *  any other selected media (quick look). Returns true if it did something. */
export function spaceReleased(): boolean {
  const s = spaceDown;
  spaceDown = null;
  if (!s || s.moved || performance.now() - s.at > 280) return false;
  const id = selectedVideo();
  if (id) return togglePlay(id), true;
  const a = selectedAudio();
  if (a) return audioToggle(a.id, a.url), true;
  const { selection, nodes } = getState();
  const n = selection.length === 1 ? nodes.find((x) => x.id === selection[0]) : undefined;
  if (n && previewable(n)) return openPreview(n.id), true;
  return false;
}

/** Canvas keys for the selected video; true if handled. */
export function canvasVideoKey(e: KeyboardEvent): boolean {
  if (e.metaKey || e.ctrlKey || e.altKey) return false;
  const a = selectedAudio();
  if (a) {
    switch (e.key) {
      case 'k': case 'K': audioToggle(a.id, a.url); return true;
      case 'j': case 'J': audioSeekBy(a.id, a.url, -5); return true;
      case 'l': case 'L': audioSeekBy(a.id, a.url, 5); return true;
      case ',': case '<': audioSeekBy(a.id, a.url, -1); return true;
      case '.': case '>': audioSeekBy(a.id, a.url, 1); return true;
      case 'm': case 'M': audioMute(a.id, a.url); return true;
    }
    return false;
  }
  const id = selectedVideo();
  if (!id) return false;
  switch (e.key) {
    case 'k': case 'K': togglePlay(id); return true;
    case 'j': case 'J': seekBy(id, -5); return true;
    case 'l': case 'L': seekBy(id, 5); return true;
    case ',': case '<': seekBy(id, -FRAME); return true;
    case '.': case '>': seekBy(id, FRAME); return true;
    case 'm': case 'M': toggleMute(id); return true;
  }
  return false;
}

// ---- focus view --------------------------------------------------------------------------

/** The focus view's <video> (set by the component). */
export const focus: { el: HTMLVideoElement | null } = { el: null };

export function openFocus(id: string) {
  audioPause(); // one sound at a time
  setState({ focusVideo: id });
}

export function closeFocus() {
  setState({ focusVideo: null });
}

export function stepFocus(dir: 1 | -1) {
  const cur = getState().focusVideo;
  const list = videosInOrder();
  if (!cur || list.length < 2) return;
  const i = list.indexOf(cur);
  const next = list[(i + dir + list.length) % list.length];
  setState({ focusVideo: next });
  getState().app?.setSelection([next]);
}

/** Keys while the focus view is open (it owns the keyboard). Returns true if handled. */
export function focusKey(e: KeyboardEvent): boolean {
  const el = focus.el;
  if (e.metaKey || e.ctrlKey) return false;
  switch (e.key) {
    case 'Escape': closeFocus(); return true;
    case 'ArrowLeft': stepFocus(-1); return true;
    case 'ArrowRight': stepFocus(1); return true;
  }
  if (!el) return false;
  const seek = (t: number) => { el.currentTime = Math.max(0, Math.min(t, (el.duration || t) - 0.01)); };
  switch (e.key) {
    case ' ': case 'k': case 'K': if (el.paused) void el.play().catch(() => {}); else el.pause(); return true;
    case 'j': case 'J': seek(el.currentTime - 5); return true;
    case 'l': case 'L': seek(el.currentTime + 5); return true;
    case ',': case '<': el.pause(); seek(el.currentTime - FRAME); return true;
    case '.': case '>': el.pause(); seek(el.currentTime + FRAME); return true;
    case 'm': case 'M': el.muted = !el.muted; return true;
  }
  return false;
}
