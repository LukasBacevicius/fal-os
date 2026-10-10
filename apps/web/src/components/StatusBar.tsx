import { useEffect, useRef, useState } from 'react';
import { useStore } from '../store';

const debug = new URLSearchParams(location.search).has('debug');

/** Bottom-left dim text, only when useful: the zoom level while it changes (fades after
 *  ~1 s), "offline — reconnecting" while the host is unreachable, and with `?debug` the
 *  node count and fps. */
export function StatusBar() {
  const zoom = useStore((s) => s.camera.zoom);
  const conn = useStore((s) => s.conn);
  const fps = useStore((s) => s.stats?.fps ?? 0);
  const frames = useStore((s) => s.stats?.frames ?? 0);
  const nodes = useStore((s) => s.nodes.length);
  const [zooming, setZooming] = useState(false);
  const first = useRef(true);
  useEffect(() => {
    if (first.current) {
      first.current = false;
      return;
    }
    setZooming(true);
    const t = window.setTimeout(() => setZooming(false), 1000);
    return () => window.clearTimeout(t);
  }, [zoom]);
  const offline = conn === 'reconnecting';
  return (
    // id="stats": read by the desktop shell's FALOS_PROBE (apps/desktop).
    <div className="statusbar" id="stats">
      {offline && <div>offline — reconnecting</div>}
      <div className={`zoom-level ${zooming || debug ? 'on' : ''}`}>{Math.round(zoom * 100)}%</div>
      {debug && <div>{nodes} {nodes === 1 ? 'node' : 'nodes'} · {frames > 0 ? `${fps} fps` : 'idle'}</div>}
    </div>
  );
}
