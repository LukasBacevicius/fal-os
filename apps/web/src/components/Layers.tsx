import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { getState, setState, toast, useStore } from '../store';
import { worldRects } from '../geometry';
import { activityColors } from '../activity';
import { chatColor, type ChatState } from '../chat';
import { addContext, focusNode } from '../controller';
import { sendTo } from '../chatActions';
import { usd } from '../format';
import type { NodeView } from '../types';
import { fuzzy } from './Palette';
import { NodeRef } from './Transcript';
import { PanelTabs } from './AppMenu';
import { openMenuAt } from './ContextMenu';
import { openPrompt } from './SelectionPrompt';
import {
  applyRecipe, assetOf, assetRecord, copyPrompt, copyRecipe, FILTERS, matchesFilter, parentRefs, promptOf, rerun, roleOf,
  setRole, sourceOf, upstreamChanged, type Filter, type ParentRef,
} from '../out';

const ROW = 18;
const isContainer = (n: NodeView) => n.kind === 'frame' || n.kind === 'group';

export const GLYPH: Record<string, string> = {
  frame: '▣', group: '▢', image: '▧', video: '▶', audio: '♪', note: '¶', text: '¶', generation: '◌',
};

function rename(id: string, name: string) {
  getState().app?.applyEdit(`${id} ${JSON.stringify(name)}`);
}

/** Moves `id` under `parent` at `index`, keeping its world position. */
function moveTo(id: string, parent: string | null, index?: number) {
  const { app, nodes } = getState();
  if (!app || id === parent) return;
  const byId = new Map(nodes.map((n) => [n.id, n]));
  for (let p = parent; p; p = byId.get(p)?.parent ?? null) if (p === id) return;
  const rects = worldRects(nodes);
  const r = rects.get(id)!;
  const po = parent ? rects.get(parent)! : [0, 0];
  app.apply([
    { op: 'move', id, parent, index },
    { op: 'update', id, props: { x: Math.round(r[0] - po[0]), y: Math.round(r[1] - po[1]) } },
  ]);
}

/** Node id → color of the chat that created or last touched it (tool events in every
 *  thread, then live activity). */
function nodeColors(chat: ChatState, activity: ReturnType<typeof getState>['activity']): Map<string, string> {
  const m = new Map<string, string>();
  for (const id of chat.order) {
    const color = chatColor(chat, id);
    for (const e of chat.threads[id]?.events ?? []) {
      const nodes = (e as { nodes?: unknown }).nodes;
      if ((e.kind === 'tool' || e.kind === 'done') && Array.isArray(nodes)) for (const n of nodes) if (typeof n === 'string') m.set(n, color);
    }
  }
  for (const [n, c] of activityColors(activity)) m.set(n, c);
  return m;
}

interface FlatRow { n: NodeView; depth: number; kids: number }

/** Left panel (⌘⇧\): every node as a layer, top of the list on top, plus the properties of
 *  the selection below. Selection syncs both ways; hover highlights the node on the canvas. */
export function Layers() {
  const nodes = useStore((s) => s.nodes);
  const selection = useStore((s) => s.selection);
  const chat = useStore((s) => s.chat);
  const activity = useStore((s) => s.activity);
  const app = useStore((s) => s.app);
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set());
  const [filter, setFilter] = useState('');
  const [chips, setChips] = useState<Set<Filter>>(() => new Set());
  const [editing, setEditing] = useState<string | null>(null);
  const [drop, setDrop] = useState<{ id: string; where: 'in' | 'before' | 'after' } | null>(null);
  const [scroll, setScroll] = useState(0);
  const [height, setHeight] = useState(400);
  const list = useRef<HTMLDivElement>(null);
  const filterInput = useRef<HTMLInputElement>(null);
  const colors = useMemo(() => nodeColors(chat, activity), [chat, activity]);

  const kids = useMemo(() => {
    const m = new Map<string | null, NodeView[]>();
    for (const n of nodes) {
      if (n.kind === 'connector') continue;
      const k = n.parent ?? null;
      m.set(k, [...(m.get(k) ?? []), n]);
    }
    return m;
  }, [nodes]);
  const byId = useMemo(() => new Map(nodes.map((n) => [n.id, n])), [nodes]);

  // Selecting on the canvas reveals the row (expands its ancestors).
  useEffect(() => {
    const add: string[] = [];
    for (const id of selection) for (let p = byId.get(id)?.parent; p; p = byId.get(p)?.parent) if (!expanded.has(p)) add.push(p);
    if (add.length) setExpanded((e) => new Set([...e, ...add]));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selection, byId]);

  const rows = useMemo(() => {
    const out: FlatRow[] = [];
    const q = filter.trim();
    let keep: Set<string> | null = null;
    if (q || chips.size) {
      keep = new Set();
      for (const n of nodes) {
        const byName = !q || fuzzy(q, n.name || n.text || n.kind) >= 0 || n.id === q;
        const src = chips.size && assetOf(n) ? sourceOf(assetRecord(n.id)) : null;
        const byChip = !chips.size || [...chips].some((f) => matchesFilter(n, f, src));
        if (byName && byChip) {
          for (let c: NodeView | undefined = n; c; c = c.parent ? byId.get(c.parent) : undefined) keep.add(c.id);
        }
      }
    }
    const walk = (parent: string | null, depth: number) => {
      // Top of the z-order first.
      for (const n of [...(kids.get(parent) ?? [])].reverse()) {
        if (keep && !keep.has(n.id)) continue;
        const k = kids.get(n.id)?.length ?? 0;
        out.push({ n, depth, kids: k });
        if (k && (keep || expanded.has(n.id))) walk(n.id, depth + 1);
      }
    };
    walk(null, 0);
    return out;
  }, [kids, expanded, filter, nodes, byId, chips]);

  useLayoutEffect(() => {
    const el = list.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setHeight(el.clientHeight));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // Keep the (last) selected row in view.
  useEffect(() => {
    const el = list.current;
    const id = selection[selection.length - 1];
    if (!el || !id) return;
    const i = rows.findIndex((r) => r.n.id === id);
    if (i < 0) return;
    if (i * ROW < el.scrollTop) el.scrollTop = i * ROW;
    else if ((i + 1) * ROW > el.scrollTop + el.clientHeight) el.scrollTop = (i + 1) * ROW - el.clientHeight;
  }, [selection, rows]);

  const select = (id: string, e: React.MouseEvent | React.KeyboardEvent) => {
    if (!app) return;
    const add = e.shiftKey || e.metaKey || e.ctrlKey;
    const sel = getState().selection;
    const next = add ? (sel.includes(id) ? sel.filter((x) => x !== id) : [...sel, id]) : [id];
    app.setSelection(next);
  };
  const toggle = (id: string) => setExpanded((e) => {
    const n = new Set(e);
    if (n.has(id)) n.delete(id);
    else n.add(id);
    return n;
  });

  const onKey = (e: React.KeyboardEvent) => {
    if (editing) return;
    const sel = getState().selection;
    const cur = rows.findIndex((r) => r.n.id === sel[sel.length - 1]);
    const mod = e.metaKey || e.ctrlKey;
    if (mod && (e.key === 'f' || e.key === 'F')) {
      e.preventDefault();
      e.stopPropagation();
      filterInput.current?.focus();
    } else if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      const r = rows[Math.max(0, Math.min(rows.length - 1, cur + (e.key === 'ArrowDown' ? 1 : -1)))];
      if (r) app?.setSelection([r.n.id]);
    } else if (e.key === 'ArrowRight' && cur >= 0 && rows[cur].kids) {
      setExpanded((x) => new Set([...x, rows[cur].n.id]));
    } else if (e.key === 'ArrowLeft' && cur >= 0) {
      const r = rows[cur];
      if (r.kids && expanded.has(r.n.id)) toggle(r.n.id);
      else if (r.n.parent) app?.setSelection([r.n.parent]);
    } else if (e.key === 'Enter' && cur >= 0) {
      e.preventDefault();
      setEditing(rows[cur].n.id);
    } else if ((e.key === 'Backspace' || e.key === 'Delete') && sel.length) {
      e.preventDefault();
      app?.apply(sel.map((id) => ({ op: 'delete', id })));
    } else if (e.key === 'Escape') {
      (e.target as HTMLElement).blur();
    }
  };

  const first = Math.max(0, Math.floor(scroll / ROW) - 10);
  const last = Math.min(rows.length, Math.ceil((scroll + height) / ROW) + 10);

  return (
    <aside className="layers">
      <PanelTabs />
      <div className="layers-head">

        <input
          ref={filterInput}
          className="layers-filter"
          value={filter}
          placeholder="⌘F filter"
          spellCheck={false}
          onChange={(e) => setFilter(e.target.value)}
          onKeyDown={(e) => {
            e.stopPropagation();
            if (e.key === 'Escape') {
              setFilter('');
              list.current?.focus();
            } else if (e.key === 'Enter' || e.key === 'ArrowDown') {
              e.preventDefault();
              if (rows[0]) app?.setSelection([rows.find((r) => fuzzy(filter, r.n.name || r.n.text || '') >= 0)?.n.id ?? rows[0].n.id]);
              list.current?.focus();
            }
          }}
        />
      </div>
      <div className="layers-chips">
        {FILTERS.map((f) => (
          <button
            key={f}
            className={`link ${chips.has(f) ? '' : 'faint'}`}
            title={`show only ${f}`}
            onClick={() => setChips((c) => {
              const n = new Set(c);
              if (n.has(f)) n.delete(f);
              else n.add(f);
              return n;
            })}
          >
            {f}
          </button>
        ))}
      </div>
      <div className="layers-list" ref={list} tabIndex={0} onKeyDown={onKey} onScroll={(e) => setScroll(e.currentTarget.scrollTop)} onMouseLeave={() => app?.setHighlight([])}>
        <div style={{ height: rows.length * ROW, position: 'relative' }}>
          {rows.slice(first, last).map((r, i) => {
            const n = r.n;
            const top = (first + i) * ROW;
            const on = selection.includes(n.id);
            const color = colors.get(n.id);
            const name = n.name || (isContainer(n) ? n.kind : n.text?.split('\n')[0].slice(0, 40) || n.kind);
            return (
              <div
                key={n.id}
                className={`layer-row ${on ? 'on' : ''} ${drop?.id === n.id ? `drop-${drop.where}` : ''}`}
                style={{ top, paddingLeft: 6 + r.depth * 12 }}
                draggable={editing !== n.id}
                onClick={(e) => select(n.id, e)}
                onDoubleClick={() => setEditing(n.id)}
                onContextMenu={(e) => {
                  e.preventDefault();
                  openMenuAt(e.clientX, e.clientY, n.id);
                }}
                onMouseEnter={() => app?.setHighlight([n.id])}
                onDragStart={(e) => {
                  e.dataTransfer.setData('application/x-falos-node', n.id);
                  e.dataTransfer.effectAllowed = 'copyMove';
                }}
                onDragOver={(e) => {
                  if (!e.dataTransfer.types.includes('application/x-falos-node')) return;
                  e.preventDefault();
                  const b = e.currentTarget.getBoundingClientRect();
                  const f = (e.clientY - b.top) / b.height;
                  const where = isContainer(n) && f > 0.25 && f < 0.75 ? 'in' : f < 0.5 ? 'before' : 'after';
                  if (drop?.id !== n.id || drop.where !== where) setDrop({ id: n.id, where });
                }}
                onDragLeave={() => setDrop(null)}
                onDrop={(e) => {
                  e.preventDefault();
                  const id = e.dataTransfer.getData('application/x-falos-node');
                  const where = drop?.where;
                  setDrop(null);
                  if (!id || id === n.id || !where) return;
                  if (where === 'in') return moveTo(id, n.id);
                  const siblings = (kids.get(n.parent ?? null) ?? []).filter((m) => m.id !== id);
                  const idx = siblings.findIndex((m) => m.id === n.id);
                  // Rows list the top of the z-order first; the doc's index counts from the bottom.
                  moveTo(id, n.parent, Math.max(0, Math.min(siblings.length, where === 'before' ? idx + 1 : idx)));
                }}
              >
                <span
                  className="faint layer-caret"
                  onClick={(e) => {
                    e.stopPropagation();
                    if (r.kids) toggle(n.id);
                  }}
                >
                  {r.kids ? (filter || expanded.has(n.id) ? '▾' : '▸') : ' '}
                </span>
                <span className="layer-glyph dim">{GLYPH[n.kind] ?? '·'}</span>
                {editing === n.id ? (
                  <input
                    className="layer-edit"
                    autoFocus
                    defaultValue={n.name}
                    onFocus={(e) => e.currentTarget.select()}
                    onClick={(e) => e.stopPropagation()}
                    onKeyDown={(e) => {
                      e.stopPropagation();
                      if (e.key === 'Enter') {
                        rename(n.id, e.currentTarget.value.trim());
                        setEditing(null);
                        list.current?.focus();
                      } else if (e.key === 'Escape') {
                        setEditing(null);
                        list.current?.focus();
                      }
                    }}
                    onBlur={(e) => {
                      if (e.currentTarget.value.trim() !== n.name) rename(n.id, e.currentTarget.value.trim());
                      setEditing(null);
                    }}
                  />
                ) : (
                  <span className={`layer-name ${isContainer(n) ? '' : 'dim'}`}>{name}</span>
                )}
                {roleOf(n) === 'final' && <span className="dim" title="final">★</span>}
                {color && <span className="layer-dot" style={{ background: color }} title="made or touched by an agent" />}
                {n.locked && <span className="faint">⊘</span>}
              </div>
            );
          })}
        </div>
        {!rows.length && <div className="faint layers-empty">{filter || chips.size ? 'no match' : 'no layers yet · drop files or press N'}</div>}
      </div>
      <Properties />
    </aside>
  );
}

// ---- properties ------------------------------------------------------------------------

interface Lineage { model?: string; op?: string; kind?: string; params?: Record<string, unknown>; recipe?: Record<string, unknown>; seed?: number; parents?: unknown[]; cost?: number; actor?: string }

const ROLES = ['input', 'candidate', 'wip', 'final', 'rejected'];

/** `← op{args} of <parent>` lines up the first-parent chain (at most 4). */
function chain(id: string): { op: string; parent: string | null; asset: string }[] {
  const out: { op: string; parent: string | null; asset: string }[] = [];
  const nodes = getState().nodes;
  let cur: string | null = id;
  for (let i = 0; i < 4 && cur; i++) {
    const lin: Lineage | undefined = assetRecord(cur)?.lineage;
    if (!lin || (!lin.op && (!lin.model || lin.model === 'import'))) break;
    const ref: ParentRef | undefined = parentRefs(lin)[0];
    const parent: string | null = ref ? (ref.node && nodes.some((n) => n.id === ref.node) ? ref.node : nodes.find((n) => n.props.asset === ref.asset)?.id ?? null) : null;
    out.push({ op: String(lin.op ?? lin.model), parent, asset: ref?.asset ?? '' });
    cur = parent;
  }
  return out;
}

/** One editable mono field: Enter or blur applies, Esc reverts. `value` null = mixed. */
function Field({ label, value, onSet, width = 7 }: { label: string; value: string | null; onSet: (v: string) => void; width?: number }) {
  const [v, setV] = useState(value ?? '');
  useEffect(() => setV(value ?? ''), [value]);
  return (
    <label className="prop">
      <span className="faint">{label}</span>
      <input
        value={v}
        placeholder={value === null ? 'mixed' : ''}
        style={{ width: `${width}ch` }}
        spellCheck={false}
        onChange={(e) => setV(e.target.value)}
        onKeyDown={(e) => {
          e.stopPropagation();
          if (e.key === 'Enter') (e.target as HTMLInputElement).blur();
          else if (e.key === 'Escape') {
            setV(value ?? '');
            (e.target as HTMLInputElement).blur();
          } else if (e.key === 'ArrowUp' || e.key === 'ArrowDown') {
            const n = Number(v);
            if (!Number.isFinite(n)) return;
            e.preventDefault();
            const d = (e.key === 'ArrowUp' ? 1 : -1) * (e.shiftKey ? 10 : 1);
            setV(String(n + d));
            onSet(String(n + d));
          }
        }}
        onBlur={() => v !== (value ?? '') && v.trim() !== '' && onSet(v)}
      />
    </label>
  );
}

const shared = (vals: (string | number | boolean | undefined)[]) => (vals.every((x) => x === vals[0]) ? String(vals[0] ?? '') : null);

function Properties() {
  const nodes = useStore((s) => s.nodes);
  const selection = useStore((s) => s.selection);
  const jobs = useStore((s) => s.jobs);
  const app = useStore((s) => s.app);
  const sel = useMemo(() => nodes.filter((n) => selection.includes(n.id)), [nodes, selection]);
  const one = sel.length === 1 ? sel[0] : null;
  const detail = useMemo(() => (one && app ? (app.node(one.id) as { asset?: { lineage?: Lineage } } | null) : null), [one, app]);
  if (!sel.length || !app) return null;
  const set = (props: Record<string, unknown>) => {
    try {
      app.apply(sel.map((n) => ({ op: 'update', id: n.id, props })));
    } catch (e) {
      toast(String(e));
    }
  };
  const num = (k: string) => (v: string) => {
    const n = Number(v);
    if (Number.isFinite(n)) set({ [k]: Math.round(n * 100) / 100 });
  };
  const locked = sel.every((n) => n.locked);
  const raw = detail?.asset?.lineage;
  // Imported media carry an `import` lineage: no generation to rerun.
  const lin = raw && (raw.op || (raw.model && raw.model !== 'import')) ? raw : undefined;
  const media = one && !!assetOf(one);
  const prompt = one ? promptOf(one.id) : '';
  const src = one ? sourceOf((detail?.asset as Record<string, any> | undefined) ?? null) : null;
  const steps = one && lin ? chain(one.id) : [];
  const stale = one && lin ? upstreamChanged(one.id) : [];
  const role = sel.length && sel.every((n) => roleOf(n) === roleOf(sel[0])) ? roleOf(sel[0]) : null;
  const chip = one ? [{ kind: 'node' as const, node: one.id }] : [];
  const current = () => {
    const a = getState().chat.active;
    return a && !a.startsWith('ext:') ? a : null;
  };
  const gen = one?.kind === 'generation' ? one : null;
  const job = gen ? jobs[gen.id] : undefined;

  return (
    <div className="props">
      <div className="props-head dim">{one ? `${one.kind} ${one.id}` : `${sel.length} selected`}</div>
      <div className="props-grid">
        <Field label="x" value={shared(sel.map((n) => Math.round(n.x)))} onSet={num('x')} />
        <Field label="y" value={shared(sel.map((n) => Math.round(n.y)))} onSet={num('y')} />
        <Field label="w" value={shared(sel.map((n) => Math.round(n.w)))} onSet={num('w')} />
        <Field label="h" value={shared(sel.map((n) => Math.round(n.h)))} onSet={num('h')} />
        <Field label="rot" value={shared(sel.map((n) => Math.round((n.rot ?? 0) * 10) / 10))} onSet={num('rot')} />
        <label className="prop">
          <span className="faint">lock</span>
          <button className="link" onClick={() => set({ locked: !locked })}>{locked ? '[x]' : '[ ]'}</button>
        </label>
      </div>
      {one && (one.kind === 'note' || one.kind === 'text') && (
        <div className="props-grid">
          <Field label="text size" value={String(Number(one.props.fontSize) || 12.5)} onSet={(v) => Number(v) > 0 && set({ fontSize: Number(v) })} />
        </div>
      )}
      {gen && (
        <div className="props-sec">
          <div><span className="faint">status </span>{String(job?.status ?? gen.props.status ?? 'queued')}{typeof gen.props.progress === 'number' ? ` · ${Math.round(Number(gen.props.progress) * 100)}%` : ''}</div>
          {gen.props.model ? <div><span className="faint">model </span>{String(gen.props.model)}</div> : null}
          {(gen.props.params as any)?.prompt ? <div className="props-prompt">{String((gen.props.params as any).prompt)}</div> : null}
          <button className="link dim" title="delete the placeholder (cancels the job)" onClick={() => app.apply({ op: 'delete', id: gen.id })}>[cancel]</button>
        </div>
      )}
      <div className="props-roles">
        <span className="faint">role</span>
        {ROLES.map((r) => (
          <button key={r} className={`link ${role === r ? '' : 'faint'}`} onClick={() => setRole(sel.map((n) => n.id), role === r ? null : r)}>{r}</button>
        ))}
      </div>
      {media && (
        <div className="props-sec">
          <div><span className="faint">src </span>{src ?? 'import'}{lin?.actor ? <span className="faint"> by @{lin.actor}</span> : null}</div>
          {lin ? (
            <>
              {steps.map((st, i) => (
                <div key={i} className="props-chain">
                  <span className="faint">{'  '.repeat(i)}← </span>{st.op}
                  {st.parent ? <> <span className="faint">of</span> <NodeRef id={st.parent} /></> : st.asset ? <span className="faint"> of {st.asset.replace('sha256:', '').slice(0, 6)}</span> : null}
                </div>
              ))}
              {stale.length > 0 && (
                <div className="props-stale" title="a parent node now shows different bytes than this was made from">
                  ! upstream changed:{stale.map((p) => <span key={p}> <NodeRef id={p} /></span>)}
                </div>
              )}
              {prompt && <div className="props-prompt">{prompt}</div>}
              {lin.seed != null && <div><span className="faint">seed </span>{lin.seed}</div>}
              {lin.cost != null && <div><span className="faint">cost </span>{usd(lin.cost)}</div>}
            </>
          ) : null}
          <div className="props-actions">
            {lin && (
              <>
                <button className="link dim" title="run the same recipe again (host op registry), else ask the chat" onClick={() => void rerun(one!.id).catch(() => sendTo(current(), 'Rerun this with the same settings.', chip))}>[rerun]</button>
                <button className="link dim" title="say what to change" onClick={openPrompt}>[rerun with changes]</button>
                <button className="link dim" onClick={() => copyRecipe(one!.id)}>[copy recipe]</button>
                {selection.length > 1 && <button className="link dim" title="the first selected node's recipe on the others" onClick={applyRecipe}>[apply to selection]</button>}
              </>
            )}
            {prompt && <button className="link dim" onClick={() => copyPrompt(one!.id)}>[copy prompt]</button>}
            <button className="link dim" title="ask the current chat for variations" onClick={() => sendTo(current(), 'Make 4 variations of this.', chip)}>[variations]</button>
            <button className="link dim" title="add as a chip to the chat" onClick={() => { addContext(chip); setState({ chatOpen: true }); }}>[use as reference]</button>
            <button className="link dim" onClick={() => one && focusNode(one.id)}>[zoom]</button>
          </div>
        </div>
      )}
    </div>
  );
}
