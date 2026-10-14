// Keyboard: one capture-phase handler for the whole app, plus the shortcut table used by
// the `?` overlay, the ⌘K palette and docs/SHORTCUTS.md. Canvas shortcuts never fire while
// typing (chat input, note editor, palette); a few global ones (⌘K, ⌘\, chat switching,
// ⌘⏎) work everywhere. Browsers reserve ⌘N, ⌘1…9 and ⌘⇧[ ]; each has a ⌃ alias.

import { getState, setState } from './store';
import { addSelectionToChat } from './controller';
import { hostSync } from './sync';
import { isExternal, selectChat, tabs } from './chat';
import { copy, cycleSelection, duplicate, frameSelection, groupIntoFrame, paste, reorder, ungroup, zoomBy } from './actions';
import { setDriver, toggleHistory } from './chatActions';
import { openPrompt } from './components/SelectionPrompt';
import { createNote, editNode } from './components/TextLayer';
import { openPicker } from './components/Projects';
import { canvasVideoKey, focusKey, openFocus, spacePressed, spaceReleased } from './video';
import { openLibrary } from './library';
import { openPreview, previewable, previewKey } from './preview';
import { copyFiles, openExport } from './out';
import { blurApp, focusApp, installAppMessages } from './apps';
import { phys } from './hotkeys';
import { withProgress } from './progress';

const isMac = navigator.platform.toLowerCase().includes('mac') || navigator.userAgent.includes('Mac');
export const MOD = isMac ? '⌘' : 'Ctrl+';

export interface Shortcut { keys: string; desc: string; group: 'canvas' | 'chat' | 'app' | 'out' }

/** The shortcut list (shown by `?`, mirrored in docs/SHORTCUTS.md). */
export const SHORTCUTS: Shortcut[] = [
  { group: 'app', keys: '⌘K', desc: 'command palette: actions, projects, chats, nodes' },
  { group: 'app', keys: '⌘O', desc: 'projects: ⏎ open, n new, r rename, ⌘D duplicate, d delete' },
  { group: 'app', keys: '?', desc: 'this list' },
  { group: 'app', keys: '⌘\\', desc: 'show / hide the chat panel' },
  { group: 'app', keys: '⌘⇧\\', desc: 'show / hide layers and properties' },
  { group: 'app', keys: '⌘⇧A  ⌘⇧L', desc: 'asset library: panel / full screen (space look, ⏎ or drag places, f t c u l s)' },
  { group: 'chat', keys: '/  or  click [>]', desc: 'with a selection: ask about it (inline prompt)' },
  { group: 'chat', keys: '⏎  ⌘⏎  ⇥', desc: 'in the ask prompt: new chat / current chat / open in chat' },
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
  { group: 'canvas', keys: '⏎', desc: 'edit the selected note / focus view of a video / play the selected app / preview other media; on a frame or group: select its children' },
  { group: 'canvas', keys: '⇧⏎', desc: 'select the parent frame or group' },
  { group: 'canvas', keys: 'click app  esc', desc: 'app node: click (or ⏎) gives it keyboard and mouse; esc returns to the canvas' },
  { group: 'canvas', keys: 'space (tap)  K', desc: 'selected video or audio: play / pause; other media: preview (hold space to pan)' },
  { group: 'canvas', keys: 'J  L', desc: 'selected video / audio: back / forward 5 s (arrows still nudge)' },
  { group: 'canvas', keys: ',  .', desc: 'selected video: one frame back / forward (audio: 1 s)' },
  { group: 'canvas', keys: 'M', desc: 'selected video / audio: sound on / off' },
  { group: 'canvas', keys: 'double-click a video', desc: 'focus view: ← → other videos, esc closes' },
  { group: 'canvas', keys: 'double-click media', desc: 'preview: image zoom (+ − 0 1, drag pans), pdf pages (PgUp PgDn), 3d orbit, audio waveform; ← → other media, esc / space closes' },
  { group: 'canvas', keys: 'click the waveform', desc: 'selected audio: scrub (drag the label row to move the node)' },
  { group: 'canvas', keys: 'tab  ⇧tab', desc: 'select next / previous node (reading order)' },
  { group: 'canvas', keys: '⌘A', desc: 'select all' },
  { group: 'canvas', keys: 'esc', desc: 'step out: leave the text, cancel a drag, up to the parent frame / group, deselect' },
  { group: 'canvas', keys: '⌘D', desc: 'duplicate; again after moving the copy: the same offset again' },
  { group: 'canvas', keys: 'drag', desc: 'snaps to edges, centres, equal spacing (guides); hold ⌘ to move freely; ⇧ one axis; hold still ½ s for 1 px moves' },
  { group: 'canvas', keys: 'drag an edge', desc: 'resize along the whole edge (tiny nodes: no handles, they just drag); space moves the box mid-resize' },
  { group: 'canvas', keys: '⌥ hover', desc: 'with a selection: distances to the hovered node (insets inside it)' },
  { group: 'canvas', keys: '⌘G  ⌘⌥G  ⌘⇧G', desc: 'group / frame the selection / ungroup' },
  { group: 'canvas', keys: '⌘]  ⌘[', desc: 'bring forward / send backward' },
  { group: 'canvas', keys: 'double-click  esc', desc: 'enter a group / back up a level' },
  { group: 'canvas', keys: '⌘ click', desc: 'select inside groups directly' },
  { group: 'canvas', keys: 'drag onto the chat', desc: 'attach the dragged node as a chip' },
  { group: 'canvas', keys: '⌘C  ⌘V', desc: 'copy / paste nodes; paste images and files as media, Figma selections as layers' },
  { group: 'canvas', keys: '⌫', desc: 'delete' },
  { group: 'canvas', keys: 'arrows  ⇧arrows', desc: 'nudge 1 px / 10 px (guides show what it lines up with)' },
  { group: 'canvas', keys: '⌘Z  ⌘⇧Z', desc: 'undo / redo (your own changes), with the selection that went with them' },
  { group: 'canvas', keys: '⇧1  ⇧2', desc: 'fit all / zoom to selection' },
  { group: 'canvas', keys: '⌘0  ⌘+  ⌘−', desc: '100% / zoom in / zoom out' },
  { group: 'canvas', keys: 'drop files', desc: 'import at the pointer as a grid, into the frame under it (uploaded to the host)' },
  { group: 'canvas', keys: 'drag into a frame', desc: 'reparent (the frame highlights); drag out to leave it; hold space to keep the parent; esc cancels' },
  { group: 'out', keys: 'drag out of the window', desc: 'desktop: drag selected media to Finder or any app (also [⇲] by the selection)' },
  { group: 'out', keys: 'right-click', desc: 'node menu: copy file / image, open in…, reveal, export, mark final, lineage' },
  { group: 'out', keys: '⌘⇧C', desc: 'copy the selection as files (⌘V in Finder, Slack, Premiere); web: links' },
  { group: 'out', keys: '⌘⇧E', desc: 'export panel for the selection; nothing selected: export all marked' },
  { group: 'out', keys: '⌘K', desc: 'more: copy fal URL / prompt / recipe, mark candidate / rejected, ops (remove bg, upscale, reframe, trim, …)' },
  { group: 'canvas', keys: '⌘F', desc: 'in layers: filter' },
];

/** Where the same actions live as clicks (the `?` overlay lists these; every button's
 *  tooltip shows its key). */
export const CLICKS: { where: string; what: string }[] = [
  { where: '[≡]  top-left', what: 'projects, layers / library, chats, import, export, connect agent, undo / redo, theme, reduce motion, shortcuts, palette' },
  { where: 'layers · library', what: 'left panel tabs; [⤢] full-screen library, [×] closes the panel' },
  { where: 'right-click a node', what: 'ask, add to chat, copy file, open in, export, final, copy / paste, duplicate, delete, group, frame, order, zoom, lineage' },
  { where: 'right-click canvas', what: 'paste, add note, import files, select all, zoom to fit / 100%, undo / redo' },
  { where: '−  43%  +', what: 'bottom-left zoom; click the level to fit all' },
  { where: '[>]  [⇲]', what: 'by the selection: ask about it / drag out (click downloads on the web)' },
  { where: 'chat', what: '+ new · ≡ history · opus fal codex · [+] attach files · [+ N selected] · [send] · [stop]' },
  { where: 'library tiles', what: 'drag onto the canvas / a frame / the chat; double-click places; [place] [+ chat] [look] [♥] [tag] [collect] [upload] [local] [similar]' },
];

export function editable(t: EventTarget | null) {
  const el = t as HTMLElement | null;
  // `data-own-keys`: widgets that handle their own keys (e.g. the driver dropdown).
  return !!el && (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA' || el.tagName === 'SELECT' || el.isContentEditable || !!el.closest?.('[data-own-keys]'));
}

// ---- actions shared by keys and the palette ----------------------------------------------

export function focusChat() {
  setState({ chatOpen: true });
  requestAnimationFrame(() => window.dispatchEvent(new Event('falos:focus-chat')));
}

export function newChat() {
  setState({ chatOpen: true, chatView: 'chat', subView: null });
  // The current chat is still empty: reuse it instead of piling up empty "New chat" tabs.
  const { chat } = getState();
  if (chat.active && !isExternal(chat.active) && !chat.threads[chat.active]?.events.length) return focusChat();
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

/** Enter on the canvas: edit a note, open media, or zoom to it. Frames and groups fall
 *  through to the engine, where ⏎ selects their children. */
function enter(): boolean {
  const { app, selection, nodes } = getState();
  if (!app || selection.length !== 1) return false;
  const n = nodes.find((x) => x.id === selection[0]);
  if (!n || n.kind === 'frame' || n.kind === 'group') return false;
  if (n.kind === 'note' || n.kind === 'text') editNode(n.id);
  else if (n.kind === 'app') focusApp(n.id);
  else if (n.kind === 'video' && n.props.asset) openFocus(n.id);
  else if (previewable(n)) openPreview(n.id);
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

/** A modifier went down or up without the pointer moving: the engine re-evaluates the drag
 *  (⇧ axis lock, ⌘ no snapping) and the hover (⌥ distances) at the same spot. */
function modifiers(e: KeyboardEvent) {
  if (e.key !== 'Shift' && e.key !== 'Alt' && e.key !== 'Meta' && e.key !== 'Control') return;
  const { app } = getState();
  if (!app || !pointer.at) return;
  app.pointerMove(pointer.at[0], pointer.at[1], e.shiftKey, e.altKey, isMac ? e.metaKey : e.ctrlKey);
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
    modifiers(e);
    // Esc with a context menu open closes the menu only (the selection stays).
    if (k === 'Escape' && getState().menu) return done(), setState({ menu: null });
    // The canvas preview owns the keyboard.
    if (getState().preview) {
      if (previewKey(e)) done();
      else if (!mod) done();
      return;
    }
    // The read-only app sources (`[src]`): esc closes; the viewer handles the rest.
    if (getState().appSrc) {
      if (k === 'Escape') done(), setState({ appSrc: null });
      return;
    }
    // A focused app: esc gives the keyboard back (keys inside the app never reach us).
    if (getState().appFocus && k === 'Escape') return done(), blurApp();
    // The video focus view owns the keyboard.
    if (getState().focusVideo) {
      if (focusKey(e)) done();
      else if (!mod) done();
      return;
    }
    // Global (also while typing).
    if (mod && !e.shiftKey && (k === 'k' || k === 'K')) return done(), setState({ overlay: overlay === 'palette' ? null : 'palette' });
    if (mod && !e.shiftKey && !e.altKey && (k === 'o' || k === 'O')) return done(), openPicker();
    if (mod && e.shiftKey && (k === '\\' || k === '|' || phys(e, '|', 'Backslash'))) return done(), toggleOutline();
    if (mod && e.shiftKey && e.code === 'KeyA') return done(), openLibrary('panel');
    if (mod && e.shiftKey && e.code === 'KeyL') return done(), openLibrary('full');
    if (mod && phys(e, '\\', 'Backslash')) return done(), toggleChat();
    if ((mod || ctrl) && !e.shiftKey && (k === 'n' || k === 'N')) return done(), newChat();
    if ((mod || ctrl) && e.shiftKey && (k === '{' || phys(e, '[', 'BracketLeft'))) return done(), switchChat('prev');
    if ((mod || ctrl) && e.shiftKey && (k === '}' || phys(e, ']', 'BracketRight'))) return done(), switchChat('next');
    if ((mod || ctrl) && !e.shiftKey && !e.altKey && /^[1-9]$/.test(k)) return done(), switchChat(Number(k) - 1);
    if (mod && !e.shiftKey && (k === 'i' || k === 'I')) return done(), focusChat();
    if ((mod || ctrl) && !e.shiftKey && (k === 'y' || k === 'Y')) return done(), toggleHistory();
    if (mod && k === '.') return done(), setDriver();
    if (overlay) return; // the palette / help own the keyboard
    if (getState().library === 'full' || (e.target as HTMLElement | null)?.closest?.('.library')) return; // the library owns it
    if (editable(e.target) || getState().editing) return;

    // Canvas.
    if (!app) return;
    if (k === '?' || (e.shiftKey && phys(e, '?', 'Slash'))) return done(), setState({ overlay: 'help' });
    if (!mod && (k === '/' || (!e.shiftKey && phys(e, '/', 'Slash')))) return done(), getState().selection.length ? openPrompt() : focusChat();
    if (mod && (k === 'l' || k === 'L')) return done(), addSelectionToChat();
    if (mod && (k === 'd' || k === 'D')) return done(), duplicate();
    if (mod && e.shiftKey && e.code === 'KeyE') return done(), openExport();
    if (mod && e.shiftKey && e.code === 'KeyC') return done(), void copyFiles();
    if (mod && e.code === 'KeyG') return done(), e.shiftKey ? ungroup() : e.altKey ? frameSelection() : groupIntoFrame();
    if (mod && !e.shiftKey && (phys(e, ']', 'BracketRight') || phys(e, '[', 'BracketLeft'))) return done(), reorder(phys(e, ']', 'BracketRight') ? 1 : -1);
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
    if (k === 'Enter' && !mod && !e.shiftKey && enter()) return done();
    if (canvasVideoKey(e)) return done();
    if (k === ' ' && !e.repeat) spacePressed();
    if (app.keyDown(k, e.shiftKey, e.altKey, mod)) done();
  };
  const blur = () => {
    if (getState().modHeld) setState({ modHeld: false });
    // Modifiers released outside the window: no stuck ⌥ distances or ⇧ axis lock.
    const { app } = getState();
    if (app && pointer.at) app.pointerMove(pointer.at[0], pointer.at[1], false, false, false);
  };
  const ku = (e: KeyboardEvent) => {
    if (e.key === (isMac ? 'Meta' : 'Control') && getState().modHeld) setState({ modHeld: false });
    modifiers(e);
    if (editable(e.target)) return;
    getState().app?.keyUp(e.key);
    if (e.key === ' ' && !getState().focusVideo && !getState().preview && !getState().overlay && !getState().editing) spaceReleased();
  };
  const onCopy = (e: ClipboardEvent) => {
    if (!editable(e.target) && !getState().editing && !getState().overlay) copy(e);
  };
  const onPaste = (e: ClipboardEvent) => {
    if (!editable(e.target) && !getState().editing && !getState().overlay) void withProgress('pasting', () => paste(e, pointer.at), { key: 'paste', done: () => 'pasted' });
  };
  const offApps = installAppMessages();
  window.addEventListener('keydown', kd, true);
  window.addEventListener('keyup', ku);
  window.addEventListener('blur', blur);
  document.addEventListener('copy', onCopy);
  document.addEventListener('paste', onPaste);
  return () => {
    offApps();
    window.removeEventListener('keydown', kd, true);
    window.removeEventListener('keyup', ku);
    window.removeEventListener('blur', blur);
    document.removeEventListener('copy', onCopy);
    document.removeEventListener('paste', onPaste);
  };
}
