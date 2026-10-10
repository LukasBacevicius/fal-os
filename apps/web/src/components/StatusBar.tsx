import { useStore } from '../store';

const debug = new URLSearchParams(location.search).has('debug');

/** Bottom-left dim status: `43% · 7 nodes` (fps with `?debug`). */
export function StatusBar() {
  const zoom = useStore((s) => s.camera.zoom);
  const fps = useStore((s) => s.stats?.fps ?? 0);
  const frames = useStore((s) => s.stats?.frames ?? 0);
  const nodes = useStore((s) => s.nodes.length);
  return (
    // id="stats": read by the desktop shell's FALOS_PROBE (apps/desktop).
    <div className="statusbar" id="stats">
      {Math.round(zoom * 100)}% · {nodes} {nodes === 1 ? 'node' : 'nodes'}
      {debug && ` · ${frames > 0 ? `${fps} fps` : 'idle'}`}
    </div>
  );
}
