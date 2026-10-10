import { useStore } from './store';
import { TopBar } from './components/TopBar';
import { LeftPanel } from './components/LeftPanel';
import { RightPanel } from './components/RightPanel';
import { Stage } from './components/Stage';

export function App() {
  const toast = useStore((s) => s.toast);
  return (
    <div className="app">
      <TopBar />
      <div className="main">
        <LeftPanel />
        <Stage />
        <RightPanel />
      </div>
      {toast && <div className="toast" role="status">{toast}</div>}
    </div>
  );
}
