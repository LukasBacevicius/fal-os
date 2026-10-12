import { getState, useStore } from '../store';
import { zoomBy } from '../actions';
import { MOD } from '../keys';

const debug = new URLSearchParams(location.search).has('debug');

/** Bottom-left dim text: a small zoom control `− 43% +` (click the level to fit all, ⇧1),
 *  "offline — reconnecting" while the host is unreachable, and with `?debug` the node count
 *  and fps. */
export function StatusBar() {
  const zoom = useStore((s) => s.camera.zoom);
  const conn = useStore((s) => s.conn);
  const fps = useStore((s) => s.stats?.fps ?? 0);
  const frames = useStore((s) => s.stats?.frames ?? 0);
  const nodes = useStore((s) => s.nodes.length);
  const offline = conn === 'reconnecting';
  return (
    // id="stats": read by the desktop shell's FALOS_PROBE (apps/desktop).
    <div className="statusbar" id="stats">
      {offline && <div>offline — reconnecting</div>}
      <div className="zoom-ctl">
        <button className="link" title={`zoom out (${MOD}−)`} onClick={() => zoomBy(0.8)}>−</button>
        <button className="link zoom-level" title={`fit all (⇧1) · ${MOD}0 for 100%`} onClick={() => getState().app?.fitAll()}>{Math.round(zoom * 100)}%</button>
        <button className="link" title={`zoom in (${MOD}+)`} onClick={() => zoomBy(1.25)}>+</button>
      </div>
      {debug && <div>{nodes} {nodes === 1 ? 'node' : 'nodes'} · {frames > 0 ? `${fps} fps` : 'idle'}</div>}
    </div>
  );
}
