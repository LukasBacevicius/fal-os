import type { Presence } from './types';

/** Placeholder actors until the host connects real agents. */
export interface Actor { id: string; name: string; kind: 'human' | 'agent'; color: string; initials: string }

export const AGENTS: Actor[] = [
  { id: 'claude', name: 'Claude', kind: 'agent', color: '#d97757', initials: 'C' },
  { id: 'codex', name: 'Codex', kind: 'agent', color: '#10a37f', initials: 'Cx' },
  { id: 'fal-agent', name: 'fal Agent', kind: 'agent', color: '#8b5cf6', initials: 'f' },
];

export const ME: Actor = { id: 'you', name: 'You', kind: 'human', color: '#4c8dff', initials: 'Y' };

export function actorById(id: string | null | undefined): Actor {
  if (!id) return { id: '', name: 'Unassigned', kind: 'agent', color: '#52525b', initials: '–' };
  return AGENTS.find((a) => a.id === id) ?? (id === ME.id ? ME : { id, name: id, kind: 'human', color: '#71717a', initials: id.slice(0, 1).toUpperCase() });
}

const STATUSES = ['reading the brief…', 'generating 4 variations…', 'upscaling v2…', 'reviewing annotations…', 'writing the script…'];

/**
 * Local demo of agent presence: cursors drift between canvas nodes with a status line.
 * Real presence arrives from the host through the client's SyncTransport.
 */
export function simulateAgents(
  targets: () => { x: number; y: number; w: number; h: number }[],
  onUpdate: (p: Presence[]) => void,
): () => void {
  const state = AGENTS.map((a, i) => ({ a, x: i * 200, y: 0, tx: 0, ty: 0, status: STATUSES[i], next: 0 }));
  let t = 0;
  const id = window.setInterval(() => {
    t += 1;
    const nodes = targets();
    for (const s of state) {
      if (t >= s.next && nodes.length) {
        const n = nodes[Math.floor(Math.random() * nodes.length)];
        s.tx = n.x + n.w * (0.2 + Math.random() * 0.6);
        s.ty = n.y + n.h * (0.2 + Math.random() * 0.6);
        s.next = t + 40 + Math.floor(Math.random() * 60);
        if (Math.random() < 0.4) s.status = STATUSES[Math.floor(Math.random() * STATUSES.length)];
      }
      s.x += (s.tx - s.x) * 0.08;
      s.y += (s.ty - s.y) * 0.08;
    }
    onUpdate(state.map((s) => ({ actor: s.a.id, name: s.a.name, kind: 'agent', color: s.a.color, cursor: [s.x, s.y], status: s.status })));
  }, 50);
  return () => window.clearInterval(id);
}
