import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { getState, setState, useStore } from '../store';
import { hostSync } from '../sync';
import { addSelectionToChat, attachFiles, emptySpot, pickFiles } from '../controller';
import { ASSET_MIME, draggedShas, placeMany } from '../library';
import { addChips, agentLine, chatColor, chatTitle, groupTurns, isExternal, slug, transcript, type Chip, type Transcript } from '../chat';
import { sendChatMessage, setDriver, steerQueued, steerText } from '../chatActions';
import { toast } from '../store';
import { Chips } from './Chips';
import { ChatHistory, ChatTabs, DeleteConfirm } from './ChatTabs';
import { bar, Row, selectBatch, TurnView } from './Transcript';
import { fuzzy } from './Palette';
import { ResizeHandle } from './ResizeHandle';
import { DriverPicker } from './DriverPicker';

export { sendChatMessage } from '../chatActions';

/** `@query` / `#query` right before the caret, for node / chat completion. */
function mention(text: string, caret: number): { start: number; q: string; kind: '@' | '#' } | null {
  const m = /(^|\s)([@#])([\w.-]*)$/.exec(text.slice(0, caret));
  return m ? { start: caret - m[3].length - 1, q: m[3].toLowerCase(), kind: m[2] as '@' | '#' } : null;
}

const NODE_MIME = 'application/x-falos-node';

/** The composer: chips above a `>` textarea. Enter sends (queued by the host while the chat
 *  runs: injected after the next step), ⌘Enter while running sends now (steers the running
 *  turn; with an empty input it sends the first queued message), ⇧Enter newline, Esc stops /
 *  leaves, ⌫ on empty removes the last chip, `@` completes nodes and `#` chats; drop or paste
 *  images and files (imported to the canvas, attached). In a subagent's view (`agent`) every
 *  message goes to that subagent. */
function Composer({ running, readOnly, question, agent, queued }: {
  running: boolean; readOnly: boolean; question: string[] | null; agent: string | null; queued: { id: string; text: string }[];
}) {
  const chat = useStore((s) => s.chat);
  const chatDriver = chat.active ? chat.meta[chat.active]?.driver : undefined;
  useEffect(() => {
    if (chatDriver === 'opus' || chatDriver === 'fal' || chatDriver === 'codex') setDriver(chatDriver);
  }, [chat.active, chatDriver]);
  const nodes = useStore((s) => s.nodes);
  const selection = useStore((s) => s.selection);
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

  /** Enter: send (queues while running); in a subagent's view: to that subagent. */
  const send = () => {
    const v = text.trim();
    if (!v || readOnly) return;
    if (agent ? steerText(v, agent) : sendChatMessage(v)) setText('');
  };

  /** ⌘Enter: while running, deliver now instead of after the next step. */
  const sendNow = () => {
    if (readOnly) return;
    const v = text.trim();
    if (!running && !agent) return send();
    if (!v) {
      if (queued[0] && steerQueued(queued[0].id)) toast('sent now');
      return;
    }
    if (chat.chips.length) {
      // Steering carries text only: chips go with a normal (queued) send.
      if (sendChatMessage(v)) setText('');
      return;
    }
    if (steerText(v, agent)) setText('');
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
      if (e.metaKey || e.ctrlKey) sendNow();
      else send();
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
    : agent
      ? `message ${agent}…`
      : chat.active
        ? running ? 'steer: ⏎ queue · ⌘⏎ send now…' : `message ${chatTitle(chat, chat.active)}…`
        : 'start a chat…';
  const mod = navigator.platform.toLowerCase().includes('mac') ? '⌘' : 'ctrl+';
  const unattached = selection.filter((id) => !chat.chips.some((c) => c.kind === 'node' && c.node === id)).length;

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
      <Chips chips={chat.chips} onRemove={removeChip} onClear={() => setState((s) => ({ chat: { ...s.chat, chips: [] } }))} />
      {!readOnly && unattached > 0 && (
        <div className="sel-add">
          <button className="link faint" title={`add the canvas selection to the chat (${mod}L)`} onMouseDown={(e) => e.preventDefault()} onClick={addSelectionToChat}>
            [+ {unattached} selected]
          </button>
        </div>
      )}
      <div className="prompt">
        {!readOnly && <DriverPicker />}
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
        {!readOnly && (
          <span className="composer-btns">
            <button className="link faint" title="attach files (or drop / paste them here)" onMouseDown={(e) => e.preventDefault()} onClick={() => pickFiles((f) => void attachFiles(f))}>[+]</button>
            {running && !agent && !text.trim() ? (
              <button className="link dim" title="stop the run (esc)" onMouseDown={(e) => e.preventDefault()} onClick={() => chat.active && hostSync()?.stopChat(chat.active)}>[stop]</button>
            ) : (
              <button className={`link ${text.trim() ? '' : 'faint'}`} disabled={!text.trim()} title={running && !agent ? `queue after the next step (⏎) · ${mod}⏎ send now` : 'send (⏎) · ⇧⏎ new line'} onMouseDown={(e) => e.preventDefault()} onClick={send}>[send]</button>
            )}
          </span>
        )}
      </div>
      {running && !agent && text.trim() && !readOnly && (
        <div className="steer-hint faint">
          <button className="link" onMouseDown={(e) => { e.preventDefault(); send(); }}>⏎ queue</button>
          <span> after the next step · </span>
          <button className="link" onMouseDown={(e) => { e.preventDefault(); sendNow(); }}>{mod}⏎ send now</button>
        </div>
      )}
    </div>
  );
}

/** Missing tools/drivers at chat start: one dim line, collapsed; the fix copies on click.
 *  Nothing when everything is available. */
function Capabilities({ caps }: { caps: Transcript['caps'] }) {
  const [open, setOpen] = useState(false);
  if (!caps?.missing.length) return null;
  const copy = (fix: string) => {
    void navigator.clipboard?.writeText(fix).then(() => toast('copied'), () => toast(fix));
  };
  const line = (m: { name: string; fix: string }) => (
    <div key={m.name} className="caps-line" onClick={() => m.fix && copy(m.fix)} title={m.fix ? `${m.name} unavailable — ${m.fix} (click to copy)` : `${m.name} unavailable`}>
      · {m.name} unavailable
      {m.fix && (
        <>
          {' — '}
          <button className="link md-code caps-fix">
            {/^(run|install|set|sign|log)\b/i.test(m.fix) ? m.fix : `run ${m.fix}`}
          </button>
        </>
      )}
    </div>
  );
  if (caps.missing.length === 1) return <div className="caps faint">{line(caps.missing[0])}</div>;
  return (
    <div className="caps faint">
      <div className="caps-line row-line" onClick={() => setOpen(!open)}>
        {open ? '▾' : '▸'} {caps.missing.length} unavailable: {caps.missing.map((m) => m.name).join(', ')}
      </div>
      {open && <div className="caps-body">{caps.missing.map(line)}</div>}
    </div>
  );
}

/** The right-hand chat panel (docs/UX-V1.md §2–3): tabs, the transcript (or a subagent's
 *  own transcript, or the full history), the swarm strip, queued messages and the composer.
 *  Images, files and canvas nodes dropped anywhere on it are attached. */
export function ChatPanel() {
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

  const pending = chat.pending && (chat.pending.chat === chat.active || chat.pending.chat === null) ? chat.pending : null;
  const accepts = (dt: DataTransfer) => dt.types.includes('Files') || dt.types.includes(NODE_MIME) || dt.types.includes(ASSET_MIME);

  return (
    <aside
      className={`chat ${dropping || dragToChat ? 'dropping' : ''}`}
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
        const shas = draggedShas(e.dataTransfer);
        if (shas.length) {
          // Library tiles: placed in empty space near the view, then attached as chips.
          const app = getState().app;
          if (app) {
            const [x, y] = emptySpot();
            const [wx, wy] = app.screenToWorld(x, y);
            placeMany(shas, [wx, wy], { attach: true });
          }
        } else if (node) {
          setState((s) => ({ chat: { ...s.chat, chips: addChips(s.chat.chips, [{ kind: 'node', node }]) } }));
          window.dispatchEvent(new Event('falos:focus-chat'));
        } else void attachFiles(Array.from(e.dataTransfer.files));
      }}
    >
      <ResizeHandle panel="chat" edge="left" />
      <ChatTabs />
      <DeleteConfirm />
      {view === 'history' ? (
        <ChatHistory />
      ) : (
        <>
          {subView && (() => {
            const a = main.agents[subView];
            const line = a ? agentLine(subView, a.role, a.status) : { role: '', status: '' };
            const ended = a?.state === 'done' || a?.state === 'failed';
            const status = ended ? `${a!.state}${line.status ? ` · ${line.status}` : ''}` : line.status || line.role;
            return (
              <div className="sub-head">
                <button className="link dim" onClick={() => setState({ subView: null })}>← main chat</button>
                <span className="sub-name"><span style={{ color }}>{a?.state === 'failed' ? '✕' : ended ? '✓' : '●'}</span> {subView}</span>
                {status && <span className="dim sub-status" title={status}>{status}</span>}
              </div>
            );
          })()}
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
                select images or notes and press <span className="dim">/</span> (or click <span className="dim">[&gt;]</span>) to start from them
                <br />
                or type below · drop images here to attach
              </div>
            )}
            {!subView && <Capabilities caps={main.caps} />}
            {subView
              ? <div className="sub-log">{t.items.map((it) => <Row key={it.key} item={it.type === 'user' ? { ...it, to: undefined } : it} ids={ids} chat={chat.active!} kindOf={kindOf} />)}</div>
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
            {conn === 'local' && <div className="dim">no fal OS host: chat is unavailable</div>}
            {t.running && !subView && (
              <div className="faint">
                running · <button className="link" title="stop the run (esc in the input)" onClick={() => chat.active && hostSync()?.stopChat(chat.active)}>[stop]</button>
              </div>
            )}
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
                  <span style={{ color }}>●</span> <span>{a.agent.padEnd(12)}</span>{' '}
                  <span className="dim swarm-status">{(() => { const l = agentLine(a.agent, a.role, a.status); return l.status || l.role; })()}</span>
                  {a.tools > 0 && <span className="faint">  {a.tools}</span>}
                </div>
              ))}
            </div>
          )}
          {main.queued.length > 0 && chat.active && (
            <div className="queued">
              {main.queued.map((q) => (
                <div
                  key={q.id}
                  className="queued-line"
                  tabIndex={0}
                  title={`queued: delivered after the next step · ${navigator.platform.toLowerCase().includes('mac') ? '⌘' : 'ctrl+'}⏎ send now`}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
                      e.preventDefault();
                      steerQueued(q.id);
                    } else if (e.key === 'Backspace' || e.key === 'Delete') {
                      e.preventDefault();
                      hostSync()?.unqueue(chat.active!, q.id);
                    }
                  }}
                >
                  <span className="faint">queued:</span> <span className="queued-text">{q.text}</span>
                  {main.running && (
                    <button className="link dim" title="deliver to the running turn now (⌘⏎)" onClick={() => steerQueued(q.id)}>
                      [send now]
                    </button>
                  )}
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
          <Composer
            running={main.running}
            readOnly={readOnly}
            question={main.question?.options.length ? main.question.options : null}
            agent={subView && main.agents[subView] && !['done', 'failed'].includes(main.agents[subView].state) ? subView : null}
            queued={main.queued}
          />
        </>
      )}
      {(dropping || dragToChat) && <div className="chat-drop faint">drop to attach</div>}
    </aside>
  );
}

/** For other modules that want the current state without subscribing. */
export const activeChat = () => getState().chat.active;
