import { useEffect, useState } from 'react';
import { useStore } from './store';
import { Stage } from './components/Stage';
import { ChatPanel } from './components/ChatPanel';
import { Help, Palette } from './components/Palette';

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
  const chatOpen = useStore((s) => s.chatOpen);
  const overlay = useStore((s) => s.overlay);
  const [chatWidth, setChatWidth] = useState(() => stored('falos.chatWidth', 360));

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
        <Stage />
        {chatOpen && <ChatPanel width={chatWidth} onResize={setChatWidth} />}
      </div>
      {overlay === 'palette' && <Palette />}
      {overlay === 'help' && <Help />}
      {toast && <div className="toast" role="status">{toast}</div>}
    </div>
  );
}
