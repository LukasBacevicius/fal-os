import type { Actor } from '../agents';

export function Avatar({ actor, size = 20, ring }: { actor: Actor; size?: number; ring?: boolean }) {
  return (
    <span
      className={`avatar ${actor.kind} ${ring ? 'ring' : ''}`}
      style={{ width: size, height: size, background: actor.color, fontSize: Math.max(8, size * 0.42) }}
      title={actor.name}
    >
      {actor.initials}
    </span>
  );
}
