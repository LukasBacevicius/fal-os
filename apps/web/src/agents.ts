import { getState } from './store';

/** Display info for an actor. Live sessions from the host win over the built-in list. */
export interface Actor { id: string; name: string; kind: 'human' | 'agent'; color: string; initials: string }

/** Agents that annotations can be assigned to (also shown when they're not connected). */
export const AGENTS: Actor[] = [
  { id: 'claude', name: 'Claude', kind: 'agent', color: '#d97757', initials: 'C' },
  { id: 'codex', name: 'Codex', kind: 'agent', color: '#10a37f', initials: 'Cx' },
  { id: 'fal-agent', name: 'fal Agent', kind: 'agent', color: '#8b5cf6', initials: 'f' },
];

export const ME: Actor = { id: 'you', name: 'You', kind: 'human', color: '#4c8dff', initials: 'Y' };

export function initials(name: string) {
  const parts = name.trim().split(/\s+/).filter(Boolean);
  if (parts.length > 1) return (parts[0][0] + parts[1][0]).toUpperCase();
  return (parts[0] ?? '?').slice(0, 2).replace(/^./, (c) => c.toUpperCase());
}

export function actorById(id: string | null | undefined): Actor {
  if (!id) return { id: '', name: 'Unassigned', kind: 'agent', color: '#52525b', initials: '–' };
  const live = getState().sessions.find((s) => s.actor === id);
  if (live) return { id, name: live.agentName, kind: live.kind, color: live.color, initials: initials(live.agentName) };
  return AGENTS.find((a) => a.id === id) ?? (id === ME.id ? ME : { id, name: id, kind: 'human', color: '#71717a', initials: id.slice(0, 1).toUpperCase() });
}
