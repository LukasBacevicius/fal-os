import { useEffect, useRef, useState } from 'react';
import { setState, useStore } from '../store';
import { AGENTS, ME } from '../agents';
import { exportCanvas, importCanvas, loadSamples, newCanvas } from '../controller';
import { IconMore, IconRedo, IconUndo } from '../icons';
import { Avatar } from './Avatar';

export function TopBar() {
  const app = useStore((s) => s.app);
  const name = useStore((s) => s.canvasName);
  const presence = useStore((s) => s.presence);
  const simulate = useStore((s) => s.simulateAgents);
  const annotations = useStore((s) => s.annotations);
  const [menu, setMenu] = useState(false);
  const open = useRef<HTMLInputElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!menu) return;
    const close = (e: PointerEvent) => {
      if (!menuRef.current?.contains(e.target as Node)) setMenu(false);
    };
    window.addEventListener('pointerdown', close);
    return () => window.removeEventListener('pointerdown', close);
  }, [menu]);

  const assigned = (id: string) => annotations.filter((a) => a.assignee === id && a.status !== 'resolved').length;

  return (
    <header className="topbar">
      <div className="topbar-left">
        <div className="logo" aria-label="fal-os">
          <svg width="18" height="18" viewBox="0 0 24 24"><path d="M4 4h16v4H8v4h9v4H8v4H4z" fill="currentColor" /></svg>
        </div>
        <div className="crumbs">
          <span className="crumb-dim">Local project</span>
          <span className="crumb-sep">/</span>
          <span className="crumb">{name}</span>
        </div>
      </div>
      <div className="topbar-right">
        <div className="presence" aria-label="Agents on this canvas">
          {AGENTS.map((a) => {
            const p = presence.find((x) => x.actor === a.id);
            const n = assigned(a.id);
            return (
              <div key={a.id} className={`presence-item ${p ? 'live' : ''}`} title={`${a.name}${p?.status ? ` — ${p.status}` : p ? ' — on canvas' : ' — not connected'}${n ? ` · ${n} open task${n > 1 ? 's' : ''}` : ''}`}>
                <Avatar actor={a} size={24} ring={!!p} />
                {n > 0 && <span className="presence-badge">{n}</span>}
              </div>
            );
          })}
        </div>
        <button
          className={`toggle ${simulate ? 'on' : ''}`}
          onClick={() => setState({ simulateAgents: !simulate })}
          title="Simulate agent presence (local demo; real agents connect through the host)"
        >
          <span className="toggle-dot" /> Live agents
        </button>
        <div className="topbar-sep" />
        <button className="icon-btn" title="Undo (⌘Z)" onClick={() => app?.undo()}><IconUndo /></button>
        <button className="icon-btn" title="Redo (⇧⌘Z)" onClick={() => app?.redo()}><IconRedo /></button>
        <div className="menu-wrap" ref={menuRef}>
          <button className="icon-btn" title="Canvas menu" onClick={() => setMenu(!menu)}><IconMore /></button>
          {menu && (
            <div className="menu">
              <button onClick={() => { setMenu(false); loadSamples(); }}>Add sample media</button>
              <button onClick={() => { setMenu(false); exportCanvas(); }}>Export canvas…</button>
              <button onClick={() => { setMenu(false); open.current?.click(); }}>Open canvas file…</button>
              <div className="menu-sep" />
              <button className="danger" onClick={() => { setMenu(false); if (confirm('Clear this local canvas and its media?')) newCanvas(); }}>New empty canvas</button>
            </div>
          )}
        </div>
        <input ref={open} type="file" accept=".json,application/json" hidden onChange={(e) => { const f = e.target.files?.[0]; if (f) importCanvas(f); e.target.value = ''; }} />
        <Avatar actor={ME} size={26} />
      </div>
    </header>
  );
}
