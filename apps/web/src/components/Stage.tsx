import { useEffect, useRef, useState } from 'react';
import { createCanvasApp } from '../wasm';
import { setState, useStore } from '../store';
import { addSelectionToChat, attach, CANVAS_ID, importFiles, restore } from '../controller';
import { connectHost, hostSync } from '../sync';
import { NodeOverlays } from './NodeOverlays';
import { PresenceLayer } from './PresenceLayer';
import { StatusBar } from './StatusBar';
import { theme } from '../theme';

/** The local user. The host suffixes the actor id if several browsers connect. */
const ME = { id: 'you', name: 'you' };

const isMac = navigator.platform.toLowerCase().includes('mac') || navigator.userAgent.includes('Mac');

function editable(t: EventTarget | null) {
  const el = t as HTMLElement | null;
  return !!el && (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA' || el.tagName === 'SELECT' || el.isContentEditable);
}

/** The canvas surface: owns the wasm app, forwards input, hosts DOM overlays. */
export function Stage() {
  const wrap = useRef<HTMLDivElement>(null);
  const canvas = useRef<HTMLCanvasElement>(null);
  const [dragging, setDragging] = useState(false);
  const app = useStore((s) => s.app);
  const error = useStore((s) => s.error);
  const loading = useStore((s) => s.loading);
  const empty = useStore((s) => s.nodes.length === 0);

  // Create the app once.
  useEffect(() => {
    let disposed = false;
    let created: { stop(): void } | null = null;
    const el = canvas.current!;
    const box = wrap.current!.getBoundingClientRect();
    el.width = Math.max(1, Math.round(box.width * devicePixelRatio));
    el.height = Math.max(1, Math.round(box.height * devicePixelRatio));
    createCanvasApp(el, { actor: ME, canvas: { id: CANVAS_ID, name: CANVAS_ID } })
      .then(async (a) => {
        if (disposed) return;
        created = a;
        attach(a);
        a.setTheme(theme());
        a.resize(box.width, box.height, devicePixelRatio);
        setState({ app: a });
        const hosted = await connectHost(a, CANVAS_ID, ME);
        await restore(a, hosted);
        a.start();
        setState({ loading: false });
        (window as any).falos = a; // handy for debugging and agents driving the page
      })
      .catch((e) => setState({ error: String(e?.message ?? e), loading: false }));
    return () => {
      // Remounts (e.g. dev hot reload) get a fresh app; stop this one's render loop so two
      // GPU devices never present to the same canvas.
      disposed = true;
      created?.stop();
    };
  }, []);

  // Size + DPR tracking.
  useEffect(() => {
    if (!app) return;
    const ro = new ResizeObserver(() => {
      const r = wrap.current!.getBoundingClientRect();
      app.resize(r.width, r.height, devicePixelRatio);
    });
    ro.observe(wrap.current!);
    return () => ro.disconnect();
  }, [app]);

  // Pointer, wheel and gesture input.
  useEffect(() => {
    if (!app) return;
    const el = canvas.current!;
    const pos = (e: { clientX: number; clientY: number }) => {
      const r = el.getBoundingClientRect();
      return [e.clientX - r.left, e.clientY - r.top] as const;
    };
    const meta = (e: MouseEvent | KeyboardEvent) => (isMac ? e.metaKey : e.ctrlKey);
    const down = (e: PointerEvent) => {
      el.setPointerCapture(e.pointerId);
      const [x, y] = pos(e);
      app.pointerDown(x, y, e.button, e.shiftKey, e.altKey, meta(e));
    };
    const move = (e: PointerEvent) => {
      const [x, y] = pos(e);
      app.pointerMove(x, y, e.shiftKey, e.altKey, meta(e));
      const sync = hostSync();
      if (sync) {
        const w = app.screenToWorld(x, y);
        const c = app.camera() as { x: number; y: number; zoom: number; width: number; height: number };
        sync.sendPresence({ cursor: [w[0], w[1]], viewport: [c.x, c.y, c.width / c.zoom, c.height / c.zoom] });
      }
    };
    const up = (e: PointerEvent) => {
      const [x, y] = pos(e);
      app.pointerUp(x, y, e.shiftKey, e.altKey, meta(e));
    };
    const leave = () => app.pointerLeave();
    const wheel = (e: WheelEvent) => {
      e.preventDefault();
      const [x, y] = pos(e);
      const k = e.deltaMode === 1 ? 16 : e.deltaMode === 2 ? 400 : 1;
      // Browsers report trackpad pinch as ctrl+wheel.
      app.wheel(x, y, e.deltaX * k, e.deltaY * k, e.shiftKey, e.altKey, e.ctrlKey || e.metaKey);
    };
    let lastScale = 1;
    const gstart = (e: any) => { e.preventDefault(); lastScale = 1; };
    const gchange = (e: any) => {
      e.preventDefault();
      const [x, y] = pos(e);
      app.pinch(x, y, e.scale / lastScale);
      lastScale = e.scale;
    };
    const ctx = (e: Event) => e.preventDefault();
    el.addEventListener('pointerdown', down);
    el.addEventListener('pointermove', move);
    el.addEventListener('pointerup', up);
    el.addEventListener('pointercancel', up);
    el.addEventListener('pointerleave', leave);
    el.addEventListener('wheel', wheel, { passive: false });
    el.addEventListener('gesturestart', gstart as any);
    el.addEventListener('gesturechange', gchange as any);
    el.addEventListener('contextmenu', ctx);
    return () => {
      el.removeEventListener('pointerdown', down);
      el.removeEventListener('pointermove', move);
      el.removeEventListener('pointerup', up);
      el.removeEventListener('pointercancel', up);
      el.removeEventListener('pointerleave', leave);
      el.removeEventListener('wheel', wheel);
      el.removeEventListener('gesturestart', gstart as any);
      el.removeEventListener('gesturechange', gchange as any);
      el.removeEventListener('contextmenu', ctx);
    };
  }, [app]);

  // Keyboard shortcuts (ignored while typing in inputs).
  useEffect(() => {
    if (!app) return;
    const kd = (e: KeyboardEvent) => {
      if (editable(e.target)) return;
      if ((isMac ? e.metaKey : e.ctrlKey) && (e.key === 'l' || e.key === 'L')) {
        e.preventDefault();
        addSelectionToChat();
        return;
      }
      if (app.keyDown(e.key, e.shiftKey, e.altKey, isMac ? e.metaKey : e.ctrlKey)) e.preventDefault();
    };
    const ku = (e: KeyboardEvent) => {
      if (!editable(e.target)) app.keyUp(e.key);
    };
    window.addEventListener('keydown', kd);
    window.addEventListener('keyup', ku);
    return () => {
      window.removeEventListener('keydown', kd);
      window.removeEventListener('keyup', ku);
    };
  }, [app]);

  const onDrop = async (e: React.DragEvent) => {
    e.preventDefault();
    setDragging(false);
    const files = Array.from(e.dataTransfer.files);
    if (!files.length) return;
    const r = canvas.current!.getBoundingClientRect();
    await importFiles(files, [e.clientX - r.left, e.clientY - r.top]);
  };

  return (
    <div
      className="stage"
      ref={wrap}
      onDragOver={(e) => {
        if (e.dataTransfer.types.includes('Files')) {
          e.preventDefault();
          e.dataTransfer.dropEffect = 'copy';
          setDragging(true);
        }
      }}
      onDragLeave={(e) => {
        if (e.currentTarget === e.target || !wrap.current?.contains(e.relatedTarget as Node)) setDragging(false);
      }}
      onDrop={onDrop}
    >
      <canvas ref={canvas} className="stage-canvas" />
      <NodeOverlays />
      <PresenceLayer />
      {!loading && !error && empty && <div className="empty faint">empty canvas · drop files to import</div>}
      {error && <div className="empty">can't start the canvas: {error}</div>}
      {dragging && <div className="drop-hint">drop to import</div>}
      <StatusBar />
    </div>
  );
}
