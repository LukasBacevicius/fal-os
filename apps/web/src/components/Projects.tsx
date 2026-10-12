import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { getState, setState, useStore } from '../store';
import { isRunning } from '../chat';
import {
  byRecent, createProject, deleteProject, duplicateProject, openProject, renameProject, type ProjectInfo,
} from '../projects';
import { ago } from './ChatTabs';
import { fuzzy } from './Palette';
import { MOD } from '../keys';
import { WorkingMark } from './Mark';

export const openPicker = () => setState((s) => ({ overlay: s.overlay === 'projects' ? null : 'projects' }));

/** Running chats in a project: live from the chat list for the open one, else the host's
 *  count. */
function useRunning() {
  const chat = useStore((s) => s.chat);
  const project = useStore((s) => s.project);
  const local = useMemo(() => chat.order.filter((id) => isRunning(chat, id)).length, [chat]);
  return (p: ProjectInfo) => (p.id === project ? local : p.running);
}

/** The project name (left panel header, or top-left of the canvas without the panel), with a
 *  dim `· fal` when it's linked to a fal Agent project. Click or ⌘O opens the picker. */
export function ProjectHeader({ floating = false }: { floating?: boolean }) {
  const projects = useStore((s) => s.projects);
  const id = useStore((s) => s.project);
  const workspace = useStore((s) => s.workspace);
  const conn = useStore((s) => s.conn);
  const p = projects.find((x) => x.id === id);
  // Older host (no /workspace) or local-only: nothing to switch, keep the chrome quiet.
  if (!p && (workspace !== 'connected' || conn === 'local')) {
    return floating ? null : <span className="dim project-head"><WorkingMark />layers</span>;
  }
  return (
    <button className="link project-head" onClick={openPicker} title={`switch project (${MOD}O)`}>
      <WorkingMark />
      <span className="project-name">{p?.name ?? '…'}</span>
      {p?.fal_project && <span className="faint" title={`linked to fal project ${p.fal_project}`}>{'\u00a0· fal'}</span>}
      <span className="faint">{'\u00a0▾'}</span>
    </button>
  );
}

type Mode = { kind: 'list' } | { kind: 'new' } | { kind: 'rename'; id: string } | { kind: 'confirm'; id: string };

/** ⌘O: every project, recent first. ↑↓ move, ⏎ open; with an empty search: n new, r rename,
 *  d delete (inline y/n; it goes to the trash); ⌘D duplicate; esc closes. */
export function ProjectPicker() {
  const projects = useStore((s) => s.projects);
  const current = useStore((s) => s.project);
  const workspace = useStore((s) => s.workspace);
  const running = useRunning();
  const [q, setQ] = useState('');
  const [sel, setSel] = useState(0);
  const [mode, setMode] = useState<Mode>({ kind: 'list' });
  const input = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);

  const rows = useMemo(() => {
    const all = byRecent(projects);
    if (!q) return all;
    return all
      .map((p) => ({ p, s: fuzzy(q, p.name) }))
      .filter((x) => x.s >= 0)
      .sort((a, b) => b.s - a.s)
      .map((x) => x.p);
  }, [projects, q]);
  const cur = rows[Math.min(sel, rows.length - 1)];
  // Keep the selected project selected when the list reorders (duplicate, touch, rename).
  const curId = useRef<string | null>(null);
  useLayoutEffect(() => {
    const i = curId.current ? rows.findIndex((p) => p.id === curId.current) : -1;
    if (i >= 0 && i !== sel) setSel(i);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [rows]);
  useEffect(() => {
    curId.current = cur?.id ?? null;
  }, [cur?.id]);

  useEffect(() => {
    if (mode.kind === 'list') input.current?.focus();
  }, [mode.kind]);

  // Start on the open project.
  useEffect(() => {
    const i = rows.findIndex((p) => p.id === getState().project);
    if (i > 0) setSel(i);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useLayoutEffect(() => {
    listRef.current?.querySelector<HTMLElement>('.proj-row.on')?.scrollIntoView({ block: 'nearest' });
  }, [sel, rows.length]);

  // Inline delete confirm owns the keyboard.
  useEffect(() => {
    if (mode.kind !== 'confirm') return;
    const id = mode.id;
    const key = (e: KeyboardEvent) => {
      const k = e.key.toLowerCase();
      if (k === 'y' || k === 'enter') {
        deleteProject(id);
        setMode({ kind: 'list' });
      } else if (k === 'n' || k === 'escape') setMode({ kind: 'list' });
      else return;
      e.preventDefault();
      e.stopPropagation();
    };
    window.addEventListener('keydown', key, true);
    return () => window.removeEventListener('keydown', key, true);
  }, [mode]);

  const close = () => setState({ overlay: null });
  const open = (p?: ProjectInfo) => {
    if (!p) return;
    close();
    openProject(p.id);
  };

  const onKey = (e: React.KeyboardEvent<HTMLInputElement>) => {
    const mod = e.metaKey || e.ctrlKey;
    if (e.key === 'ArrowDown' || (e.key === 'Tab' && !e.shiftKey)) {
      e.preventDefault();
      setSel((s) => Math.min(rows.length - 1, s + 1));
    } else if (e.key === 'ArrowUp' || (e.key === 'Tab' && e.shiftKey)) {
      e.preventDefault();
      setSel((s) => Math.max(0, s - 1));
    } else if (e.key === 'Enter') {
      e.preventDefault();
      if (cur) open(cur);
      else if (q.trim()) {
        // No match: Enter creates a project with that name.
        close();
        void createProject(q.trim());
      }
    } else if (e.key === 'Escape') {
      e.preventDefault();
      if (q) setQ('');
      else close();
    } else if (mod && (e.key === 'd' || e.key === 'D')) {
      e.preventDefault();
      if (cur) duplicateProject(cur.id);
    } else if (mod && (e.key === 'o' || e.key === 'O')) {
      e.preventDefault();
      close();
    } else if (!q && !mod && !e.altKey) {
      // With an empty search, single letters act on the list.
      if (e.key === 'n') {
        e.preventDefault();
        setMode({ kind: 'new' });
      } else if (e.key === 'r' && cur) {
        e.preventDefault();
        setMode({ kind: 'rename', id: cur.id });
      } else if (e.key === 'd' && cur) {
        e.preventDefault();
        setMode({ kind: 'confirm', id: cur.id });
      }
    }
  };

  const nameInput = (initial: string, commit: (v: string) => void) => (
    <input
      className="proj-input"
      autoFocus
      defaultValue={initial}
      spellCheck={false}
      onFocus={(e) => e.currentTarget.select()}
      onKeyDown={(e) => {
        e.stopPropagation();
        if (e.key === 'Enter') {
          e.preventDefault();
          commit(e.currentTarget.value);
        } else if (e.key === 'Escape') {
          e.preventDefault();
          setMode({ kind: 'list' });
        }
      }}
      onBlur={() => setMode({ kind: 'list' })}
    />
  );

  const unavailable = workspace !== 'connected';

  return (
    <div className="modal" onPointerDown={close}>
      <div className="palette projects" onPointerDown={(e) => e.stopPropagation()}>
        <div className="prompt">
          <span className="dim">/</span>
          <input
            ref={input}
            value={q}
            placeholder="search projects"
            spellCheck={false}
            disabled={mode.kind !== 'list'}
            onChange={(e) => {
              setQ(e.target.value);
              setSel(0);
            }}
            onKeyDown={onKey}
          />
        </div>
        <div className="palette-list" ref={listRef}>
          {unavailable && <div className="faint proj-note">this host has a single canvas · projects need a newer fal-os host</div>}
          {rows.map((p, i) => {
            const n = running(p);
            const on = i === Math.min(sel, rows.length - 1) && mode.kind === 'list';
            if (mode.kind === 'rename' && mode.id === p.id) {
              return (
                <div key={p.id} className="proj-row editing">
                  <span className="proj-mark">{p.id === current ? '●' : ' '}</span>
                  {nameInput(p.name, (v) => {
                    renameProject(p.id, v);
                    setMode({ kind: 'list' });
                  })}
                </div>
              );
            }
            if (mode.kind === 'confirm' && mode.id === p.id) {
              return (
                <div key={p.id} className="proj-row confirming">
                  <span className="proj-mark"> </span>
                  <span className="palette-label">delete '{p.name}'? it moves to the trash</span>
                  <span>
                    <button className="link" onClick={() => { deleteProject(p.id); setMode({ kind: 'list' }); }}>[y</button>/
                    <button className="link" onClick={() => setMode({ kind: 'list' })}>n]</button>
                  </span>
                </div>
              );
            }
            return (
              <div key={p.id} className={`palette-row proj-row ${on ? 'on' : ''}`} onMouseMove={() => mode.kind === 'list' && setSel(i)} onClick={() => open(p)}>
                <span className="proj-mark">{p.id === current ? '●' : ' '}</span>
                <span className="palette-label">
                  {p.name}
                  {p.fal_project && <span className="dim"> · fal</span>}
                </span>
                {n > 0 && <span title={`${n} running chat${n === 1 ? '' : 's'}`}>*{n}</span>}
                <span className="dim proj-ago">{ago(p.updated)}</span>
              </div>
            );
          })}
          {mode.kind === 'new' ? (
            <div className="proj-row editing">
              <span className="proj-mark">+</span>
              {nameInput('untitled', (v) => {
                setMode({ kind: 'list' });
                close();
                void createProject(v);
              })}
            </div>
          ) : (
            !unavailable && (
              <div className="palette-row proj-row dim" onClick={() => setMode({ kind: 'new' })}>
                <span className="proj-mark">+</span>
                <span className="palette-label">new project{q && !rows.length ? ` '${q}' (⏎)` : ''}</span>
                <span className="faint">n</span>
              </div>
            )
          )}
        </div>
        <div className="proj-keys faint">⏎ open · n new · r rename · {MOD}D duplicate · d delete · esc</div>
      </div>
    </div>
  );
}
