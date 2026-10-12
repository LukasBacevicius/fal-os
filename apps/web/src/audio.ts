// Audio playback: one <audio> element for the whole app, shared by canvas audio nodes and
// the previews (canvas preview, library quick look), so only one sound plays at a time. The
// video focus view pauses it. A playhead store ticks every frame while playing (timeupdate
// is ~4 Hz: too choppy for a playhead) and is idle otherwise.
//
// Keys on a selected audio node match video's: space (tap) or K play/pause, J / L −5 s / +5 s,
// , / . −1 s / +1 s, M sound, ⏎ / double-click opens the preview.

import { useSyncExternalStore } from 'react';

export interface AudioPb {
  /** What is loaded (a node id, `lib:<sha>`, …); null = nothing. */
  key: string | null;
  time: number;
  duration: number;
  paused: boolean;
  muted: boolean;
}

let el: HTMLAudioElement | null = null;
let st: AudioPb = { key: null, time: 0, duration: 0, paused: true, muted: false };
let loadedUrl = '';
const subs = new Set<() => void>();
/** Durations by URL path (asset ids carry no token in the path). */
const durations = new Map<string, number>();

const pathOf = (url: string) => {
  try {
    return new URL(url, location.href).pathname;
  } catch {
    return url;
  }
};

function emit(next: Partial<AudioPb>) {
  const n = { ...st, ...next };
  if (n.key === st.key && n.time === st.time && n.duration === st.duration && n.paused === st.paused && n.muted === st.muted) return;
  st = n;
  subs.forEach((f) => f());
}

let raf = 0;
function tick() {
  raf = 0;
  if (!el) return;
  emit({ time: el.currentTime, duration: Number.isFinite(el.duration) ? el.duration : st.duration, paused: el.paused, muted: el.muted });
  if (!el.paused) raf = requestAnimationFrame(tick);
}
const kick = () => {
  if (!raf) raf = requestAnimationFrame(tick);
};

function player(): HTMLAudioElement {
  if (el) return el;
  el = new Audio();
  el.preload = 'auto';
  const sync = () => kick();
  for (const ev of ['play', 'pause', 'seeked', 'loadedmetadata', 'durationchange', 'volumechange', 'ended']) el.addEventListener(ev, sync);
  el.addEventListener('loadedmetadata', () => {
    if (el && Number.isFinite(el.duration)) durations.set(pathOf(loadedUrl), el.duration);
  });
  return el;
}

function load(key: string, url: string) {
  const a = player();
  if (st.key === key && loadedUrl === url) return a;
  a.pause();
  loadedUrl = url;
  a.src = url;
  emit({ key, time: 0, duration: durations.get(pathOf(url)) ?? 0, paused: true });
  return a;
}

export const audioState = () => st;

/** The playback state of `key` (zeros while something else is loaded). */
export function useAudio(key: string | null): AudioPb {
  return useSyncExternalStore(
    (f) => {
      subs.add(f);
      return () => subs.delete(f);
    },
    () => (key && st.key === key ? st : IDLE),
  );
}
const IDLE: AudioPb = { key: null, time: 0, duration: 0, paused: true, muted: false };

export function audioPlay(key: string, url: string, at?: number) {
  const a = load(key, url);
  if (at != null) a.currentTime = at;
  void a.play().catch(() => kick());
  kick();
}

export function audioToggle(key: string, url: string) {
  if (st.key === key && el && !el.paused) return audioPause();
  audioPlay(key, url);
}

export function audioPause() {
  el?.pause();
  kick();
}

/** Pauses only if `key` is what's loaded. */
export function audioRelease(key: string) {
  if (st.key === key) audioPause();
}

export function audioSeek(key: string, url: string, t: number) {
  const a = load(key, url);
  const d = Number.isFinite(a.duration) ? a.duration : st.duration;
  const to = Math.max(0, d > 0 ? Math.min(t, d - 0.01) : t);
  a.currentTime = to;
  emit({ time: to });
  kick();
}

export function audioSeekBy(key: string, url: string, dt: number) {
  audioSeek(key, url, (st.key === key ? st.time : 0) + dt);
}

export function audioMute(key: string, url: string) {
  const a = load(key, url);
  a.muted = !a.muted;
  kick();
}

/** Known duration of a URL (seconds), probing its metadata once if needed. */
const probing = new Set<string>();
export function audioDuration(url: string, onKnown?: () => void): number {
  const p = pathOf(url);
  const d = durations.get(p);
  if (d) return d;
  if (!probing.has(p)) {
    probing.add(p);
    const a = new Audio();
    a.preload = 'metadata';
    a.addEventListener('loadedmetadata', () => {
      if (Number.isFinite(a.duration)) durations.set(p, a.duration);
      a.removeAttribute('src');
      a.load();
      onKnown?.();
    }, { once: true });
    a.addEventListener('error', () => probing.delete(p), { once: true });
    a.src = url;
  }
  return 0;
}

export function rememberDuration(url: string, d: number | null | undefined) {
  if (d && d > 0) durations.set(pathOf(url), d);
}

// ---- peaks (waveforms drawn by the page) -------------------------------------------------

const peakCache = new Map<string, Promise<Float32Array | null>>();

/** Normalized peak amplitudes (0..1) in `n` buckets: the file decoded at 8 kHz mono. */
export function audioPeaks(url: string, n = 1024): Promise<Float32Array | null> {
  const k = `${pathOf(url)}|${n}`;
  let p = peakCache.get(k);
  if (p) return p;
  p = (async () => {
    try {
      const r = await fetch(url);
      if (!r.ok) return null;
      const buf = await r.arrayBuffer();
      if (buf.byteLength > 96 * 1024 * 1024) return null;
      const AC = (globalThis as any).OfflineAudioContext || (globalThis as any).webkitOfflineAudioContext;
      const ctx = new AC(1, 1, 8000);
      const audio: AudioBuffer = await ctx.decodeAudioData(buf);
      rememberDuration(url, audio.duration);
      const data = audio.getChannelData(0);
      const step = Math.max(1, Math.floor(data.length / n));
      const out = new Float32Array(n);
      let max = 1e-6;
      for (let i = 0; i < n; i++) {
        let m = 0;
        const end = Math.min(data.length, (i + 1) * step);
        for (let j = i * step; j < end; j += 2) m = Math.max(m, Math.abs(data[j]));
        out[i] = m;
        max = Math.max(max, m);
      }
      for (let i = 0; i < n; i++) out[i] /= max;
      return out;
    } catch {
      return null;
    }
  })();
  peakCache.set(k, p);
  if (peakCache.size > 24) peakCache.delete(peakCache.keys().next().value!);
  return p;
}
