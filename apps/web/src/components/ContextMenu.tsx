import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { getState, setState, useStore } from '../store';
import { isDesktop, type AppInfo } from '../desktop';
import { copyFiles, copyImage, download, openExport, openWith, openWithList, outNodes, promptOf, reveal, roleOf, toggleFinal, toggleLineage } from '../out';
import { openPrompt } from './SelectionPrompt';
import { addSelectionToChat, importPicked } from '../controller';
import { copySelection, deleteSelection, duplicate, frameSelection, groupIntoFrame, pasteFromClipboard, reorder, ungroup } from '../actions';
import { createNote } from './TextLayer';
import { MOD } from '../keys';

interface Item { label: string; hint?: string; run?: () => void; sub?: boolean; sep?: boolean }

/** Right-click. On nodes: chat, out (copy file, open in, reveal, export), edit, arrange and
 *  view actions; on empty canvas: paste, note, import, zoom, undo. Every row shows its key
 *  (everything else is in ⌘K). */
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

  const at = menu.at ?? null;
  const m = MOD;
  const containers = sel.some((n) => n.kind === 'frame' || n.kind === 'group');
  const items: Item[] = ids.length
    ? [
        { label: 'ask about it…', hint: '/', run: go(openPrompt) },
        { label: 'add to chat', hint: `${m}L`, run: go(addSelectionToChat) },
        ...(media.length
          ? [
              { label: desk ? (media.length > 1 ? `copy ${media.length} files` : 'copy file') : 'copy link', hint: `${m}⇧C`, run: go(() => void copyFiles(ids)), sep: true },
              ...(image ? [{ label: 'copy image', run: go(() => void copyImage(ids[0])) }] : []),
              desk ? { label: 'open in', sub: true } : { label: 'download', run: go(() => void download(ids)) },
              ...(desk ? [{ label: 'reveal in finder', run: go(() => void reveal(ids)) }] : []),
            ]
          : []),
        { label: 'export…', hint: `${m}⇧E`, run: go(openExport), sep: !media.length },
        { label: final ? 'unmark final' : 'mark final', run: go(() => toggleFinal(ids)) },
        { label: 'copy', hint: `${m}C`, run: go(() => void copySelection()), sep: true },
        { label: 'paste', hint: `${m}V`, run: go(() => void pasteFromClipboard(at)) },
        { label: 'duplicate', hint: `${m}D`, run: go(duplicate) },
        { label: 'delete', hint: '⌫', run: go(deleteSelection) },
        { label: 'group', hint: `${m}G`, run: go(groupIntoFrame), sep: true },
        { label: 'frame selection', hint: `${m}⌥G`, run: go(frameSelection) },
        ...(containers ? [{ label: 'ungroup', hint: `${m}⇧G`, run: go(ungroup) }] : []),
        { label: 'bring forward', hint: `${m}]`, run: go(() => reorder(1)) },
        { label: 'send backward', hint: `${m}[`, run: go(() => reorder(-1)) },
        { label: 'zoom to selection', hint: '⇧2', run: go(() => getState().app?.zoomToNodes(ids)), sep: true },
        { label: 'show lineage', run: go(toggleLineage) },
        ...(media.length === 1 && promptOf(media[0].id) ? [{ label: 'rerun with changes…', run: go(openPrompt) }] : []),
        { label: 'more…', hint: `${m}K`, run: go(() => setState({ overlay: 'palette' })), sep: true },
      ]
    : [
        { label: 'paste', hint: `${m}V`, run: go(() => void pasteFromClipboard(at)) },
        { label: 'add note', hint: 'N', run: go(() => createNote(at ?? [getState().camera.width / 2, getState().camera.height / 2])) },
        { label: 'import files…', hint: 'drop', run: go(() => importPicked(at ?? undefined)) },
        { label: 'select all', hint: `${m}A`, run: go(() => getState().app?.setSelection(getState().nodes.filter((n) => !n.parent).map((n) => n.id))), sep: true },
        { label: 'zoom to fit', hint: '⇧1', run: go(() => getState().app?.fitAll()) },
        { label: 'zoom to 100%', hint: `${m}0`, run: go(() => getState().app?.zoomTo(1)) },
        { label: 'undo', hint: `${m}Z`, run: go(() => getState().app?.undo()), sep: true },
        { label: 'redo', hint: `${m}⇧Z`, run: go(() => getState().app?.redo()) },
        { label: 'keyboard shortcuts', hint: '?', run: go(() => setState({ overlay: 'help' })), sep: true },
        { label: 'more…', hint: `${m}K`, run: go(() => setState({ overlay: 'palette' })) },
      ];

  return (
    <div className="modal menu-veil" onPointerDown={close} onContextMenu={(e) => (e.preventDefault(), close())}>
      <div ref={box} className="menu" style={{ left: pos.x, top: pos.y }} onPointerDown={(e) => e.stopPropagation()}>
        {items.map((it) => (
          <div
            key={it.label}
            className={`menu-row ${it.sub && subOpen ? 'on' : ''} ${it.sep ? 'menu-sep' : ''}`}
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
 *  of the selection) and open the node menu; on empty canvas, the canvas menu. `at` is the
 *  canvas point (paste / new note land there). */
export function openMenuAt(x: number, y: number, node: string | null, at?: [number, number]) {
  const { app, selection } = getState();
  if (!app) return;
  if (!node) {
    if (selection.length) app.setSelection([]);
    setState({ menu: { x, y, ids: [], at } });
    return;
  }
  const ids = selection.includes(node) ? selection : [node];
  if (ids !== selection) app.setSelection(ids);
  setState({ menu: { x, y, ids, at } });
}
