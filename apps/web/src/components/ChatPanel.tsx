import { Fragment, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { getState, setState, toast, useStore } from '../store';
import { hostSync } from '../sync';
import { focusNode } from '../controller';
import { chatTitle, chipLabel, selectChat, slug, transcript, type Chip, type ChatState, type Item } from '../chat';
import { usd } from '../format';
import { newChat } from '../keys';

/** Sends a message on the active chat with the pending chips (plus `extra`). */
export function sendChatMessage(text: string, extra: Chip[] = []): boolean {
  const s = getState();
  const sync = hostSync();
  const context = [...s.chat.chips, ...extra];
  if (!sync || s.conn !== 'connected') {
    toast('chat needs a fal-os host');
    return false;
  }
  if (!sync.sendChat(text, context, s.chat.active)) {
    toast('not connected to the host');
    return false;
  }
  setState((st) => ({ chat: { ...st.chat, chips: [] } }));
  return true;
}

const openChat = (id: string) => setState((s) => ({ chat: selectChat(s.chat, id) }));

function NodeRef({ id }: { id: string }) {
  const exists = useStore((s) => s.nodes.some((n) => n.id === id));
  return (
    <button className={`link node-ref ${exists ? '' : 'gone'}`} onClick={() => exists && focusNode(id)} title={exists ? 'pan to node' : 'node not on the canvas'}>
      {id}
    </button>
  );
}

function Refs({ ids }: { ids: string[] }) {
  return <>{ids.map((n) => <Fragment key={n}> <NodeRef id={n} /></Fragment>)}</>;
}

/** Text with known node ids made clickable. */
function Linked({ text, ids }: { text: string; ids: Set<string> }) {
  const parts = text.split(/([a-z0-9]{6})/i);
  return (
    <>
      {parts.map((p, i) => (i % 2 === 1 && ids.has(p) ? <NodeRef key={i} id={p} /> : <Fragment key={i}>{p}</Fragment>))}
    </>
  );
}

function Chips({ chips, onRemove }: { chips: Chip[]; onRemove?: (i: number) => void }) {
  const nodes = useStore((s) => s.nodes);
  const chat = useStore((s) => s.chat);
  if (!chips.length) return null;
  return (
    <div className="chips">
      {chips.map((c, i) => {
        const n = c.kind === 'chat' ? null : nodes.find((x) => x.id === c.node);
        const go = () => (c.kind === 'chat' ? openChat(c.chat) : n && focusNode(n.id));
        return (
          <span key={i} className="chip">
            <button className="link" onClick={go}>[{chipLabel(c, n, c.kind === 'chat' ? chatTitle(chat, c.chat) : undefined)}</button>
            {onRemove && <button className="link chip-x" onClick={() => onRemove(i)} title="remove">×</button>}
            ]
          </span>
        );
      })}
    </div>
  );
}

const MARK: Record<string, string> = { running: '…', started: '…', working: '…', done: '', error: ' !', failed: ' !' };

function Row({ item, ids }: { item: Item; ids: Set<string> }) {
  const [open, setOpen] = useState(false);
  const chat = useStore((s) => s.chat);
  switch (item.type) {
    case 'user':
      return (
        <div className="msg user">
          {item.fromChat ? (
            <div>
              <button className="link dim" onClick={() => openChat(item.fromChat!)}>from #{slug(item.fromTitle || chatTitle(chat, item.fromChat))}:</button> {item.text}
            </div>
          ) : (
            <div><span className="dim">&gt; </span>{item.text}</div>
          )}
          <Chips chips={item.context} />
        </div>
      );
    case 'text':
      return (
        <div className="msg">
          <span className="dim">{item.agent}: </span>
          <Linked text={item.text} ids={ids} />
        </div>
      );
    case 'thinking':
      return (
        <div className="row" onClick={() => setOpen(!open)}>
          <div className="row-line">· {item.agent}  thinking</div>
          {open && <div className="row-body">{item.text}</div>}
        </div>
      );
    case 'tool':
      return (
        <div className="row">
          <div className="row-line" onClick={() => setOpen(!open)}>
            · {item.agent.padEnd(10)} {item.name} {item.summary}
            {item.nodes.length > 0 && <> →<Refs ids={item.nodes} /></>}
            {MARK[item.state] ?? ` ${item.state}`}
          </div>
          {open && <div className="row-body">state: {item.state}{item.nodes.length ? `\nnodes: ${item.nodes.join(' ')}` : ''}</div>}
        </div>
      );
    case 'agent':
      return (
        <div className="row">
          <div className="row-line" onClick={() => setOpen(!open)}>
            · {item.agent.padEnd(10)} {item.state}{item.status ? `  ${item.status}` : ''}
          </div>
          {open && <div className="row-body">{item.role || 'agent'}</div>}
        </div>
      );
    case 'plan':
      return (
        <div className="msg plan">
          <div className="dim">{item.agent}: plan</div>
          {item.steps.map((s, i) => (
            <div key={i} className={s.state === 'done' ? 'dim' : ''}>
              {s.state === 'done' ? '[x]' : s.state === 'running' ? '[>]' : '[ ]'} {s.title}
            </div>
          ))}
        </div>
      );
    case 'error':
      return <div className="msg">error: {item.text}</div>;
    case 'done':
      return (
        <div className="row">
          <div className="row-line">
            · done  <Linked text={item.summary} ids={ids} />
            {item.nodes.length > 0 && <> →<Refs ids={item.nodes} /></>}
          </div>
        </div>
      );
  }
}

function ago(ms?: number): string {
  if (!ms) return '';
  const s = Math.max(0, Math.round((Date.now() - ms) / 1000));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.round(s / 60)}m`;
  if (s < 86400) return `${Math.round(s / 3600)}h`;
  return `${Math.round(s / 86400)}d`;
}

/** State of a chat: the host's list when it has one, else the local transcript. */
function chatState(chat: ChatState, id: string): string {
  return chat.meta[id]?.state ?? (transcript(chat.threads[id]).running ? 'running' : 'idle');
}

const GLYPH: Record<string, string> = { running: '*', idle: '·', error: '!' };

/** One mono line per chat: state glyph, title, active agents, cost, last update. */
function ChatList({ onPick }: { onPick: () => void }) {
  const chat = useStore((s) => s.chat);
  const ids = [...chat.order].reverse();
  return (
    <div className="chat-list">
      {ids.map((id, i) => {
        const m = chat.meta[id];
        const agents = m?.agents?.length ?? 0;
        return (
          <div key={id} className={`chat-list-row ${id === chat.active ? 'active' : ''}`} onClick={() => { openChat(id); onPick(); }}>
            <span className="glyph">{GLYPH[chatState(chat, id)] ?? '·'}</span>
            <span className="chat-list-title">{chatTitle(chat, id)}{chat.unread[id] ? ' •' : ''}</span>
            <span className="dim">
              {agents ? `${agents}a ` : ''}
              {m?.cost != null ? `${usd(m.cost)} ` : ''}
              {ago(m?.updated)}
            </span>
            <span className="faint">{chat.order.length - i <= 9 ? `⌘${chat.order.length - i}` : ''}</span>
            <button
              className="link faint"
              title="archive"
              onClick={(e) => {
                e.stopPropagation();
                hostSync()?.archiveChat(id);
              }}
            >
              ×
            </button>
          </div>
        );
      })}
    </div>
  );
}

/** `#query` right before the caret, for chat completion. */
function hashQuery(text: string, caret: number): { start: number; q: string } | null {
  const m = /(^|\s)#([\w-]*)$/.exec(text.slice(0, caret));
  return m ? { start: caret - m[2].length - 1, q: m[2].toLowerCase() } : null;
}

/** The right-hand chat panel (UI-SPEC.md, CHAT-PROTOCOL.md): several chats, each streaming
 *  in the background; switching is instant. */
export function ChatPanel({ width, onResize }: { width: number; onResize: (w: number) => void }) {
  const chat = useStore((s) => s.chat);
  const conn = useStore((s) => s.conn);
  const nodes = useStore((s) => s.nodes);
  const [text, setText] = useState('');
  const [caret, setCaret] = useState(0);
  const [pick, setPick] = useState(0);
  const [listOpen, setListOpen] = useState(false);
  const [renaming, setRenaming] = useState(false);
  const input = useRef<HTMLTextAreaElement>(null);
  const log = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  const thread = chat.active ? chat.threads[chat.active] : undefined;
  const t = useMemo(() => transcript(thread), [thread]);
  const ids = useMemo(() => new Set(nodes.map((n) => n.id)), [nodes]);
  const title = chat.active ? chatTitle(chat, chat.active) : 'new chat';
  const unreadElsewhere = Object.keys(chat.unread).some((id) => id !== chat.active && chat.unread[id]);
  const running = chat.order.filter((id) => chatState(chat, id) === 'running').length;

  const hq = hashQuery(text, caret);
  const matches = hq
    ? chat.order.filter((id) => id !== chat.active && slug(chatTitle(chat, id)).includes(hq.q)).slice(-6).reverse()
    : [];

  useEffect(() => {
    const focus = () => input.current?.focus();
    window.addEventListener('falos:focus-chat', focus);
    return () => window.removeEventListener('falos:focus-chat', focus);
  }, []);

  // Switching chats: show the end of the log.
  useEffect(() => {
    stick.current = true;
  }, [chat.active]);

  // Follow the end of the log unless the user scrolled up.
  useLayoutEffect(() => {
    const el = log.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  });

  // Auto-grow the textarea up to ~8 lines.
  useLayoutEffect(() => {
    const el = input.current;
    if (!el) return;
    el.style.height = '0px';
    el.style.height = `${Math.min(el.scrollHeight, 8 * 18 + 8)}px`;
  }, [text]);

  const send = () => {
    const v = text.trim();
    if (!v) return;
    if (sendChatMessage(v)) setText('');
  };

  const stop = () => {
    if (chat.active) hostSync()?.stopChat(chat.active);
  };

  const complete = (id: string) => {
    if (!hq) return;
    const next = text.slice(0, hq.start) + text.slice(caret);
    setText(next);
    setState((s) => ({ chat: { ...s.chat, chips: [...s.chat.chips.filter((c) => !(c.kind === 'chat' && c.chat === id)), { kind: 'chat', chat: id }] } }));
    requestAnimationFrame(() => input.current?.setSelectionRange(hq.start, hq.start));
  };

  const onKey = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (matches.length && (e.key === 'ArrowDown' || e.key === 'ArrowUp')) {
      e.preventDefault();
      setPick((p) => (p + (e.key === 'ArrowDown' ? 1 : -1) + matches.length) % matches.length);
      return;
    }
    if (matches.length && (e.key === 'Tab' || (e.key === 'Enter' && !e.shiftKey))) {
      e.preventDefault();
      complete(matches[Math.min(pick, matches.length - 1)]);
      return;
    }
    if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
      e.preventDefault();
      send();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      if (t.running) stop();
      else input.current?.blur();
    } else if (e.key === 'Backspace' && !text && chat.chips.length) {
      setState((s) => ({ chat: { ...s.chat, chips: s.chat.chips.slice(0, -1) } }));
    }
  };

  const startResize = (e: React.PointerEvent) => {
    e.preventDefault();
    const x0 = e.clientX;
    const w0 = width;
    const move = (ev: PointerEvent) => onResize(Math.max(260, Math.min(720, w0 - (ev.clientX - x0))));
    const up = () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
  };

  const removeChip = (i: number) => setState((s) => ({ chat: { ...s.chat, chips: s.chat.chips.filter((_, j) => j !== i) } }));
  const pending = chat.pending && (chat.pending.chat === chat.active || chat.pending.chat === null) ? chat.pending : null;

  return (
    <aside className="chat" style={{ width }}>
      <div className="chat-resize" onPointerDown={startResize} />
      <div className="chat-head">
        {renaming && chat.active ? (
          <input
            className="chat-rename"
            autoFocus
            defaultValue={title}
            onKeyDown={(e) => {
              if (e.key === 'Enter') {
                const v = e.currentTarget.value.trim();
                if (v && chat.active) hostSync()?.renameChat(chat.active, v);
                setRenaming(false);
              } else if (e.key === 'Escape') setRenaming(false);
            }}
            onBlur={() => setRenaming(false)}
          />
        ) : (
          <button
            className="link chat-title"
            onClick={() => setListOpen(!listOpen)}
            onDoubleClick={() => chat.active && setRenaming(true)}
            title="chats (double-click to rename)"
          >
            {chat.active ? `${GLYPH[chatState(chat, chat.active)] ?? '·'} ` : ''}{title}
          </button>
        )}
        {(chat.order.length > 1 || listOpen) && (
          <button className="link dim" onClick={() => setListOpen(!listOpen)}>
            {listOpen ? '[-]' : `[${chat.order.length}${running ? ` *${running}` : ''}${unreadElsewhere ? ' •' : ''}]`}
          </button>
        )}
        <span className="spacer" />
        <button className="link" onClick={newChat} title="new chat (⌘N)">[new]</button>
      </div>
      {listOpen && <ChatList onPick={() => setListOpen(false)} />}
      <div
        className="chat-log"
        ref={log}
        onScroll={(e) => {
          const el = e.currentTarget;
          stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
        }}
      >
        {t.items.map((it) => <Row key={it.key} item={it} ids={ids} />)}
        {pending && (
          <div className="msg user faint">
            <div>&gt; {pending.text}</div>
            <Chips chips={pending.context} />
          </div>
        )}
        {conn === 'connected' && chat.runtime === 'missing' && <div className="dim">agent runtime not connected</div>}
        {conn === 'local' && <div className="dim">no fal-os host: chat is unavailable</div>}
        {t.running && <div className="faint">running · esc to stop</div>}
      </div>
      {t.swarm.length > 0 && (
        <div className="swarm">
          {t.swarm.map((a) => (
            <div key={a.agent} className="swarm-line">
              <span>{a.agent.padEnd(10)}</span> <span className="dim">{a.status || a.role}</span>
            </div>
          ))}
        </div>
      )}
      <div className="chat-input">
        {matches.length > 0 && (
          <div className="complete">
            {matches.map((id, i) => (
              <div key={id} className={i === Math.min(pick, matches.length - 1) ? 'on' : 'dim'} onMouseDown={(e) => { e.preventDefault(); complete(id); }}>
                #{slug(chatTitle(chat, id))}
              </div>
            ))}
          </div>
        )}
        <Chips chips={chat.chips} onRemove={removeChip} />
        <div className="prompt">
          <span className="dim">&gt;</span>
          <textarea
            ref={input}
            rows={1}
            value={text}
            onChange={(e) => {
              setText(e.target.value);
              setCaret(e.target.selectionStart);
              setPick(0);
            }}
            onSelect={(e) => setCaret(e.currentTarget.selectionStart)}
            onKeyDown={onKey}
            spellCheck={false}
          />
        </div>
      </div>
    </aside>
  );
}
