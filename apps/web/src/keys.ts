// Keyboard: one capture-phase handler for the whole app, plus the shortcut table used by
// the `?` overlay, the ⌘K palette and docs/SHORTCUTS.md. Canvas shortcuts never fire while
// typing (chat input, note editor, palette); a few global ones (⌘K, ⌘\, chat switching,
// ⌘⏎) work everywhere. Browsers reserve ⌘N, ⌘1…9 and ⌘⇧[ ]; each has a ⌃ alias.

import { getState, setState } from './store';
import { addSelectionToChat } from './controller';
import { hostSync } from './sync';
import { selectChat } from './chat';
import { copy, cycleSelection, duplicate, groupIntoFrame, paste, ungroup, zoomBy } from './actions';
import { createNote, editNode } from './components/TextLayer';

const isMac = navigator.platform.toLowerCase().includes('mac') || navigator.userAgent.includes('Mac');
export const MOD = isMac ? '⌘' : 'Ctrl+';

export interface Shortcut { keys: string; desc: string; group: 'canvas' | 'chat' | 'app' }

/** The shortcut list (shown by `?`, mirrored in docs/SHORTCUTS.md). */
export const SHORTCUTS: Shortcut[] = [
  { group: 'app', keys: '⌘K', desc: 'command palette: actions, chats, nodes' },
  { group: 'app', keys: '?', desc: 'this list' },
  { group: 'app', keys: '⌘\\', desc: 'show / hide the chat panel' },
  { group: 'chat', keys: '⌘L', desc: 'add the selection to the chat' },
  { group: 'chat', keys: '⌘I  or  /', desc: 'focus the chat input' },
  { group: 'chat', keys: '⏎  /  ⌘⏎', desc: 'send (⇧⏎ new line)' },
  { group: 'chat', keys: 'esc', desc: 'in the input: stop the run, else leave the input' },
  { group: 'chat', keys: '#', desc: 'in the input: add another chat as context' },
  { group: 'chat', keys: '⌘N  or  ⌃N', desc: 'new chat' },
  { group: 'chat', keys: '⌘⇧[ ⌘⇧]  or  ⌃⇧[ ⌃⇧]', desc: 'previous / next chat' },
  { group: 'chat', keys: '⌘1…9  or  ⌃1…9', desc: 'switch to chat 1…9' },
  { group: 'chat', keys: '⌥ click / ⌥ drag', desc: 'add a spot / region (video: frame) on media' },
  { group: 'canvas', keys: 'V', desc: 'select tool' },
  { group: 'canvas', keys: 'H  or  hold space', desc: 'pan' },
  { group: 'canvas', keys: 'N  or  double-click', desc: 'new note at the cursor (editing)' },
  { group: 'canvas', keys: '⏎', desc: 'edit the selected note / zoom to selected media' },
  { group: 'canvas', keys: 'tab  ⇧tab', desc: 'select next / previous node (reading order)' },
  { group: 'canvas', keys: '⌘A', desc: 'select all' },
  { group: 'canvas', keys: 'esc', desc: 'deselect' },
  { group: 'canvas', keys: '⌘D', desc: 'duplicate' },
  { group: 'canvas', keys: '⌘G  ⌘⇧G', desc: 'group / ungroup (children keep their place)' },
  { group: 'canvas', keys: 'double-click  esc', desc: 'enter a group / back up a level' },
  { group: 'canvas', keys: '⌘ click', desc: 'select inside groups directly' },
  { group: 'app', keys: '⌘⇧\\', desc: 'show / hide the outline (frames and groups)' },
  { group: 'canvas', keys: '⌘C  ⌘V', desc: 'copy / paste nodes; paste images and files as media' },
  { group: 'canvas', keys: '⌫', desc: 'delete' },
  { group: 'canvas', keys: 'arrows  ⇧arrows', desc: 'nudge 1 px / 10 px' },
  { group: 'canvas', keys: '⌘Z  ⌘⇧Z', desc: 'undo / redo (your own changes)' },
  { group: 'canvas', keys: '⇧1  ⇧2', desc: 'fit all / zoom to selection' },
  { group: 'canvas', keys: '⌘0  ⌘+  ⌘−', desc: '100% / zoom in / zoom out' },
  { group: 'canvas', keys: 'drop files', desc: 'import (uploaded to the host)' },
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
  setState({ chatOpen: true });
  const sync = hostSync();
  if (sync) sync.newChat();
  else setState((s) => ({ chat: selectChat(s.chat, null) }));
  focusChat();
}

export function switchChat(i: number | 'prev' | 'next') {
  const { chat } = getState();
  const list = chat.order;
  if (!list.length) return;
  const cur = chat.active ? list.indexOf(chat.active) : list.length;
  const idx = i === 'prev' ? Math.max(0, cur - 1) : i === 'next' ? Math.min(list.length - 1, cur + 1) : i;
  const id = list[idx];
  if (id) setState((s) => ({ chatOpen: true, chat: selectChat(s.chat, id) }));
}

export function toggleOutline() {
  setState((s) => {
    const open = !(s.outlineOpen ?? s.nodes.some((n) => n.kind === 'frame' || n.kind === 'group'));
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

    // Global (also while typing).
    if (mod && !e.shiftKey && (k === 'k' || k === 'K')) return done(), setState({ overlay: overlay === 'palette' ? null : 'palette' });
    if (mod && e.shiftKey && (k === '\\' || k === '|')) return done(), toggleOutline();
    if (mod && k === '\\') return done(), toggleChat();
    if ((mod || ctrl) && !e.shiftKey && (k === 'n' || k === 'N')) return done(), newChat();
    if ((mod || ctrl) && e.shiftKey && (k === '[' || k === '{')) return done(), switchChat('prev');
    if ((mod || ctrl) && e.shiftKey && (k === ']' || k === '}')) return done(), switchChat('next');
    if ((mod || ctrl) && !e.shiftKey && !e.altKey && /^[1-9]$/.test(k)) return done(), switchChat(Number(k) - 1);
    if (mod && !e.shiftKey && (k === 'i' || k === 'I')) return done(), focusChat();
    if (overlay) return; // the palette / help own the keyboard
    if (editable(e.target) || getState().editing) return;

    // Canvas.
    if (!app) return;
    if (k === '?' || (k === '/' && e.shiftKey)) return done(), setState({ overlay: 'help' });
    if (k === '/' && !mod) return done(), focusChat();
    if (mod && (k === 'l' || k === 'L')) return done(), addSelectionToChat();
    if (mod && (k === 'd' || k === 'D')) return done(), duplicate();
    if (mod && (k === 'g' || k === 'G')) return done(), e.shiftKey ? ungroup() : groupIntoFrame();
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
  const ku = (e: KeyboardEvent) => {
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
  document.addEventListener('copy', onCopy);
  document.addEventListener('paste', onPaste);
  return () => {
    window.removeEventListener('keydown', kd, true);
    window.removeEventListener('keyup', ku);
    document.removeEventListener('copy', onCopy);
    document.removeEventListener('paste', onPaste);
  };
}
