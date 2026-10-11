// Keyboard: one capture-phase handler for the whole app, plus the shortcut table used by
// the `?` overlay, the ⌘K palette and docs/SHORTCUTS.md. Canvas shortcuts never fire while
// typing (chat input, note editor, palette); a few global ones (⌘K, ⌘\, chat switching,
// ⌘⏎) work everywhere. Browsers reserve ⌘N, ⌘1…9 and ⌘⇧[ ]; each has a ⌃ alias.

import { getState, setState } from './store';
import { addSelectionToChat } from './controller';
import { hostSync } from './sync';
import { selectChat, tabs } from './chat';
import { copy, cycleSelection, duplicate, frameSelection, groupIntoFrame, paste, ungroup, zoomBy } from './actions';
import { setDriver, toggleHistory } from './chatActions';
import { openPrompt } from './components/SelectionPrompt';
import { createNote, editNode } from './components/TextLayer';
import { openPicker } from './components/Projects';

const isMac = navigator.platform.toLowerCase().includes('mac') || navigator.userAgent.includes('Mac');
export const MOD = isMac ? '⌘' : 'Ctrl+';

export interface Shortcut { keys: string; desc: string; group: 'canvas' | 'chat' | 'app' }

/** The shortcut list (shown by `?`, mirrored in docs/SHORTCUTS.md). */
export const SHORTCUTS: Shortcut[] = [
  { group: 'app', keys: '⌘K', desc: 'command palette: actions, projects, chats, nodes' },
  { group: 'app', keys: '⌘O', desc: 'projects: ⏎ open, n new, r rename, ⌘D duplicate, d delete' },
  { group: 'app', keys: '?', desc: 'this list' },
  { group: 'app', keys: '⌘\\', desc: 'show / hide the chat panel' },
  { group: 'app', keys: '⌘⇧\\', desc: 'show / hide layers and properties' },
  { group: 'chat', keys: '/  or  click [✦]', desc: 'with a selection: ask about it (inline prompt)' },
  { group: 'chat', keys: '⏎  ⌘⏎  ⇥', desc: 'in the ✦ prompt: new chat / current chat / open in chat' },
  { group: 'chat', keys: '⌘L', desc: 'add the selection to the chat' },
  { group: 'chat', keys: '⌘I  or  /', desc: 'focus the chat input (no selection)' },
  { group: 'chat', keys: '⏎', desc: 'send (⇧⏎ new line); while running it queues (after the next step)' },
  { group: 'chat', keys: '⌘⏎', desc: 'while running: send now (steer); on a queued message: send it now' },
  { group: 'chat', keys: 'esc', desc: 'in the input: stop the run, else leave the input' },
  { group: 'chat', keys: '@  #', desc: 'in the input: mention a node / another chat' },
  { group: 'chat', keys: 'drop or paste', desc: 'images and files on the chat: import + attach' },
  { group: 'chat', keys: '1…9', desc: 'answer an agent question (empty input)' },
  { group: 'chat', keys: '⌘.', desc: 'agent for the next message: opus / fal / codex' },
  { group: 'chat', keys: '⌘N  or  ⌃N', desc: 'new chat' },
  { group: 'chat', keys: '⌘Y', desc: 'all chats (history): ⏎ open, d delete, a archive' },
  { group: 'chat', keys: '⌘⇧[ ⌘⇧]  or  ⌃⇧[ ⌃⇧]', desc: 'previous / next tab' },
  { group: 'chat', keys: '⌘1…9  or  ⌃1…9', desc: 'switch to tab 1…9 (hold ⌘ to see numbers)' },
  { group: 'chat', keys: '⌘⌫', desc: 'on a focused tab: delete the chat (asks y/n)' },
  { group: 'chat', keys: '⌥ click / ⌥ drag', desc: 'add a spot / region (video: frame) on media' },
  { group: 'canvas', keys: 'V', desc: 'select tool' },
  { group: 'canvas', keys: 'H  or  hold space', desc: 'pan' },
  { group: 'canvas', keys: 'N  or  double-click', desc: 'new note at the cursor (editing)' },
  { group: 'canvas', keys: '⏎', desc: 'edit the selected note / zoom to selected media' },
  { group: 'canvas', keys: 'tab  ⇧tab', desc: 'select next / previous node (reading order)' },
  { group: 'canvas', keys: '⌘A', desc: 'select all' },
  { group: 'canvas', keys: 'esc', desc: 'deselect' },
  { group: 'canvas', keys: '⌘D', desc: 'duplicate' },
  { group: 'canvas', keys: '⌘G  ⌘⌥G  ⌘⇧G', desc: 'group / frame the selection / ungroup' },
  { group: 'canvas', keys: 'double-click  esc', desc: 'enter a group / back up a level' },
  { group: 'canvas', keys: '⌘ click', desc: 'select inside groups directly' },
  { group: 'canvas', keys: 'drag onto the chat', desc: 'attach the dragged node as a chip' },
  { group: 'canvas', keys: '⌘C  ⌘V', desc: 'copy / paste nodes; paste images and files as media' },
  { group: 'canvas', keys: '⌫', desc: 'delete' },
  { group: 'canvas', keys: 'arrows  ⇧arrows', desc: 'nudge 1 px / 10 px' },
  { group: 'canvas', keys: '⌘Z  ⌘⇧Z', desc: 'undo / redo (your own changes)' },
  { group: 'canvas', keys: '⇧1  ⇧2', desc: 'fit all / zoom to selection' },
  { group: 'canvas', keys: '⌘0  ⌘+  ⌘−', desc: '100% / zoom in / zoom out' },
  { group: 'canvas', keys: 'drop files', desc: 'import (uploaded to the host)' },
  { group: 'canvas', keys: '⌘F', desc: 'in layers: filter' },
];

export function editable(t: EventTarget | null) {
  const el = t as HTMLElement | null;
  return !!el && (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA' || el.tagName === 'SELECT' || el.isContentEditable);
}

// ---- actions shared by keys and the palette ----------------------------------------------

export function focusChat() {
  setState({ chatOpen: true });
  requestAnimationFrame(() => window.dispatchEvent(new Event('falos:focus-chat')));
}

export function newChat() {
  setState({ chatOpen: true, chatView: 'chat', subView: null });
  const sync = hostSync();
  if (sync) sync.newChat();
  else setState((s) => ({ chat: selectChat(s.chat, null) }));
  focusChat();
}

export function switchChat(i: number | 'prev' | 'next') {
  const { chat } = getState();
  const list = tabs(chat);
  if (!list.length) return;
  const cur = chat.active ? list.indexOf(chat.active) : list.length;
  const idx = i === 'prev' ? Math.max(0, cur - 1) : i === 'next' ? Math.min(list.length - 1, cur + 1) : i;
  const id = list[idx];
  if (id) setState((s) => ({ chatOpen: true, chatView: 'chat', subView: null, chat: selectChat(s.chat, id) }));
}

export function toggleOutline() {
  setState((s) => {
    const open = !(s.outlineOpen ?? s.nodes.length > 0);
    try {
      localStorage.setItem('falos.outline', open ? '1' : '0');
    } catch {
      /* private mode */
    }
    return { outlineOpen: open };
  });
}

export function toggleChat() {
  setState((s) => ({ chatOpen: !s.chatOpen }));
}

/** Enter on the canvas: edit a note, or zoom to the selected media. */
function enter(): boolean {
  const { app, selection, nodes } = getState();
  if (!app || selection.length !== 1) return false;
  const n = nodes.find((x) => x.id === selection[0]);
  if (!n) return false;
  if (n.kind === 'note' || n.kind === 'text') editNode(n.id);
  else app.zoomToNodes([n.id]);
  return true;
}

/** Last pointer position over the canvas (CSS px relative to it), for keyboard actions at
 *  the cursor. Set by the stage. */
export const pointer: { at: [number, number] | null } = { at: null };

function atCursor(): [number, number] {
  const c = getState().camera;
  return pointer.at ?? [c.width / 2, c.height / 2];
}

// ---- the handler ---------------------------------------------------------------------------

export function installKeys(): () => void {
  const kd = (e: KeyboardEvent) => {
    const { app, overlay } = getState();
    const mod = isMac ? e.metaKey : e.ctrlKey;
    const ctrl = e.ctrlKey && !e.metaKey;
    const k = e.key;
    const done = () => {
      e.preventDefault();
      e.stopPropagation();
    };

    if (k === (isMac ? 'Meta' : 'Control') && !getState().modHeld) setState({ modHeld: true });
    // Global (also while typing).
    if (mod && !e.shiftKey && (k === 'k' || k === 'K')) return done(), setState({ overlay: overlay === 'palette' ? null : 'palette' });
    if (mod && !e.shiftKey && !e.altKey && (k === 'o' || k === 'O')) return done(), openPicker();
    if (mod && e.shiftKey && (k === '\\' || k === '|')) return done(), toggleOutline();
    if (mod && k === '\\') return done(), toggleChat();
    if ((mod || ctrl) && !e.shiftKey && (k === 'n' || k === 'N')) return done(), newChat();
    if ((mod || ctrl) && e.shiftKey && (k === '[' || k === '{')) return done(), switchChat('prev');
    if ((mod || ctrl) && e.shiftKey && (k === ']' || k === '}')) return done(), switchChat('next');
    if ((mod || ctrl) && !e.shiftKey && !e.altKey && /^[1-9]$/.test(k)) return done(), switchChat(Number(k) - 1);
    if (mod && !e.shiftKey && (k === 'i' || k === 'I')) return done(), focusChat();
    if ((mod || ctrl) && !e.shiftKey && (k === 'y' || k === 'Y')) return done(), toggleHistory();
    if (mod && k === '.') return done(), setDriver();
    if (overlay) return; // the palette / help own the keyboard
    if (editable(e.target) || getState().editing) return;

    // Canvas.
    if (!app) return;
    if (k === '?' || (k === '/' && e.shiftKey)) return done(), setState({ overlay: 'help' });
    if (k === '/' && !mod) return done(), getState().selection.length ? openPrompt() : focusChat();
    if (mod && (k === 'l' || k === 'L')) return done(), addSelectionToChat();
    if (mod && (k === 'd' || k === 'D')) return done(), duplicate();
    if (mod && e.code === 'KeyG') return done(), e.shiftKey ? ungroup() : e.altKey ? frameSelection() : groupIntoFrame();
    if (mod && k === '0') return done(), app.zoomTo(1);
    if (mod && (k === '=' || k === '+')) return done(), zoomBy(1.25);
    if (mod && (k === '-' || k === '_')) return done(), zoomBy(0.8);
    if (mod && k === 'Enter') return done(), focusChat();
    if (!mod && !e.altKey && (k === 'n' || k === 'N')) return done(), createNote(atCursor());
    if (!mod && e.shiftKey && (k === '@' || k === '2')) {
      const sel = getState().selection;
      return done(), app.zoomToNodes(sel.length ? sel : getState().nodes.map((n) => n.id));
    }
    if (k === 'Tab' && !mod) return done(), cycleSelection(e.shiftKey ? -1 : 1);
    if (k === 'Enter' && !mod && enter()) return done();
    if (app.keyDown(k, e.shiftKey, e.altKey, mod)) done();
  };
  const blur = () => getState().modHeld && setState({ modHeld: false });
  const ku = (e: KeyboardEvent) => {
    if (e.key === (isMac ? 'Meta' : 'Control')) blur();
    if (!editable(e.target)) getState().app?.keyUp(e.key);
  };
  const onCopy = (e: ClipboardEvent) => {
    if (!editable(e.target) && !getState().editing && !getState().overlay) copy(e);
  };
  const onPaste = (e: ClipboardEvent) => {
    if (!editable(e.target) && !getState().editing && !getState().overlay) void paste(e, pointer.at);
  };
  window.addEventListener('keydown', kd, true);
  window.addEventListener('keyup', ku);
  window.addEventListener('blur', blur);
  document.addEventListener('copy', onCopy);
  document.addEventListener('paste', onPaste);
  return () => {
    window.removeEventListener('keydown', kd, true);
    window.removeEventListener('keyup', ku);
    window.removeEventListener('blur', blur);
    document.removeEventListener('copy', onCopy);
    document.removeEventListener('paste', onPaste);
  };
}
