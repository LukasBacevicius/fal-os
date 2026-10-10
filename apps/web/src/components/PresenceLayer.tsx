import { useStore } from '../store';

/** Mono text labels for agent cursors (`motion: rendering`); the arrows are drawn by the
 *  engine. Only real host sessions with recent activity appear; idle ones fade. */
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
          <div key={p.actor} className="cursor-label" style={{ transform: `translate(${x + 12}px, ${y + 14}px)`, opacity: p.opacity ?? 1 }}>
            {p.name}{p.status ? `: ${p.status}` : ''}
          </div>
        );
      })}
    </div>
  );
}
