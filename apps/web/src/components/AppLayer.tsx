import { useEffect, useMemo, useRef, useState } from 'react';
import { getState, setState, useStore } from '../store';
import { worldRects } from '../geometry';
import type { NodeView } from '../types';
import { appId, appUrl, appVersion, blurApp, focusApp, focusFrame, frames, isApp, sendMute, toggleSound } from '../apps';
import { openPreview } from '../preview';

/** Live iframes only for apps at least this big on screen (CSS px); smaller ones show the
 *  engine's snapshot still. */
const LIVE_MIN_W = 220;
const LIVE_MIN_H = 140;
/** At most this many live apps at once (the biggest on screen; the focused one always). */
const LIVE_MAX = 3;
/** Header line only when the node is at least this wide on screen. */
const HEAD_MIN_W = 140;

/** `[reload]` from the header: the live view reloads (a new iframe swaps in when loaded). */
export function reloadApp(id: string) {
  window.dispatchEvent(new CustomEvent('falos:app-reload', { detail: id }));
}

interface Layer {
  key: number;
  url: string;
  ready: boolean;
}

/**
 * One live app: a sandboxed iframe (scripts only: an opaque origin, no access to the canvas)
 * at the node's world rect inside the camera-transformed layer. A new version loads in a
 * second, hidden iframe that swaps in once loaded, so a live reload never flashes.
 */
function LiveApp({ node, rect, focused, selected, zoom }: { node: NodeView; rect: [number, number, number, number]; focused: boolean; selected: boolean; zoom: number }) {
  const v = appVersion(node);
  const [nonce, setNonce] = useState(0);
  const url = useMemo(() => appUrl(node, v), [node.id, node.props.entry, v]); // eslint-disable-line react-hooks/exhaustive-deps
  const [layers, setLayers] = useState<Layer[]>([]);
  const seq = useRef(0);
  const refs = useRef(new Map<number, HTMLIFrameElement>());

  useEffect(() => {
    const on = (e: Event) => (e as CustomEvent).detail === node.id && setNonce((n) => n + 1);
    window.addEventListener('falos:app-reload', on);
    return () => window.removeEventListener('falos:app-reload', on);
  }, [node.id]);

  useEffect(() => {
    if (!url) return;
    const key = ++seq.current;
    const u = nonce ? `${url}&r=${nonce}` : url;
    setLayers((ls) => [...ls.filter((l) => l.ready).slice(-1), { key, url: u, ready: false }]);
  }, [url, nonce]);

  // The top ready iframe is "the" frame of this node (focus, mute messages).
  useEffect(() => {
    const top = [...layers].reverse().find((l) => l.ready);
    const el = top ? refs.current.get(top.key) : undefined;
    if (el) frames.set(node.id, el);
    for (const l of layers) {
      const f = refs.current.get(l.key);
      if (f && !l.ready) frames.set(`${node.id}:${l.key}`, f);
      else frames.delete(`${node.id}:${l.key}`);
    }
    if (focused) focusFrame(node.id);
    return () => {
      if (frames.get(node.id) === el) frames.delete(node.id);
    };
  }, [layers, node.id, focused]);

  useEffect(() => () => {
    for (const k of [...frames.keys()]) if (k === node.id || k.startsWith(`${node.id}:`)) frames.delete(k);
  }, [node.id]);

  const loaded = (key: number) => {
    // A beat after `load` so the first frame has painted, then drop the old one.
    window.setTimeout(() => {
      setLayers((ls) => {
        const i = ls.findIndex((l) => l.key === key);
        if (i < 0) return ls;
        return ls.slice(i).map((l) => (l.key === key ? { ...l, ready: true } : l));
      });
      sendMute(node.id);
    }, 120);
  };

  return (
    <div
      className={`app-live${focused ? ' focused' : selected ? ' selected' : ''}`}
      style={{ left: rect[0], top: rect[1], width: rect[2], height: rect[3], outlineWidth: `${1 / zoom}px`, outlineOffset: `${-1 / zoom}px` }}
    >
      {layers.map((l) => (
        <iframe
          key={l.key}
          ref={(el) => {
            if (el) refs.current.set(l.key, el);
            else refs.current.delete(l.key);
          }}
          title={node.name || appId(node)}
          src={l.url}
          sandbox="allow-scripts"
          allow="autoplay 'none'; camera 'none'; microphone 'none'; geolocation 'none'"
          referrerPolicy="no-referrer"
          tabIndex={focused ? 0 : -1}
          onLoad={() => loaded(l.key)}
          style={{ opacity: l.ready ? 1 : 0, pointerEvents: focused && l.ready ? 'auto' : 'none' }}
        />
      ))}
    </div>
  );
}

/** `app · name · [src] [reload] [open] [mute]` above an app node (screen space). */
function Head({ node, left, top, width, focused }: { node: NodeView; left: number; top: number; width: number; focused: boolean }) {
  const id = appId(node);
  const errors = useStore((s) => s.apps.errors[id] ?? 0);
  const sound = useStore((s) => !!s.apps.sound[id]);
  const stop = (e: React.SyntheticEvent) => e.stopPropagation();
  return (
    <div
      className={`app-head${focused ? ' focused' : ''}`}
      style={{ transform: `translate(${left}px, ${top - 16}px)`, maxWidth: Math.max(width, 260) }}
      onPointerDown={stop}
      onMouseDown={(e) => e.preventDefault()}
      onDoubleClick={stop}
    >
      <span className="faint">app</span> · <span className="app-name">{node.name || id}</span>
      {errors > 0 && <span title="console errors in the current version (agents see them via app_logs)"> · ! {errors}</span>}
      {focused && <span className="faint"> · esc</span>}
      {' · '}
      <button className="link" title="read-only sources" onClick={() => setState({ appSrc: node.id })}>[src]</button>{' '}
      <button className="link" title="reload" onClick={() => reloadApp(node.id)}>[reload]</button>{' '}
      <button className="link" title="full screen (space)" onClick={() => openPreview(node.id)}>[open]</button>{' '}
      <button className="link" title={sound ? 'mute' : 'sound on'} onClick={() => toggleSound(node.id)}>{sound ? '[sound]' : '[mute]'}</button>
    </div>
  );
}

/**
 * App nodes over the canvas: live iframes for the apps big enough on screen (the rest show
 * the engine's snapshot still), each with a one-line mono header. Click or ⏎ focuses an app
 * (Stage / keys), Esc returns to the canvas.
 */
export function AppLayer() {
  const app = useStore((s) => s.app);
  const nodes = useStore((s) => s.nodes);
  const camera = useStore((s) => s.camera);
  const focus = useStore((s) => s.appFocus);
  const selection = useStore((s) => s.selection);
  const base = useStore((s) => s.apps.base);
  const preview = useStore((s) => s.preview);
  useStore((s) => s.apps.versions); // re-render on live reloads
  const rects = useMemo(() => worldRects(nodes), [nodes]);
  const apps = useMemo(() => nodes.filter(isApp), [nodes]);

  // A focused app that left the canvas (deleted, reverted) gives the keyboard back.
  useEffect(() => {
    if (focus && !apps.some((n) => n.id === focus)) blurApp();
  }, [focus, apps]);

  if (!app || !apps.length) return null;
  const z = camera.zoom;
  const screen = (r: [number, number, number, number]) => {
    const [x0, y0] = app.worldToScreen(r[0], r[1]);
    const [x1, y1] = app.worldToScreen(r[0] + r[2], r[1] + r[3]);
    return { left: x0, top: y0, width: x1 - x0, height: y1 - y0 };
  };
  const onScreen = (b: { left: number; top: number; width: number; height: number }) =>
    b.left < camera.width && b.top < camera.height && b.left + b.width > 0 && b.top + b.height > 0;

  const boxes = apps.map((n) => ({ n, r: rects.get(n.id)!, b: screen(rects.get(n.id)!) })).filter((x) => x.r && onScreen(x.b));
  const live = new Set(
    boxes
      .filter((x) => x.n.id === focus || (x.b.width >= LIVE_MIN_W && x.b.height >= LIVE_MIN_H))
      .sort((a, b) => (a.n.id === focus ? -1 : b.n.id === focus ? 1 : b.b.width * b.b.height - a.b.width * a.b.height))
      .slice(0, LIVE_MAX)
      .map((x) => x.n.id),
  );
  // The full-screen preview runs its own copy: pause the canvas one meanwhile.
  if (preview) live.delete(preview);

  return (
    <div className="app-layer">
      {base && (
        <div className="app-world" style={{ transform: `translate(${-camera.x * z}px, ${-camera.y * z}px) scale(${z})` }}>
          {boxes
            .filter((x) => live.has(x.n.id))
            .map((x) => (
              <LiveApp key={x.n.id} node={x.n} rect={x.r} zoom={z} focused={focus === x.n.id} selected={selection.includes(x.n.id)} />
            ))}
        </div>
      )}
      {boxes
        .filter((x) => x.b.width >= HEAD_MIN_W && x.b.top > 8)
        .map((x) => (
          <Head key={x.n.id} node={x.n} left={x.b.left} top={x.b.top} width={x.b.width} focused={focus === x.n.id} />
        ))}
    </div>
  );
}

/** Canvas click on an app node (no drag): focus it. */
export function clickedApp(id: string | null) {
  const n = id ? getState().nodes.find((x) => x.id === id) : null;
  if (isApp(n)) focusApp(n.id);
  else if (getState().appFocus) blurApp();
}
