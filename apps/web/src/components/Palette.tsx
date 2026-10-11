import { useEffect, useMemo, useRef, useState } from 'react';
import { getState, setState, useStore } from '../store';
import { chatTitle } from '../chat';
import { openChat, setDriver, toggleHistory } from '../chatActions';
import { setReduceMotion } from '../activity';
import { openPrompt } from './SelectionPrompt';
import { focusNode, addSelectionToChat } from '../controller';
import { cycleSelection, duplicate, frameSelection, groupIntoFrame, ungroup, zoomBy } from '../actions';
import { createNote } from './TextLayer';
import { focusChat, newChat, SHORTCUTS, toggleChat, toggleOutline } from '../keys';
import { byRecent, openProject } from '../projects';
import { outActions } from '../outActions';

interface Entry { label: string; hint: string; run: () => void }

/** Subsequence match score (higher is better; -1 = no match). */
export function fuzzy(q: string, s: string): number {
  if (!q) return 0;
  const t = s.toLowerCase();
  let i = 0, score = 0, last = -2;
  for (const ch of q.toLowerCase()) {
    const j = t.indexOf(ch, i);
    if (j < 0) return -1;
    score += j === last + 1 ? 3 : j === 0 || /[\s/_-]/.test(t[j - 1]) ? 2 : 1;
    last = j;
    i = j + 1;
  }
  return score - t.length * 0.01;
}

function actions(): Entry[] {
  const app = () => getState().app;
  const center = (): [number, number] => {
    const c = getState().camera;
    return [c.width / 2, c.height / 2];
  };
  return [
    { label: 'new chat', hint: '⌘N', run: newChat },
    { label: 'switch project…', hint: '⌘O', run: () => setState({ overlay: 'projects' }) },
    { label: 'new project…', hint: 'project', run: () => setState({ overlay: 'projects' }) },
    { label: 'connect agent (Claude Code, Codex, Cursor, …)', hint: 'mcp', run: () => setState({ overlay: 'connect' }) },
    { label: 'ask about the selection', hint: '/', run: openPrompt },
    { label: 'all chats (history)', hint: '⌘Y', run: toggleHistory },
    { label: `agent for next message: next (now ${getState().driver})`, hint: '⌘.', run: () => setDriver() },
    { label: 'toggle layers panel', hint: '⌘⇧\\', run: toggleOutline },
    { label: `reduce motion: ${getState().reduceMotion ? 'turn off' : 'turn on'}`, hint: 'setting', run: () => setReduceMotion(!getState().reduceMotion) },
    { label: 'frame selection', hint: '⌘⌥G', run: frameSelection },
    { label: 'focus chat input', hint: '⌘I', run: focusChat },
    { label: 'toggle chat panel', hint: '⌘\\', run: toggleChat },
    { label: 'add selection to chat', hint: '⌘L', run: addSelectionToChat },
    { label: 'new note', hint: 'N', run: () => createNote(center()) },
    { label: 'duplicate', hint: '⌘D', run: duplicate },
    { label: 'group selection', hint: '⌘G', run: groupIntoFrame },
    { label: 'ungroup', hint: '⌘⇧G', run: ungroup },
    { label: 'select all', hint: '⌘A', run: () => app()?.setSelection(getState().nodes.map((n) => n.id)) },
    { label: 'select next node', hint: 'tab', run: () => cycleSelection(1) },
    { label: 'fit all', hint: '⇧1', run: () => app()?.fitAll() },
    { label: 'zoom to selection', hint: '⇧2', run: () => app()?.zoomToNodes(getState().selection) },
    { label: 'zoom to 100%', hint: '⌘0', run: () => app()?.zoomTo(1) },
    { label: 'zoom in', hint: '⌘+', run: () => zoomBy(1.25) },
    { label: 'zoom out', hint: '⌘−', run: () => zoomBy(0.8) },
    { label: 'undo', hint: '⌘Z', run: () => app()?.undo() },
    { label: 'redo', hint: '⌘⇧Z', run: () => app()?.redo() },
    { label: 'keyboard shortcuts', hint: '?', run: () => setState({ overlay: 'help' }) },
    ...outActions(),
  ];
}

/** ⌘K: one fuzzy list over actions, chats and nodes (by name). */
export function Palette() {
  const nodes = useStore((s) => s.nodes);
  const chat = useStore((s) => s.chat);
  const projects = useStore((s) => s.projects);
  const project = useStore((s) => s.project);
  const [q, setQ] = useState('');
  const [sel, setSel] = useState(0);
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => input.current?.focus(), []);

  const entries = useMemo(() => {
    const all: Entry[] = [
      ...actions(),
      ...byRecent(projects).map((p) => ({ label: `${p.name}${p.fal_project ? ' · fal' : ''}`, hint: p.id === project ? 'project · open' : 'project', run: () => openProject(p.id) })),
      ...[...chat.order].reverse().map((id) => ({ label: `#${chatTitle(chat, id)}`, hint: 'chat', run: () => openChat(id) })),
      ...nodes
        .filter((n) => n.kind !== 'connector')
        .map((n) => ({ label: n.name || (n.text ?? '').slice(0, 40) || n.id, hint: `${n.kind} ${n.id}`, run: () => focusNode(n.id) })),
    ];
    if (!q) return all.slice(0, 50);
    return all
      .map((e) => ({ e, s: Math.max(fuzzy(q, e.label), fuzzy(q, e.hint) - 1) }))
      .filter((x) => x.s >= 0)
      .sort((a, b) => b.s - a.s)
      .slice(0, 50)
      .map((x) => x.e);
  }, [q, nodes, chat, projects, project]);

  const close = () => setState({ overlay: null });
  const run = (e?: Entry) => {
    close();
    e?.run();
  };

  return (
    <div className="modal" onPointerDown={close}>
      <div className="palette" onPointerDown={(e) => e.stopPropagation()}>
        <div className="prompt">
          <span className="dim">&gt;</span>
          <input
            ref={input}
            value={q}
            spellCheck={false}
            onChange={(e) => {
              setQ(e.target.value);
              setSel(0);
            }}
            onKeyDown={(e) => {
              if (e.key === 'Escape') close();
              else if (e.key === 'ArrowDown' || (e.key === 'Tab' && !e.shiftKey)) {
                e.preventDefault();
                setSel((s) => Math.min(entries.length - 1, s + 1));
              } else if (e.key === 'ArrowUp' || (e.key === 'Tab' && e.shiftKey)) {
                e.preventDefault();
                setSel((s) => Math.max(0, s - 1));
              } else if (e.key === 'Enter') {
                e.preventDefault();
                run(entries[sel]);
              }
            }}
          />
        </div>
        <div className="palette-list">
          {entries.map((e, i) => (
            <div key={`${e.label}-${e.hint}-${i}`} className={`palette-row ${i === sel ? 'on' : ''}`} onMouseMove={() => setSel(i)} onClick={() => run(e)}>
              <span className="palette-label">{e.label}</span>
              <span className="dim">{e.hint}</span>
            </div>
          ))}
          {!entries.length && <div className="faint">no match</div>}
        </div>
      </div>
    </div>
  );
}

/** `?`: every shortcut, mono, grouped. */
export function Help() {
  useEffect(() => {
    const kd = (e: KeyboardEvent) => {
      if (e.key === 'Escape' || e.key === '?' || (e.key === '/' && e.shiftKey)) {
        e.preventDefault();
        setState({ overlay: null });
      }
    };
    window.addEventListener('keydown', kd);
    return () => window.removeEventListener('keydown', kd);
  }, []);
  const groups: ('app' | 'chat' | 'canvas' | 'out')[] = ['app', 'chat', 'canvas', 'out'];
  return (
    <div className="modal" onPointerDown={() => setState({ overlay: null })}>
      <div className="help" onPointerDown={(e) => e.stopPropagation()}>
        {groups.map((g) => (
          <div key={g} className="help-group">
            <div className="dim">{g}</div>
            {SHORTCUTS.filter((s) => s.group === g).map((s) => (
              <div key={s.keys + s.desc} className="help-row">
                <span className="help-keys">{s.keys}</span>
                <span>{s.desc}</span>
              </div>
            ))}
          </div>
        ))}
        <div className="faint">esc to close · ⌘ = ctrl on windows/linux</div>
      </div>
    </div>
  );
}
