import { useStore } from '../store';

/** DOM name labels for remote cursors; the cursors themselves are drawn by the engine overlay. */
export function PresenceLayer() {
  const app = useStore((s) => s.app);
  const presence = useStore((s) => s.presence);
  useStore((s) => s.camera); // re-position on camera changes
  if (!app || !presence.length) return null;
  return (
    <div className="presence-layer">
      {presence.map((p) => {
        if (!p.cursor) return null;
        const [x, y] = app.worldToScreen(p.cursor[0], p.cursor[1]);
        return (
          <div key={p.actor} className="cursor-label" style={{ transform: `translate(${x + 14}px, ${y + 18}px)`, background: p.color, opacity: p.opacity ?? 1 }}>
            <span className="cursor-name">{p.name}</span>
            {p.status && <span className="cursor-status">{p.status}</span>}
          </div>
        );
      })}
    </div>
  );
}
