import { useEffect, useMemo, useRef, useState } from 'react';
import { toast, useStore } from '../store';
import { AGENTS, actorById } from '../agents';
import { anchorLabel, kindLabel, STATUS_LABEL, timeAgo } from '../format';
import { IconCheck, IconComment, IconSend, IconSparkle, IconTarget, IconTrash, KindIcon } from '../icons';
import { Avatar } from './Avatar';
import type { AnnotationView, NodeDetail, NodeView, Status } from '../types';

export function RightPanel() {
  return (
    <aside className="panel right">
      <Inspector />
      <Annotations />
    </aside>
  );
}

// ---- inspector ---------------------------------------------------------------------------------

function NumField({ label, value, onCommit }: { label: string; value: number; onCommit: (v: number) => void }) {
  const [v, setV] = useState(String(Math.round(value)));
  useEffect(() => setV(String(Math.round(value))), [value]);
  const commit = () => {
    const n = Number(v);
    if (Number.isFinite(n) && Math.round(n) !== Math.round(value)) onCommit(n);
    else setV(String(Math.round(value)));
  };
  return (
    <label className="num">
      <span>{label}</span>
      <input
        value={v}
        onChange={(e) => setV(e.target.value)}
        onBlur={commit}
        onKeyDown={(e) => {
          if (e.key === 'Enter') (e.target as HTMLInputElement).blur();
          if (e.key === 'Escape') { setV(String(Math.round(value))); (e.target as HTMLInputElement).blur(); }
        }}
      />
    </label>
  );
}

function Inspector() {
  const app = useStore((s) => s.app);
  const selection = useStore((s) => s.selection);
  const nodes = useStore((s) => s.nodes);
  const annotations = useStore((s) => s.annotations);
  const node = selection.length === 1 ? nodes.find((n) => n.id === selection[0]) : undefined;
  // Re-read the detail whenever the node view changes.
  const detail = useMemo(() => (app && node ? (app.node(node.id) as NodeDetail | null) : null), [app, node]);

  if (selection.length > 1) {
    return (
      <section className="section">
        <div className="section-head"><span>Selection</span></div>
        <div className="inspector-multi">{selection.length} layers selected</div>
      </section>
    );
  }
  if (!node || !detail) {
    const open = annotations.filter((a) => a.status !== 'resolved').length;
    return (
      <section className="section">
        <div className="section-head"><span>Canvas</span></div>
        <div className="kv"><span>Layers</span><span>{nodes.length}</span></div>
        <div className="kv"><span>Open annotations</span><span>{open}</span></div>
        <div className="hint">
          <kbd>V</kbd> move · <kbd>H</kbd> hand · <kbd>C</kbd> annotate · <kbd>⇧1</kbd> fit · <kbd>⌘Z</kbd> undo
        </div>
      </section>
    );
  }
  const update = (props: Record<string, number>) => {
    try {
      app!.apply({ op: 'update', id: node.id, props });
    } catch (e) {
      toast(String(e));
    }
  };
  const lineage = detail.asset?.lineage;
  const prompt = lineage && typeof lineage.params === 'object' && lineage.params ? (lineage.params as Record<string, unknown>).prompt : undefined;
  return (
    <section className="section">
      <div className="section-head">
        <span className="inspector-title"><KindIcon kind={node.kind} /> {node.name || kindLabel(node.kind)}</span>
        <span className="chip">{kindLabel(node.kind)}</span>
      </div>
      <div className="grid2">
        <NumField label="X" value={node.x} onCommit={(x) => update({ x })} />
        <NumField label="Y" value={node.y} onCommit={(y) => update({ y })} />
        <NumField label="W" value={node.w} onCommit={(w) => update({ w: Math.max(1, w) })} />
        <NumField label="H" value={node.h} onCommit={(h) => update({ h: Math.max(1, h) })} />
      </div>
      <div className="kv"><span>Created by</span><span className="kv-actor"><Avatar actor={actorById(node.createdBy)} size={16} /> {detail.createdByName ?? node.createdBy}</span></div>
      {detail.asset && (
        <div className="kv"><span>Asset</span><span className="mono" title={detail.asset.id}>{detail.asset.mime ?? ''} · {shortId(detail.asset.id)}</span></div>
      )}
      {node.text && node.kind !== 'note' && <div className="text-preview">{node.text.slice(0, 280)}</div>}
      <div className="lineage">
        <div className="lineage-head"><IconSparkle size={13} /> Lineage</div>
        {lineage ? (
          <>
            <div className="kv"><span>Model</span><span className="mono">{lineage.model}</span></div>
            {prompt ? <div className="prompt">“{String(prompt)}”</div> : null}
            {lineage.seed != null && <div className="kv"><span>Seed</span><span className="mono">{lineage.seed}</span></div>}
            {lineage.cost != null && <div className="kv"><span>Cost</span><span>${lineage.cost.toFixed(3)}</span></div>}
            <div className="kv"><span>By</span><span>@{lineage.actor}</span></div>
          </>
        ) : (
          <div className="muted">Imported file — no generation lineage.</div>
        )}
      </div>
    </section>
  );
}

const shortId = (id: string) => (id.includes(':') ? `${id.split(':')[0]}:${id.split(':')[1].slice(0, 8)}` : id.slice(0, 12));

// ---- annotations -------------------------------------------------------------------------------

type Filter = 'open' | 'resolved' | 'all';

function Annotations() {
  const annotations = useStore((s) => s.annotations);
  const selection = useStore((s) => s.selection);
  const selected = useStore((s) => s.selectedAnnotation);
  const nodes = useStore((s) => s.nodes);
  const tool = useStore((s) => s.tool);
  const app = useStore((s) => s.app);
  const [filter, setFilter] = useState<Filter>('open');
  const scopeNode = selection.length === 1 ? selection[0] : null;
  const [scoped, setScoped] = useState(true);
  const byId = useMemo(() => new Map(nodes.map((n) => [n.id, n])), [nodes]);
  const number = useMemo(() => new Map(annotations.map((a, i) => [a.id, i + 1])), [annotations]);

  const list = annotations
    .filter((a) => filter === 'all' || (filter === 'resolved' ? a.status === 'resolved' : a.status !== 'resolved'))
    .filter((a) => !(scoped && scopeNode) || a.nodeId === scopeNode || a.id === selected)
    .slice()
    .reverse();

  // Keep the selected thread visible.
  useEffect(() => {
    if (!selected) return;
    const a = annotations.find((x) => x.id === selected);
    if (a && filter !== 'all' && (filter === 'resolved') !== (a.status === 'resolved')) setFilter('all');
    requestAnimationFrame(() => document.getElementById(`ann-${selected}`)?.scrollIntoView({ block: 'nearest', behavior: 'smooth' }));
  }, [selected]);

  const counts = {
    open: annotations.filter((a) => a.status !== 'resolved').length,
    resolved: annotations.filter((a) => a.status === 'resolved').length,
    all: annotations.length,
  };

  return (
    <section className="section grow">
      <div className="section-head">
        <span>Annotations</span>
        <div className="seg">
          {(['open', 'resolved', 'all'] as Filter[]).map((f) => (
            <button key={f} className={filter === f ? 'on' : ''} onClick={() => setFilter(f)}>
              {f === 'open' ? 'Open' : f === 'resolved' ? 'Resolved' : 'All'} <span className="seg-n">{counts[f]}</span>
            </button>
          ))}
        </div>
      </div>
      {scopeNode && (
        <label className="scope">
          <input type="checkbox" checked={scoped} onChange={(e) => setScoped(e.target.checked)} />
          Only “{byId.get(scopeNode)?.name || 'selected layer'}”
        </label>
      )}
      <div className="threads">
        {list.length === 0 && (
          <div className="threads-empty">
            <IconComment size={20} />
            <div>{annotations.length ? 'Nothing here.' : 'No annotations yet.'}</div>
            <div className="muted">
              Pick the annotate tool <kbd>C</kbd>, then click a point or drag a region on an image or video frame.
            </div>
            {tool !== 'annotate' && <button className="btn" onClick={() => app?.setTool('annotate')}>Start annotating</button>}
          </div>
        )}
        {list.map((a) => (
          <Thread key={a.id} a={a} n={number.get(a.id) ?? 0} node={byId.get(a.nodeId)} open={a.id === selected} />
        ))}
      </div>
    </section>
  );
}

function StatusPill({ status }: { status: Status }) {
  return <span className={`status ${status}`}>{STATUS_LABEL[status]}</span>;
}

function Thread({ a, n, node, open }: { a: AnnotationView; n: number; node?: NodeView; open: boolean }) {
  const app = useStore((s) => s.app);
  const [reply, setReply] = useState('');
  const input = useRef<HTMLTextAreaElement>(null);
  const author = actorById(a.author);
  const [first, ...rest] = a.thread;
  const act = (f: () => void) => {
    try { f(); } catch (e) { toast(String(e)); }
  };
  const send = () => {
    if (!reply.trim() || !app) return;
    act(() => app.reply(a.id, reply.trim()));
    setReply('');
  };
  useEffect(() => { if (open) input.current?.focus({ preventScroll: true }); }, [open]);

  return (
    <div
      id={`ann-${a.id}`}
      className={`thread ${open ? "active" : ""} status-${a.status}`}
      onClick={() => !open && app?.selectAnnotation(a.id, true)}
    >
      <div className="thread-head">
        <span className={`pin-badge ${a.status}`}>{n}</span>
        <Avatar actor={author} size={20} />
        <span className="thread-author">{author.name}</span>
        <span className="thread-time">{timeAgo(a.createdAt)}</span>
        <span className="spacer" />
        <StatusPill status={a.status} />
      </div>
      <button
        className="thread-target"
        onClick={(e) => { e.stopPropagation(); app?.selectAnnotation(a.id, true); }}
        title="Show on canvas"
      >
        <IconTarget size={12} />
        <span>{node?.name || node?.kind || 'deleted layer'}</span>
        <span className="dot">·</span>
        <span>{anchorLabel(a.anchor)}</span>
      </button>
      {first && <div className="msg-text first">{first.text}</div>}
      {!open && rest.length > 0 && <div className="thread-more">{rest.length} {rest.length === 1 ? 'reply' : 'replies'}</div>}
      {open && (
        <>
          {rest.map((m, i) => {
            const who = actorById(m.author);
            return (
              <div className="msg" key={i}>
                <Avatar actor={who} size={18} />
                <div className="msg-body">
                  <div className="msg-meta"><span>{who.name}</span><span className="thread-time">{timeAgo(m.at)}</span></div>
                  <div className="msg-text">{m.text}</div>
                </div>
              </div>
            );
          })}
          <div className="thread-controls" onClick={(e) => e.stopPropagation()}>
            <label className="assignee">
              <span>Assignee</span>
              <select
                value={a.assignee ?? ''}
                onChange={(e) => act(() => app!.setAnnotationAssignee(a.id, e.target.value || null))}
              >
                <option value="">Unassigned</option>
                {AGENTS.map((ag) => <option key={ag.id} value={ag.id}>@{ag.id}</option>)}
              </select>
            </label>
            <select
              className="status-select"
              value={a.status}
              onChange={(e) => act(() => app!.setAnnotationStatus(a.id, e.target.value))}
              title="Status"
            >
              <option value="open">Open</option>
              <option value="in_progress">In progress</option>
              <option value="resolved">Resolved</option>
            </select>
            <button className="icon-btn danger" title="Delete annotation" onClick={() => act(() => app!.deleteAnnotation(a.id))}>
              <IconTrash size={14} />
            </button>
          </div>
          <div className="reply" onClick={(e) => e.stopPropagation()}>
            <textarea
              ref={input}
              rows={1}
              placeholder="Reply…"
              value={reply}
              onChange={(e) => setReply(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); send(); }
                if (e.key === 'Escape') app?.selectAnnotation(undefined, false);
              }}
            />
            <button className="icon-btn primary" disabled={!reply.trim()} onClick={send} title="Send reply"><IconSend size={14} /></button>
          </div>
          <div className="thread-footer" onClick={(e) => e.stopPropagation()}>
            {a.status !== 'resolved' ? (
              <button className="btn small" onClick={() => act(() => app!.setAnnotationStatus(a.id, 'resolved'))}><IconCheck size={13} /> Resolve</button>
            ) : (
              <button className="btn small" onClick={() => act(() => app!.setAnnotationStatus(a.id, 'open'))}>Reopen</button>
            )}
            {a.assignee && <span className="assigned-to"><Avatar actor={actorById(a.assignee)} size={16} /> assigned to @{a.assignee}</span>}
          </div>
        </>
      )}
      {!open && a.assignee && (
        <div className="thread-assignee"><Avatar actor={actorById(a.assignee)} size={14} /> @{a.assignee}</div>
      )}
    </div>
  );
}
