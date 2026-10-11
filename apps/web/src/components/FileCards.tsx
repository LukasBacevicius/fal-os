import { useRef, useState } from 'react';
import { toast, useStore } from '../store';
import { hostSync } from '../sync';
import { desktop, isDesktop, type AppInfo } from '../desktop';
import { assetOf, setDownloadUrl } from '../out';
import { bytes } from '../format';
import type { OutFile } from '../chat';
import { NodeRef } from './Transcript';

/** A `files` chat event as cards: thumb, name, preset · size; drag out (desktop: native
 *  file drag; web: Chrome `DownloadURL` when the node is known), [reveal] [open in] [copy]. */
export function FileCards({ files, dir }: { files: OutFile[]; dir: string }) {
  const desk = isDesktop();
  const all = files.map((f) => f.path);
  return (
    <div className="msg files">
      <div className="dim">
        · {files.length} file{files.length === 1 ? '' : 's'}
        {dir && <span className="faint"> → {dir.split('/').slice(-2).join('/')}</span>}
        {desk && files.length > 1 && (
          <>
            {'  '}
            <button className="link faint" onClick={() => desktop.reveal(all[0]).catch((e) => toast(String(e)))}>[reveal]</button>
            <button className="link faint" onClick={() => desktop.copyFiles(all).then(() => toast('files copied'), (e) => toast(String(e)))}>[copy all]</button>
          </>
        )}
      </div>
      {files.map((f) => <FileCard key={f.path} f={f} />)}
    </div>
  );
}

function FileCard({ f }: { f: OutFile }) {
  const node = useStore((s) => (f.node ? s.nodes.find((n) => n.id === f.node) : undefined));
  const [apps, setApps] = useState<AppInfo[] | null>(null);
  const press = useRef<{ x: number; y: number; done: boolean } | null>(null);
  const desk = isDesktop();
  const sync = hostSync();
  const asset = assetOf(node);
  const thumb = node?.kind === 'image' && asset && sync ? sync.assetHref(asset) : null;
  const meta = [f.preset, f.w && f.h ? `${f.w}×${f.h}` : '', f.bytes != null ? bytes(f.bytes) : ''].filter(Boolean).join(' · ');
  const err = (e: unknown) => toast(String(e));

  return (
    <div
      className="file-card"
      title={desk ? `${f.path}\ndrag to Finder or any app` : f.path}
      draggable={!desk && !!node}
      onDragStart={(e) => {
        if (!node || !setDownloadUrl(e, node)) e.preventDefault();
      }}
      onPointerDown={(e) => {
        if (desk && e.button === 0) press.current = { x: e.clientX, y: e.clientY, done: false };
      }}
      onPointerMove={(e) => {
        const p = press.current;
        if (!p || p.done || !(e.buttons & 1) || Math.hypot(e.clientX - p.x, e.clientY - p.y) < 4) return;
        p.done = true;
        desktop.dragOut([f.path]).catch(err);
      }}
      onPointerUp={() => (press.current = null)}
    >
      <div className="file-thumb">{thumb ? <img src={thumb} alt="" draggable={false} /> : <span className="faint">{glyph(f.name)}</span>}</div>
      <div className="file-body">
        <div className="file-name">{f.name}</div>
        <div className="faint">
          {meta}
          {f.node && <> · <NodeRef id={f.node} /></>}
        </div>
        <div className="file-actions">
          {desk ? (
            <>
              <button className="link dim" onClick={() => desktop.reveal(f.path).catch(err)}>[reveal]</button>
              <button className="link dim" onClick={() => (apps ? setApps(null) : desktop.openWithList(f.path).then(setApps, err))}>[open in{apps ? ' ▾' : ''}]</button>
              <button className="link dim" onClick={() => desktop.copyFiles([f.path]).then(() => toast('file copied'), err)}>[copy]</button>
            </>
          ) : (
            <button className="link dim" onClick={() => navigator.clipboard?.writeText(f.path).then(() => toast('path copied'), err)}>[copy path]</button>
          )}
        </div>
        {apps && (
          <div className="file-apps">
            {apps.slice(0, 10).map((a) => (
              <button key={a.path} className="link dim" onClick={() => (setApps(null), desktop.openWith(f.path, a).catch(err))}>
                {a.name}
              </button>
            ))}
            {!apps.length && <span className="faint">no apps</span>}
          </div>
        )}
      </div>
    </div>
  );
}

function glyph(name: string): string {
  const ext = name.split('.').pop()?.toLowerCase() ?? '';
  if (['mp4', 'mov', 'webm', 'm4v'].includes(ext)) return '▶';
  if (['wav', 'mp3', 'aac', 'm4a', 'flac'].includes(ext)) return '♪';
  if (['png', 'jpg', 'jpeg', 'webp', 'avif', 'gif'].includes(ext)) return '▧';
  if (ext === 'zip') return '[zip]';
  if (ext === 'pdf') return '[pdf]';
  return '·';
}
