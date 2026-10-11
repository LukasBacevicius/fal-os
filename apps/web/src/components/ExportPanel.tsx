import { useEffect, useMemo, useState } from 'react';
import { getState, setState, toast, useStore } from '../store';
import { desktop, isDesktop } from '../desktop';
import { exportMarked, exportRowsOf, loadPresets, runExport, validScale, FALLBACK_PRESETS, type ExportRow, type Preset } from '../out';

const FORMATS = ['', 'png', 'jpg', 'webp', 'avif', 'gif', 'mp4', 'webm', 'mov', 'wav', 'mp3', 'pdf'];
const NAMING = '{project}_{name}_{preset}_{w}x{h}_v{n}.{ext}';

function lastRows(): ExportRow[] | null {
  try {
    const v = JSON.parse(localStorage.getItem('falos.exportRows') ?? 'null');
    return Array.isArray(v) && v.length ? v : null;
  } catch {
    return null;
  }
}

const defaultRow = (kind: string | undefined): ExportRow =>
  ({ preset: kind === 'video' ? 'youtube-1080' : kind === 'audio' ? 'wav' : 'web-image' });

/** ⌘⇧E: rows of preset × scale × format, naming, destination, separate files or one zip /
 *  pdf, optionally saved on the nodes (`export` prop). Compact, keyboard-first: ⏎ exports. */
export function ExportPanel() {
  const ids = useStore((s) => s.exportIds);
  const nodes = useStore((s) => s.nodes);
  const sel = useMemo(() => nodes.filter((n) => ids.includes(n.id)), [nodes, ids]);
  const marked = useMemo(() => nodes.filter((n) => exportRowsOf(n).length), [nodes]);
  const [presets, setPresets] = useState<Preset[]>(FALLBACK_PRESETS);
  const [rows, setRows] = useState<ExportRow[]>(() => (sel[0] && exportRowsOf(sel[0]).length ? exportRowsOf(sel[0]) : lastRows() ?? [defaultRow(sel[0]?.kind)]));
  const [naming, setNaming] = useState(() => {
    try {
      return localStorage.getItem('falos.exportNaming') ?? NAMING;
    } catch {
      return NAMING;
    }
  });
  const [dir, setDir] = useState<string | null>(null);
  const [combine, setCombine] = useState<'' | 'zip' | 'pdf'>('');
  const [save, setSave] = useState(false);
  const [busy, setBusy] = useState(false);
  const close = () => setState({ overlay: null });

  useEffect(() => void loadPresets().then(setPresets), []);
  useEffect(() => {
    const kd = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        close();
      }
    };
    window.addEventListener('keydown', kd);
    return () => window.removeEventListener('keydown', kd);
  }, []);

  const set = (i: number, patch: Partial<ExportRow>) => setRows((r) => r.map((x, j) => (j === i ? { ...x, ...patch } : x)));
  const clean = rows.map((r) => Object.fromEntries(Object.entries(r).filter(([, v]) => v)) as ExportRow).filter((r) => r.preset || r.format);
  const bad = rows.some((r) => !validScale(r.scale ?? ''));

  const go = async () => {
    if (busy || bad) return;
    if (!sel.length) {
      setBusy(true);
      await exportMarked();
      setBusy(false);
      return close();
    }
    try {
      localStorage.setItem('falos.exportRows', JSON.stringify(clean));
      localStorage.setItem('falos.exportNaming', naming);
    } catch {
      /* private mode */
    }
    if (save) getState().app?.apply(sel.map((n) => ({ op: 'update', id: n.id, props: { export: clean } })));
    setBusy(true);
    const r = await runExport(sel.map((n) => n.id), clean, { dir: dir ?? undefined, naming: naming !== NAMING ? naming : undefined, combine: combine || undefined });
    setBusy(false);
    if (r) close();
  };

  const choose = async () => {
    try {
      const d = await desktop.chooseFolder();
      if (d) setDir(d);
    } catch (e) {
      toast(`folder: ${e}`);
    }
  };

  return (
    <div className="modal" onPointerDown={close}>
      <div
        className="export"
        onPointerDown={(e) => e.stopPropagation()}
        onKeyDown={(e) => {
          e.stopPropagation();
          if (e.key === 'Enter' && (e.target as HTMLElement).tagName !== 'BUTTON') {
            e.preventDefault();
            void go();
          }
        }}
      >
        <div className="export-head">
          <span>export</span>
          <span className="dim">{sel.length ? (sel.length === 1 ? sel[0].name || sel[0].id : `${sel.length} nodes`) : `${marked.length} marked`}</span>
          <button className="link dim" onClick={close}>[×]</button>
        </div>
        {sel.length > 0 && (
          <>
            <div className="export-row faint">
              <span className="ex-preset">preset</span><span className="ex-scale">scale</span><span className="ex-format">format</span><span className="ex-suffix">suffix</span>
            </div>
            {rows.map((r, i) => (
              <div key={i} className="export-row">
                <select className="ex-preset" value={r.preset ?? ''} onChange={(e) => set(i, { preset: e.target.value || undefined })}>
                  <option value="">—</option>
                  {presets.map((p) => <option key={p.id} value={p.id}>{p.label}</option>)}
                </select>
                <input className={`ex-scale ${validScale(r.scale ?? '') ? '' : 'bad'}`} value={r.scale ?? ''} placeholder="1x" spellCheck={false} title="2x · 1080p · 512w" onChange={(e) => set(i, { scale: e.target.value })} />
                <select className="ex-format" value={r.format ?? ''} onChange={(e) => set(i, { format: e.target.value || undefined })}>
                  {FORMATS.map((f) => <option key={f} value={f}>{f || '—'}</option>)}
                </select>
                <input className="ex-suffix" value={r.suffix ?? ''} placeholder="" spellCheck={false} onChange={(e) => set(i, { suffix: e.target.value })} />
                <button className="link dim" title="remove row" onClick={() => setRows((x) => x.filter((_, j) => j !== i))}>[−]</button>
              </div>
            ))}
            <button className="link dim" onClick={() => setRows((x) => [...x, { format: 'png', scale: '2x' }])}>[+ row]</button>
            <label className="export-line">
              <span className="faint">name</span>
              <input value={naming} spellCheck={false} onChange={(e) => setNaming(e.target.value)} title="{project} {name} {preset} {w} {h} {n} {index} {ext}" />
            </label>
            <div className="export-line">
              <span className="faint">to</span>
              <button className={`link ${dir ? 'dim' : ''}`} onClick={() => setDir(null)}>{dir ? '( )' : '(•)'} exports/</button>
              {isDesktop() && (
                <button className={`link ${dir ? '' : 'dim'}`} onClick={choose} title={dir ?? 'choose a folder'}>
                  {dir ? `(•) ${dir.split('/').slice(-2).join('/')}` : '( ) choose…'}
                </button>
              )}
            </div>
            <div className="export-line">
              <span className="faint">as</span>
              {(['', 'zip', 'pdf'] as const).map((c) => (
                <button key={c} className={`link ${combine === c ? '' : 'dim'}`} onClick={() => setCombine(c)}>
                  {combine === c ? '(•)' : '( )'} {c || 'files'}
                </button>
              ))}
              <button className={`link ${save ? '' : 'dim'}`} title="remember these rows on the node(s): ⌘⇧E with nothing selected exports them" onClick={() => setSave(!save)}>
                {save ? '[x]' : '[ ]'} save on node{sel.length > 1 ? 's' : ''}
              </button>
            </div>
          </>
        )}
        {!sel.length && <div className="dim">{marked.length ? marked.map((n) => n.name || n.id).slice(0, 8).join(', ') : 'nothing marked: select nodes and save export settings'}</div>}
        <div className="export-line">
          <button className="link" disabled={busy || bad || (!sel.length && !marked.length)} onClick={() => void go()}>
            {busy ? 'exporting…' : sel.length ? `[export${clean.length > 1 ? ` ×${clean.length}` : ''}] ⏎` : '[export all marked] ⏎'}
          </button>
        </div>
      </div>
    </div>
  );
}
