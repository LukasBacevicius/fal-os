import { useEffect, useMemo, useReducer, useRef, useState } from 'react';
import { getState, useStore } from '../store';
import { worldRects } from '../geometry';
import { clock } from '../video';
import { audioDuration, audioMute, audioPause, audioSeek, audioToggle, rememberDuration, useAudio } from '../audio';
import { assetRecord } from '../out';
import { openPreview } from '../preview';
import { Transport } from './VideoLayer';

const BADGE_MIN_W = 120;
const BADGE_MIN_H = 48;
/** The engine's audio card (crates/client media.rs `cardSource`): the waveform sits 10 world
 *  units in from the sides and bottom, under a 36 unit label row. */
const PAD = 10;
const TOP = 36.4;

/** Forwards wheel events from an overlay to the canvas (zoom / pan keep working over it). */
function forwardWheel(e: React.WheelEvent) {
  const c = document.querySelector('.stage-canvas');
  if (!c) return;
  c.dispatchEvent(new WheelEvent('wheel', e.nativeEvent));
}

/**
 * Audio affordances over the canvas: a `♪ 0:30` badge on audio tiles (time while playing), and
 * on the selected one a playhead over its waveform (click / drag scrubs) plus the same
 * transport as video. One audio plays at a time (audio.ts).
 */
export function AudioLayer() {
  const app = useStore((s) => s.app);
  const nodes = useStore((s) => s.nodes);
  const selection = useStore((s) => s.selection);
  const preview = useStore((s) => s.preview);
  useStore((s) => s.camera); // follow pans and zooms
  const editing = useStore((s) => s.editing);
  const [, bump] = useReducer((x: number) => x + 1, 0);
  const rects = useMemo(() => worldRects(nodes), [nodes]);
  const audios = useMemo(() => nodes.filter((n) => n.kind === 'audio' && typeof n.props.asset === 'string'), [nodes]);
  const selected = selection.length === 1 && audios.some((a) => a.id === selection[0]) ? selection[0] : null;
  const sel = useAudio(selected);

  // Leaving the selected audio stops it (unless its preview is open).
  const prev = useRef<string | null>(null);
  useEffect(() => {
    const was = prev.current;
    prev.current = selected;
    if (was && was !== selected && getState().preview !== was) {
      const cur = getState().selection;
      if (!cur.includes(was)) audioPause();
    }
  }, [selected]);

  if (!app || !audios.length) return null;
  const cam = getState().camera;
  const urlOf = (asset: string) => app.assetUrl(asset) ?? '';
  const durOf = (id: string, asset: string) => {
    const url = urlOf(asset);
    const rec = assetRecord(id);
    const d = Number(rec?.media?.duration) || 0;
    if (d) rememberDuration(url, d);
    return d || (url ? audioDuration(url, bump) : 0);
  };
  const screen = (id: string) => {
    const r = rects.get(id)!;
    const [x0, y0] = app.worldToScreen(r[0], r[1]);
    const [x1, y1] = app.worldToScreen(r[0] + r[2], r[1] + r[3]);
    return { left: x0, top: y0, width: x1 - x0, height: y1 - y0, k: (x1 - x0) / Math.max(1, r[2]) };
  };
  const visible = (b: { left: number; top: number; width: number; height: number }) =>
    b.left < cam.width && b.top < cam.height && b.left + b.width > 0 && b.top + b.height > 0;

  const out: React.ReactNode[] = [];
  for (const a of audios) {
    const b = screen(a.id);
    if (!visible(b)) continue;
    const asset = String(a.props.asset);
    const dur = durOf(a.id, asset);
    if (a.id !== selected) {
      if (b.width < BADGE_MIN_W || b.height < BADGE_MIN_H) continue;
      out.push(
        <div key={`ab-${a.id}`} className="video-badge" style={{ transform: `translate(${b.left + 6}px, ${b.top + b.height - 20}px)` }}>
          {dur > 0 ? `♪ ${clock(dur)}` : '♪'}
        </div>,
      );
      continue;
    }
    // Selected: playhead + scrub over the waveform, transport under the tile.
    const url = urlOf(asset);
    const d = sel.duration || dur;
    const wx = b.left + PAD * b.k, ww = Math.max(1, b.width - 2 * PAD * b.k);
    const wy = b.top + Math.min(TOP * b.k, b.height * 0.4), wh = Math.max(1, b.top + b.height - PAD * b.k - wy);
    const frac = d > 0 ? Math.min(1, sel.time / d) : 0;
    if (b.height >= 40 && !editing) {
      out.push(
        <Scrub key={`aw-${a.id}`} id={a.id} url={url} duration={d} frac={frac} active={sel.key === a.id} box={{ left: wx, top: wy, width: ww, height: wh }} />,
      );
    }
    if (!editing && preview !== a.id) {
      const w = Math.max(280, Math.min(b.width, 560));
      const below = b.top + b.height + 30 < cam.height;
      const top = below ? b.top + b.height + 6 : Math.min(b.top + b.height, cam.height) - 26;
      const left = Math.min(Math.max(4, b.left), Math.max(4, cam.width - w - 4));
      out.push(
        <div key={`at-${a.id}`} className={`video-transport ${below ? '' : 'over'}`} style={{ transform: `translate(${left}px, ${top}px)`, width: w }} onWheel={forwardWheel}>
          <Transport
            pb={{ time: sel.time, duration: d, paused: sel.paused, muted: sel.muted }}
            onToggle={() => audioToggle(a.id, url)}
            onSeek={(t) => audioSeek(a.id, url, t)}
            onMute={() => audioMute(a.id, url)}
          >
            <button className="link" title="preview (⏎ / double-click)" onClick={() => openPreview(a.id)}>[⤢]</button>
          </Transport>
        </div>,
      );
    }
  }
  return <div className="node-overlays audio-layer">{out}</div>;
}

/** The waveform area of the selected audio tile: playhead, played tint, hover time,
 *  click / drag to scrub (the label row above still drags the node). */
function Scrub({ id, url, duration, frac, active, box }: { id: string; url: string; duration: number; frac: number; active: boolean; box: { left: number; top: number; width: number; height: number } }) {
  const [hover, setHover] = useState<number | null>(null);
  const down = useRef(false);
  const at = (e: React.PointerEvent) => {
    const r = e.currentTarget.getBoundingClientRect();
    return Math.min(1, Math.max(0, (e.clientX - r.left) / Math.max(1, r.width)));
  };
  return (
    <div
      className="audio-scrub"
      style={{ transform: `translate(${box.left}px, ${box.top}px)`, width: box.width, height: box.height }}
      title="click or drag to scrub · space plays"
      onPointerDown={(e) => {
        if (e.button !== 0 || !duration) return;
        e.stopPropagation();
        e.preventDefault();
        e.currentTarget.setPointerCapture(e.pointerId);
        down.current = true;
        audioSeek(id, url, at(e) * duration);
      }}
      onPointerMove={(e) => {
        if (!duration) return;
        const f = at(e);
        setHover(f);
        if (down.current) audioSeek(id, url, f * duration);
      }}
      onPointerUp={() => (down.current = false)}
      onPointerCancel={() => (down.current = false)}
      onPointerLeave={() => setHover(null)}
      onDoubleClick={(e) => {
        e.stopPropagation();
        openPreview(id);
      }}
      onWheel={forwardWheel}
    >
      {(active || frac > 0) && <div className="audio-played" style={{ width: `${frac * 100}%` }} />}
      <div className="audio-head" style={{ left: `${frac * 100}%` }} />
      {hover != null && duration > 0 && <div className="scrub-tip" style={{ left: `${hover * 100}%` }}>{clock(hover * duration)}</div>}
    </div>
  );
}
