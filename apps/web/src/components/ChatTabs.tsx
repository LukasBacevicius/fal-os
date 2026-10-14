import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { getState, setState, useStore } from '../store';
import { hostSync } from '../sync';
import { chatColor, chatTitle, isExternal, isRunning, tabs, type ChatState } from '../chat';
import { askDelete, closeTab, confirmDelete, openChat, persistTabOrder, toggleHistory } from '../chatActions';
import { newChat, MOD } from '../keys';
import { usd } from '../format';
import { fuzzy } from './Palette';

export function ago(ms?: number): string {
  if (!ms) return '';
  const s = Math.max(0, Math.round((Date.now() - ms) / 1000));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.round(s / 60)}m`;
  if (s < 86400) return `${Math.round(s / 3600)}h`;
  return `${Math.round(s / 86400)}d`;
}

function Swatch({ chat, id }: { chat: ChatState; id: string }) {
  return <span className="swatch" style={{ background: chatColor(chat, id) }} />;
}

/** Right-click menu on a tab: plain text rows. */
function TabMenu({ id, at, onClose }: { id: string; at: [number, number]; onClose: () => void }) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const off = (e: PointerEvent) => !ref.current?.contains(e.target as Node) && onClose();
    const esc = (e: KeyboardEvent) => e.key === 'Escape' && onClose();
    window.addEventListener('pointerdown', off, true);
    window.addEventListener('keydown', esc, true);
    return () => {
      window.removeEventListener('pointerdown', off, true);
      window.removeEventListener('keydown', esc, true);
    };
  }, [onClose]);
  const run = (f: () => void) => () => {
    onClose();
    f();
  };
  const ext = isExternal(id);
  return (
    <div className="menu" ref={ref} style={{ left: at[0], top: at[1] }}>
      <div className="menu-row" onClick={run(() => window.dispatchEvent(new CustomEvent('falos:rename-tab', { detail: id })))}>rename</div>
      <div className="menu-row" onClick={run(() => closeTab(id))}>close tab</div>
      {!ext && <div className="menu-row" onClick={run(() => hostSync()?.archiveChat(id))}>archive</div>}
      {!ext && <div className="menu-row" onClick={run(() => askDelete(id))}>delete… <span className="faint">{MOD}⌫</span></div>}
    </div>
  );
}

/** The tab strip at the top of the chat panel: one tab per open chat (swatch, title, `*`
 *  running, `•` unread, ⌘1…9 badges while ⌘ is held), drag to reorder, `+` new, `≡` history. */
export function ChatTabs() {
  const chat = useStore((s) => s.chat);
  const modHeld = useStore((s) => s.modHeld);
  const view = useStore((s) => s.chatView);
  const list = useMemo(() => tabs(chat), [chat]);
  const [menu, setMenu] = useState<{ id: string; at: [number, number] } | null>(null);
  const [renaming, setRenaming] = useState<string | null>(null);
  const [drag, setDrag] = useState<{ id: string; over: string | null; after: boolean } | null>(null);
  const strip = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const r = (e: Event) => setRenaming((e as CustomEvent).detail);
    window.addEventListener('falos:rename-tab', r);
    return () => window.removeEventListener('falos:rename-tab', r);
  }, []);

  // Keep the active tab in view.
  useLayoutEffect(() => {
    strip.current?.querySelector<HTMLElement>('.tab.on')?.scrollIntoView({ block: 'nearest', inline: 'nearest' });
  }, [chat.active, list.length]);

  const drop = () => {
    if (!drag?.over || drag.over === drag.id) return setDrag(null);
    const order = list.filter((x) => x !== drag.id);
    const i = order.indexOf(drag.over) + (drag.after ? 1 : 0);
    order.splice(i, 0, drag.id);
    persistTabOrder(order);
    setDrag(null);
  };

  return (
    <div className="tabs-bar">
      <div className="tabs" ref={strip} onWheel={(e) => { if (strip.current && Math.abs(e.deltaY) > Math.abs(e.deltaX)) strip.current.scrollLeft += e.deltaY; }}>
        {list.map((id, i) => {
          const running = isRunning(chat, id);
          const on = id === chat.active && view === 'chat';
          return (
            <div
              key={id}
              className={`tab ${on ? 'on' : ''} ${drag?.over === id ? (drag.after ? 'drop-after' : 'drop-before') : ''}`}
              tabIndex={0}
              draggable={renaming !== id}
              title={`${chatTitle(chat, id)}${isExternal(id) ? ' · your agent (message it here)' : chat.meta[id]?.driver ? ` · ${chat.meta[id]?.driver}` : ''}`}
              onClick={() => openChat(id)}
              onDoubleClick={() => setRenaming(id)}
              onContextMenu={(e) => {
                e.preventDefault();
                setMenu({ id, at: [e.clientX, e.clientY] });
              }}
              onKeyDown={(e) => {
                if (renaming === id) return;
                const mod = e.metaKey || e.ctrlKey;
                if (mod && e.key === 'Backspace' && !isExternal(id)) {
                  e.preventDefault();
                  askDelete(id);
                } else if (e.key === 'Enter') openChat(id);
                else if (e.key === 'ArrowRight' || e.key === 'ArrowLeft') {
                  e.preventDefault();
                  const els = Array.from(strip.current?.querySelectorAll<HTMLElement>('.tab') ?? []);
                  els[Math.max(0, Math.min(els.length - 1, i + (e.key === 'ArrowRight' ? 1 : -1)))]?.focus();
                }
              }}
              onDragStart={(e) => {
                e.dataTransfer.setData('application/x-falos-tab', id);
                e.dataTransfer.effectAllowed = 'move';
                setDrag({ id, over: null, after: false });
              }}
              onDragOver={(e) => {
                if (!drag) return;
                e.preventDefault();
                const r = e.currentTarget.getBoundingClientRect();
                setDrag({ ...drag, over: id, after: e.clientX > r.left + r.width / 2 });
              }}
              onDrop={(e) => {
                e.preventDefault();
                drop();
              }}
              onDragEnd={() => setDrag(null)}
            >
              <Swatch chat={chat} id={id} />
              {renaming === id ? (
                <input
                  className="tab-rename"
                  autoFocus
                  defaultValue={chatTitle(chat, id)}
                  onFocus={(e) => e.currentTarget.select()}
                  onClick={(e) => e.stopPropagation()}
                  onKeyDown={(e) => {
                    e.stopPropagation();
                    if (e.key === 'Enter') {
                      const v = e.currentTarget.value.trim();
                      if (v) hostSync()?.renameChat(id, v);
                      setRenaming(null);
                    } else if (e.key === 'Escape') setRenaming(null);
                  }}
                  onBlur={() => setRenaming(null)}
                />
              ) : (
                <span className="tab-title">{chatTitle(chat, id)}</span>
              )}
              {running && <span className="tab-mark">*</span>}
              {!running && chat.unread[id] && <span className="tab-mark">•</span>}
              {modHeld && i < 9 ? (
                <span className="tab-badge">{MOD}{i + 1}</span>
              ) : (
                !running && (
                  <button
                    className="link tab-x"
                    title="close tab (stays in history)"
                    onClick={(e) => {
                      e.stopPropagation();
                      closeTab(id);
                    }}
                  >
                    ×
                  </button>
                )
              )}
            </div>
          );
        })}
      </div>
      <button className="link tab-btn" onClick={newChat} title={`new chat (${MOD}N)`}>+</button>
      <button className={`link tab-btn ${view === 'history' ? 'on' : ''}`} onClick={toggleHistory} title={`all chats (${MOD}Y)`}>≡</button>
      {menu && <TabMenu id={menu.id} at={menu.at} onClose={() => setMenu(null)} />}
    </div>
  );
}

/** `delete 'Pizza image'? [y/n]`: keyboard-first confirm for deleting a chat. */
export function DeleteConfirm() {
  const id = useStore((s) => s.confirmDelete);
  const chat = useStore((s) => s.chat);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!id) return;
    ref.current?.focus();
    const key = (e: KeyboardEvent) => {
      const k = e.key.toLowerCase();
      if (k === 'y' || k === 'enter') confirmDelete(true);
      else if (k === 'n' || k === 'escape') confirmDelete(false);
      else return;
      e.preventDefault();
      e.stopPropagation();
    };
    window.addEventListener('keydown', key, true);
    return () => window.removeEventListener('keydown', key, true);
  }, [id]);
  if (!id) return null;
  return (
    <div className="confirm" ref={ref} tabIndex={-1}>
      delete '{chatTitle(chat, id)}'?{' '}
      <button className="link" onClick={() => confirmDelete(true)}>[y</button>/<button className="link" onClick={() => confirmDelete(false)}>n]</button>
    </div>
  );
}

const DAY = 86_400_000;

/** The full chat list (≡ / ⌘Y): search, grouped running / recent / older / archived.
 *  ↑↓ move, ⏎ opens as a tab, d deletes (confirm), a archives, esc goes back. */
export function ChatHistory() {
  const chat = useStore((s) => s.chat);
  const [q, setQ] = useState('');
  const [sel, setSel] = useState(0);
  const input = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const confirming = useStore((s) => s.confirmDelete);
  useEffect(() => {
    if (!confirming) input.current?.focus();
  }, [confirming]);

  const groups = useMemo(() => {
    const ids = [...chat.order].reverse();
    const hit = (id: string) => !q || fuzzy(q, chatTitle(chat, id)) >= 0;
    const now = Date.now();
    const g: { name: string; ids: string[] }[] = [
      { name: 'running', ids: [] },
      { name: 'recent', ids: [] },
      { name: 'older', ids: [] },
      { name: 'archived', ids: [] },
    ];
    for (const id of ids.filter(hit)) {
      const m = chat.meta[id];
      if (m?.archived) g[3].ids.push(id);
      else if (isRunning(chat, id)) g[0].ids.push(id);
      else if (!m?.updated || now - m.updated < DAY) g[1].ids.push(id);
      else g[2].ids.push(id);
    }
    return g.filter((x) => x.ids.length);
  }, [chat, q]);
  const flat = groups.flatMap((g) => g.ids);
  const cur = flat[Math.min(sel, flat.length - 1)];

  useLayoutEffect(() => {
    listRef.current?.querySelector<HTMLElement>('.hist-row.on')?.scrollIntoView({ block: 'nearest' });
  }, [sel]);

  const back = () => setState({ chatView: 'chat' });
  return (
    <div className="history">
      <div className="prompt hist-search">
        <span className="dim">/</span>
        <input
          ref={input}
          value={q}
          placeholder="search chats"
          spellCheck={false}
          onChange={(e) => {
            setQ(e.target.value);
            setSel(0);
          }}
          onKeyDown={(e) => {
            if (getState().confirmDelete) return;
            if (e.key === 'ArrowDown') {
              e.preventDefault();
              setSel((s) => Math.min(flat.length - 1, s + 1));
            } else if (e.key === 'ArrowUp') {
              e.preventDefault();
              setSel((s) => Math.max(0, s - 1));
            } else if (e.key === 'Enter' && cur) {
              e.preventDefault();
              openChat(cur);
            } else if (e.key === 'Escape') {
              e.preventDefault();
              back();
            } else if (!q && cur && !isExternal(cur) && (e.key === 'd' || e.key === 'a')) {
              // With an empty search, single letters act on the selected row.
              e.preventDefault();
              if (e.key === 'd') askDelete(cur);
              else hostSync()?.archiveChat(cur);
            }
          }}
        />
        <span className="faint hist-keys">⏎ open · d delete · a archive · esc back</span>
      </div>
      <div className="hist-list" ref={listRef}>
        {groups.map((g) => (
          <div key={g.name}>
            <div className="hist-group faint">{g.name}</div>
            {g.ids.map((id) => {
              const m = chat.meta[id];
              const i = flat.indexOf(id);
              return (
                <div key={id} className={`hist-row ${i === sel ? 'on' : ''}`} onMouseMove={() => setSel(i)} onClick={() => openChat(id)}>
                  <Swatch chat={chat} id={id} />
                  <span className="hist-title">{chatTitle(chat, id)}{chat.unread[id] ? ' •' : ''}</span>
                  <span className="dim">
                    {m?.driver ? <span className="faint">{m.driver} </span> : null}
                    {isRunning(chat, id) ? '* ' : m?.state === 'error' ? '! ' : ''}
                    {m?.cost ? `${usd(m.cost)} ` : ''}
                    {ago(m?.updated)}
                  </span>
                </div>
              );
            })}
          </div>
        ))}
        {!flat.length && <div className="faint hist-empty">{q ? 'no match' : 'no chats yet'}</div>}
      </div>
    </div>
  );
}
