import { Fragment, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { getState, setState, toast, useStore } from '../store';
import { hostSync } from '../sync';
import { focusNode } from '../controller';
import { chipLabel, transcript, type Chip, type Item } from '../chat';
import { usd } from '../format';

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
  if (!chips.length) return null;
  return (
    <div className="chips">
      {chips.map((c, i) => {
        const n = nodes.find((x) => x.id === c.node);
        return (
          <span key={i} className="chip">
            <button className="link" onClick={() => n && focusNode(c.node)}>[{chipLabel(c, n)}</button>
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
  switch (item.type) {
    case 'user':
      return (
        <div className="msg user">
          <div><span className="dim">&gt; </span>{item.text}</div>
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

/** The right-hand chat panel (UI-SPEC.md, CHAT-PROTOCOL.md). */
export function ChatPanel({ width, onResize }: { width: number; onResize: (w: number) => void }) {
  const chat = useStore((s) => s.chat);
  const conn = useStore((s) => s.conn);
  const nodes = useStore((s) => s.nodes);
  const [text, setText] = useState('');
  const input = useRef<HTMLTextAreaElement>(null);
  const log = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  const thread = chat.active ? chat.threads[chat.active] : undefined;
  const t = useMemo(() => transcript(thread), [thread]);
  const ids = useMemo(() => new Set(nodes.map((n) => n.id)), [nodes]);

  useEffect(() => {
    const focus = () => input.current?.focus();
    window.addEventListener('falos:focus-chat', focus);
    return () => window.removeEventListener('falos:focus-chat', focus);
  }, []);

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

  const onKey = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
      e.preventDefault();
      send();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      if (t.running) stop();
      else input.current?.blur();
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
        <span>chat</span>
        <span className="dim chat-title">{thread?.title ? `/ ${thread.title}` : ''}</span>
        <span className="spacer" />
        {t.cost != null && <span className="dim">{usd(t.cost)} </span>}
        {chat.order.length > 1 && (
          <select
            className="chat-pick"
            value={chat.active ?? ''}
            onChange={(e) => setState((s) => ({ chat: { ...s.chat, active: e.target.value || null } }))}
            title="chats"
          >
            {chat.order.map((id) => (
              <option key={id} value={id}>{chat.threads[id]?.title || id}</option>
            ))}
            <option value="">(new)</option>
          </select>
        )}
        <button className="link" onClick={() => setState((s) => ({ chat: { ...s.chat, active: null } }))} title="start a new chat">[new]</button>
      </div>
      <div
        className="chat-log"
        ref={log}
        onScroll={(e) => {
          const el = e.currentTarget;
          stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
        }}
      >
        {!thread && !pending && (
          <div className="faint">
            {conn === 'connected'
              ? 'ask the director. select nodes + ⌘L, or alt-click / alt-drag on media, to add context.'
              : 'chat needs a fal-os host.'}
          </div>
        )}
        {t.items.map((it) => <Row key={it.key} item={it} ids={ids} />)}
        {pending && (
          <div className="msg user faint">
            <div>&gt; {pending.text}</div>
            <Chips chips={pending.context} />
          </div>
        )}
        {conn === 'connected' && chat.runtime === 'missing' && <div className="dim">agent runtime not connected</div>}
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
        <Chips chips={chat.chips} onRemove={removeChip} />
        <div className="prompt">
          <span className="dim">&gt;</span>
          <textarea
            ref={input}
            rows={1}
            value={text}
            placeholder={conn === 'connected' ? '' : 'offline'}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={onKey}
            spellCheck={false}
          />
        </div>
      </div>
    </aside>
  );
}
