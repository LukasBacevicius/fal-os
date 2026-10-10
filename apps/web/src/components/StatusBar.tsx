import { useStore } from '../store';

export function StatusBar() {
  const app = useStore((s) => s.app);
  const zoom = useStore((s) => s.camera.zoom);
  const fps = useStore((s) => s.stats?.fps ?? 0);
  const frames = useStore((s) => s.stats?.frames ?? 0);
  const nodes = useStore((s) => s.nodes.length);
  const saved = useStore((s) => s.saved);
  const conn = useStore((s) => s.conn);
  return (
    // id="stats": read by the desktop shell's FALOS_PROBE (apps/desktop).
    <div className="statusbar" id="stats">
      <button className="status-zoom" title="Zoom to fit (Shift+1)" onClick={() => app?.fitAll()}>
        {Math.round(zoom * 100)}%
      </button>
      <span className="status-sep" />
      <span title="Frames rendered per second (0 when idle: the engine only draws on change)">
        {frames > 0 ? `${fps} fps` : 'idle'}
      </span>
      <span className="status-sep" />
      <span>{nodes} {nodes === 1 ? 'node' : 'nodes'}</span>
      <span className="status-sep" />
      {conn === 'local' ? (
        <span className={`status-save ${saved}`}>{saved === 'saving' ? 'Saving…' : saved === 'saved' ? 'Saved locally' : 'Local'}</span>
      ) : (
        <span className="status-save saved">{conn === 'connected' ? 'Synced with host' : 'Offline — changes sync on reconnect'}</span>
      )}
    </div>
  );
}
