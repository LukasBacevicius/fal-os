// Click paths for everything that also has a key: the top-left `[≡]` app menu (projects,
// panels, chats, import / export, connect agent, settings, shortcuts, palette) and the left
// panel's `layers · library` tabs. Plain mono text rows; every row shows its shortcut so
// clicks teach keys.

import { useEffect, useRef, useState } from 'react';
import { getState, setState, toast, useStore } from '../store';
import { invoke } from '../desktop';
import { MOD, newChat, toggleChat, toggleOutline } from '../keys';
import { continueInAgent, toggleHistory } from '../chatActions';
import { openLibrary } from '../library';
import { openExport } from '../out';
import { setReduceMotion } from '../activity';
import { setTheme, theme } from '../theme';
import { importPicked } from '../controller';
import { openPicker, ProjectHeader } from './Projects';

interface Row { label: string; hint?: string; run: () => void; on?: boolean; sep?: boolean }

const isDesk = () => !!(window as any).__TAURI_INTERNALS__;

/** The layers tab (remembered, like ⌘⇧\). */
function showLayers() {
  try {
    localStorage.setItem('falos.outline', '1');
  } catch {
    /* private mode */
  }
  setState({ library: null, outlineOpen: true });
}

function rows(): Row[] {
  const s = getState();
  const layersOpen = s.library !== 'panel' && (s.outlineOpen ?? s.nodes.length > 0);
  const light = theme() === 'light';
  const hosted = s.workspace === 'connected' && s.conn !== 'local';
  return [
    ...(hosted ? [{ label: 'projects…', hint: `${MOD}O`, run: openPicker }] : []),
    { label: 'layers', hint: `${MOD}⇧\\`, on: layersOpen, run: () => (s.library === 'panel' ? showLayers() : toggleOutline()) },
    { label: 'library', hint: `${MOD}⇧A`, on: s.library === 'panel', run: () => openLibrary('panel') },
    { label: 'library, full screen', hint: `${MOD}⇧L`, on: s.library === 'full', run: () => openLibrary('full') },
    { label: 'chat panel', hint: `${MOD}\\`, on: s.chatOpen, run: toggleChat },
    { label: 'new chat', hint: `${MOD}N`, run: newChat, sep: true },
    { label: 'all chats', hint: `${MOD}Y`, run: toggleHistory },
    { label: 'import files…', hint: 'drop', run: () => importPicked(), sep: true },
    { label: 'export…', hint: `${MOD}⇧E`, run: openExport },
    { label: 'continue in Claude Code', hint: 'copy cmd', run: () => continueInAgent('claude'), sep: true },
    { label: 'continue in Codex', hint: 'copy cmd', run: () => continueInAgent('codex') },
    { label: 'connect agent…', hint: 'mcp', run: () => setState({ overlay: 'connect' }) },
    ...(isDesk() ? [{ label: 'setup…', hint: 'app', run: () => setState({ overlay: 'setup' as const }) }] : []),
    ...(isDesk() ? [{ label: 'view logs', hint: 'finder', run: () => void invoke('open_logs').catch((e) => toast(`open logs failed: ${e}`)) }] : []),
    { label: 'undo', hint: `${MOD}Z`, run: () => s.app?.undo(), sep: true },
    { label: 'redo', hint: `${MOD}⇧Z`, run: () => s.app?.redo() },
    { label: `theme: ${light ? 'light' : 'dark'}`, hint: light ? '→ dark' : '→ light', run: () => setTheme(light ? 'dark' : 'light', s.app), sep: true },
    { label: `reduce motion: ${s.reduceMotion ? 'on' : 'off'}`, hint: s.reduceMotion ? '→ off' : '→ on', run: () => setReduceMotion(!s.reduceMotion) },
    { label: 'keyboard shortcuts', hint: '?', run: () => setState({ overlay: 'help' }), sep: true },
    { label: 'command palette', hint: `${MOD}K`, run: () => setState({ overlay: 'palette' }) },
  ];
}

/** `[≡]`: the app menu (top-left). */
export function AppMenu() {
  const [open, setOpen] = useState<{ x: number; y: number } | null>(null);
  const box = useRef<HTMLDivElement>(null);
  // Re-render rows when the states they show change.
  useStore((s) => `${s.library}|${s.outlineOpen}|${s.chatOpen}|${s.reduceMotion}|${s.workspace}|${s.conn}`);
  useEffect(() => {
    if (!open) return;
    const off = (e: PointerEvent) => !box.current?.contains(e.target as Node) && setOpen(null);
    const esc = (e: KeyboardEvent) => e.key === 'Escape' && (e.preventDefault(), e.stopPropagation(), setOpen(null));
    window.addEventListener('pointerdown', off, true);
    window.addEventListener('keydown', esc, true);
    return () => {
      window.removeEventListener('pointerdown', off, true);
      window.removeEventListener('keydown', esc, true);
    };
  }, [open]);
  return (
    <span className="app-menu" ref={box}>
      <button
        className={`link ${open ? '' : 'dim'}`}
        title={`menu: projects, panels, chats, export, settings, shortcuts (${MOD}K for everything)`}
        onClick={(e) => {
          const r = e.currentTarget.getBoundingClientRect();
          setOpen(open ? null : { x: r.left, y: r.bottom + 2 });
        }}
      >
        [≡]
      </button>
      {open && (
        <div className="menu" style={{ left: open.x, top: open.y }}>
          {rows().map((r) => (
            <div
              key={r.label}
              className={`menu-row ${r.sep ? 'menu-sep' : ''}`}
              onClick={() => {
                setOpen(null);
                r.run();
              }}
            >
              <span>{r.on ? '✓ ' : '  '}{r.label}</span>
              <span className="dim">{r.hint ?? ''}</span>
            </div>
          ))}
        </div>
      )}
    </span>
  );
}

/** Left panel tabs: `layers · library` (click switches), `[⤢]` full-screen library, `[×]`
 *  closes the panel. */
export function PanelTabs() {
  const library = useStore((s) => s.library);
  const closePanel = () => {
    try {
      localStorage.setItem('falos.outline', '0');
    } catch {
      /* private mode */
    }
    setState({ library: null, outlineOpen: false });
  };
  return (
    <div className="panel-head">
      <div className="panel-row">
        <AppMenu />
        <ProjectHeader />
        <span className="panel-gap" />
        <button className="link faint" title={`close the panel (${MOD}⇧\\)`} onClick={closePanel}>[×]</button>
      </div>
      <div className="panel-row panel-tabs">
        <button className={`link ${library !== 'panel' ? 'on' : 'dim'}`} title={`layers and properties (${MOD}⇧\\)`} onClick={() => showLayers()}>layers</button>
        <span className="faint">·</span>
        <button className={`link ${library === 'panel' ? 'on' : 'dim'}`} title={`asset library (${MOD}⇧A)`} onClick={() => setState({ library: 'panel' })}>library</button>
        <span className="panel-gap" />
        <button className="link dim" title={`library full screen (${MOD}⇧L)`} onClick={() => setState({ library: 'full' })}>[⤢]</button>
      </div>
    </div>
  );
}

/** Top-left of the canvas when the left panel is closed: `[≡] project ▾  layers · library`. */
export function FloatingHead() {
  return (
    <div className="project-float">
      <AppMenu />
      <ProjectHeader floating />
      <button className="link faint" title={`layers and properties (${MOD}⇧\\)`} onClick={() => showLayers()}>layers</button>
      <span className="faint">·</span>
      <button className="link faint" title={`asset library (${MOD}⇧A)`} onClick={() => setState({ library: 'panel' })}>library</button>
    </div>
  );
}
