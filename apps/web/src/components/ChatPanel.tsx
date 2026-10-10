import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { getState, setState, useStore } from '../store';
import { hostSync } from '../sync';
import { attachFiles } from '../controller';
import { addChips, chatColor, chatTitle, groupTurns, isExternal, slug, transcript, type Chip } from '../chat';
import { DRIVERS, sendChatMessage, setDriver } from '../chatActions';
import { Chips } from './Chips';
import { ChatHistory, ChatTabs, DeleteConfirm } from './ChatTabs';
import { bar, Row, selectBatch, TurnView } from './Transcript';
import { fuzzy } from './Palette';

export { sendChatMessage } from '../chatActions';

/** `@query` / `#query` right before the caret, for node / chat completion. */
function mention(text: string, caret: number): { start: number; q: string; kind: '@' | '#' } | null {
  const m = /(^|\s)([@#])([\w.-]*)$/.exec(text.slice(0, caret));
  return m ? { start: caret - m[3].length - 1, q: m[3].toLowerCase(), kind: m[2] as '@' | '#' } : null;
}

const NODE_MIME = 'application/x-falos-node';

/** The composer: chips above a `>` textarea. Enter sends (queued by the host while the chat
 *  runs), ⇧Enter newline, Esc stops / leaves, ⌫ on empty removes the last chip, `@` completes
 *  nodes and `#` chats; drop or paste images and files (imported to the canvas, attached). */
function Composer({ running, readOnly, question }: { running: boolean; readOnly: boolean; question: string[] | null }) {
  const chat = useStore((s) => s.chat);
  const driver = useStore((s) => s.driver);
  const chatDriver = chat.active ? chat.meta[chat.active]?.driver : undefined;
  useEffect(() => {
    if (chatDriver === 'opus' || chatDriver === 'fal' || chatDriver === 'codex') setDriver(chatDriver);
  }, [chat.active, chatDriver]);
  const nodes = useStore((s) => s.nodes);
  const draftKey = chat.active ?? '';
  const text = useStore((s) => s.drafts[draftKey] ?? '');
  const [caret, setCaret] = useState(0);
  const [pick, setPick] = useState(0);
  const input = useRef<HTMLTextAreaElement>(null);
  const setText = (v: string) => setState((s) => ({ drafts: { ...s.drafts, [draftKey]: v } }));

  useEffect(() => {
    const focus = () => input.current?.focus();
    window.addEventListener('falos:focus-chat', focus);
    return () => window.removeEventListener('falos:focus-chat', focus);
  }, []);

  // Auto-grow up to ~8 lines.
  useLayoutEffect(() => {
    const el = input.current;
    if (!el) return;
    el.style.height = '0px';
    el.style.height = `${Math.min(el.scrollHeight, 8 * 18 + 8)}px`;
  }, [text]);

  const mq = mention(text, caret);
  const matches = useMemo(() => {
    if (!mq) return [] as { id: string; label: string; hint: string; chip: Chip }[];
    if (mq.kind === '#') {
      return chat.order
        .filter((id) => id !== chat.active && (!mq.q || slug(chatTitle(chat, id)).includes(mq.q)))
        .slice(-6)
        .reverse()
        .map((id) => ({ id, label: `#${slug(chatTitle(chat, id))}`, hint: 'chat', chip: { kind: 'chat', chat: id } as Chip }));
    }
    return nodes
      .filter((n) => n.kind !== 'connector')
      .map((n) => ({ n, s: mq.q ? Math.max(fuzzy(mq.q, n.name || ''), fuzzy(mq.q, n.id) - 1) : 0 }))
      .filter((x) => x.s >= 0)
      .sort((a, b) => b.s - a.s)
      .slice(0, 6)
      .map(({ n }) => ({ id: n.id, label: `@${n.name || n.text?.slice(0, 24) || n.id}`, hint: `${n.kind} ${n.id}`, chip: { kind: 'node', node: n.id } as Chip }));
  }, [mq?.kind, mq?.q, chat, nodes]);

  const complete = (chip: Chip) => {
    if (!mq) return;
    const next = text.slice(0, mq.start) + text.slice(caret);
    setText(next);
    setState((s) => ({ chat: { ...s.chat, chips: addChips(s.chat.chips, [chip]) } }));
    requestAnimationFrame(() => {
      input.current?.setSelectionRange(mq.start, mq.start);
      setCaret(mq.start);
    });
  };

  const send = () => {
    const v = text.trim();
    if (!v || readOnly) return;
    if (sendChatMessage(v)) setText('');
  };

  const onKey = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (matches.length && (e.key === 'ArrowDown' || e.key === 'ArrowUp')) {
      e.preventDefault();
      setPick((p) => (p + (e.key === 'ArrowDown' ? 1 : -1) + matches.length) % matches.length);
      return;
    }
    if (matches.length && (e.key === 'Tab' || (e.key === 'Enter' && !e.shiftKey))) {
      e.preventDefault();
      complete(matches[Math.min(pick, matches.length - 1)].chip);
      return;
    }
    if (question && !text && /^[1-9]$/.test(e.key) && Number(e.key) <= question.length) {
      e.preventDefault();
      sendChatMessage(question[Number(e.key) - 1]);
      return;
    }
    if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
      e.preventDefault();
      send();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      if (running && chat.active) hostSync()?.stopChat(chat.active);
      else input.current?.blur();
    } else if (e.key === 'Backspace' && !text && chat.chips.length) {
      setState((s) => ({ chat: { ...s.chat, chips: s.chat.chips.slice(0, -1) } }));
    }
  };

  const removeChip = (i: number) => setState((s) => ({ chat: { ...s.chat, chips: s.chat.chips.filter((_, j) => j !== i) } }));
  const placeholder = readOnly
    ? 'external agent · read-only'
    : chat.active
      ? running ? 'queue a message…' : `message ${chatTitle(chat, chat.active)}…`
      : 'start a chat…';

  return (
    <div className="chat-input">
      {matches.length > 0 && (
        <div className="complete">
          {matches.map((m, i) => (
            <div key={m.id} className={i === Math.min(pick, matches.length - 1) ? 'on' : 'dim'} onMouseDown={(e) => { e.preventDefault(); complete(m.chip); }}>
              {m.label} <span className="faint">{m.hint}</span>
            </div>
          ))}
        </div>
      )}
      <Chips chips={chat.chips} onRemove={removeChip} />
      <div className="prompt">
        {!readOnly && (
          <span className="driver" title="agent for the next message (⌘.)">
            {DRIVERS.map((d) => (
              <button key={d} className={`link ${d === driver ? 'on' : 'faint'}`} onClick={() => setDriver(d)}>{d}</button>
            ))}
          </span>
        )}
        <span className="dim">&gt;</span>
        <textarea
          ref={input}
          rows={1}
          value={text}
          disabled={readOnly}
          placeholder={placeholder}
          onChange={(e) => {
            setText(e.target.value);
            setCaret(e.target.selectionStart);
            setPick(0);
          }}
          onSelect={(e) => setCaret(e.currentTarget.selectionStart)}
          onKeyDown={onKey}
          onPaste={(e) => {
            const files = Array.from(e.clipboardData.files);
            if (files.length) {
              e.preventDefault();
              void attachFiles(files);
            }
          }}
          spellCheck={false}
        />
      </div>
    </div>
  );
}

/** The right-hand chat panel (docs/UX-V1.md §2–3): tabs, the transcript (or a subagent's
 *  own transcript, or the full history), the swarm strip, queued messages and the composer.
 *  Images, files and canvas nodes dropped anywhere on it are attached. */
export function ChatPanel({ width, onResize }: { width: number; onResize: (w: number) => void }) {
  const chat = useStore((s) => s.chat);
  const conn = useStore((s) => s.conn);
  const nodes = useStore((s) => s.nodes);
  const view = useStore((s) => s.chatView);
  const subView = useStore((s) => s.subView);
  const dragToChat = useStore((s) => s.dragToChat);
  const [dropping, setDropping] = useState(false);
  const [atBottom, setAtBottom] = useState(true);
  const log = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  const thread = chat.active ? chat.threads[chat.active] : undefined;
  const main = useMemo(() => transcript(thread), [thread]);
  const t = useMemo(() => (subView ? transcript(thread, subView) : main), [thread, subView, main]);
  const turns = useMemo(() => groupTurns(t.items), [t]);
  const ids = useMemo(() => new Set(nodes.map((n) => n.id)), [nodes]);
  const kinds = useMemo(() => new Map(nodes.map((n) => [n.id, n.kind])), [nodes]);
  const kindOf = (id: string) => kinds.get(id);
  const color = chatColor(chat, chat.active);
  const readOnly = !!chat.active && isExternal(chat.active);
  const empty = !chat.order.length && !chat.pending;

  // Switching chats or views: show the end of the log.
  useEffect(() => {
    stick.current = true;
    setAtBottom(true);
  }, [chat.active, subView, view]);

  // Follow output only while scrolled to the bottom.
  useLayoutEffect(() => {
    const el = log.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  });

  const startResize = (e: React.PointerEvent) => {
    e.preventDefault();
    const x0 = e.clientX;
    const w0 = width;
    const move = (ev: PointerEvent) => onResize(Math.max(280, Math.min(760, w0 - (ev.clientX - x0))));
    const up = () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
  };

  const pending = chat.pending && (chat.pending.chat === chat.active || chat.pending.chat === null) ? chat.pending : null;
  const accepts = (dt: DataTransfer) => dt.types.includes('Files') || dt.types.includes(NODE_MIME);

  return (
    <aside
      className={`chat ${dropping || dragToChat ? 'dropping' : ''}`}
      style={{ width }}
      onDragOver={(e) => {
        if (!accepts(e.dataTransfer)) return;
        e.preventDefault();
        e.dataTransfer.dropEffect = 'copy';
        setDropping(true);
      }}
      onDragLeave={(e) => {
        if (!e.currentTarget.contains(e.relatedTarget as Node)) setDropping(false);
      }}
      onDrop={(e) => {
        if (!accepts(e.dataTransfer)) return;
        e.preventDefault();
        setDropping(false);
        const node = e.dataTransfer.getData(NODE_MIME);
        if (node) {
          setState((s) => ({ chat: { ...s.chat, chips: addChips(s.chat.chips, [{ kind: 'node', node }]) } }));
          window.dispatchEvent(new Event('falos:focus-chat'));
        } else void attachFiles(Array.from(e.dataTransfer.files));
      }}
    >
      <div className="chat-resize" onPointerDown={startResize} />
      <ChatTabs />
      <DeleteConfirm />
      {view === 'history' ? (
        <ChatHistory />
      ) : (
        <>
          {subView && (
            <div className="sub-head">
              <button className="link dim" onClick={() => setState({ subView: null })}>← main chat</button>
              <span><span style={{ color }}>●</span> {subView}</span>
            </div>
          )}
          <div
            className="chat-log"
            ref={log}
            onScroll={(e) => {
              const el = e.currentTarget;
              const bottom = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
              stick.current = bottom;
              if (bottom !== atBottom) setAtBottom(bottom);
            }}
          >
            {empty && conn === 'connected' && (
              <div className="faint empty-chat">
                select images or notes and press <span className="dim">/</span> (or click <span className="dim">✦</span>) to start from them
                <br />
                or type below · drop images here to attach
              </div>
            )}
            {subView
              ? <div className="sub-log">{t.items.map((it) => <Row key={it.key} item={it} ids={ids} chat={chat.active!} kindOf={kindOf} />)}</div>
              : turns.map((tn, i) => (
                  <TurnView key={tn.key} turn={tn} live={t.running && i === turns.length - 1} ids={ids} chat={chat.active!} kindOf={kindOf} />
                ))}
            {pending && (
              <div className="turn">
                <div className="msg user faint">
                  <div>&gt; {pending.text}</div>
                  <Chips chips={pending.context} />
                </div>
              </div>
            )}
            {conn === 'connected' && chat.runtime === 'missing' && <div className="dim">agent runtime not connected</div>}
            {conn === 'local' && <div className="dim">no fal-os host: chat is unavailable</div>}
            {t.running && !subView && <div className="faint">running · esc to stop</div>}
          </div>
          {!atBottom && (
            <button
              className="link latest"
              onClick={() => {
                stick.current = true;
                setAtBottom(true);
                if (log.current) log.current.scrollTop = log.current.scrollHeight;
              }}
            >
              ↓ latest{t.running ? ' …' : ''}
            </button>
          )}
          {!subView && (main.swarm.length > 0 || main.batches.length > 0) && (
            <div className="swarm">
              {main.batches.map((b) => (
                <div key={b.id} className="swarm-line" onClick={() => selectBatch(b)} title="select the batch on the canvas">
                  <span style={{ color }}>●</span> <span>{`${b.done}/${b.total}`.padEnd(12)}</span>{' '}
                  <span className="dim">{b.model.replace(/^fal-ai\//, '')}</span> <span className="faint">{bar(b.total ? b.done / b.total : 0, 10)}</span>
                </div>
              ))}
              {main.swarm.map((a) => (
                <div key={a.agent} className="swarm-line" onClick={() => setState({ subView: a.agent })} title="open this agent's transcript">
                  <span style={{ color }}>●</span> <span>{a.agent.padEnd(12)}</span> <span className="dim">{a.status || a.role}</span>
                  {a.tools > 0 && <span className="faint">  {a.tools}</span>}
                </div>
              ))}
            </div>
          )}
          {main.queued.length > 0 && chat.active && (
            <div className="queued">
              {main.queued.map((q) => (
                <div key={q.id} className="queued-line">
                  <span className="faint">queued:</span> <span className="queued-text">{q.text}</span>
                  <button
                    className="link faint"
                    title="edit (moves it back into the input)"
                    onClick={() => {
                      hostSync()?.unqueue(chat.active!, q.id);
                      const key = chat.active!;
                      setState((s) => ({ drafts: { ...s.drafts, [key]: q.text } }));
                      window.dispatchEvent(new Event('falos:focus-chat'));
                    }}
                  >
                    [edit]
                  </button>
                  <button className="link faint" title="remove" onClick={() => hostSync()?.unqueue(chat.active!, q.id)}>×</button>
                </div>
              ))}
            </div>
          )}
          <Composer running={main.running} readOnly={readOnly} question={main.question?.options.length ? main.question.options : null} />
        </>
      )}
      {(dropping || dragToChat) && <div className="chat-drop faint">drop to attach</div>}
    </aside>
  );
}

/** For other modules that want the current state without subscribing. */
export const activeChat = () => getState().chat.active;
