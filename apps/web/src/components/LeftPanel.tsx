import { useMemo, useState } from 'react';
import { useStore } from '../store';
import { bytes, kindLabel } from '../format';
import { IconComment, KindIcon } from '../icons';

export function LeftPanel() {
  const [tab, setTab] = useState<'layers' | 'assets'>('layers');
  return (
    <aside className="panel left">
      <div className="tabs">
        <button className={tab === 'layers' ? 'on' : ''} onClick={() => setTab('layers')}>Layers</button>
        <button className={tab === 'assets' ? 'on' : ''} onClick={() => setTab('assets')}>Assets</button>
      </div>
      {tab === 'layers' ? <Layers /> : <Assets />}
    </aside>
  );
}

function Layers() {
  const app = useStore((s) => s.app);
  const nodes = useStore((s) => s.nodes);
  const selection = useStore((s) => s.selection);
  const hover = useStore((s) => s.hoverNode);
  const annotations = useStore((s) => s.annotations);
  const counts = useMemo(() => {
    const m = new Map<string, number>();
    for (const a of annotations) if (a.status !== 'resolved') m.set(a.nodeId, (m.get(a.nodeId) ?? 0) + 1);
    return m;
  }, [annotations]);
  // Top of the z-order first, like every design tool.
  const rows = useMemo(() => [...nodes].reverse(), [nodes]);
  if (!rows.length) return <div className="panel-empty">No layers yet. Drop files on the canvas.</div>;
  return (
    <div className="list" role="tree">
      {rows.map((n) => (
        <div
          key={n.id}
          role="treeitem"
          aria-selected={selection.includes(n.id)}
          className={`row ${selection.includes(n.id) ? 'selected' : ''} ${hover === n.id ? 'hover' : ''}`}
          style={{ paddingLeft: 12 + n.depth * 14 }}
          onClick={(e) => {
            const next = e.shiftKey ? (selection.includes(n.id) ? selection.filter((s) => s !== n.id) : [...selection, n.id]) : [n.id];
            app?.setSelection(next);
          }}
          onDoubleClick={() => app?.focusNode(n.id)}
        >
          <span className="row-icon"><KindIcon kind={n.kind} /></span>
          <span className="row-name">{n.name || kindLabel(n.kind)}</span>
          {counts.get(n.id) ? (
            <span className="row-badge" title="Open annotations"><IconComment size={11} />{counts.get(n.id)}</span>
          ) : null}
        </div>
      ))}
    </div>
  );
}

function Assets() {
  const app = useStore((s) => s.app);
  const assets = useStore((s) => s.assets);
  const nodes = useStore((s) => s.nodes);
  if (!assets.length) return <div className="panel-empty">Imported files appear here.</div>;
  return (
    <div className="assets">
      {assets.map((a) => {
        const url = a.mime.startsWith('image/') ? app?.assetUrl(a.id) : undefined;
        const uses = nodes.filter((n) => n.props.asset === a.id);
        return (
          <div
            key={a.id}
            className="asset"
            title={a.id}
            onClick={() => uses[0] && app?.setSelection(uses.map((u) => u.id))}
            onDoubleClick={() => uses[0] && app?.focusNode(uses[0].id)}
          >
            <div className="asset-thumb">
              {url ? <img src={url} alt="" /> : <KindIcon kind={kindOfMime(a.mime)} size={18} />}
            </div>
            <div className="asset-meta">
              <div className="asset-name">{a.name}</div>
              <div className="asset-sub">{a.mime.split('/')[1] || a.mime} · {bytes(a.size)}{uses.length > 1 ? ` · ${uses.length} uses` : ''}</div>
            </div>
          </div>
        );
      })}
    </div>
  );
}

function kindOfMime(m: string) {
  if (m.startsWith('video/')) return 'video';
  if (m.startsWith('audio/')) return 'audio';
  if (m.startsWith('text/')) return 'text';
  if (m.includes('pdf')) return 'document';
  if (m.startsWith('model/')) return 'model3d';
  return 'document';
}
