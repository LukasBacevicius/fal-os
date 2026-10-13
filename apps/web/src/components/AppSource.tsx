import { useEffect, useState } from 'react';
import { setState, useStore } from '../store';
import { appBase, appVersion, isApp } from '../apps';
import { bytes } from '../format';

interface FileRow { path: string; bytes: number }

/** `[src]` of an app node: its files, read-only (a file list and the selected file's text).
 *  ↑ / ↓ pick a file, esc closes. Follows live updates (a new version refetches). */
export function AppSource() {
  const id = useStore((s) => s.appSrc);
  const node = useStore((s) => s.nodes.find((n) => n.id === s.appSrc));
  useStore((s) => s.apps.versions);
  const version = node && isApp(node) ? appVersion(node) : 0;
  const [files, setFiles] = useState<FileRow[] | null>(null);
  const [sel, setSel] = useState<string | null>(null);
  const [text, setText] = useState<string>('');
  const [err, setErr] = useState<string | null>(null);
  const base = node && isApp(node) ? appBase(node) : null;

  useEffect(() => {
    if (id && !node) setState({ appSrc: null });
  }, [id, node]);

  useEffect(() => {
    if (!base) return;
    let gone = false;
    fetch(`${base}__falos/files`, { cache: 'no-store' })
      .then((r) => (r.ok ? r.json() : Promise.reject(new Error(String(r.status)))))
      .then((j) => {
        if (gone) return;
        const list: FileRow[] = Array.isArray(j.files) ? j.files : [];
        setFiles(list);
        setSel((s) => (s && list.some((f) => f.path === s) ? s : list.find((f) => f.path === 'index.html')?.path ?? list[0]?.path ?? null));
      })
      .catch((e) => !gone && setErr(String(e)));
    return () => {
      gone = true;
    };
  }, [base, version]);

  useEffect(() => {
    if (!base || !sel) return;
    let gone = false;
    const f = files?.find((x) => x.path === sel);
    if (f && f.bytes > 2_000_000) {
      setText(`(${bytes(f.bytes)}: too big to show)`);
      return;
    }
    fetch(`${base}${sel.split('/').map(encodeURIComponent).join('/')}?v=${version}`, { cache: 'no-store' })
      .then(async (r) => {
        const type = r.headers.get('content-type') ?? '';
        if (!/^(text\/|application\/(json|xml)|image\/svg)/.test(type)) return `(binary · ${type || 'unknown type'})`;
        let t = await r.text();
        // The host injects its guard script into HTML; show the file as written.
        t = t.replace(/<script>\(\(\)=>\{if\(window\.__falos\)[\s\S]*?<\/script>/, '');
        return t;
      })
      .then((t) => !gone && setText(t))
      .catch((e) => !gone && setText(`(can't read: ${e})`));
    return () => {
      gone = true;
    };
  }, [base, sel, version, files]);

  useEffect(() => {
    if (!files?.length) return;
    const on = (e: KeyboardEvent) => {
      if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return;
      e.preventDefault();
      const i = files.findIndex((f) => f.path === sel);
      const j = Math.min(files.length - 1, Math.max(0, i + (e.key === 'ArrowDown' ? 1 : -1)));
      setSel(files[j].path);
    };
    window.addEventListener('keydown', on);
    return () => window.removeEventListener('keydown', on);
  }, [files, sel]);

  if (!id || !node) return null;
  const close = () => setState({ appSrc: null });
  const lines = text.split('\n');
  return (
    <div className="pv-overlay app-src" onPointerDown={(e) => e.target === e.currentTarget && close()}>
      <div className="pv-head">
        <span>{node.name || String(node.props.app)}</span>
        <span className="pv-faint">src · read-only · v{version}{files ? ` · ${files.length} file${files.length === 1 ? '' : 's'}` : ''}</span>
        <span className="pv-spacer" />
        <button className="link" title="close (esc)" onClick={close}>[esc]</button>
      </div>
      <div className="pv-body app-src-body">
        <div className="app-src-files">
          {err && <div className="pv-dim">{err}</div>}
          {files?.map((f) => (
            <button key={f.path} className={`app-src-file${f.path === sel ? ' on' : ''}`} onClick={() => setSel(f.path)} title={`${f.path} · ${bytes(f.bytes)}`}>
              {f.path}
            </button>
          ))}
        </div>
        <pre className="app-src-code">
          {lines.map((l, i) => (
            <div key={i}>
              <span className="app-src-ln">{i + 1}</span>
              {l || ' '}
            </div>
          ))}
        </pre>
      </div>
    </div>
  );
}
