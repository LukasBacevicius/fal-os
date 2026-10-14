import { dropData } from '../actions';
import { useEffect, useRef } from 'react';
import { ASSET_MIME, libraryDrop } from '../library';
import { dnd, endDrag, filesSession, startDrag } from '../dnd';
import { DropLayer, dropOver } from './DropLayer';
import { createCanvasApp } from '../wasm';
import { getState, setState, useStore } from '../store';
import { attach, CANVAS_ID, importFiles, restore } from '../controller';
import { connectHost, hostSync } from '../sync';
import { NodeOverlays } from './NodeOverlays';
import { PresenceLayer } from './PresenceLayer';
import { StatusBar } from './StatusBar';
import { ActivityBars } from './ActivityBars';
import { Guard } from './Guard';
import { SelectionPrompt } from './SelectionPrompt';
import { addContext } from '../controller';
import { TextLayer, createNote, editNode } from './TextLayer';
import { installKeys, pointer } from '../keys';
import { theme } from '../theme';
import { FloatingHead } from './AppMenu';
import { VideoLayer } from './VideoLayer';
import { AppLayer, clickedApp } from './AppLayer';
import { blurApp } from '../apps';
import { AudioLayer } from './AudioLayer';
import { openPreview, previewable } from '../preview';
import { openFocus, pointerPressed } from '../video';
import { openMenuAt } from './ContextMenu';
import { dragOut, prepare } from '../out';
import { isDesktop } from '../desktop';

/** The local user. The host suffixes the actor id if several browsers connect. */
const ME = { id: 'you', name: 'you' };

const isMac = navigator.platform.toLowerCase().includes('mac') || navigator.userAgent.includes('Mac');

/** Backing-store step (CSS px): the canvas is allocated at the window size rounded up to
 *  this, so resizing the visible area (window drag, chat panel) only moves the GPU viewport. */
const BUCKET = 64;

/** Visible size in device pixels + over-allocated backing store size, for `resizePx`. */
function canvasSizes(wrap: HTMLElement, entry?: ResizeObserverEntry): [number, number, number, number, number] {
  const dpr = devicePixelRatio || 1;
  const r = wrap.getBoundingClientRect();
  const dp = entry?.devicePixelContentBoxSize?.[0];
  const vw = Math.max(1, dp ? dp.inlineSize : Math.round(r.width * dpr));
  const vh = Math.max(1, dp ? dp.blockSize : Math.round(r.height * dpr));
  const bw = Math.ceil(Math.max(r.width, innerWidth) / BUCKET) * BUCKET;
  const bh = Math.ceil(Math.max(r.height, innerHeight) / BUCKET) * BUCKET;
  return [vw, vh, Math.max(vw, Math.round(bw * dpr)), Math.max(vh, Math.round(bh * dpr)), dpr];
}


/** The canvas surface: owns the wasm app, forwards input, hosts DOM overlays. */
export function Stage() {
  const wrap = useRef<HTMLDivElement>(null);
  const canvas = useRef<HTMLCanvasElement>(null);

  const app = useStore((s) => s.app);
  const error = useStore((s) => s.error);
  const loading = useStore((s) => s.loading);
  const empty = useStore((s) => s.nodes.length === 0);
  const layers = useStore((s) => s.outlineOpen ?? s.nodes.length > 0);
  const library = useStore((s) => s.library);

  // Create the app once.
  useEffect(() => {
    let disposed = false;
    let created: { stop(): void } | null = null;
    const el = canvas.current!;
    const initial = canvasSizes(wrap.current!);
    el.width = initial[2];
    el.height = initial[3];
    el.style.width = `${initial[2] / initial[4]}px`;
    el.style.height = `${initial[3] / initial[4]}px`;
    createCanvasApp(el, { actor: ME, canvas: { id: CANVAS_ID, name: CANVAS_ID } })
      .then(async (a) => {
        if (disposed) return;
        created = a;
        attach(a);
        a.setTheme(theme());
        a.resizePx(...canvasSizes(wrap.current!));
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

  // Size + DPR tracking. The ResizeObserver callback runs after layout and before paint;
  // `resizePx` reconfigures (only when the backing bucket changes) and draws a frame in the
  // same task, so the browser never shows a cleared or stretched canvas.
  useEffect(() => {
    if (!app) return;
    let last = '';
    const apply = (entry?: ResizeObserverEntry) => {
      if (!wrap.current) return; // unmounted
      const s = canvasSizes(wrap.current, entry);
      const key = s.join(',');
      if (key === last) return;
      last = key;
      app.resizePx(...s);
    };
    const ro = new ResizeObserver((entries) => apply(entries[entries.length - 1]));
    try {
      ro.observe(wrap.current!, { box: 'device-pixel-content-box' });
    } catch {
      ro.observe(wrap.current!); // WebKit: no device-pixel-content-box
    }
    // Window growth past the backing bucket without a stage size change (rare) and DPR
    // changes in browsers without device-pixel boxes.
    const onWin = () => apply();
    window.addEventListener('resize', onWin);
    return () => {
      ro.disconnect();
      window.removeEventListener('resize', onWin);
    };
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
    // Dragging a canvas node onto the chat panel attaches it (and puts it back).
    let press: { x: number; y: number; node: string | null } | null = null;
    // A click (no drag) on an app node focuses it.
    let appPress: { x: number; y: number; node: string | null } | null = null;
    const overChat = (e: PointerEvent) => {
      const c = document.querySelector('.chat');
      if (!c) return false;
      const r = c.getBoundingClientRect();
      return e.clientX >= r.left && e.clientX <= r.right && e.clientY >= r.top && e.clientY <= r.bottom;
    };
    const down = (e: PointerEvent) => {
      // The canvas isn't focusable: clicking it must still take focus away from the chat
      // input (or a note editor) so canvas shortcuts work.
      const active = document.activeElement as HTMLElement | null;
      if (active && active !== document.body) active.blur();
      el.setPointerCapture(e.pointerId);
      pointerPressed(); // a space press with a drag pans instead of toggling the video
      const [x, y] = pos(e);
      app.pointerDown(x, y, e.button, e.shiftKey, e.altKey, meta(e));
      const hit = app.hitTest(x, y) ?? null;
      appPress = e.button === 0 && !e.altKey && !e.shiftKey && !meta(e) && getState().tool === 'select' ? { x, y, node: hit } : null;
      if (getState().appFocus && hit !== getState().appFocus) blurApp();
      const sel = getState().selection;
      press = e.button === 0 && !e.altKey && getState().tool === 'select' && sel.length ? { x, y, node: app.hitTest(x, y) ?? null } : null;
      // Files for a drag-out are prepared before the drag needs them.
      if (press?.node) prepare(sel.includes(press.node) ? sel : [press.node], 0);
    };
    const move = (e: PointerEvent) => {
      const [x, y] = pos(e);
      pointer.at = [x, y];
      app.pointerMove(x, y, e.shiftKey, e.altKey, meta(e));
      if (press?.node) {
        const over = overChat(e);
        if (over !== getState().dragToChat) setState({ dragToChat: over });
        // Desktop drag-out: the pointer left the window with the button down. The canvas move
        // snaps back and a native file drag takes over (it needs the button still down, so
        // the paths were materialized ahead of time).
        const outside = e.clientX <= 0 || e.clientY <= 0 || e.clientX >= innerWidth - 1 || e.clientY >= innerHeight - 1;
        if (outside && (e.buttons & 1) && isDesktop()) {
          const sel = getState().selection;
          const ids = sel.includes(press.node) ? sel : [press.node];
          app.pointerMove(press.x, press.y, false, false, false);
          app.pointerUp(press.x, press.y, false, false, false);
          press = null;
          if (getState().dragToChat) setState({ dragToChat: false });
          try {
            el.releasePointerCapture(e.pointerId);
          } catch {
            /* not captured */
          }
          dragOut(ids);
          return;
        }
      }
      const sync = hostSync();
      if (sync) {
        const w = app.screenToWorld(x, y);
        const c = app.camera() as { x: number; y: number; zoom: number; width: number; height: number };
        sync.sendPresence({ cursor: [w[0], w[1]], viewport: [c.x, c.y, c.width / c.zoom, c.height / c.zoom] });
      }
    };
    const up = (e: PointerEvent) => {
      const [x, y] = pos(e);
      if (press?.node && getState().dragToChat) {
        // Dropped on the chat: the move snaps back (zero delta) and the selection is attached.
        app.pointerMove(press.x, press.y, false, false, false);
        app.pointerUp(press.x, press.y, false, false, false);
        setState({ dragToChat: false });
        addContext(getState().selection.map((node) => ({ kind: 'node' as const, node })));
        press = null;
        return;
      }
      press = null;
      if (getState().dragToChat) setState({ dragToChat: false });
      app.pointerUp(x, y, e.shiftKey, e.altKey, meta(e));
      if (appPress && Math.hypot(x - appPress.x, y - appPress.y) < 4 && appPress.node) clickedApp(appPress.node);
      appPress = null;
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
    // Right-click: the node menu (copy, open in, reveal, export, …).
    const ctx = (e: MouseEvent) => {
      e.preventDefault();
      const [x, y] = pos(e);
      openMenuAt(e.clientX, e.clientY, app.hitTest(x, y) ?? null, [x, y]);
    };
    // Double-click: edit a note/text in place, or create a note on empty canvas.
    const dbl = (e: MouseEvent) => {
      const [x, y] = pos(e);
      // Groups: the first double-click enters the group (selects inside it).
      const r = app.doubleClick(x, y) as { entered: string | null; node: string | null };
      if (r.entered) return;
      const n = r.node ? getState().nodes.find((m) => m.id === r.node) : null;
      if (n && (n.kind === 'note' || n.kind === 'text')) editNode(n.id);
      else if (n && n.kind === 'video' && n.props.asset) openFocus(n.id);
      else if (n && previewable(n)) openPreview(n.id);
      else if (!n || n.kind === 'frame') createNote([x, y]);
    };
    el.addEventListener('dblclick', dbl);
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
      el.removeEventListener('dblclick', dbl);
    };
  }, [app]);

  // Keyboard: see keys.ts (one handler for the whole app).
  useEffect(() => installKeys(), []);

  // Files from Finder and library tiles: sized ghosts at the pointer, the frame under it
  // highlighted, and the drop lands there as a grid (into the frame). Esc cancels (native).
  const local = (e: React.DragEvent): [number, number] => {
    const r = canvas.current!.getBoundingClientRect();
    return [e.clientX - r.left, e.clientY - r.top];
  };
  const onDrop = async (e: React.DragEvent) => {
    e.preventDefault();
    const p = local(e);
    const frame = app?.frameAt(p[0], p[1]) ?? null;
    dropOver(null);
    endDrag();
    if (libraryDrop(e, canvas.current!.getBoundingClientRect(), frame)) return;
    const files = Array.from(e.dataTransfer.files);
    if (!files.length) return void dropData(e.dataTransfer, p, frame);
    await importFiles(files, p, { parent: frame });
  };

  return (
    <div
      className="stage"
      ref={wrap}
      onDragOver={(e) => {
        const types = e.dataTransfer.types;
        const lib = types.includes(ASSET_MIME);
        const other = types.includes('text/uri-list') || types.includes('text/html') || types.includes('text/plain');
        if (!lib && !types.includes('Files') && !other) return;
        e.preventDefault();
        e.dataTransfer.dropEffect = 'copy';
        if (!dnd().session && (lib || types.includes('Files'))) startDrag(lib ? { source: 'library', items: [{ w: 512, h: 512, kind: 'image' }] } : filesSession(e.dataTransfer));
        const p = local(e);
        dropOver(p, app?.frameAt(p[0], p[1]) ?? null);
      }}
      onDragLeave={(e) => {
        if (!wrap.current?.contains(e.relatedTarget as Node)) dropOver(null);
      }}
      onDrop={onDrop}
    >
      <canvas ref={canvas} className="stage-canvas" />
      <Guard name="apps"><AppLayer /></Guard>
      <Guard name="text"><TextLayer /></Guard>
      <Guard name="overlays"><NodeOverlays /></Guard>
      <Guard name="video"><VideoLayer /></Guard>
      <Guard name="audio"><AudioLayer /></Guard>
      <Guard name="presence"><PresenceLayer /></Guard>
      <Guard name="selection-prompt"><SelectionPrompt /></Guard>
      {!loading && !error && empty && (
        <div className="empty faint">
          <div>
            drop files or press N · ? for keys
            <br />
            <button className="link dim empty-connect" onClick={() => setState({ overlay: 'connect' })}>[connect your agent]</button>
          </div>
        </div>
      )}
      {error && <div className="empty">can't start the canvas: {error}</div>}
      <Guard name="drop"><DropLayer /></Guard>
      <ActivityBars />
      <StatusBar />
      {!layers && library !== 'panel' && <FloatingHead />}
    </div>
  );
}
