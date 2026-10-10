import { useMemo, useRef, useState } from 'react';
import { getState, useStore } from '../store';
import { worldRects } from '../geometry';
import type { NodeView } from '../types';

const isContainer = (n: NodeView) => n.kind === 'frame' || n.kind === 'group';

function rename(id: string, name: string) {
  const { app } = getState();
  // The text format sets a node's name by id: `<id> "<name>"`.
  app?.applyEdit(`${id} ${JSON.stringify(name)}`);
}

/** Moves `id` under `parent` at `index`, keeping its world position. */
function moveTo(id: string, parent: string | null, index?: number) {
  const { app, nodes } = getState();
  if (!app || id === parent) return;
  // No cycles: the target must not be inside the moved node.
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

function Row({ n, depth, kids, all }: { n: NodeView; depth: number; kids: Map<string, NodeView[]>; all: NodeView[] }) {
  const selection = useStore((s) => s.selection);
  const [open, setOpen] = useState(false);
  const [editing, setEditing] = useState(false);
  const [drop, setDrop] = useState<'in' | 'before' | 'after' | null>(null);
  const ref = useRef<HTMLDivElement>(null);
  const children = kids.get(n.id) ?? [];
  const container = isContainer(n);
  const selected = selection.includes(n.id);

  const select = () => {
    const { app } = getState();
    app?.setSelection([n.id]);
    app?.zoomToNodes([n.id]);
  };

  return (
    <>
      <div
        ref={ref}
        tabIndex={0}
        draggable={!editing}
        className={`outline-row ${selected ? 'on' : ''} ${drop ? `drop-${drop}` : ''}`}
        style={{ paddingLeft: 8 + depth * 12 }}
        onClick={select}
        onDoubleClick={() => setEditing(true)}
        onKeyDown={(e) => {
          if (editing) return;
          if (e.key === 'Enter') {
            e.preventDefault();
            setEditing(true);
          } else if (e.key === 'Backspace' || e.key === 'Delete') {
            e.preventDefault();
            getState().app?.apply({ op: 'delete', id: n.id });
          } else if (e.key === 'ArrowRight' && container) setOpen(true);
          else if (e.key === 'ArrowLeft') setOpen(false);
          else if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
            e.preventDefault();
            const rows = Array.from(document.querySelectorAll<HTMLElement>('.outline-row'));
            rows[rows.indexOf(ref.current!) + (e.key === 'ArrowDown' ? 1 : -1)]?.focus();
          }
        }}
        onDragStart={(e) => e.dataTransfer.setData('application/x-falos-node', n.id)}
        onDragOver={(e) => {
          if (!e.dataTransfer.types.includes('application/x-falos-node')) return;
          e.preventDefault();
          const r = ref.current!.getBoundingClientRect();
          const f = (e.clientY - r.top) / r.height;
          setDrop(container && f > 0.25 && f < 0.75 ? 'in' : f < 0.5 ? 'before' : 'after');
        }}
        onDragLeave={() => setDrop(null)}
        onDrop={(e) => {
          e.preventDefault();
          const id = e.dataTransfer.getData('application/x-falos-node');
          const where = drop;
          setDrop(null);
          if (!id || id === n.id) return;
          if (where === 'in') return moveTo(id, n.id);
          const siblings = all.filter((m) => m.parent === n.parent && m.id !== id);
          const i = siblings.findIndex((m) => m.id === n.id);
          // Rows list the top of the z-order first; the doc's index counts from the bottom.
          const docIndex = where === 'before' ? i + 1 : i;
          moveTo(id, n.parent, Math.max(0, Math.min(siblings.length, docIndex)));
        }}
      >
        <span
          className="faint outline-caret"
          onClick={(e) => {
            e.stopPropagation();
            setOpen(!open);
          }}
        >
          {children.length ? (open ? '-' : '+') : ' '}
        </span>
        {editing ? (
          <input
            className="outline-edit"
            autoFocus
            defaultValue={n.name}
            onFocus={(e) => e.currentTarget.select()}
            onClick={(e) => e.stopPropagation()}
            onKeyDown={(e) => {
              e.stopPropagation();
              if (e.key === 'Enter') {
                rename(n.id, e.currentTarget.value.trim());
                setEditing(false);
                ref.current?.focus();
              } else if (e.key === 'Escape') {
                setEditing(false);
                ref.current?.focus();
              }
            }}
            onBlur={(e) => {
              rename(n.id, e.currentTarget.value.trim());
              setEditing(false);
            }}
          />
        ) : (
          <span className={`outline-name ${container ? '' : 'dim'}`}>{n.name || (container ? n.kind : n.text?.slice(0, 30) || n.kind)}</span>
        )}
        {container && <span className="faint">{children.length}</span>}
      </div>
      {open && [...children].reverse().map((c) => <Row key={c.id} n={c} depth={depth + 1} kids={kids} all={all} />)}
    </>
  );
}

/** Left outline: frames and groups as a tree (expand for their contents). Click selects
 *  and zooms, double-click / Enter renames, drag reorders or reparents, ⌫ deletes. */
export function Outline() {
  const nodes = useStore((s) => s.nodes);
  const kids = useMemo(() => {
    const m = new Map<string, NodeView[]>();
    for (const n of nodes) if (n.parent) m.set(n.parent, [...(m.get(n.parent) ?? []), n]);
    return m;
  }, [nodes]);
  const roots = nodes.filter((n) => !n.parent && isContainer(n));
  return (
    <aside className="outline">
      <div className="outline-list">
        {[...roots].reverse().map((n) => <Row key={n.id} n={n} depth={0} kids={kids} all={nodes} />)}
        {!roots.length && <div className="faint outline-empty">no frames or groups · ⌘G groups the selection</div>}
      </div>
    </aside>
  );
}
