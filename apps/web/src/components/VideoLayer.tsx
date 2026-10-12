import { useEffect, useMemo, useRef, useState } from 'react';
import { getState, useStore } from '../store';
import { worldRects } from '../geometry';
import { clock, closeFocus, focus, openFocus, stepFocus, toggleMute, togglePlay, videosInOrder, videoState, type VideoState } from '../video';

/** What a transport shows and drives (canvas video or the focus view's element). */
export interface Playback {
  time: number;
  duration: number;
  paused: boolean;
  muted: boolean;
}

interface TransportProps {
  pb: Playback;
  onToggle(): void;
  onSeek(t: number): void;
  onMute(): void;
  /** Extra trailing buttons. */
  children?: React.ReactNode;
}

/** `[▶] 0:07 / 0:30 ───●─── [sound]`: mono text buttons and a 1px scrub line. */
export function Transport({ pb, onToggle, onSeek, onMute, children }: TransportProps) {
  const bar = useRef<HTMLDivElement>(null);
  const [hoverT, setHoverT] = useState<{ x: number; t: number } | null>(null);
  const [scrub, setScrub] = useState<number | null>(null);
  const timeAt = (clientX: number) => {
    const r = bar.current!.getBoundingClientRect();
    const f = Math.min(1, Math.max(0, (clientX - r.left) / Math.max(1, r.width)));
    return { x: f * r.width, t: f * pb.duration };
  };
  const shown = scrub ?? pb.time;
  const frac = pb.duration > 0 ? Math.min(1, shown / pb.duration) : 0;
  return (
    <div
      className="transport"
      onPointerDown={(e) => e.stopPropagation()}
      // Buttons never take focus: space / K stay the canvas's play toggle.
      onMouseDown={(e) => e.preventDefault()}
      onDoubleClick={(e) => e.stopPropagation()}
    >
      <button className="link" title={pb.paused ? 'play (space / K)' : 'pause (space / K)'} onClick={onToggle}>
        {pb.paused ? '[▶]' : '[❚❚]'}
      </button>
      <span className="transport-time">
        {clock(shown)} <span className="faint">/</span> {pb.duration > 0 ? clock(pb.duration) : '–:––'}
      </span>
      <div
        ref={bar}
        className="scrub"
        onPointerDown={(e) => {
          if (!pb.duration) return;
          e.currentTarget.setPointerCapture(e.pointerId);
          const { t } = timeAt(e.clientX);
          setScrub(t);
          onSeek(t);
        }}
        onPointerMove={(e) => {
          if (!pb.duration) return;
          const h = timeAt(e.clientX);
          setHoverT(h);
          if (scrub != null) {
            setScrub(h.t);
            onSeek(h.t);
          }
        }}
        onPointerUp={() => setScrub(null)}
        onPointerCancel={() => setScrub(null)}
        onPointerLeave={() => setHoverT(null)}
      >
        <div className="scrub-track" />
        <div className="scrub-fill" style={{ width: `${frac * 100}%` }} />
        <div className="scrub-head" style={{ left: `${frac * 100}%` }} />
        {hoverT && <div className="scrub-tip" style={{ left: hoverT.x }}>{clock(hoverT.t)}</div>}
      </div>
      <button className="link" title="sound (M)" onClick={onMute}>
        {pb.muted ? '[mute]' : '[sound]'}
      </button>
      {children}
    </div>
  );
}

/** Polls a canvas video's state every frame while `id` is set (rAF pauses in hidden tabs). */
function useVideoState(id: string | null): VideoState | null {
  const [st, setSt] = useState<VideoState | null>(null);
  useEffect(() => {
    if (!id) {
      setSt(null);
      return;
    }
    let raf = 0;
    let last = '';
    const tick = () => {
      const s = videoState(id);
      const key = s ? `${s.time.toFixed(2)}|${s.duration}|${s.paused}|${s.muted}|${s.playing}|${s.ready}` : '';
      if (key !== last) {
        last = key;
        setSt(s);
      }
      raf = requestAnimationFrame(tick);
    };
    tick();
    return () => cancelAnimationFrame(raf);
  }, [id]);
  return st;
}

// Durations never change for an asset: read once (the state call crosses into wasm).
const durations = new Map<string, number>();
function durationOf(id: string, asset: string): number {
  const d = durations.get(asset);
  if (d) return d;
  const v = videoState(id)?.duration ?? 0;
  if (v > 0) durations.set(asset, v);
  return v;
}

const BADGE_MIN_W = 120;
const BADGE_MIN_H = 64;

/**
 * Video affordances over the canvas: a `▶ 0:30` badge on video tiles big enough on screen
 * (hover plays them muted), and a minimal transport under the selected video.
 */
export function VideoLayer() {
  const app = useStore((s) => s.app);
  const nodes = useStore((s) => s.nodes);
  const hover = useStore((s) => s.hoverNode);
  const selection = useStore((s) => s.selection);
  const camera = useStore((s) => s.camera);
  const editing = useStore((s) => s.editing);
  const focusOpen = useStore((s) => s.focusVideo != null);
  const rects = useMemo(() => worldRects(nodes), [nodes]);
  const videos = useMemo(() => nodes.filter((n) => n.kind === 'video' && n.props.asset), [nodes]);
  const selected = selection.length === 1 && videos.some((v) => v.id === selection[0]) ? selection[0] : null;
  const hovered = hover && hover !== selected && videos.some((v) => v.id === hover) ? hover : null;
  const sel = useVideoState(selected);
  const hov = useVideoState(hovered);

  // Leaving a video: back to muted, and drop a user pause so hover / selection play it again.
  useEffect(() => {
    if (!selected || !app) return;
    return () => {
      app.videoMute(selected, true);
      app.videoPlay(selected, true);
    };
  }, [selected, app]);

  if (!app || !videos.length) return null;
  const screen = (id: string) => {
    const r = rects.get(id)!;
    const [x0, y0] = app.worldToScreen(r[0], r[1]);
    const [x1, y1] = app.worldToScreen(r[0] + r[2], r[1] + r[3]);
    return { left: x0, top: y0, width: x1 - x0, height: y1 - y0 };
  };
  const visible = (b: { left: number; top: number; width: number; height: number }) =>
    b.left < camera.width && b.top < camera.height && b.left + b.width > 0 && b.top + b.height > 0;

  const badges = videos.map((v) => {
    if (v.id === selected) return null;
    const b = screen(v.id);
    if (!visible(b) || b.width < BADGE_MIN_W || b.height < BADGE_MIN_H) return null;
    const live = v.id === hovered && hov?.playing ? hov : null;
    const dur = live?.duration || durationOf(v.id, String(v.props.asset));
    const text = live ? `${clock(live.time)} / ${clock(dur)}` : dur > 0 ? `▶ ${clock(dur)}` : '▶';
    return (
      <div key={`vb-${v.id}`} className="video-badge" style={{ transform: `translate(${b.left + 6}px, ${b.top + b.height - 20}px)` }}>
        {text}
      </div>
    );
  });

  let transport: React.ReactNode = null;
  if (selected && sel && !editing && !focusOpen) {
    const b = screen(selected);
    if (visible(b)) {
      const w = Math.max(280, Math.min(b.width, 560));
      // Under the tile when there is room, else over its bottom edge.
      const below = b.top + b.height + 30 < camera.height;
      const top = below ? b.top + b.height + 6 : Math.min(b.top + b.height, camera.height) - 26;
      const left = Math.min(Math.max(4, b.left), Math.max(4, camera.width - w - 4));
      transport = (
        <div className={`video-transport ${below ? '' : 'over'}`} style={{ transform: `translate(${left}px, ${top}px)`, width: w }}>
          <Transport
            pb={{ time: sel.time, duration: sel.duration, paused: sel.paused, muted: sel.muted }}
            onToggle={() => togglePlay(selected)}
            onSeek={(t) => app.videoSeek(selected, t)}
            onMute={() => toggleMute(selected)}
          >
            <button className="link" title="focus view (⏎ / double-click)" onClick={() => openFocus(selected)}>
              [⤢]
            </button>
          </Transport>
        </div>
      );
    }
  }

  return (
    <div className="node-overlays video-layer">
      {badges}
      {transport}
    </div>
  );
}

/**
 * Focus view: the video near-fullscreen on a dark backdrop, in a real <video> with sound at
 * native resolution (same asset URL, no re-download), with the transport. ← / → walk the
 * canvas's videos, esc closes; the canvas video resumes from where the focus view left off.
 */
export function VideoFocus() {
  const id = useStore((s) => s.focusVideo);
  const app = useStore((s) => s.app);
  const el = useRef<HTMLVideoElement>(null);
  const [pb, setPb] = useState<Playback>({ time: 0, duration: 0, paused: true, muted: false });
  const info = useMemo(() => (id ? videoState(id) : null), [id]);
  const name = useStore((s) => s.nodes.find((n) => n.id === id)?.name ?? '');
  const count = useMemo(() => (id ? videosInOrder() : []), [id]);
  const [win, setWin] = useState<[number, number]>([innerWidth, innerHeight]);
  useEffect(() => {
    if (!id) return;
    const on = () => setWin([innerWidth, innerHeight]);
    on();
    window.addEventListener('resize', on);
    return () => window.removeEventListener('resize', on);
  }, [id]);

  useEffect(() => {
    const v = el.current;
    if (!id || !app || !v || !info?.url) return;
    focus.el = v;
    const start = info.time;
    app.videoPlay(id, false); // hold the canvas copy while the focus view plays
    const onMeta = () => {
      if (start > 0 && start < (v.duration || Infinity)) v.currentTime = start;
      v.play().catch(() => {
        // Autoplay with sound refused (no user gesture): start muted instead.
        v.muted = true;
        void v.play().catch(() => {});
      });
    };
    v.addEventListener('loadedmetadata', onMeta, { once: true });
    v.src = info.url;
    let raf = 0;
    const tick = () => {
      setPb((p) => {
        const n = { time: v.currentTime, duration: Number.isFinite(v.duration) ? v.duration : 0, paused: v.paused, muted: v.muted };
        return n.time === p.time && n.duration === p.duration && n.paused === p.paused && n.muted === p.muted ? p : n;
      });
      raf = requestAnimationFrame(tick);
    };
    tick();
    return () => {
      cancelAnimationFrame(raf);
      v.removeEventListener('loadedmetadata', onMeta);
      const t = v.currentTime;
      v.pause();
      v.removeAttribute('src');
      v.load();
      if (focus.el === v) focus.el = null;
      if (getState().nodes.some((n) => n.id === id)) {
        app.videoSeek(id, t);
        app.videoPlay(id, true);
      }
    };
  }, [id, app, info]);

  if (!id || !info) return null;
  const idx = count.indexOf(id);
  const seek = (t: number) => {
    const v = el.current;
    if (v) v.currentTime = Math.max(0, Math.min(t, (v.duration || t) - 0.01));
  };
  // Fit the frame to the window (head + transport rows above and below), at its aspect.
  const [nw, nh] = info.width > 0 && info.height > 0 ? [info.width, info.height] : [16, 9];
  const k = Math.min((win[0] - 32) / nw, (win[1] - 32 - 2 * 20 - 2 * 8) / nh);
  const box = { width: Math.max(160, Math.floor(nw * k)), height: Math.max(90, Math.floor(nh * k)) };
  return (
    <div className="video-focus" onMouseDown={(e) => e.preventDefault()} onPointerDown={(e) => e.target === e.currentTarget && closeFocus()}>
      <div className="video-focus-head" style={{ width: box.width }}>
        <span className="dim">{name || id}</span>
        <span className="faint">
          {info.width > 0 ? `${info.width}×${info.height}` : ''}
          {count.length > 1 ? ` · ${idx + 1}/${count.length}` : ''}
        </span>
        <span className="video-focus-spacer" />
        {count.length > 1 && (
          <>
            <button className="link dim" title="previous video (←)" onClick={() => stepFocus(-1)}>[←]</button>{' '}
            <button className="link dim" title="next video (→)" onClick={() => stepFocus(1)}>[→]</button>{' '}
          </>
        )}
        <button className="link dim" title="close (esc)" onClick={closeFocus}>[esc]</button>
      </div>
      <video
        ref={el}
        className="video-focus-video"
        playsInline
        loop
        crossOrigin="anonymous"
        style={box}
        onClick={() => el.current && (el.current.paused ? void el.current.play() : el.current.pause())}
      />
      <div className="video-focus-transport" style={{ width: box.width }}>
        <Transport
          pb={pb}
          onToggle={() => el.current && (el.current.paused ? void el.current.play() : el.current.pause())}
          onSeek={seek}
          onMute={() => el.current && (el.current.muted = !el.current.muted)}
        />
      </div>
    </div>
  );
}
