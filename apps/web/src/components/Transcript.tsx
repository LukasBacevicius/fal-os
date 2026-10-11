import { Fragment, useState, type ReactNode } from 'react';
import { setState, useStore } from '../store';
import { hostSync } from '../sync';
import { focusNode } from '../controller';
import { actCost, actLabel, actNodes, agentLine, chatColor, chatTitle, cleanError, slug, workSummary, type Item, type Turn } from '../chat';
import { getState } from '../store';
import { openChat, sendChatMessage } from '../chatActions';
import { usd } from '../format';
import { Chips } from './Chips';

export function NodeRef({ id }: { id: string }) {
  const exists = useStore((s) => s.nodes.some((n) => n.id === id));
  return (
    <button className={`link node-ref ${exists ? '' : 'gone'}`} onClick={() => exists && focusNode(id)} title={exists ? 'pan to node' : 'node not on the canvas'}>
      {id}
    </button>
  );
}

/** Up to `max` node ids, then `+n`. */
export function Refs({ ids, max = 6 }: { ids: string[]; max?: number }) {
  return (
    <>
      {ids.slice(0, max).map((n) => <Fragment key={n}> <NodeRef id={n} /></Fragment>)}
      {ids.length > max && <span className="faint"> +{ids.length - max}</span>}
    </>
  );
}

/** Inline text: known node ids are clickable, `**` and backticks dropped. */
function Inline({ text, ids }: { text: string; ids: Set<string> }) {
  const parts = text.replace(/\*\*|__/g, '').split(/(`[^`]+`|\b[a-z0-9]{6}\b)/i);
  return (
    <>
      {parts.map((p, i) => {
        if (p.startsWith('`') && p.endsWith('`') && p.length > 1) return <span key={i} className="md-code">{p.slice(1, -1)}</span>;
        if (i % 2 === 1 && ids.has(p)) return <NodeRef key={i} id={p} />;
        return <Fragment key={i}>{p}</Fragment>;
      })}
    </>
  );
}

/** Minimal markdown: fenced code, bullet and numbered lists, headings as plain emphasis. */
export function Md({ text, ids }: { text: string; ids: Set<string> }) {
  const out: ReactNode[] = [];
  const blocks = text.split(/```[^\n]*\n?/);
  blocks.forEach((b, bi) => {
    if (bi % 2 === 1) {
      out.push(<pre key={bi} className="md-pre">{b.replace(/\n$/, '')}</pre>);
      return;
    }
    const lines = b.split('\n');
    let para: string[] = [];
    const flush = (k: string) => {
      const t = para.join('\n').trim();
      if (t) out.push(<div key={k} className="md-p"><Inline text={t} ids={ids} /></div>);
      para = [];
    };
    lines.forEach((l, li) => {
      const k = `${bi}-${li}`;
      const li_ = /^(\s*)([-*+]|\d+[.)])\s+(.*)$/.exec(l);
      const h = /^#{1,6}\s+(.*)$/.exec(l);
      if (li_) {
        flush(`${k}p`);
        const bullet = /\d/.test(li_[2]) ? li_[2] : '·';
        out.push(
          <div key={k} className="md-li" style={{ paddingLeft: `${Math.min(6, Math.floor(li_[1].length / 2)) * 2 + 2}ch` }}>
            <span className="md-bullet">{bullet}</span>
            <Inline text={li_[3]} ids={ids} />
          </div>,
        );
      } else if (h) {
        flush(`${k}p`);
        out.push(<div key={k} className="md-h"><Inline text={h[1]} ids={ids} /></div>);
      } else if (!l.trim()) flush(`${k}p`);
      else para.push(l);
    });
    flush(`${bi}-end`);
  });
  return <div className="md">{out}</div>;
}

function secs(ms: number | null): string {
  if (ms == null) return '';
  const s = Math.round(ms / 1000);
  return s < 60 ? `${s}s` : `${Math.floor(s / 60)}m${String(s % 60).padStart(2, '0')}s`;
}

/** One transcript item. `chat` is the thread's id (for revert / subagent links). */
export function Row({ item, ids, chat, kindOf }: { item: Item; ids: Set<string>; chat: string; kindOf: (id: string) => string | undefined }) {
  const [open, setOpen] = useState(false);
  const state = useStore((s) => s.chat);
  const color = chatColor(state, chat);
  switch (item.type) {
    case 'user':
      return (
        <div className="msg user">
          {item.fromExt ? (
            <div><span className="dim">from {item.fromExt}: </span>{item.text}</div>
          ) : item.fromChat ? (
            <div>
              <button className="link dim" onClick={() => openChat(item.fromChat!)}>from #{slug(item.fromTitle || chatTitle(state, item.fromChat))}:</button> {item.text}
            </div>
          ) : (
            <div>
              <span className="dim">&gt; </span>
              {item.to && <span className="dim">→ {item.to}  </span>}
              {item.text}
              {item.delivered === 'steer' && <span className="faint">  · sent now</span>}
            </div>
          )}
          <Chips chips={item.context} />
        </div>
      );
    case 'text':
      return <div className="msg"><Md text={item.text} ids={ids} /></div>;
    case 'thinking': {
      const label = item.done ? (item.seconds != null ? `thought for ${Math.max(1, Math.round(item.seconds))}s` : 'thought') : 'thinking…';
      return (
        <div className="row">
          <div className="row-line" onClick={() => item.text && setOpen(!open)}>· {label}</div>
          {open && <div className="row-body">{item.text}</div>}
        </div>
      );
    }
    case 'act': {
      const nodes = actNodes(item);
      const cost = actCost(item);
      return (
        <div className="row">
          <div className={`row-line act-line ${open ? 'open' : ''}`} onClick={() => setOpen(!open)}>
            <span className="act-label" title={open ? undefined : actLabel(item, kindOf)}>· {actLabel(item, kindOf)}</span>
            {nodes.length > 0 && nodes.length <= 4 && <> →<Refs ids={nodes} /></>}
            {nodes.length > 4 && (
              <>
                {' → '}
                <button
                  className="link node-ref"
                  title="select them on the canvas"
                  onClick={(e) => {
                    e.stopPropagation();
                    const app = getState().app;
                    const have = nodes.filter((id) => getState().nodes.some((n) => n.id === id));
                    app?.setSelection(have);
                    if (have.length) app?.zoomToNodes(have);
                  }}
                >
                  [{nodes.length} nodes]
                </button>
              </>
            )}
            {cost > 0 && <span className="faint">   {usd(cost)}</span>}
          </div>
          {open && (
            <div className="row-body">
              {nodes.length > 4 && <div><Refs ids={nodes} max={200} /></div>}
              {item.calls.map((c) => (
                <div key={c.id}>
                  {c.summary || c.name}{c.state === 'error' ? ' · failed' : c.state === 'running' ? ' …' : ''}
                </div>
              ))}
            </div>
          )}
        </div>
      );
    }
    case 'sub': {
      const go = () => setState({ subView: item.agent });
      const line = agentLine(item.agent, item.role, item.status);
      if (item.phase === 'start') {
        return (
          <div className="row sub-card one-line" onClick={go} title={`open this agent's transcript${line.status ? ` · ${line.status}` : ''}`}>
            <span style={{ color }}>●</span> {item.agent}
            <span className="faint">{line.role ? ` · ${line.role}` : ''}</span>
            {line.status && <span> · {line.status}</span>}
          </div>
        );
      }
      const failed = item.state === 'failed';
      return (
        <div className="row sub-card" onClick={go} title="open this agent's transcript">
          <span style={{ color }}>{failed ? '✕' : '✓'}</span> {item.agent} {failed ? 'failed' : 'done'}
          <span className="faint">
            {item.tools > 0 ? ` · ${item.tools} tool${item.tools === 1 ? '' : 's'}` : ''}
            {item.ms != null && item.ms >= 1000 ? ` · ${secs(item.ms)}` : ''}
          </span>
          {item.nodes.length > 0 && <> →<Refs ids={item.nodes} max={4} /></>}
          {line.status && <div className="row-body">{line.status}</div>}
        </div>
      );
    }
    case 'batch':
      return <BatchLine b={item} />;
    case 'question':
      return (
        <div className={`msg question ${item.answered ? 'answered' : ''}`}>
          <Md text={item.prompt} ids={ids} />
          {item.options.map((o, i) => (
            <div key={i} className="q-opt">
              {item.answered ? (
                <span className="dim">{i + 1}. {o}</span>
              ) : (
                <button className="link" onClick={() => sendChatMessage(o)}>
                  <span className="dim">{i + 1}.</span> {o}
                </button>
              )}
            </div>
          ))}
          {!item.answered && item.options.length > 0 && <div className="faint">press 1–{Math.min(9, item.options.length)} or type an answer</div>}
        </div>
      );
    case 'plan':
      return (
        <div className="msg plan">
          <div className="dim">plan</div>
          {item.steps.map((s, i) => (
            <div key={i} className={s.state === 'done' ? 'dim' : ''}>
              {s.state === 'done' ? '[x]' : s.state === 'running' ? '[>]' : '[ ]'} {s.title}
            </div>
          ))}
        </div>
      );
    case 'error':
      return (
        <div className="row err-line">
          ! {cleanError(item.text)}
          {item.actions.includes('continue') && (
            <>
              {'  '}
              <button className="link" onClick={() => hostSync()?.continueChat(chat)}>[continue]</button>
            </>
          )}
        </div>
      );
    case 'done': {
      const turn = item.turn;
      return (
        <>
          {item.summary && <div className="msg"><Md text={item.summary} ids={ids} /></div>}
          <div className="row">
            <div className="row-line done-line">
              {item.reverted ? '· reverted' : '· done'}
              {!item.reverted && item.nodes.length > 0 && <> →<Refs ids={item.nodes} /></>}
              {turn && (item.reverted || item.nodes.length > 0) && (
                <button
                  className="link faint revert"
                  title={item.reverted ? 'redo this turn' : "undo everything this turn's agents changed"}
                  onClick={() => (item.reverted ? hostSync()?.redoTurn(chat, turn) : hostSync()?.revertTurn(chat, turn))}
                >
                  [{item.reverted ? 'redo' : 'revert'}]
                </button>
              )}
            </div>
          </div>
        </>
      );
    }
  }
}

/** One turn: `> message` + chips, a collapsed work section (expanded while running, with the
 *  latest activity lines live), the final assistant text, errors, then the done line. */
export function TurnView({ turn, live, ids, chat, kindOf }: { turn: Turn; live: boolean; ids: Set<string>; chat: string; kindOf: (id: string) => string | undefined }) {
  const [open, setOpen] = useState<boolean | null>(null);
  const expanded = open ?? false;
  const summary = workSummary(turn.work);
  const tail = live && !expanded
    ? [
        ...turn.work.filter((w) => w.type === 'batch' && w.done + w.failed < w.total),
        ...turn.work.filter((w) => w.type !== 'text' && w.type !== 'batch').slice(-3),
      ]
    : [];
  return (
    <div className="turn">
      {turn.user && <Row item={turn.user} ids={ids} chat={chat} kindOf={kindOf} />}
      {turn.work.length > 0 && (
        <div className="work">
          <div
            className="work-head"
            tabIndex={0}
            onClick={() => setOpen(!expanded)}
            onKeyDown={(e) => {
              if (e.key === 'ArrowRight' || e.key === 'Enter') setOpen(true);
              else if (e.key === 'ArrowLeft') setOpen(false);
            }}
          >
            {expanded ? '▾' : '▸'} {summary || 'working'}{live ? '…' : ''}
          </div>
          {expanded && <div className="work-body">{turn.work.map((it) => <Row key={it.key} item={it} ids={ids} chat={chat} kindOf={kindOf} />)}</div>}
          {tail.length > 0 && <div className="work-body">{tail.map((it) => <Row key={it.key} item={it} ids={ids} chat={chat} kindOf={kindOf} />)}</div>}
        </div>
      )}
      {turn.final.map((it) => <Row key={it.key} item={it} ids={ids} chat={chat} kindOf={kindOf} />)}
      {turn.errors.map((it) => <Row key={it.key} item={it} ids={ids} chat={chat} kindOf={kindOf} />)}
      {turn.done && !(turn.errors.length && !turn.done.nodes.length && !turn.done.summary) && <Row item={turn.done} ids={ids} chat={chat} kindOf={kindOf} />}
    </div>
  );
}

/** `▕████░░░░▏` text progress bar. */
export function bar(frac: number, width = 12): string {
  const f = Math.max(0, Math.min(1, frac));
  const full = Math.round(f * width);
  return `${'█'.repeat(full)}${'░'.repeat(width - full)}`;
}

/** Selects (and zooms to) a batch: its frame, else its placeholders, else a frame named
 *  `… ×<total>`. */
export function selectBatch(b: Extract<Item, { type: 'batch' }>) {
  const { app, nodes } = getState();
  if (!app) return;
  let ids = b.frame && nodes.some((n) => n.id === b.frame) ? [b.frame] : b.nodes.filter((id) => nodes.some((n) => n.id === id));
  if (!ids.length) {
    const f = [...nodes].reverse().find((n) => n.kind === 'frame' && n.name.includes(`×${b.total}`));
    if (f) ids = [f.id];
  }
  if (!ids.length) return;
  app.setSelection(ids);
  app.zoomToNodes(ids);
}

/** `· generating 37/200 · flux/schnell ████░░░░ ` (click selects the batch). */
export function BatchLine({ b }: { b: Extract<Item, { type: 'batch' }> }) {
  const finished = b.done + b.failed >= b.total && b.total > 0;
  const model = b.model.replace(/^fal-ai\//, '');
  return (
    <div className="row batch-line" onClick={() => selectBatch(b)} title="select the batch on the canvas">
      · {finished ? `generated ${b.done}/${b.total}` : `generating ${b.done}/${b.total}`}
      {model && <span> · {model}</span>}
      {!finished && <span className="faint"> {bar(b.total ? b.done / b.total : 0)}</span>}
      {b.failed > 0 && <span className="faint"> · {b.failed} failed</span>}
    </div>
  );
}
