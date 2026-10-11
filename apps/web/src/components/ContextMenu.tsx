import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { getState, setState, useStore } from '../store';
import { isDesktop, type AppInfo } from '../desktop';
import { copyFiles, copyImage, download, openExport, openWith, openWithList, outNodes, promptOf, reveal, roleOf, toggleFinal, toggleLineage } from '../out';
import { openPrompt } from './SelectionPrompt';

interface Item { label: string; hint?: string; run?: () => void; sub?: boolean }

/** Right-click on nodes: the few most useful "out" actions; everything else is in ⌘K. */
export function ContextMenu() {
  const menu = useStore((s) => s.menu)!;
  const nodes = useStore((s) => s.nodes);
  const box = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ x: menu.x, y: menu.y });
  const [apps, setApps] = useState<{ path: string; apps: AppInfo[] } | 'loading' | null>(null);
  const [subOpen, setSubOpen] = useState(false);
  const close = () => setState({ menu: null });

  useEffect(() => {
    const kd = (e: KeyboardEvent) => e.key === 'Escape' && (e.preventDefault(), close());
    window.addEventListener('keydown', kd, true);
    return () => window.removeEventListener('keydown', kd, true);
  }, []);
  // Keep the menu on screen.
  useLayoutEffect(() => {
    const r = box.current?.getBoundingClientRect();
    if (r) setPos({ x: Math.min(menu.x, innerWidth - r.width - 4), y: Math.min(menu.y, innerHeight - r.height - 4) });
  }, [menu.x, menu.y]);

  const ids = menu.ids;
  const media = outNodes(ids);
  const sel = nodes.filter((n) => ids.includes(n.id));
  const image = media.length === 1 && media[0].kind === 'image';
  const final = sel.length > 0 && sel.every((n) => roleOf(n) === 'final');
  const desk = isDesktop();
  const go = (f: () => void) => () => {
    close();
    f();
  };
  const openSub = () => {
    setSubOpen(true);
    if (apps === null) {
      setApps('loading');
      void openWithList(ids).then((r) => setApps(r ?? { path: '', apps: [] }));
    }
  };

  const items: Item[] = [
    ...(media.length
      ? [
          { label: desk ? (media.length > 1 ? `copy ${media.length} files` : 'copy file') : 'copy link', hint: '⌘⇧C', run: go(() => void copyFiles(ids)) },
          ...(image ? [{ label: 'copy image', run: go(() => void copyImage(ids[0])) }] : []),
          desk ? { label: 'open in', sub: true } : { label: 'download', run: go(() => void download(ids)) },
          ...(desk ? [{ label: 'reveal in finder', run: go(() => void reveal(ids)) }] : []),
        ]
      : []),
    { label: 'export…', hint: '⌘⇧E', run: go(openExport) },
    { label: final ? 'unmark final' : 'mark final', run: go(() => toggleFinal(ids)) },
    { label: 'show lineage', run: go(toggleLineage) },
    ...(media.length === 1 && promptOf(media[0].id) ? [{ label: 'rerun with changes…', run: go(openPrompt) }] : []),
    { label: 'more…', hint: '⌘K', run: go(() => setState({ overlay: 'palette' })) },
  ];

  return (
    <div className="modal menu-veil" onPointerDown={close} onContextMenu={(e) => (e.preventDefault(), close())}>
      <div ref={box} className="menu" style={{ left: pos.x, top: pos.y }} onPointerDown={(e) => e.stopPropagation()}>
        {items.map((it) => (
          <div
            key={it.label}
            className={`menu-row ${it.sub && subOpen ? 'on' : ''}`}
            onMouseEnter={() => (it.sub ? openSub() : setSubOpen(false))}
            onClick={it.sub ? openSub : it.run}
          >
            <span>{it.label}</span>
            <span className="dim">{it.sub ? '▸' : it.hint ?? ''}</span>
            {it.sub && subOpen && (
              <div className="menu menu-sub" onClick={(e) => e.stopPropagation()}>
                {apps === 'loading' || apps === null ? (
                  <div className="menu-row faint">…</div>
                ) : apps.apps.length ? (
                  apps.apps.map((a, i) => (
                    <div
                      key={a.path}
                      className={`menu-row ${i > 0 && a.pinned !== apps.apps[i - 1].pinned ? 'menu-sep' : ''}`}
                      onClick={() => {
                        close();
                        openWith((apps as { path: string }).path, a);
                      }}
                    >
                      <span>{a.name}</span>
                      <span className="dim">{a.default ? 'default' : ''}</span>
                    </div>
                  ))
                ) : (
                  <div className="menu-row faint">no apps</div>
                )}
              </div>
            )}
          </div>
        ))}
      </div>
    </div>
  );
}

/** Right-click on the canvas: select the node under the pointer (unless it is already part
 *  of the selection) and open the menu. */
export function openMenuAt(x: number, y: number, node: string | null) {
  const { app, selection } = getState();
  if (!app || !node) return;
  const ids = selection.includes(node) ? selection : [node];
  if (ids !== selection) app.setSelection(ids);
  setState({ menu: { x, y, ids } });
}
