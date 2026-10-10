import { useStore } from '../store';

const CONN_LABEL = { local: 'local', connecting: 'connecting', connected: 'host', reconnecting: 'offline' } as const;

/** `fal-os / <project> / <canvas>` on the left; host status and fal account on the right. */
export function TopBar() {
  const name = useStore((s) => s.canvasName);
  const conn = useStore((s) => s.conn);
  const project = useStore((s) => s.project);
  const hostAddr = useStore((s) => s.hostAddr);
  const account = useStore((s) => s.account);
  const live = conn === 'connected';
  return (
    <header className="topbar">
      <div className="crumbs">
        <span className="dim">fal-os</span>
        <span className="faint"> / </span>
        <span className="dim">{project ?? (conn === 'local' ? 'local' : 'project')}</span>
        <span className="faint"> / </span>
        <span>{name || 'main'}</span>
      </div>
      <div className="topbar-right">
        <span
          className={live ? '' : 'faint'}
          title={conn === 'local' ? 'no fal-os host found: this canvas lives in the browser' : `fal-os host ${hostAddr ?? ''}`}
        >
          {live ? '●' : '○'} {CONN_LABEL[conn]}
        </span>
        {account && (
          <>
            <span className="faint"> · </span>
            <span className="dim">{account}</span>
          </>
        )}
      </div>
    </header>
  );
}
