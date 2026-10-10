import { useEffect, useRef, useState } from 'react';
import { useStore } from '../store';
import { ME, initials } from '../agents';
import { IDLE_MS } from '../sync';
import { exportCanvas, importCanvas, loadSamples, newCanvas } from '../controller';
import { IconMore, IconRedo, IconUndo } from '../icons';
import { Avatar } from './Avatar';

export function TopBar() {
  const app = useStore((s) => s.app);
  const name = useStore((s) => s.canvasName);
  const sessions = useStore((s) => s.sessions);
  const conn = useStore((s) => s.conn);
  const hostAddr = useStore((s) => s.hostAddr);
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
          <span className="crumb-dim">{conn === 'local' ? 'Local project' : 'Host project'}</span>
          <span className="crumb-sep">/</span>
          <span className="crumb">{name}</span>
        </div>
      </div>
      <div className="topbar-right">
        <div className="presence" aria-label="Who is on this canvas">
          {sessions.map((s) => {
            const active = Date.now() - s.presence.last_activity < IDLE_MS;
            const n = assigned(s.actor);
            const a = { id: s.actor, name: s.agentName, kind: s.kind, color: s.color, initials: initials(s.agentName) };
            return (
              <div key={s.id} className={`presence-item ${active ? 'live' : ''}`} title={`${s.agentName} (@${s.actor})${s.presence.status ? ` — ${s.presence.status}` : ''}${n ? ` · ${n} open task${n > 1 ? 's' : ''}` : ''}`}>
                <Avatar actor={a} size={24} ring={active} />
                {n > 0 && <span className="presence-badge">{n}</span>}
              </div>
            );
          })}
        </div>
        <span
          className={`conn-pill ${conn}`}
          title={conn === 'local' ? 'No fal-os host found: this canvas lives in the browser' : `fal-os host ${hostAddr ?? ''}`}
        >
          <span className="conn-dot" />
          {conn === 'local' ? 'Local' : conn === 'connected' ? 'Live' : conn === 'connecting' ? 'Connecting…' : 'Reconnecting…'}
        </span>
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
