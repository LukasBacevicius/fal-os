// Viewers for every modality, shared by the canvas preview (space tap / ⏎ / double-click on
// a node) and the library quick look: image (fit, zoom, pan), svg (crisp at any zoom), video
// and audio (one transport; waveform with playhead and scrub), PDF (page strip + paging),
// 3D (orbit viewer), font specimen, LUT before / after, subtitle cues, markdown / text,
// sandboxed HTML, OTIO / JSON, and a file card for anything else. Always on a dark backdrop.

import { createElement, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { getState, toast, useStore } from '../store';
import { clock } from '../video';
import { audioPause, audioPeaks, audioPlay, audioRelease, audioSeek, audioToggle, audioMute, audioSeekBy, rememberDuration, useAudio } from '../audio';
import { closePreview, hostPath, kindLabel, nodeItem, previewOrder, stepPreview, viewerKeys, type PreviewItem } from '../preview';
import { applyLut, cueTime, drawChart, parseCube, parseOtio, parseSubs, type Lut } from '../formats';
import { Transport, type Playback } from './VideoLayer';
import { Md } from './Transcript';
import { desktop, isDesktop } from '../desktop';
import { reveal as revealNodes } from '../out';
import { bytes as fmtBytes } from '../format';
import { appUrl, frames, isApp, sendMute, toggleSound } from '../apps';

const NO_IDS = new Set<string>();

/** Registers the viewer's own keys while it is mounted (see preview.ts `viewerKeys`). */
function useViewerKeys(handle: ((e: KeyboardEvent) => boolean) | null, deps: unknown[]) {
  useEffect(() => {
    if (!handle) return;
    viewerKeys.handle = handle;
    return () => {
      if (viewerKeys.handle === handle) viewerKeys.handle = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);
}

/** Fetched text (≤ 4 MB) of a URL, or the text at hand. */
function useText(url: string, given?: string | null): { text: string | null; error: string | null } {
  const [st, setSt] = useState<{ text: string | null; error: string | null }>({ text: given ?? null, error: null });
  useEffect(() => {
    if (given != null) return setSt({ text: given, error: null });
    if (!url) return setSt({ text: null, error: 'no file' });
    let live = true;
    setSt({ text: null, error: null });
    (async () => {
      try {
        const r = await fetch(url);
        if (!r.ok) throw new Error(`${r.status}`);
        const b = await r.blob();
        const t = await b.slice(0, 4 * 1024 * 1024).text();
        if (live) setSt({ text: b.size > 4 * 1024 * 1024 ? `${t}\n…` : t, error: null });
      } catch (e) {
        if (live) setSt({ text: null, error: `can't read the file (${e instanceof Error ? e.message : e})` });
      }
    })();
    return () => {
      live = false;
    };
  }, [url, given]);
  return st;
}

function useSize(ref: React.RefObject<HTMLElement>): [number, number] {
  const [size, setSize] = useState<[number, number]>([0, 0]);
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setSize([el.clientWidth, el.clientHeight]));
    ro.observe(el);
    setSize([el.clientWidth, el.clientHeight]);
    return () => ro.disconnect();
  }, [ref]);
  return size;
}

// ---- image / svg ---------------------------------------------------------------------------

interface View { s: number; x: number; y: number }

/** Fit, zoom (pinch / ⌘-wheel / + − / double-click), pan (drag / wheel when zoomed). The
 *  image is laid out at its zoomed size (not a CSS scale), so SVG stays crisp. */
function ImageView({ item }: { item: PreviewItem }) {
  const box = useRef<HTMLDivElement>(null);
  const [cw, ch] = useSize(box);
  const [src, setSrc] = useState(item.url || item.poster || '');
  const [nat, setNat] = useState<[number, number] | null>(null);
  const [view, setView] = useState<View | null>(null); // null = fit
  const vector = item.kind === 'svg';
  useEffect(() => {
    setSrc(item.url || item.poster || '');
    setNat(null);
    setView(null);
  }, [item.url, item.poster]);

  const fitS = nat && cw && ch ? Math.min((cw - 32) / nat[0], (ch - 32) / nat[1], vector ? 64 : 1) : 1;
  const fit: View = nat ? { s: fitS, x: (cw - nat[0] * fitS) / 2, y: (ch - nat[1] * fitS) / 2 } : { s: 1, x: 0, y: 0 };
  const v = view ?? fit;
  const zoomAt = (px: number, py: number, f: number) => {
    if (!nat) return;
    const s = Math.min(vector ? 256 : 32, Math.max(fitS * 0.5, v.s * f));
    setView({ s, x: px - (px - v.x) * (s / v.s), y: py - (py - v.y) * (s / v.s) });
  };
  const center = (): [number, number] => [cw / 2, ch / 2];
  const actual = () => nat && setView({ s: 1, x: (cw - nat[0]) / 2, y: (ch - nat[1]) / 2 });

  useViewerKeys((e) => {
    if (e.key === '+' || e.key === '=') return zoomAt(...center(), 1.25), true;
    if (e.key === '-' || e.key === '_') return zoomAt(...center(), 0.8), true;
    if (e.key === '0') return setView(null), true;
    if (e.key === '1') return actual(), true;
    return false;
  }, [v, nat, cw, ch]);

  // Wheel needs a non-passive listener to stop page zoom on pinch.
  const vref = useRef(v);
  vref.current = v;
  useEffect(() => {
    const el = box.current;
    if (!el) return;
    const wheel = (e: WheelEvent) => {
      e.preventDefault();
      const r = el.getBoundingClientRect();
      const [px, py] = [e.clientX - r.left, e.clientY - r.top];
      const zoomed = vref.current.s > fitS * 1.001;
      if (e.ctrlKey || e.metaKey || !zoomed) zoomAt(px, py, Math.exp(-e.deltaY * (e.ctrlKey ? 0.01 : 0.002)));
      else setView({ ...vref.current, x: vref.current.x - e.deltaX, y: vref.current.y - e.deltaY });
    };
    el.addEventListener('wheel', wheel, { passive: false });
    return () => el.removeEventListener('wheel', wheel);
  });

  const drag = useRef<{ x: number; y: number; v: View } | null>(null);
  return (
    <div
      ref={box}
      className="pv-image"
      onPointerDown={(e) => {
        if (e.button !== 0) return;
        e.currentTarget.setPointerCapture(e.pointerId);
        drag.current = { x: e.clientX, y: e.clientY, v };
      }}
      onPointerMove={(e) => {
        const d = drag.current;
        if (d) setView({ ...d.v, x: d.v.x + e.clientX - d.x, y: d.v.y + e.clientY - d.y });
      }}
      onPointerUp={() => (drag.current = null)}
      onDoubleClick={(e) => {
        const r = e.currentTarget.getBoundingClientRect();
        if (view && Math.abs(view.s - fitS) > 1e-3) setView(null);
        else zoomAt(e.clientX - r.left, e.clientY - r.top, Math.max(2, 1 / fitS) );
      }}
    >
      {src ? (
        <img
          src={src}
          alt=""
          draggable={false}
          style={nat ? { left: v.x, top: v.y, width: nat[0] * v.s, height: nat[1] * v.s } : { visibility: 'hidden' }}
          onLoad={(e) => {
            const im = e.currentTarget;
            let w = im.naturalWidth, h = im.naturalHeight;
            if (vector && (!w || !h)) [w, h] = [1024, 1024];
            setNat([w || item.w || 512, h || item.h || 512]);
          }}
          onError={() => item.poster && src !== item.poster && setSrc(item.poster)}
        />
      ) : (
        <div className="pv-empty">no image</div>
      )}
      {nat && (
        <div className="pv-hud" onPointerDown={(e) => e.stopPropagation()}>
          <span>{view ? `${Math.round(v.s * 100)}%` : `fit ${Math.round(fitS * 100)}%`}</span>
          <button className="link" title="fit (0)" onClick={() => setView(null)}>[fit]</button>
          <button className="link" title="actual size (1)" onClick={actual}>[1:1]</button>
          <button className="link" title="zoom out (−)" onClick={() => zoomAt(...center(), 0.8)}>[−]</button>
          <button className="link" title="zoom in (+)" onClick={() => zoomAt(...center(), 1.25)}>[+]</button>
          <span className="pv-faint">{nat[0]}×{nat[1]}{vector ? ' svg' : ''}</span>
        </div>
      )}
    </div>
  );
}

// ---- video ---------------------------------------------------------------------------------

function VideoView({ item }: { item: PreviewItem }) {
  const el = useRef<HTMLVideoElement>(null);
  const [pb, setPb] = useState<Playback>({ time: 0, duration: item.duration ?? 0, paused: true, muted: true });
  useEffect(() => {
    let raf = 0;
    const tick = () => {
      const v = el.current;
      if (v) setPb((p) => {
        const n = { time: v.currentTime, duration: Number.isFinite(v.duration) ? v.duration : p.duration, paused: v.paused, muted: v.muted };
        return n.time === p.time && n.duration === p.duration && n.paused === p.paused && n.muted === p.muted ? p : n;
      });
      raf = requestAnimationFrame(tick);
    };
    tick();
    return () => cancelAnimationFrame(raf);
  }, []);
  const toggle = () => {
    const v = el.current;
    if (!v) return;
    if (v.paused) void v.play().catch(() => {});
    else v.pause();
  };
  const seek = (t: number) => {
    const v = el.current;
    if (v) v.currentTime = Math.max(0, Math.min(t, (v.duration || t) - 0.01));
  };
  useViewerKeys((e) => {
    const v = el.current;
    if (!v) return false;
    switch (e.key) {
      case 'k': case 'K': toggle(); return true;
      case 'j': case 'J': seek(v.currentTime - 5); return true;
      case 'l': case 'L': seek(v.currentTime + 5); return true;
      case ',': case '<': v.pause(); seek(v.currentTime - 1 / 30); return true;
      case '.': case '>': v.pause(); seek(v.currentTime + 1 / 30); return true;
      case 'm': case 'M': v.muted = !v.muted; if (!v.muted) audioPause(); return true;
    }
    return false;
  }, []);
  return (
    <div className="pv-av">
      <video
        ref={el}
        className="pv-video"
        src={item.url}
        poster={item.poster}
        autoPlay
        loop
        muted
        playsInline
        onClick={toggle}
        onVolumeChange={(e) => !e.currentTarget.muted && audioPause()}
      />
      <div className="pv-transport">
        <Transport pb={pb} onToggle={toggle} onSeek={seek} onMute={() => el.current && (el.current.muted = !el.current.muted)} />
      </div>
    </div>
  );
}

// ---- audio ---------------------------------------------------------------------------------

/** A waveform drawn from the decoded file: played part bright, a playhead, click / drag to
 *  scrub, hover shows the time. Shares the app's single audio player. */
export function Waveform({ k, url, duration, poster, height = 160 }: { k: string; url: string; duration: number; poster?: string; height?: number }) {
  const pb = useAudio(k);
  const wrap = useRef<HTMLDivElement>(null);
  const cv = useRef<HTMLCanvasElement>(null);
  const [w] = useSize(wrap);
  const [peaks, setPeaks] = useState<Float32Array | null | undefined>(undefined);
  const [hover, setHover] = useState<number | null>(null);
  const dur = pb.duration || duration;
  useEffect(() => {
    let live = true;
    setPeaks(undefined);
    void audioPeaks(url, 1024).then((p) => live && setPeaks(p));
    return () => {
      live = false;
    };
  }, [url]);
  const frac = dur > 0 ? Math.min(1, pb.time / dur) : 0;
  useEffect(() => {
    const c = cv.current;
    if (!c || !w || !peaks) return;
    const dpr = devicePixelRatio || 1;
    c.width = Math.round(w * dpr);
    c.height = Math.round(height * dpr);
    const g = c.getContext('2d')!;
    g.clearRect(0, 0, c.width, c.height);
    const bar = 2 * dpr, gap = 1 * dpr;
    const n = Math.max(1, Math.floor(c.width / (bar + gap)));
    const mid = c.height / 2;
    const played = frac * c.width;
    for (let i = 0; i < n; i++) {
      const a = Math.max(0.02, peaks[Math.floor((i * peaks.length) / n)] ?? 0);
      const bh = Math.max(dpr, a * c.height * 0.9);
      const x = i * (bar + gap);
      g.fillStyle = x + bar <= played ? '#e6e6e6' : '#5a5a5a';
      g.fillRect(x, mid - bh / 2, bar, bh);
    }
  }, [peaks, w, height, frac]);
  const timeAt = (clientX: number) => {
    const r = wrap.current!.getBoundingClientRect();
    return Math.min(1, Math.max(0, (clientX - r.left) / Math.max(1, r.width))) * dur;
  };
  const scrubbing = useRef(false);
  return (
    <div
      ref={wrap}
      className="pv-wave"
      style={{ height }}
      onPointerDown={(e) => {
        if (!dur) return;
        e.currentTarget.setPointerCapture(e.pointerId);
        scrubbing.current = true;
        audioSeek(k, url, timeAt(e.clientX));
      }}
      onPointerMove={(e) => {
        if (!dur) return;
        const t = timeAt(e.clientX);
        setHover(t);
        if (scrubbing.current) audioSeek(k, url, t);
      }}
      onPointerUp={() => (scrubbing.current = false)}
      onPointerCancel={() => (scrubbing.current = false)}
      onPointerLeave={() => setHover(null)}
    >
      {peaks ? <canvas ref={cv} /> : peaks === null && poster ? <img src={poster} alt="" draggable={false} /> : <div className="pv-wave-line" />}
      {dur > 0 && <div className="pv-playhead" style={{ left: `${frac * 100}%` }} />}
      {hover != null && dur > 0 && (
        <div className="pv-wave-tip" style={{ left: `${(hover / dur) * 100}%` }}>{clock(hover)}</div>
      )}
    </div>
  );
}

function AudioView({ item, keep }: { item: PreviewItem; keep?: boolean }) {
  const k = item.key;
  const pb = useAudio(k);
  useEffect(() => {
    if (!item.url) return;
    rememberDuration(item.url, item.duration);
    audioPlay(k, item.url);
    return () => {
      if (!keep) audioRelease(k);
    };
  }, [k, item.url, keep, item.duration]);
  useViewerKeys((e) => {
    switch (e.key) {
      case 'k': case 'K': audioToggle(k, item.url); return true;
      case 'j': case 'J': audioSeekBy(k, item.url, -5); return true;
      case 'l': case 'L': audioSeekBy(k, item.url, 5); return true;
      case ',': case '<': audioSeekBy(k, item.url, -1); return true;
      case '.': case '>': audioSeekBy(k, item.url, 1); return true;
      case 'm': case 'M': audioMute(k, item.url); return true;
    }
    return false;
  }, [k, item.url]);
  if (!item.url) return <FileCard item={item} note="no audio file" />;
  const dur = pb.duration || item.duration || 0;
  return (
    <div className="pv-audio">
      <Waveform k={k} url={item.url} duration={dur} poster={item.poster} height={180} />
      <div className="pv-transport">
        <Transport
          pb={{ time: pb.time, duration: dur, paused: pb.paused, muted: pb.muted }}
          onToggle={() => audioToggle(k, item.url)}
          onSeek={(t) => audioSeek(k, item.url, t)}
          onMute={() => audioMute(k, item.url)}
        />
      </div>
    </div>
  );
}

// ---- pdf -----------------------------------------------------------------------------------

/** Pages rendered by the host (`/thumbs/pdf/<sha>/<page>`): a page strip and the pages
 *  stacked; PgUp / PgDn or [ ] page. Without host rendering: the browser's PDF viewer. */
function PdfView({ item }: { item: PreviewItem }) {
  const [pages, setPages] = useState<number | null>(null);
  const [native, setNative] = useState(false);
  const [cur, setCur] = useState(1);
  const main = useRef<HTMLDivElement>(null);
  const info = item.sha ? hostPath(item.url, `/thumbs/pdf/${item.sha}`) : null;
  const pageUrl = (p: number, w: number) => (item.sha ? hostPath(item.url, `/thumbs/pdf/${item.sha}/${p}?w=${w}`) ?? '' : '');
  useEffect(() => {
    setPages(null);
    setNative(false);
    setCur(1);
    if (!info) return setNative(true);
    let live = true;
    fetch(info)
      .then((r) => (r.ok ? r.json() : Promise.reject(new Error(String(r.status)))))
      .then((j) => live && setPages(Math.max(1, Number(j.pages) || 1)))
      .catch(() => live && setNative(true));
    return () => {
      live = false;
    };
  }, [info]);
  const go = (p: number) => {
    if (!pages) return;
    const n = Math.min(pages, Math.max(1, p));
    main.current?.querySelector(`[data-page="${n}"]`)?.scrollIntoView({ block: 'start' });
    setCur(n);
  };
  useViewerKeys((e) => {
    if (e.key === 'PageDown' || e.key === ']') return go(cur + 1), true;
    if (e.key === 'PageUp' || e.key === '[') return go(cur - 1), true;
    if (e.key === 'Home') return go(1), true;
    if (e.key === 'End' && pages) return go(pages), true;
    return false;
  }, [cur, pages]);
  if (native) return <iframe className="pv-pdf-native" src={item.url} title={item.name} />;
  if (!pages) return <div className="pv-empty">reading pdf…</div>;
  const list = Array.from({ length: pages }, (_, i) => i + 1);
  return (
    <div className="pv-pdf">
      <div className="pv-pdf-strip">
        {list.map((p) => (
          <button key={p} className={`pv-pdf-thumb${p === cur ? ' on' : ''}`} onClick={() => go(p)} title={`page ${p}`}>
            <img src={pageUrl(p, 256)} alt="" loading="lazy" draggable={false} />
            <span>{p}</span>
          </button>
        ))}
      </div>
      <div
        className="pv-pdf-main"
        ref={main}
        onScroll={(e) => {
          const el = e.currentTarget;
          const mid = el.scrollTop + el.clientHeight / 3;
          let best = 1;
          for (const c of Array.from(el.children) as HTMLElement[]) if (c.offsetTop <= mid) best = Number(c.dataset.page) || best;
          if (best !== cur) setCur(best);
        }}
      >
        {list.map((p) => (
          <div key={p} className="pv-pdf-page" data-page={p}>
            <img
              src={pageUrl(p, 1600)}
              alt={`page ${p}`}
              loading={p <= 2 ? 'eager' : 'lazy'}
              draggable={false}
              onError={() => p === 1 && setNative(true)}
            />
          </div>
        ))}
      </div>
      <div className="pv-hud">
        <button className="link" title="previous page (PgUp / [)" onClick={() => go(cur - 1)}>[‹]</button>
        <span>{cur} / {pages}</span>
        <button className="link" title="next page (PgDn / ])" onClick={() => go(cur + 1)}>[›]</button>
      </div>
    </div>
  );
}

// ---- 3d ------------------------------------------------------------------------------------

let mvLoad: Promise<unknown> | null = null;
/** <model-viewer> (three.js), loaded on first use. */
export function loadModelViewer() {
  mvLoad ??= import('@google/model-viewer');
  return mvLoad;
}

function ModelView({ item }: { item: PreviewItem }) {
  const [ready, setReady] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const el = useRef<HTMLElement | null>(null);
  useEffect(() => {
    let live = true;
    loadModelViewer().then(() => live && setReady(true), (e) => live && setErr(String(e)));
    return () => {
      live = false;
    };
  }, []);
  useEffect(() => {
    const m = el.current;
    if (!m) return;
    const onErr = () => setErr("can't load this model");
    m.addEventListener('error', onErr);
    return () => m.removeEventListener('error', onErr);
  }, [ready]);
  useViewerKeys((e) => {
    if (e.key === 'r' || e.key === 'R') {
      const m = el.current as any;
      if (m) {
        m.cameraOrbit = 'auto auto auto';
        m.cameraTarget = 'auto auto auto';
        m.fieldOfView = 'auto';
        m.jumpCameraToGoal?.();
      }
      return true;
    }
    return false;
  }, []);
  if (err) return <FileCard item={item} note={err} />;
  if (!ready) return <div className="pv-empty">loading 3d viewer…</div>;
  return (
    <div className="pv-model">
      {createElement('model-viewer', {
        ref: el,
        src: item.url,
        'camera-controls': '',
        'interaction-prompt': 'none',
        'shadow-intensity': '0',
        exposure: '1',
        'environment-image': 'neutral',
        class: 'pv-model-el',
      })}
      <div className="pv-hud"><span className="pv-faint">drag orbits · scroll zooms · R resets</span></div>
    </div>
  );
}

// ---- font ----------------------------------------------------------------------------------

const SIZES = [12, 18, 24, 36, 48, 72];
let fontSeq = 0;

/** Loads a font file as a FontFace (cached by URL); resolves to its family name. */
const faces = new Map<string, Promise<string>>();
export function loadFont(url: string): Promise<string> {
  let p = faces.get(url);
  if (p) return p;
  p = (async () => {
    const r = await fetch(url);
    if (!r.ok) throw new Error(`${r.status}`);
    const family = `falos-font-${++fontSeq}`;
    const face = new FontFace(family, await r.arrayBuffer());
    await face.load();
    document.fonts.add(face);
    return family;
  })();
  faces.set(url, p);
  p.catch(() => faces.delete(url));
  return p;
}

function FontView({ item }: { item: PreviewItem }) {
  const [family, setFamily] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    setFamily(null);
    loadFont(item.url).then((f) => live && setFamily(f), (e) => live && setErr(`can't load this font (${e})`));
    return () => {
      live = false;
    };
  }, [item.url]);
  if (err) return <FileCard item={item} note={err} />;
  if (!family) return <div className="pv-empty">loading font…</div>;
  const f = { fontFamily: `"${family}"` };
  return (
    <div className="pv-scroll">
      <div className="pv-font">
        <div className="pv-font-aa" style={f}>Aa</div>
        <div className="pv-font-set" style={f}>
          ABCDEFGHIJKLMNOPQRSTUVWXYZ<br />abcdefghijklmnopqrstuvwxyz<br />0123456789 &amp;@#%?!.,:;'"()[]{'{}'}
        </div>
        {SIZES.map((s) => (
          <div key={s} className="pv-font-row">
            <span className="pv-faint">{s}</span>
            <span style={{ ...f, fontSize: s, lineHeight: 1.25 }}>The quick brown fox jumps over the lazy dog</span>
          </div>
        ))}
      </div>
    </div>
  );
}

// ---- lut -----------------------------------------------------------------------------------

function LutCanvas({ lut, img, after }: { lut: Lut; img: HTMLImageElement | null; after: boolean }) {
  const cv = useRef<HTMLCanvasElement>(null);
  useEffect(() => {
    const c = cv.current;
    if (!c) return;
    const W = 480;
    const H = img ? Math.round((W * img.naturalHeight) / Math.max(1, img.naturalWidth)) : 270;
    c.width = W;
    c.height = H;
    const g = c.getContext('2d', { willReadFrequently: true })!;
    if (img) g.drawImage(img, 0, 0, W, H);
    else drawChart(g, W, H);
    if (after) {
      const d = g.getImageData(0, 0, W, H);
      applyLut(lut, d.data);
      g.putImageData(d, 0, 0);
    }
  }, [lut, img, after]);
  return <canvas ref={cv} className="pv-lut-cv" />;
}

function LutView({ item }: { item: PreviewItem }) {
  const { text, error } = useText(item.url);
  const lut = useMemo(() => {
    if (!text) return null;
    try {
      return parseCube(text);
    } catch (e) {
      return String(e);
    }
  }, [text]);
  const [img, setImg] = useState<HTMLImageElement | null>(null);
  useEffect(() => {
    setImg(null);
    if (!item.sample) return;
    const im = new Image();
    im.crossOrigin = 'anonymous';
    im.onload = () => setImg(im);
    im.src = item.sample;
  }, [item.sample]);
  if (error || typeof lut === 'string') return <FileCard item={item} note={error ?? String(lut)} />;
  if (!lut) return <div className="pv-empty">reading lut…</div>;
  return (
    <div className="pv-scroll">
      <div className="pv-lut">
        <div className="pv-faint">{lut.title || item.name} · {lut.size}³</div>
        <div className="pv-lut-pair">
          <figure><LutCanvas lut={lut} img={null} after={false} /><figcaption>before</figcaption></figure>
          <figure><LutCanvas lut={lut} img={null} after /><figcaption>after</figcaption></figure>
        </div>
        {img && (
          <div className="pv-lut-pair">
            <figure><LutCanvas lut={lut} img={img} after={false} /><figcaption>before</figcaption></figure>
            <figure><LutCanvas lut={lut} img={img} after /><figcaption>after</figcaption></figure>
          </div>
        )}
      </div>
    </div>
  );
}

// ---- text kinds ----------------------------------------------------------------------------

function SubsView({ item }: { item: PreviewItem }) {
  const { text, error } = useText(item.url);
  const cues = useMemo(() => (text ? parseSubs(text) : []), [text]);
  if (error) return <FileCard item={item} note={error} />;
  if (text == null) return <div className="pv-empty">reading…</div>;
  if (!cues.length) return <pre className="pv-text">{text}</pre>;
  return (
    <div className="pv-scroll">
      <div className="pv-subs">
        <div className="pv-faint">{cues.length} cues · {cueTime(cues[cues.length - 1].end)}</div>
        {cues.map((c, i) => (
          <div key={i} className="pv-cue">
            <span className="pv-faint">{String(i + 1).padStart(3, ' ')}</span>
            <span className="pv-dim">{cueTime(c.start)} → {cueTime(c.end)}</span>
            <span className="pv-cue-text">{c.text}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

function TextView({ item }: { item: PreviewItem }) {
  const { text, error } = useText(item.url, item.text);
  if (error) return <FileCard item={item} note={error} />;
  if (text == null) return <div className="pv-empty">reading…</div>;
  if (item.kind === 'markdown') {
    return (
      <div className="pv-scroll"><div className="pv-md"><Md text={text} ids={NO_IDS} /></div></div>
    );
  }
  if (item.kind === 'html') return <iframe className="pv-html" sandbox="allow-scripts" srcDoc={text} title={item.name} />;
  if (item.kind === 'json' || item.kind === 'timeline') {
    let json: any = null;
    try {
      json = JSON.parse(text);
    } catch {
      /* show as text */
    }
    const otio = parseOtio(json);
    if (otio) {
      return (
        <div className="pv-scroll">
          <div className="pv-otio">
            <div className="pv-faint">{otio.name || item.name} · {otio.tracks.length} tracks{otio.rate ? ` · ${otio.rate} fps` : ''}</div>
            {otio.tracks.map((t, i) => (
              <div key={i} className="pv-otio-track">
                <div className="pv-dim">{t.kind.toLowerCase()} {t.name}</div>
                {t.items.map((c, j) => (
                  <div key={j} className="pv-otio-clip">
                    <span className="pv-faint">{c.kind === 'Gap' ? 'gap ' : 'clip'}</span>
                    <span>{c.name || '–'}</span>
                    <span className="pv-dim">{c.dur ? cueTime(c.dur) : ''}</span>
                  </div>
                ))}
              </div>
            ))}
          </div>
        </div>
      );
    }
    if (json != null) return <pre className="pv-text">{JSON.stringify(json, null, 2)}</pre>;
  }
  return <pre className="pv-text">{text}</pre>;
}

// ---- file card -----------------------------------------------------------------------------

/** Opens the file in its default app (desktop) or a new tab (web). */
async function openItem(item: PreviewItem) {
  if (!isDesktop()) {
    if (item.url) window.open(item.url, '_blank', 'noopener');
    return;
  }
  try {
    const path = await pathOf(item);
    if (!path) return toast('nothing to open');
    const apps = await desktop.openWithList(path);
    const app = apps.find((a) => a.default) ?? apps[0];
    if (!app) return toast('no app opens this file');
    await desktop.openWith(path, app);
  } catch (e) {
    toast(`open failed: ${e}`);
  }
}

async function pathOf(item: PreviewItem): Promise<string | null> {
  const project = getState().project ?? undefined;
  if (item.sha) return desktop.materialize({ sha: item.sha, name: item.name, project });
  if (item.node) return desktop.materialize({ node: item.node, name: item.name, project });
  return null;
}

async function revealItem(item: PreviewItem) {
  if (item.node) return revealNodes([item.node]);
  if (!isDesktop()) {
    if (item.url) {
      const a = document.createElement('a');
      a.href = item.url;
      a.download = item.name;
      a.click();
    }
    return;
  }
  try {
    const path = await pathOf(item);
    if (path) await desktop.reveal(path);
  } catch (e) {
    toast(`reveal failed: ${e}`);
  }
}

export function ItemActions({ item }: { item: PreviewItem }) {
  if (!item.url && !item.node) return null;
  const desk = isDesktop();
  return (
    <>
      <button className="link" title={desk ? 'open in its default app' : 'open in a new tab'} onClick={() => void openItem(item)}>[open]</button>
      <button className="link" title={desk ? 'reveal in Finder' : 'download'} onClick={() => void revealItem(item)}>{desk ? '[reveal]' : '[download]'}</button>
    </>
  );
}

function FileCard({ item, note }: { item: PreviewItem; note?: string }) {
  return (
    <div className="pv-card">
      {item.poster && <img className="pv-card-poster" src={item.poster} alt="" draggable={false} onError={(e) => (e.currentTarget.style.display = 'none')} />}
      <div><span className="pv-dim">[{kindLabel(item.kind, item.mime, item.name)}]</span> {item.name}</div>
      <div className="pv-faint">
        {[item.mime, item.bytes ? fmtBytes(item.bytes) : '', item.sha ? item.sha.slice(0, 12) : ''].filter(Boolean).join(' · ')}
      </div>
      {note && <div className="pv-dim">{note}</div>}
      <div className="pv-card-actions"><ItemActions item={item} /></div>
    </div>
  );
}

// ---- apps ------------------------------------------------------------------------------------

/** A live app full screen: the same sandboxed page as on the canvas, focused, muted unless
 *  the node's sound is on. Esc (forwarded by the app) closes. */
function AppView({ item }: { item: PreviewItem }) {
  const node = useStore((s) => s.nodes.find((n) => n.id === item.node));
  const v = useStore((s) => (node && isApp(node) ? Math.max(s.apps.versions[String(node.props.app)] ?? 0, Number(node.props.version) || 0) : 0));
  const ref = useRef<HTMLIFrameElement>(null);
  const key = `${item.node}:preview`;
  const url = node && isApp(node) ? appUrl(node, v) : null;
  useEffect(() => {
    const f = ref.current;
    if (!f) return;
    frames.set(key, f);
    return () => {
      if (frames.get(key) === f) frames.delete(key);
    };
  }, [key, url]);
  if (!url) return <FileCard item={item} note="the app server isn't reachable" />;
  return (
    <iframe
      ref={ref}
      className="pv-app"
      src={url}
      title={item.name}
      sandbox="allow-scripts"
      allow="autoplay 'none'; camera 'none'; microphone 'none'; geolocation 'none'"
      referrerPolicy="no-referrer"
      onLoad={() => {
        ref.current?.focus();
        if (item.node) sendMute(item.node);
      }}
    />
  );
}

/** `[mute]` / `[sound]` and `[tab]` for an app in the preview head. */
function AppActions({ item }: { item: PreviewItem }) {
  const node = useStore((s) => s.nodes.find((n) => n.id === item.node));
  const sound = useStore((s) => (node && isApp(node) ? !!s.apps.sound[String(node.props.app)] : false));
  if (!node || !isApp(node)) return null;
  const url = appUrl(node);
  return (
    <>
      <button className="link" title={sound ? 'mute' : 'sound on'} onClick={() => toggleSound(node.id)}>{sound ? '[sound]' : '[mute]'}</button>
      {url && (
        <a className="link" href={url} target="_blank" rel="noreferrer noopener" title="open in a browser tab (sound plays there)">[tab]</a>
      )}
    </>
  );
}

// ---- dispatch ------------------------------------------------------------------------------

/** The right viewer for an item. `keepAudio`: leave audio playing when the viewer closes
 *  (the canvas node keeps its own transport). */
export function Viewer({ item, keepAudio }: { item: PreviewItem; keepAudio?: boolean }) {
  switch (item.kind) {
    case 'image':
    case 'svg':
      return <ImageView item={item} />;
    case 'video':
      return item.url ? <VideoView item={item} /> : <FileCard item={item} />;
    case 'audio':
      return <AudioView item={item} keep={keepAudio} />;
    case 'pdf':
      return <PdfView item={item} />;
    case 'model3d':
      return item.url ? <ModelView item={item} /> : <FileCard item={item} />;
    case 'usdz':
      return <FileCard item={item} note="usdz: open it in Preview or Reality Composer" />;
    case 'font':
      return <FontView item={item} />;
    case 'lut':
      return <LutView item={item} />;
    case 'subs':
      return <SubsView item={item} />;
    case 'app':
      return <AppView item={item} />;
    case 'markdown':
    case 'text':
    case 'html':
    case 'json':
    case 'timeline':
      return <TextView item={item} />;
    default:
      return <FileCard item={item} />;
  }
}

// ---- canvas preview ------------------------------------------------------------------------

/** The canvas preview: a node's viewer near-fullscreen. ← / → other media, esc / space
 *  closes. */
export function NodePreview() {
  const id = useStore((s) => s.preview);
  const node = useStore((s) => (s.preview ? s.nodes.find((n) => n.id === s.preview) : undefined));
  const item = useMemo(() => (node ? nodeItem(node) : null), [node?.id, node?.props.asset, node?.text]); // eslint-disable-line react-hooks/exhaustive-deps
  const order = useMemo(() => (id ? previewOrder() : []), [id]);
  useEffect(() => {
    if (id && !node) closePreview();
  }, [id, node]);
  if (!id || !item) return null;
  const idx = order.indexOf(id);
  const meta = [
    kindLabel(item.kind, item.mime, item.name),
    item.w && item.h ? `${item.w}×${item.h}` : '',
    item.bytes ? fmtBytes(item.bytes) : '',
    order.length > 1 && idx >= 0 ? `${idx + 1}/${order.length}` : '',
  ].filter(Boolean).join(' · ');
  return (
    <div className="pv-overlay" onMouseDown={(e) => e.target === e.currentTarget && e.preventDefault()} onPointerDown={(e) => e.target === e.currentTarget && closePreview()}>
      <div className="pv-head">
        <span>{item.name || id}</span>
        <span className="pv-faint">{meta}</span>
        <span className="pv-spacer" />
        {order.length > 1 && (
          <>
            <button className="link" title="previous (←)" onClick={() => stepPreview(-1)}>[←]</button>
            <button className="link" title="next (→)" onClick={() => stepPreview(1)}>[→]</button>
          </>
        )}
        {item.kind === 'app' ? <AppActions item={item} /> : <ItemActions item={item} />}
        <button className="link" title="close (esc / space)" onClick={closePreview}>[esc]</button>
      </div>
      <div className="pv-body" key={item.key}>
        <Viewer item={item} keepAudio />
      </div>
    </div>
  );
}
