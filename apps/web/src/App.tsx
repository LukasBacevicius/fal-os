import { useEffect, useState } from 'react';
import { useStore } from './store';
import { Stage } from './components/Stage';
import { ChatPanel } from './components/ChatPanel';
import { Help, Palette } from './components/Palette';
import { Layers } from './components/Layers';
import { ConnectModal } from './components/ConnectModal';
import { ActivitySync } from './components/ActivityBars';
import { ProjectPicker } from './components/Projects';
import { VideoFocus } from './components/VideoLayer';
import { Library } from './components/Library';
import { ContextMenu } from './components/ContextMenu';
import { ExportPanel } from './components/ExportPanel';
import { Setup, maybeOpenSetup } from './components/Setup';

function stored(key: string, fallback: number) {
  try {
    const v = Number(localStorage.getItem(key));
    return Number.isFinite(v) && v > 0 ? v : fallback;
  } catch {
    return fallback;
  }
}

/** Canvas + chat. Nothing else unless something needs attention. */
export function App() {
  const toast = useStore((s) => s.toast);
  const toastActions = useStore((s) => s.toastActions);
  const menu = useStore((s) => s.menu);
  const chatOpen = useStore((s) => s.chatOpen);
  const overlay = useStore((s) => s.overlay);
  const layers = useStore((s) => s.outlineOpen ?? s.nodes.length > 0);
  const library = useStore((s) => s.library);
  const [chatWidth, setChatWidth] = useState(() => stored('falos.chatWidth', 360));

  // Packaged desktop app: first-run setup (fal key, Claude Code) until it's done.
  useEffect(() => {
    void maybeOpenSetup();
  }, []);

  useEffect(() => {
    try {
      localStorage.setItem('falos.chatWidth', String(chatWidth));
      localStorage.setItem('falos.chatClosed', chatOpen ? '0' : '1');
    } catch {
      /* private mode */
    }
  }, [chatWidth, chatOpen]);

  return (
    <div className="app">
      <div className="main">
        {library === 'panel' ? <Library /> : layers && <Layers />}
        <Stage />
        {chatOpen && <ChatPanel width={chatWidth} onResize={setChatWidth} />}
      </div>
      {library === 'full' && <Library />}
      {overlay === 'palette' && <Palette />}
      {overlay === 'help' && <Help />}
      {overlay === 'connect' && <ConnectModal />}
      {overlay === 'projects' && <ProjectPicker />}
      {overlay === 'export' && <ExportPanel />}
      {menu && <ContextMenu />}
      {overlay === 'setup' && <Setup />}
      <VideoFocus />
      <ActivitySync />
      {toast && (
        <div className="toast" role="status">
          {toast}
          {toastActions.map((a) => (
            <button key={a.label} className="link dim toast-action" onClick={a.run}>[{a.label}]</button>
          ))}
        </div>
      )}
    </div>
  );
}
