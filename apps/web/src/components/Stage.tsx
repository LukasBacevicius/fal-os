import { useEffect, useRef, useState } from 'react';
import { createCanvasApp } from '../wasm';
import { getState, setState, useStore } from '../store';
import { attach, CANVAS_ID, importFiles, loadSamples, restore } from '../controller';
import { ME, simulateAgents } from '../agents';
import { Composer } from './Composer';
import { PresenceLayer } from './PresenceLayer';
import { Toolbar } from './Toolbar';
import { StatusBar } from './StatusBar';
import { IconUpload } from '../icons';

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
  const simulate = useStore((s) => s.simulateAgents);

  // Create the app once.
  useEffect(() => {
    let disposed = false;
    let created: { stop(): void } | null = null;
    const el = canvas.current!;
    const box = wrap.current!.getBoundingClientRect();
    el.width = Math.max(1, Math.round(box.width * devicePixelRatio));
    el.height = Math.max(1, Math.round(box.height * devicePixelRatio));
    createCanvasApp(el, { actor: { id: ME.id, name: ME.name }, canvas: { id: CANVAS_ID, name: 'Untitled canvas' } })
      .then(async (a) => {
        if (disposed) return;
        created = a;
        attach(a);
        a.resize(box.width, box.height, devicePixelRatio);
        setState({ app: a });
        await restore(a);
        a.start();
        setState({ loading: false });
        // `?samples`: seed an empty canvas with the generated sample media (demos, shell probes).
        if (new URLSearchParams(location.search).has('samples') && getState().nodes.length === 0) {
          await loadSamples();
        }
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

  // Demo agent presence.
  useEffect(() => {
    if (!app || !simulate) {
      app?.setPresence([]);
      setState({ presence: [] });
      return;
    }
    return simulateAgents(
      () => getState().nodes.filter((n) => n.kind !== 'frame' && n.w > 0),
      (p) => {
        app.setPresence(p);
        setState({ presence: p });
      },
    );
  }, [app, simulate]);

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
      <PresenceLayer />
      <Composer />
      {!loading && !error && empty && (
        <div className="empty">
          <div className="empty-card">
            <div className="empty-icon"><IconUpload size={22} /></div>
            <div className="empty-title">Drop media to start</div>
            <div className="empty-sub">Images, video, audio, 3D, PDFs and text — or paste a link.</div>
            <button className="btn" onClick={() => loadSamples()}>Load sample media</button>
          </div>
        </div>
      )}
      {error && (
        <div className="empty">
          <div className="empty-card">
            <div className="empty-title">Can't start the canvas</div>
            <div className="empty-sub">{error}</div>
          </div>
        </div>
      )}
      {dragging && <div className="drop-hint"><span>Drop to add to canvas</span></div>}
      <Toolbar />
      <StatusBar />
    </div>
  );
}
