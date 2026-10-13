// Events from the desktop shell's menu bar item and notifications (apps/desktop/src-tauri
// `tray.rs`, `notify.rs`), dispatched into the page as DOM events:
// - `falos:open-chat` {project, chat}: switch to the project and show that chat;
// - `falos:new-project`: create a project and focus its composer.

import { getState } from './store';
import { switchProject } from './sync';
import { openChat } from './chatActions';
import { createProject } from './projects';

let pending = 0;

function openFromShell(project: string, chat: string) {
  if (getState().project !== project) switchProject(project);
  // The chat list arrives with the project's sync connection: wait for the chat (≤ 5 s).
  const token = ++pending;
  const t0 = performance.now();
  const tick = () => {
    if (token !== pending) return;
    const s = getState();
    if (s.project === project && (s.chat.meta[chat] || s.chat.threads[chat])) openChat(chat);
    else if (performance.now() - t0 < 5000) setTimeout(tick, 50);
  };
  tick();
}

export function installShellEvents() {
  window.addEventListener('falos:open-chat', (e) => {
    const d = (e as CustomEvent).detail ?? {};
    if (typeof d.project === 'string' && typeof d.chat === 'string') openFromShell(d.project, d.chat);
  });
  window.addEventListener('falos:new-project', () => void createProject('untitled'));
}
