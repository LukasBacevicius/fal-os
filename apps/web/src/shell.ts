// Events from the desktop shell's menu bar item and notifications (apps/desktop/src-tauri
// `tray.rs`, `notify.rs`), dispatched into the page as DOM events:
// - `falos:open-chat` {project, chat}: switch to the project and show that chat;
// - `falos:new-project`: create a project and focus its composer;
// - `falos:toast` {text}: a one-line note;
// - `falos:host` {state: running|restarting|stopped, reason?, restarts}: the supervised host
//   (sidecar.rs). Restarting is one held line; stopped is the card
//   `host stopped · [restart] [copy report] [open logs]`.

import { copyText, dismissNotice, getState, hideCard, notify, resolveNotice, showCard, toast } from './store';
import { hostSync, switchProject } from './sync';
import { invoke, isDesktop } from './desktop';
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

interface HostState { state?: string; reason?: string; restarts?: number; supervised?: boolean }

function onHostState(s: HostState | null | undefined) {
  if (!s?.state) return;
  if (s.state === 'restarting') {
    notify(`host ${s.reason ?? 'stopped'} · restarting`, { key: 'host', level: 'ongoing' });
  } else if (s.state === 'running') {
    hideCard('host-down');
    resolveNotice('host', 'host restarted');
    // Reconnect now instead of waiting out the backoff.
    hostSync()?.retryNow();
  } else if (s.state === 'stopped') {
    dismissNotice('host');
    dismissNotice('conn');
    showCard('host-down', `host stopped${s.reason ? ` (${s.reason})` : ''}`, [
      {
        label: 'restart',
        run: () => {
          hideCard('host-down');
          notify('restarting the host…', { key: 'host', level: 'ongoing' });
          invoke('restart_host').then(
            () => onHostState({ state: 'running' }),
            (e) => onHostState({ state: 'stopped', reason: String(e) }),
          );
        },
      },
      {
        label: 'copy report',
        run: () => void invoke<string>('host_report').then((t) => copyText(t, 'report copied'), (e) => toast(`report failed: ${e}`)),
      },
      { label: 'open logs', run: () => void invoke('open_logs').catch((e) => toast(`open logs failed: ${e}`)) },
    ]);
  }
}

export function installShellEvents() {
  window.addEventListener('falos:host', (e) => onHostState((e as CustomEvent).detail));
  // The host may have stopped before this page loaded (a webview reload).
  if (isDesktop()) void invoke<HostState>('host_status').then((s) => s?.state === 'stopped' && onHostState(s), () => {});
  window.addEventListener('falos:open-chat', (e) => {
    const d = (e as CustomEvent).detail ?? {};
    if (typeof d.project === 'string' && typeof d.chat === 'string') openFromShell(d.project, d.chat);
  });
  window.addEventListener('falos:new-project', () => void createProject('untitled'));
  // A one-line note from the shell (e.g. right before a macOS permission prompt).
  window.addEventListener('falos:toast', (e) => {
    const t = (e as CustomEvent).detail?.text;
    if (typeof t === 'string') toast(t);
  });
}
