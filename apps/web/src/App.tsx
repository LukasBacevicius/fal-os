import { useEffect, useState } from 'react';
import { useStore } from './store';
import { TopBar } from './components/TopBar';
import { Stage } from './components/Stage';
import { ChatPanel } from './components/ChatPanel';

const isMac = navigator.platform.toLowerCase().includes('mac') || navigator.userAgent.includes('Mac');

function stored(key: string, fallback: number) {
  try {
    const v = Number(localStorage.getItem(key));
    return Number.isFinite(v) && v > 0 ? v : fallback;
  } catch {
    return fallback;
  }
}

export function App() {
  const toast = useStore((s) => s.toast);
  const [chatWidth, setChatWidth] = useState(() => stored('falos.chatWidth', 360));
  const [chatOpen, setChatOpen] = useState(() => stored('falos.chatClosed', 0) !== 1);

  // ⌘\ collapses / expands the chat panel (also while typing).
  useEffect(() => {
    const kd = (e: KeyboardEvent) => {
      if ((isMac ? e.metaKey : e.ctrlKey) && e.key === '\\') {
        e.preventDefault();
        setChatOpen((o) => !o);
      }
    };
    window.addEventListener('keydown', kd);
    return () => window.removeEventListener('keydown', kd);
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
      <TopBar />
      <div className="main">
        <Stage />
        {chatOpen && <ChatPanel width={chatWidth} onResize={setChatWidth} />}
      </div>
      {toast && <div className="toast" role="status">{toast}</div>}
    </div>
  );
}
