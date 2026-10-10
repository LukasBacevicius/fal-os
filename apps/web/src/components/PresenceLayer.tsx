import { useStore } from '../store';
import { openChat } from '../chatActions';
import { transcript } from '../chat';
import { getState } from '../store';

/** Mono labels for agent cursors (`generator-2 · hero shot`) in their chat's color; the
 *  arrows are drawn by the engine. Only real host sessions with recent activity appear; idle
 *  ones fade, thinking ones are dimmed. Clicking a cursor opens its chat (at that agent). */
export function PresenceLayer() {
  const app = useStore((s) => s.app);
  const presence = useStore((s) => s.presence);
  useStore((s) => s.camera); // re-position on camera changes
  if (!app || !presence.length) return null;
  const open = (chat: string, name: string) => {
    const subs = transcript(getState().chat.threads[chat]).subagents;
    openChat(chat, subs.includes(name) ? name : null);
  };
  return (
    <div className="presence-layer">
      {presence.map((p) => {
        if (!p.cursor) return null;
        const [x, y] = app.worldToScreen(p.cursor[0], p.cursor[1]);
        const label = p.label || `${p.name}${p.status ? `: ${p.status}` : ''}`;
        const dim = p.state === 'thinking';
        const style = { transform: `translate(${x}px, ${y}px)`, opacity: (p.opacity ?? 1) * (dim ? 0.55 : 1), color: p.color || undefined };
        return (
          <div key={p.actor} className={`cursor-hit ${p.chat ? 'clickable' : ''}`} style={style} onClick={() => p.chat && open(p.chat, p.name)} title={p.chat ? 'open this agent’s chat' : undefined}>
            <span className="cursor-label">{label}</span>
          </div>
        );
      })}
    </div>
  );
}
