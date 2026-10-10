// Chat client state (docs/CHAT-PROTOCOL.md): threads built from `chat.history` and streamed
// `chat.event`s, the pending context chips, and whether an agent runtime answers at all.
// Pure functions over plain data; the store holds the result and sync.ts feeds it.

export type Box = [number, number, number, number];

export type Chip =
  | { kind: 'node'; node: string }
  | { kind: 'spot'; node: string; x: number; y: number }
  | { kind: 'region'; node: string; box: Box }
  | { kind: 'frame'; node: string; t: number; box?: Box }
  | { kind: 'range'; node: string; t0: number; t1: number };

export type ChatEvent =
  | { kind: 'user'; text: string; context?: Chip[]; author?: string }
  | { kind: 'status'; state: 'running' | 'idle' | 'error'; error?: string }
  | { kind: 'text'; agent: string; delta: string }
  | { kind: 'thinking'; agent: string; delta: string }
  | { kind: 'agent'; agent: string; role?: string; parent?: string; state: 'started' | 'working' | 'done' | 'failed'; status?: string }
  | { kind: 'tool'; agent: string; id: string; name: string; summary?: string; state: 'running' | 'done' | 'error'; nodes?: string[] }
  | { kind: 'plan'; agent: string; steps: { title: string; state: string }[] }
  | { kind: 'cost'; usd: number }
  | { kind: 'done'; summary?: string; nodes?: string[] }
  | { kind: string; [k: string]: unknown };

export interface Thread {
  chat: string;
  title?: string;
  events: ChatEvent[];
  /** Highest `seq` applied (events at or below it are duplicates). */
  seq: number;
}

/** A message sent but not yet echoed back by the host as a `user` event. */
export interface Pending { text: string; context: Chip[]; at: number; chat: string | null }

export interface ChatState {
  threads: Record<string, Thread>;
  /** Chat ids, oldest first. */
  order: string[];
  /** The chat shown (and continued by the next send); null = a new chat. */
  active: string | null;
  chips: Chip[];
  pending: Pending | null;
  /** `missing`: a send got no answer (no agent runtime behind the host). */
  runtime: 'unknown' | 'connected' | 'missing';
}

export const emptyChat = (): ChatState => ({ threads: {}, order: [], active: null, chips: [], pending: null, runtime: 'unknown' });

export function applyHistory(s: ChatState, chats: { chat: string; title?: string; events: (ChatEvent | { seq?: number; event: ChatEvent })[] }[]): ChatState {
  const threads: Record<string, Thread> = { ...s.threads };
  const order = [...s.order];
  for (const c of chats ?? []) {
    // History rows may be bare events or `{seq, event}` envelopes.
    const events = (c.events ?? []).map((e: any) => (e && typeof e === 'object' && 'event' in e && e.event ? e.event : e)) as ChatEvent[];
    const seqs = (c.events ?? []).map((e: any) => (typeof e?.seq === 'number' ? e.seq : 0));
    threads[c.chat] = { chat: c.chat, title: c.title, events, seq: Math.max(events.length, ...seqs, 0) };
    if (!order.includes(c.chat)) order.push(c.chat);
  }
  const active = s.active ?? (s.pending ? null : order[order.length - 1] ?? null);
  return { ...s, threads, order, active, runtime: chats?.length ? 'connected' : s.runtime };
}

export function applyEvent(s: ChatState, chat: string, seq: number | undefined, ev: ChatEvent): ChatState {
  const cur = s.threads[chat] ?? { chat, events: [], seq: 0 };
  if (typeof seq === 'number' && seq <= cur.seq && cur.events.length) return s;
  const t: Thread = { ...cur, events: [...cur.events, ev], seq: typeof seq === 'number' ? seq : cur.seq + 1 };
  if (!t.title && ev.kind === 'user') t.title = String((ev as any).text ?? '').slice(0, 48);
  const order = s.order.includes(chat) ? s.order : [...s.order, chat];
  let { pending, active } = s;
  // Our own message came back: the new chat (if any) becomes the active one.
  if (pending && ev.kind === 'user' && (ev as any).text === pending.text && (pending.chat === null || pending.chat === chat)) {
    pending = null;
    active = chat;
  } else if (pending && pending.chat === chat) {
    pending = null;
  }
  if (active === null && !pending && !s.order.length) active = chat;
  return { ...s, threads: { ...s.threads, [chat]: t }, order, pending, active, runtime: 'connected' };
}

// ---- transcript ------------------------------------------------------------------------

export type Item =
  | { type: 'user'; key: string; text: string; context: Chip[]; author?: string }
  | { type: 'text'; key: string; agent: string; text: string }
  | { type: 'thinking'; key: string; agent: string; text: string }
  | { type: 'tool'; key: string; agent: string; name: string; summary: string; state: string; nodes: string[] }
  | { type: 'agent'; key: string; agent: string; role: string; state: string; status: string }
  | { type: 'plan'; key: string; agent: string; steps: { title: string; state: string }[] }
  | { type: 'error'; key: string; text: string }
  | { type: 'done'; key: string; summary: string; nodes: string[] };

export interface Transcript {
  items: Item[];
  running: boolean;
  cost: number | null;
  /** Subagents currently started/working (the swarm strip). */
  swarm: { agent: string; role: string; status: string }[];
}

/** Folds a thread's events into display items: streamed deltas are joined, tool and agent
 *  rows update in place. */
export function transcript(t: Thread | undefined): Transcript {
  const items: Item[] = [];
  const tools = new Map<string, Extract<Item, { type: 'tool' }>>();
  const agents = new Map<string, Extract<Item, { type: 'agent' }>>();
  let plan: Extract<Item, { type: 'plan' }> | null = null;
  let running = false;
  let cost: number | null = null;
  (t?.events ?? []).forEach((e: any, i) => {
    const key = `${i}`;
    const last = items[items.length - 1];
    switch (e.kind) {
      case 'user':
        items.push({ type: 'user', key, text: String(e.text ?? ''), context: e.context ?? [], author: e.author });
        running = true;
        break;
      case 'text':
      case 'thinking':
        if (last && (last.type === 'text' || last.type === 'thinking') && last.type === e.kind && last.agent === e.agent) last.text += e.delta ?? '';
        else items.push({ type: e.kind, key, agent: String(e.agent ?? 'agent'), text: String(e.delta ?? '') });
        break;
      case 'tool': {
        const id = String(e.id ?? key);
        const cur = tools.get(id);
        if (cur) {
          cur.state = e.state ?? cur.state;
          cur.summary = e.summary ?? cur.summary;
          if (e.nodes?.length) cur.nodes = e.nodes;
        } else {
          const it = { type: 'tool' as const, key, agent: String(e.agent ?? ''), name: String(e.name ?? 'tool'), summary: String(e.summary ?? ''), state: String(e.state ?? 'running'), nodes: e.nodes ?? [] };
          tools.set(id, it);
          items.push(it);
        }
        break;
      }
      case 'agent': {
        const name = String(e.agent ?? '');
        const cur = agents.get(name);
        if (cur && cur.state !== 'done' && cur.state !== 'failed') {
          cur.state = e.state ?? cur.state;
          cur.status = e.status ?? cur.status;
          cur.role = e.role ?? cur.role;
        } else {
          const it = { type: 'agent' as const, key, agent: name, role: String(e.role ?? ''), state: String(e.state ?? 'started'), status: String(e.status ?? '') };
          agents.set(name, it);
          items.push(it);
        }
        break;
      }
      case 'plan':
        if (plan) plan.steps = e.steps ?? [];
        else {
          plan = { type: 'plan', key, agent: String(e.agent ?? ''), steps: e.steps ?? [] };
          items.push(plan);
        }
        break;
      case 'status':
        running = e.state === 'running';
        if (e.state === 'error') items.push({ type: 'error', key, text: String(e.error ?? 'error') });
        break;
      case 'cost':
        cost = typeof e.usd === 'number' ? e.usd : cost;
        break;
      case 'done':
        running = false;
        items.push({ type: 'done', key, summary: String(e.summary ?? ''), nodes: e.nodes ?? [] });
        break;
    }
  });
  const swarm = [...agents.values()]
    .filter((a) => running && (a.state === 'started' || a.state === 'working'))
    .map((a) => ({ agent: a.agent, role: a.role, status: a.status }));
  return { items, running, cost, swarm };
}

// ---- chips -----------------------------------------------------------------------------

const TAG: Record<string, string> = {
  image: 'img', video: 'video', audio: 'audio', text: 'text', note: 'note', frame: 'frame', generation: 'gen',
  model3d: '3d', document: 'pdf', vector: 'svg', group: 'group',
};

const r2 = (v: number) => (Math.round(v * 100) / 100).toFixed(2);

/** `[img hero-2]`, `[spot hero-2 0.42,0.31]`, `[frame clip-1 @3.24s box]`, … */
export function chipLabel(c: Chip, node?: { name: string; kind: string } | null): string {
  const name = shortName(node?.name || c.node);
  switch (c.kind) {
    case 'node':
      return `${TAG[node?.kind ?? ''] ?? node?.kind ?? 'node'} ${name}`;
    case 'spot':
      return `spot ${name} ${r2(c.x)},${r2(c.y)}`;
    case 'region':
      return `region ${name} ${c.box.map(r2).join(',')}`;
    case 'frame':
      return `frame ${name} @${c.t.toFixed(2)}s${c.box ? ' box' : ''}`;
    case 'range':
      return `range ${name} ${c.t0.toFixed(2)}-${c.t1.toFixed(2)}s`;
  }
}

export function shortName(s: string): string {
  const t = s.trim().replace(/\s+/g, '-');
  return t.length > 18 ? `${t.slice(0, 17)}…` : t;
}

const same = (a: Chip, b: Chip) => JSON.stringify(a) === JSON.stringify(b);

export function addChips(list: Chip[], add: Chip[]): Chip[] {
  const out = [...list];
  for (const c of add) if (!out.some((o) => same(o, c))) out.push(c);
  return out;
}

const round4 = (v: number) => Math.round(v * 10_000) / 10_000;

/** Chip for an Alt-click / Alt-drag pick from the canvas (`pick` event). */
export function chipFromPick(p: { nodeId: string; region: { kind: 'point'; x: number; y: number } | { kind: 'box'; x: number; y: number; w: number; h: number }; time: number | null }): Chip {
  const box: Box | undefined = p.region.kind === 'box' ? [round4(p.region.x), round4(p.region.y), round4(p.region.w), round4(p.region.h)] : undefined;
  if (p.time != null) return box ? { kind: 'frame', node: p.nodeId, t: p.time, box } : { kind: 'frame', node: p.nodeId, t: p.time };
  if (box) return { kind: 'region', node: p.nodeId, box };
  const r = p.region as { x: number; y: number };
  return { kind: 'spot', node: p.nodeId, x: round4(r.x), y: round4(r.y) };
}
