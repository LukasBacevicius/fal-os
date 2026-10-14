// Chat client state (docs/CHAT-PROTOCOL.md): threads built from `chat.history` and streamed
// `chat.event`s, the pending context chips, and whether an agent runtime answers at all.
// Pure functions over plain data; the store holds the result and sync.ts feeds it.

export type Box = [number, number, number, number];

export type Chip =
  | { kind: 'node'; node: string }
  | { kind: 'spot'; node: string; x: number; y: number }
  | { kind: 'region'; node: string; box: Box }
  | { kind: 'frame'; node: string; t: number; box?: Box }
  | { kind: 'range'; node: string; t0: number; t1: number }
  /** Another chat as context (`#title` in the input). */
  | { kind: 'chat'; chat: string; node?: undefined };

export type ChatEvent =
  | { kind: 'user'; text: string; context?: Chip[]; author?: string; from_title?: string }
  | { kind: 'title'; text: string }
  | { kind: 'status'; state: 'running' | 'idle' | 'error'; error?: string }
  | { kind: 'text'; agent: string; delta: string; parent?: string }
  | { kind: 'thinking'; agent: string; delta: string; parent?: string }
  | { kind: 'thinking_done'; agent: string; seconds: number }
  | { kind: 'agent'; agent: string; role?: string; parent?: string; state: 'started' | 'working' | 'done' | 'failed'; status?: string }
  | { kind: 'tool'; agent: string; parent?: string; id: string; name: string; summary?: string; state: 'running' | 'done' | 'error'; nodes?: string[] }
  | { kind: 'plan'; agent: string; steps: { title: string; state: string }[] }
  | { kind: 'cost'; usd: number }
  | { kind: 'done'; summary?: string; nodes?: string[] }
  | { kind: 'turn'; turn: string; state: 'start' | 'end'; nodes?: string[] }
  | { kind: 'queued'; id: string; text: string }
  | { kind: 'dequeued'; id: string }
  | { kind: 'reverted'; turn: string }
  /** At chat start: which tools/drivers work; `missing` ones come with a fix to run. */
  | { kind: 'capabilities'; available: string[]; missing: { name: string; fix?: string }[] }
  | { kind: string; [k: string]: unknown };

/** Events as kept by the client: plus the local arrival time (or the host's `ts`). */
export type StoredEvent = ChatEvent & { _ts?: number };

export interface Thread {
  chat: string;
  title?: string;
  events: StoredEvent[];
  /** Highest `seq` applied (events at or below it are duplicates). */
  seq: number;
}

/** One row of the host's `chat.list`. */
export interface ChatMeta {
  chat: string;
  title?: string;
  state?: 'running' | 'idle' | 'error' | string;
  updated?: number;
  agents?: { agent: string; status?: string }[];
  cost?: number;
  /** The chat's color (UX-V1: the one color in the UI). */
  color?: string;
  /** Which agent drives the chat: `opus` (director), `fal` or `codex`. */
  driver?: string;
  /** Shown as a tab (history keeps closed chats). Missing = open. */
  open?: boolean;
  archived?: boolean;
  /** External agents' tabs (`ext:…`): the MCP client (`codex`, `claude`, …). */
  client?: string;
  /** External agents' tabs: its last tool call (ms). */
  seen?: number;
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
  /** The host's chat list (`chat.list`), by id. */
  meta: Record<string, ChatMeta>;
  /** Chats with events the user hasn't looked at. */
  unread: Record<string, boolean>;
  /** A `chat.new` is in flight: its `chat.created` becomes the active one. */
  creating: boolean;
  /** Tab order chosen by the user (drag); tabs not in it follow, oldest first. */
  tabOrder: string[];
  /** Chats closed locally before the host confirmed (`chat.close`). */
  closed: Record<string, boolean>;
}

const loadTabOrder = (): string[] => {
  try {
    const v = JSON.parse(localStorage.getItem('falos.tabOrder') ?? '[]');
    return Array.isArray(v) ? v.filter((x) => typeof x === 'string') : [];
  } catch {
    return [];
  }
};

export const emptyChat = (): ChatState => ({
  threads: {}, order: [], active: null, chips: [], pending: null, runtime: 'unknown', meta: {}, unread: {}, creating: false,
  tabOrder: typeof localStorage === 'undefined' ? [] : loadTabOrder(), closed: {},
});

/** Muted, desaturated hues: one per chat (UX-V1). Used when the host sends no color. */
export const CHAT_COLORS = ['#c08872', '#bfa16a', '#9aae6e', '#6fae8c', '#6aa6b0', '#7b93c4', '#9a86c2', '#bf7f9f'];

function hash(s: string): number {
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) h = Math.imul(h ^ s.charCodeAt(i), 16777619);
  return h >>> 0;
}

export function chatColor(s: ChatState, chat: string | null | undefined): string {
  if (!chat) return 'var(--dim)';
  return s.meta[chat]?.color || CHAT_COLORS[hash(chat) % CHAT_COLORS.length];
}

export const isExternal = (chat: string) => chat.startsWith('ext:');

/** The composer draft's key: the chat, or (no chat yet) a new chat of this project, so a
 *  draft typed in one project's empty composer doesn't follow into another. */
export const draftKey = (chat: string | null, project: string | null) => chat ?? `new:${project ?? ''}`;

/** Host `chat.list`: replaces the metadata; unknown chats join the order (newest last). */
export function applyList(s: ChatState, chats: ChatMeta[]): ChatState {
  const meta: Record<string, ChatMeta> = {};
  for (const c of chats ?? []) meta[c.chat] = c;
  const listed = [...(chats ?? [])].sort((a, b) => (a.updated ?? 0) - (b.updated ?? 0)).map((c) => c.chat);
  // Chats the host no longer lists are gone (deleted); keep only locally known running ones
  // while the host hasn't listed anything yet.
  const order = [...listed, ...s.order.filter((id) => !meta[id] && !listed.includes(id) && s.threads[id] && !chats?.length)];
  const active = s.active && (meta[s.active] || s.threads[s.active]) ? s.active : s.active === null ? null : order[order.length - 1] ?? null;
  // A local close is settled once the host agrees.
  const closed = { ...s.closed };
  for (const id of Object.keys(closed)) if (meta[id]?.open === false || !meta[id]) delete closed[id];
  return { ...s, meta, order, active, closed, runtime: 'connected' };
}

/** Whether a chat is running (host list first, else the local transcript). */
export function isRunning(s: ChatState, chat: string): boolean {
  const st = s.meta[chat]?.state;
  if (st) return st === 'running';
  return transcript(s.threads[chat]).running;
}

/** The open tabs, in display order: chats the host marks open (and not closed here),
 *  running chats always, and the active chat. */
export function tabs(s: ChatState): string[] {
  const ids = s.active && !s.order.includes(s.active) && s.threads[s.active] ? [...s.order, s.active] : s.order;
  const known = ids.filter((id) => {
    const m = s.meta[id];
    if (id === s.active) return true;
    // An external agent's tab can be closed while it works (its next session reopens it).
    if (isRunning(s, id) && !isExternal(id)) return true;
    if (m?.archived || s.closed[id]) return false;
    return m?.open !== false;
  });
  const ordered = s.tabOrder.filter((id) => known.includes(id));
  return [...ordered, ...known.filter((id) => !ordered.includes(id))];
}

/** A chat the host deleted: gone everywhere; the neighbouring tab becomes active. */
export function removeChat(s: ChatState, chat: string): ChatState {
  const before = tabs(s);
  const threads = { ...s.threads };
  delete threads[chat];
  const meta = { ...s.meta };
  delete meta[chat];
  const unread = { ...s.unread };
  delete unread[chat];
  const order = s.order.filter((id) => id !== chat);
  let active = s.active;
  if (active === chat) {
    const i = before.indexOf(chat);
    const rest = before.filter((id) => id !== chat);
    active = rest[Math.min(Math.max(0, i), rest.length - 1)] ?? null;
  }
  return { ...s, threads, meta, unread, order, active, tabOrder: s.tabOrder.filter((id) => id !== chat) };
}

/** Switches the shown chat (null = new chat) and clears its unread marker. */
export function selectChat(s: ChatState, chat: string | null): ChatState {
  const unread = { ...s.unread };
  if (chat) delete unread[chat];
  const closed = { ...s.closed };
  if (chat) delete closed[chat];
  return { ...s, active: chat, unread, closed };
}

/** Display title of a chat. */
export function chatTitle(s: ChatState, chat: string): string {
  return s.meta[chat]?.title || s.threads[chat]?.title || (isExternal(chat) ? chat.slice(4) : 'new chat');
}

/** `headphones-ad` from a title, for `[#…]` chips and `#` completion. */
export function slug(t: string): string {
  return t.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '').slice(0, 24) || 'chat';
}

export function applyHistory(s: ChatState, chats: { chat: string; title?: string; events: (ChatEvent | { seq?: number; ts?: number; event: ChatEvent })[] }[]): ChatState {
  const threads: Record<string, Thread> = { ...s.threads };
  const order = [...s.order];
  for (const c of chats ?? []) {
    // History rows may be bare events or `{seq, ts, event}` envelopes.
    const events = (c.events ?? []).map((e: any) => {
      const ev = e && typeof e === 'object' && 'event' in e && e.event ? e.event : e;
      return typeof e?.ts === 'number' ? { ...ev, _ts: e.ts } : ev;
    }) as StoredEvent[];
    const seqs = (c.events ?? []).map((e: any) => (typeof e?.seq === 'number' ? e.seq : 0));
    threads[c.chat] = { chat: c.chat, title: c.title, events, seq: Math.max(events.length, ...seqs, 0) };
    if (!order.includes(c.chat)) order.push(c.chat);
  }
  // Never land on an external agent's read-only pseudo-chat by default.
  const own = order.filter((id) => !isExternal(id));
  const active = s.active ?? (s.pending || s.creating ? null : own[own.length - 1] ?? null);
  return { ...s, threads, order, active, runtime: chats?.length ? 'connected' : s.runtime };
}

export function applyEvent(s: ChatState, chat: string, seq: number | undefined, ev: ChatEvent, ts = Date.now()): ChatState {
  const cur = s.threads[chat] ?? { chat, events: [], seq: 0 };
  if (typeof seq === 'number' && seq <= cur.seq && cur.events.length) return s;
  const t: Thread = { ...cur, events: [...cur.events, { ...ev, _ts: ts }], seq: typeof seq === 'number' ? seq : cur.seq + 1 };
  if (ev.kind === 'title') t.title = String((ev as any).text ?? '');
  else if (!t.title && ev.kind === 'user' && !String((ev as any).author ?? '').startsWith('chat:')) t.title = String((ev as any).text ?? '').slice(0, 48);
  const order = s.order.includes(chat) ? s.order : [...s.order, chat];
  let { pending, active } = s;
  // Our own message came back (or was queued behind a running turn): the new chat (if any)
  // becomes the active one.
  const echoed = (ev.kind === 'user' || ev.kind === 'queued') && (ev as any).text === pending?.text;
  if (pending && echoed && (pending.chat === null || pending.chat === chat)) {
    pending = null;
    active = chat;
  } else if (pending && pending.chat === chat) {
    pending = null;
  }
  if (active === null && !pending && !s.order.length && !s.creating && !isExternal(chat)) active = chat;
  const quiet = ev.kind === 'cost' || ev.kind === 'status' || ev.kind === 'thinking';
  const unread = active === chat || quiet ? s.unread : { ...s.unread, [chat]: true };
  return { ...s, threads: { ...s.threads, [chat]: t }, order, pending, active, unread, runtime: 'connected' };
}

// ---- transcript ------------------------------------------------------------------------

/** One tool call inside an activity line. */
export interface Call { id: string; name: string; summary: string; state: string; nodes: string[]; cost: number }

export type Item =
  | {
      type: 'user'; key: string; text: string; context: Chip[]; author?: string; fromChat?: string; fromExt?: string; fromTitle?: string;
      /** Delivered into the running turn: `steer` = sent now (⌘⏎ / [send now]), `queued` =
       *  injected after the next step. */
      delivered?: 'steer' | 'queued';
      /** A subagent the message was sent to (`chat.steer` with `agent`). */
      to?: string;
    }
  | { type: 'text'; key: string; agent: string; text: string }
  | { type: 'thinking'; key: string; agent: string; text: string; seconds: number | null; done: boolean }
  /** One activity line: consecutive calls of the same verb by the same agent, merged. */
  | { type: 'act'; key: string; agent: string; verb: Verb; calls: Call[] }
  /** A subagent's start card or (phase end) summary card in the director's transcript. */
  | { type: 'sub'; key: string; agent: string; role: string; phase: 'start' | 'end'; state: string; status: string; tools: number; ms: number | null; nodes: string[] }
  | { type: 'plan'; key: string; agent: string; steps: { title: string; state: string }[] }
  /** A batch generation (`batch` events update it in place). */
  | { type: 'batch'; key: string; id: string; model: string; total: number; done: number; failed: number; running: number; frame: string | null; nodes: string[] }
  | { type: 'error'; key: string; text: string; actions: string[] }
  /** A question from the agent (e.g. the fal driver's pending input): numbered options,
   *  answered by number key or by typing (a normal message). */
  | { type: 'question'; key: string; agent: string; prompt: string; options: string[]; answered: boolean }
  | { type: 'done'; key: string; summary: string; nodes: string[]; turn: string | null; reverted: boolean; stopped?: boolean }
  /** Exported / produced files (`files` event, docs/ASSETS-V1.md §7): shown as file cards. */
  | { type: 'files'; key: string; files: OutFile[]; dir: string }
  /** An app the agents built or changed (`app` event): one card per app per turn, updated in place. */
  | { type: 'app'; key: string; app: string; node: string; name: string; version: number; files: string[] };

/** One file of a `files` chat event. */
export interface OutFile { path: string; name: string; node?: string; preset?: string; w?: number; h?: number; bytes?: number; sha256?: string }

/** `files` event payload → cards (tolerates partial rows: a name is derived from the path). */
export function filesOf(e: { files?: unknown }): OutFile[] {
  return (Array.isArray(e.files) ? e.files : [])
    .filter((f: any) => f && typeof f.path === 'string')
    .map((f: any) => ({
      path: f.path,
      name: typeof f.name === 'string' && f.name ? f.name : f.path.split('/').pop(),
      node: typeof f.node === 'string' ? f.node : undefined,
      preset: typeof f.preset === 'string' ? f.preset : undefined,
      w: typeof f.w === 'number' ? f.w : undefined,
      h: typeof f.h === 'number' ? f.h : undefined,
      bytes: typeof f.bytes === 'number' ? f.bytes : undefined,
      sha256: typeof f.sha256 === 'string' ? f.sha256 : undefined,
    }));
}

export interface SwarmRow { agent: string; role: string; status: string; tools: number }

export interface Transcript {
  items: Item[];
  running: boolean;
  cost: number | null;
  /** Subagents currently started/working (the swarm strip). */
  swarm: SwarmRow[];
  /** Messages waiting behind the running turn (host `queued` minus `dequeued`). */
  queued: { id: string; text: string }[];
  /** Agents that are subagents (their activity lives in their own transcript). */
  subagents: string[];
  /** Batches still generating (swarm strip). */
  batches: Extract<Item, { type: 'batch' }>[];
  /** The latest question still waiting for an answer. */
  question: Extract<Item, { type: 'question' }> | null;
  /** The latest `capabilities` event (null = none: nothing to say). */
  caps: { available: string[]; missing: { name: string; fix: string }[] } | null;
  /** Every subagent's latest role / status / state (subagent view header). */
  agents: Record<string, { role: string; status: string; state: string }>;
}

/** Verb families for activity lines. */
export type Verb = 'look' | 'gen' | 'edit' | 'import' | 'file' | 'bash' | 'web' | 'codex' | 'chat' | 'comment' | 'undo' | 'other';

/** Housekeeping calls that never show (session, status, title, knowledge, file reads, …). */
const HIDDEN = new Set([
  'session', 'set_status', 'chat_title', 'title', 'get_knowledge', 'Read', 'Glob', 'Grep', 'LS', 'claim', 'release',
  'chat_list', 'TodoWrite', 'ToolSearch', 'finish_working', 'finish_working_on_nodes', 'set_asset_uri',
]);

export function verbOf(name: string): Verb | null {
  if (HIDDEN.has(name)) return null;
  switch (name) {
    case 'read': case 'get_context': case 'list_annotations': case 'get_annotation': case 'chat_read': case 'get_selection':
      return 'look';
    case 'generate': case 'fal_model': case 'fal_agent':
      return 'gen';
    case 'edit':
      return 'edit';
    case 'import_file':
      return 'import';
    case 'Write': case 'Edit': case 'MultiEdit': case 'NotebookEdit':
      return 'file';
    case 'Bash':
      return 'bash';
    case 'WebSearch': case 'WebFetch':
      return 'web';
    case 'codex':
      return 'codex';
    case 'chat_send':
      return 'chat';
    case 'annotate': case 'reply':
      return 'comment';
    case 'undo': case 'revert_since':
      return 'undo';
    default:
      return 'other';
  }
}

/** `$0.01` / `~$0.01` in a tool summary. */
function costIn(summary: string): number {
  const m = /~?\$(\d+(?:\.\d+)?)/.exec(summary);
  return m ? Number(m[1]) : 0;
}

type DoneItem = Extract<Item, { type: 'done' }>;
type ThinkItem = Extract<Item, { type: 'thinking' }>;
type ActItem = Extract<Item, { type: 'act' }>;
type TextItem = Extract<Item, { type: 'text' }>;

/** Folds a thread's events into display items: streamed deltas are joined, tool calls merge
 *  into past-tense activity lines, subagents get start/summary cards (their own activity goes
 *  to their transcript: pass `view` = the subagent's name to get that one). */
export function transcript(t: Thread | undefined, view: string | null = null): Transcript {
  const events = t?.events ?? [];
  // Subagents: anything announced by an `agent` event or carrying a `parent`.
  const subs = new Set<string>();
  for (const e of events as any[]) {
    if (e.agent && e.agent !== 'director' && (e.kind === 'agent' || e.parent)) subs.add(String(e.agent));
  }
  const mine = (agent: unknown) => {
    const a = String(agent ?? '');
    return view === null ? !subs.has(a) : a === view;
  };
  const items: Item[] = [];
  const calls = new Map<string, Call>();
  const stats = new Map<string, { t0: number | null; tools: Set<string>; nodes: Set<string>; role: string; status: string; state: string }>();
  const turns = new Map<string, DoneItem>();
  const queued = new Map<string, string>();
  /** Queued ids taken out by a steer (`dequeued` reason `steer…`). */
  const steered = new Set<string>();
  let caps: Transcript['caps'] = null;
  const batches = new Map<string, Extract<Item, { type: 'batch' }>>();
  let plan: Extract<Item, { type: 'plan' }> | null = null;
  let turn: string | null = null;
  /** The running turn ended by the user's stop (its end line says `stopped`, not `done`). */
  let stopped = false;
  let running = false;
  let cost: number | null = null;
  let textInTurn = false;
  const settleThinking = () => {
    for (const it of items) if (it.type === 'thinking') it.done = true;
  };
  events.forEach((e: any, i) => {
    const key = `${i}`;
    const last = items[items.length - 1];
    switch (e.kind) {
      case 'user': {
        const author = String(e.author ?? '');
        const fromChat = author.startsWith('chat:') ? author.slice(5) : undefined;
        const fromExt = author.startsWith('ext:') ? author.slice(4) : undefined;
        const qid = typeof e.queued === 'string' ? e.queued : null;
        if (qid) queued.delete(qid);
        else for (const [id, text] of queued) if (text === e.text) { queued.delete(id); break; }
        for (const it of items) if (it.type === 'question') it.answered = true;
        const to = typeof e.agent === 'string' && e.agent && e.agent !== 'director' ? e.agent : typeof e.to === 'string' ? e.to : undefined;
        const isSteer = e.steer === true || e.mode === 'steer' || e.via === 'steer' || (qid !== null && steered.has(qid));
        const delivered = isSteer ? 'steer' as const : qid !== null && running ? 'queued' as const : undefined;
        if (view === null || (to && to === view)) {
          items.push({ type: 'user', key, text: String(e.text ?? ''), context: e.context ?? [], author: e.author, fromChat, fromExt, fromTitle: e.from_title, delivered, to });
        }
        if (to) break; // a message to a subagent doesn't start a director turn
        running = true;
        textInTurn = false;
        break;
      }
      case 'text':
      case 'thinking': {
        if (!mine(e.agent)) break;
        if (e.kind === 'text') textInTurn = true;
        const cont = last && (last.type === 'text' || last.type === 'thinking') && last.type === e.kind && last.agent === e.agent && !(last.type === 'thinking' && last.done);
        if (cont) (last as TextItem | ThinkItem).text += e.delta ?? '';
        else if (e.kind === 'text') items.push({ type: 'text', key, agent: String(e.agent ?? 'agent'), text: String(e.delta ?? '') });
        else items.push({ type: 'thinking', key, agent: String(e.agent ?? 'agent'), text: String(e.delta ?? ''), seconds: null, done: false });
        break;
      }
      case 'thinking_done': {
        if (!mine(e.agent)) break;
        const th = [...items].reverse().find((x): x is ThinkItem => x.type === 'thinking' && x.agent === e.agent && x.seconds === null);
        const seconds = typeof e.seconds === 'number' ? e.seconds : null;
        if (th) {
          th.seconds = seconds;
          th.done = true;
        } else items.push({ type: 'thinking', key, agent: String(e.agent ?? ''), text: '', seconds, done: true });
        break;
      }
      case 'tool': {
        const agent = String(e.agent ?? '');
        const id = String(e.id ?? key);
        const st = stats.get(agent);
        if (st) {
          st.tools.add(id);
          for (const n of e.nodes ?? []) st.nodes.add(n);
        }
        if (!mine(agent)) break;
        const cur = calls.get(id);
        if (cur) {
          cur.state = e.state ?? cur.state;
          if (e.summary) {
            cur.summary = String(e.summary);
            cur.cost = costIn(cur.summary) || cur.cost;
          }
          if (e.nodes?.length) cur.nodes = e.nodes;
          break;
        }
        const name = String(e.name ?? 'tool');
        const verb = verbOf(name);
        if (!verb) break;
        const call: Call = { id, name, summary: String(e.summary ?? ''), state: String(e.state ?? 'running'), nodes: e.nodes ?? [], cost: costIn(String(e.summary ?? '')) };
        calls.set(id, call);
        const mergeable = verb !== 'other' && verb !== 'codex' && verb !== 'chat';
        if (mergeable && last && last.type === 'act' && last.agent === agent && last.verb === verb) last.calls.push(call);
        else items.push({ type: 'act', key, agent, verb, calls: [call] });
        break;
      }
      case 'agent': {
        const name = String(e.agent ?? '');
        let st = stats.get(name);
        const ended = (x: string) => x === 'done' || x === 'failed';
        if (!st || ended(st.state)) {
          st = { t0: e._ts ?? null, tools: new Set(), nodes: new Set(), role: String(e.role ?? ''), status: String(e.status ?? ''), state: String(e.state ?? 'started') };
          stats.set(name, st);
          if (view === null) items.push({ type: 'sub', key, agent: name, role: st.role, phase: 'start', state: st.state, status: st.status, tools: 0, ms: null, nodes: [] });
        }
        st.state = String(e.state ?? st.state);
        if (e.status) st.status = String(e.status);
        if (e.role) st.role = String(e.role);
        if (ended(st.state) && (view === null || view === name)) {
          items.push({
            type: 'sub', key: `${key}e`, agent: name, role: st.role, phase: 'end', state: st.state, status: st.status, tools: st.tools.size,
            ms: st.t0 != null && e._ts != null ? e._ts - st.t0 : null, nodes: [...st.nodes],
          });
        }
        break;
      }
      case 'plan':
        if (view !== null) break;
        if (plan) plan.steps = e.steps ?? [];
        else {
          plan = { type: 'plan', key, agent: String(e.agent ?? ''), steps: e.steps ?? [] };
          items.push(plan);
        }
        break;
      case 'status':
        running = e.state === 'running';
        if (e.state === 'idle' && e.error === 'stopped') stopped = true;
        if (!running) settleThinking();
        if (e.state === 'error' && view === null) items.push({ type: 'error', key, text: String(e.error ?? 'error'), actions: Array.isArray(e.actions) ? e.actions : [] });
        break;
      case 'cost':
        cost = typeof e.usd === 'number' ? e.usd : cost;
        break;
      case 'turn':
        if (e.state === 'start') {
          turn = String(e.turn ?? '') || null;
          stopped = false;
        }
        else if (view === null) {
          const id = String(e.turn ?? '');
          const d = turns.get(id) ?? [...items].reverse().find((x): x is DoneItem => x.type === 'done' && x.turn === null);
          if (d) {
            d.turn = id;
            d.reverted = false;
            d.stopped = stopped;
            if (e.nodes?.length) d.nodes = [...new Set([...d.nodes, ...e.nodes])];
            turns.set(id, d);
          } else {
            const it: DoneItem = { type: 'done', key, summary: '', nodes: e.nodes ?? [], turn: id, reverted: false, stopped };
            items.push(it);
            turns.set(id, it);
          }
          turn = null;
        }
        break;
      case 'reverted': {
        const d = turns.get(String(e.turn ?? ''));
        if (d) d.reverted = true;
        break;
      }
      case 'question':
      case 'input':
      case 'input_request':
      case 'ask': {
        if (!mine(e.agent)) break;
        const raw = e.options ?? e.choices ?? [];
        const options = (Array.isArray(raw) ? raw : []).map((o: any) => String(typeof o === 'string' ? o : o?.label ?? o?.text ?? o?.value ?? '')).filter(Boolean);
        items.push({ type: 'question', key, agent: String(e.agent ?? ''), prompt: String(e.prompt ?? e.question ?? e.text ?? ''), options, answered: false });
        break;
      }
      case 'batch': {
        if (view !== null && !mine(e.agent)) break;
        const id = String(e.id ?? key);
        const n = (v: unknown, d: number) => (typeof v === 'number' && Number.isFinite(v) ? v : d);
        const nodes = Array.isArray(e.nodes) ? e.nodes.filter((x: unknown) => typeof x === 'string') : [];
        const frame = typeof e.frame === 'string' ? e.frame : typeof e.node === 'string' ? e.node : null;
        const b = batches.get(id);
        if (b) {
          b.model = String(e.model ?? b.model);
          b.total = n(e.total, b.total);
          b.done = n(e.done, b.done);
          b.failed = n(e.failed, b.failed);
          b.running = n(e.running, b.running);
          b.frame = frame ?? b.frame;
          if (nodes.length) b.nodes = nodes;
        } else {
          const it = { type: 'batch' as const, key, id, model: String(e.model ?? ''), total: n(e.total, 0), done: n(e.done, 0), failed: n(e.failed, 0), running: n(e.running, 0), frame, nodes };
          batches.set(id, it);
          items.push(it);
        }
        break;
      }
      case 'files': {
        if (view !== null && !mine(e.agent)) break;
        const files = filesOf(e);
        if (files.length) items.push({ type: 'files', key, files, dir: String(e.dir ?? '') });
        break;
      }
      case 'app': {
        if (view !== null && !mine(e.agent)) break;
        const app = String(e.app ?? '');
        if (!app) break;
        const card = { type: 'app' as const, key, app, node: String(e.node ?? ''), name: String(e.name ?? app), version: Number(e.version) || 0, files: Array.isArray(e.files) ? e.files.map(String) : [] };
        // Same app again in this turn (a fix after a check): update the card.
        let i = items.length - 1;
        while (i >= 0 && items[i].type !== 'user' && !(items[i].type === 'app' && (items[i] as typeof card).app === app)) i--;
        if (i >= 0 && items[i].type === 'app') items[i] = { ...card, key: items[i].key };
        else items.push(card);
        break;
      }
      case 'queued':
        queued.set(String(e.id), String(e.text ?? ''));
        break;
      case 'dequeued':
        queued.delete(String(e.id));
        if (/^steer/.test(String(e.reason ?? ''))) steered.add(String(e.id));
        break;
      case 'capabilities': {
        const missing = (Array.isArray(e.missing) ? e.missing : [])
          .map((m: any) => (typeof m === 'string' ? { name: m, fix: '' } : { name: String(m?.name ?? ''), fix: String(m?.fix ?? '') }))
          .filter((m: { name: string }) => m.name);
        caps = { available: Array.isArray(e.available) ? e.available.map(String) : [], missing };
        break;
      }
      case 'done': {
        running = false;
        settleThinking();
        if (view !== null) break;
        // The summary only shows when the director said nothing else (no duplicate text).
        const it: DoneItem = { type: 'done', key, summary: textInTurn ? '' : String(e.summary ?? ''), nodes: e.nodes ?? [], turn, reverted: false };
        items.push(it);
        if (turn) turns.set(turn, it);
        break;
      }
    }
  });
  const swarm = [...stats.entries()]
    .filter(([, a]) => running && (a.state === 'started' || a.state === 'working'))
    .map(([agent, a]) => ({ agent, role: a.role, status: a.status, tools: a.tools.size }));
  const liveBatches = [...batches.values()].filter((b) => running && b.done + b.failed < b.total);
  const question = [...items].reverse().find((x): x is Extract<Item, { type: 'question' }> => x.type === 'question' && !x.answered) ?? null;
  const agents: Transcript['agents'] = {};
  for (const [name, a] of stats) agents[name] = { role: a.role, status: a.status, state: a.state };
  return { items, running, cost, swarm, queued: [...queued].map(([id, text]) => ({ id, text })), subagents: [...subs], question, batches: liveBatches, caps, agents };
}

/** One turn of a transcript, in display order: the user's message, the collapsed "work"
 *  (thinking, activity lines, subagent cards, narration between tool calls), the final
 *  assistant text, errors and the done line. Turns never interleave. */
export interface Turn {
  key: string;
  user: Extract<Item, { type: 'user' }> | null;
  work: Item[];
  final: Item[];
  errors: Extract<Item, { type: 'error' }>[];
  done: DoneItem | null;
}

export function groupTurns(items: Item[]): Turn[] {
  const out: Turn[] = [];
  let cur: Turn | null = null;
  const start = (key: string, user: Turn['user']) => {
    cur = { key, user, work: [], final: [], errors: [], done: null };
    out.push(cur);
    return cur;
  };
  for (const it of items) {
    if (it.type === 'user') {
      start(it.key, it);
      continue;
    }
    // After a done line, more output is a new turn (an external agent's next report must not
    // fold the previous one into its collapsed work section). A late error stays with its turn.
    const open = cur as Turn | null;
    const t: Turn = open && (!open.done || it.type === 'error') ? open : start(`t${it.key}`, null);
    if (it.type === 'text' || it.type === 'question' || it.type === 'files' || it.type === 'app') t.final.push(it);
    else if (it.type === 'error') t.errors.push(it);
    else if (it.type === 'done') t.done = it;
    else {
      // Text before more work was narration: it belongs to the work section.
      if (t.final.length) {
        // File and app cards stay visible; narration text moves into the work section.
        t.work.push(...t.final.filter((x) => x.type !== 'files' && x.type !== 'app'));
        t.final = t.final.filter((x) => x.type === 'files' || x.type === 'app');
      }
      t.work.push(it);
    }
  }
  return out;
}

/** `thought 12s · 66 generations · 2 edits` for a collapsed work section. */
export function workSummary(work: Item[]): string {
  let think = 0;
  let thought = false;
  const counts = new Map<string, number>();
  const add = (k: string, n = 1) => counts.set(k, (counts.get(k) ?? 0) + n);
  const NOUN: Record<Verb, [string, string]> = {
    look: ['read', 'reads'], gen: ['generation', 'generations'], edit: ['edit', 'edits'], import: ['import', 'imports'],
    file: ['file edit', 'file edits'], bash: ['command', 'commands'], web: ['search', 'searches'], codex: ['codex task', 'codex tasks'],
    chat: ['message', 'messages'], comment: ['comment', 'comments'], undo: ['undo', 'undos'], other: ['tool call', 'tool calls'],
  };
  for (const it of work) {
    if (it.type === 'thinking') {
      thought = true;
      think += it.seconds ?? 0;
    } else if (it.type === 'batch') add('gen', it.total);
    else if (it.type === 'act') add(it.verb, it.verb === 'gen' ? Math.max(it.calls.length, actNodes(it).length) : it.calls.length);
    else if (it.type === 'sub' && it.phase === 'start') add('agents');
  }
  const parts: string[] = [];
  if (thought) parts.push(think ? `thought ${Math.max(1, Math.round(think))}s` : 'thought');
  for (const [k, n] of counts) {
    if (k === 'agents') parts.push(`${n} agent${n === 1 ? '' : 's'}`);
    else {
      const [one, many] = NOUN[k as Verb];
      parts.push(`${n} ${n === 1 ? one : many}`);
    }
  }
  return parts.join(' · ');
}

/** Error text fit for the transcript: never raw diagnostics. */
export function cleanError(text: string): string {
  if (/\[ede_diagnostic\]/i.test(text) || /^\s*[{[]/.test(text)) return 'the director stopped mid-step';
  const first = text.split('\n')[0].trim();
  return first.length > 160 ? `${first.slice(0, 159)}…` : first || 'the director stopped';
}

/** `summary` without a leading verb from `words` (the host may already say "ran …"). */
export function stripLead(summary: string, words: string[]): string {
  const t = summary.trim();
  for (const w of words) {
    const m = new RegExp(`^${w.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}(?:[\\s:·-]+|$)`, 'i').exec(t);
    if (m) return t.slice(m[0].length).trim();
  }
  return t;
}

/** Whether a summary already starts with a verb ("generated …", "ran …", "wrote …"). */
const VERBISH = /^(?:[a-z]+ed|ran|wrote|made|built|sent|read|found|got|did|took|saw|set|put|running|writing|reading|generating|editing|making|building|looking|searching|sending)\b/i;

/** An agent's role and status for one line, without repeating its name: `motion · motion ·
 *  Build film` → role '' and status 'Build film'. */
export function agentLine(agent: string, role: string, status: string): { role: string; status: string } {
  const a = agent.trim().toLowerCase();
  let st = status.trim();
  for (let i = 0; i < 3; i++) {
    const next = stripLead(st, [agent, role].filter(Boolean));
    if (next === st) break;
    st = next;
  }
  const r = role.trim();
  const sameRole = !r || r.toLowerCase() === a || st.toLowerCase().startsWith(r.toLowerCase());
  return { role: sameRole ? '' : r, status: st };
}

/** Past-tense (or, while running, present) label for an activity line. `kindOf` maps node
 *  ids to node kinds, to say "images" or "videos". */
export function actLabel(it: ActItem, kindOf: (id: string) => string | undefined): string {
  const c = it.calls;
  const n = c.length;
  const runningNow = c.some((x) => x.state === 'running');
  const failed = c.every((x) => x.state === 'error');
  const nodes = actNodes(it);
  const plural = (k: number, w: string) => `${k} ${w}${k === 1 ? '' : 's'}`;
  const media = () => {
    const kinds = new Set(nodes.map(kindOf).filter(Boolean));
    const k = kinds.size === 1 ? [...kinds][0]! : 'output';
    return plural(nodes.length || n, k === 'image' || k === 'video' || k === 'audio' ? k : 'output');
  };
  const first = c[0].summary;
  let s: string;
  switch (it.verb) {
    case 'look':
      s = runningNow ? 'looking at the canvas' : nodes.length ? `looked at ${plural(nodes.length, 'node')}` : n > 1 ? `looked at the canvas ${n}×` : 'looked at the canvas';
      break;
    case 'gen': {
      // Summaries are `model · prompt` (older runtimes) or already `generated 4 images (model)`.
      const modelOf = (x: Call) => (VERBISH.test(x.summary) ? /\(([^)]+)\)/.exec(x.summary)?.[1] ?? '' : x.summary.split(' · ')[0]);
      const models = [...new Set(c.map(modelOf).filter((m) => m && !/^fal Agent/.test(m) && !/failed/.test(m)))];
      const via = c.some((x) => x.name === 'fal_agent') ? 'fal Agent' : models.slice(0, 2).join(', ');
      const what = runningNow && !nodes.length ? (n > 1 ? `${n} outputs` : '') : media();
      // One call whose summary already says it all ("generated 4 images (flux/schnell)").
      if (n === 1 && VERBISH.test(first) && !nodes.length) s = first;
      else s = [runningNow ? 'generating' : 'generated', what, via ? `(${via})` : ''].filter(Boolean).join(' ');
      break;
    }
    case 'edit':
      s = runningNow ? 'editing the canvas' : nodes.length ? `edited ${plural(nodes.length, 'node')}` : n > 1 ? `edited the canvas ${n}×` : 'edited the canvas';
      break;
    case 'import':
      s = `${runningNow ? 'importing' : 'imported'} ${plural(n, 'file')}`;
      break;
    case 'file': {
      // "wrote X, edited X" is just "wrote X": a file written in this line counts as written.
      const WRITE = ['Write', 'wrote', 'writing', 'created', 'creating'];
      const EDIT = ['MultiEdit', 'NotebookEdit', 'Edit', 'edited', 'editing', 'updated', 'updating'];
      const wrote: string[] = [];
      const edited: string[] = [];
      for (const x of c) {
        const w = x.name === 'Write' || WRITE.some((v) => stripLead(x.summary, [v]) !== x.summary.trim());
        const f = stripLead(stripLead(x.summary, WRITE), EDIT).split('/').pop() ?? '';
        if (!f) continue;
        if (w) {
          if (!wrote.includes(f)) wrote.push(f);
        } else if (!edited.includes(f)) edited.push(f);
      }
      const onlyEdited = edited.filter((f) => !wrote.includes(f));
      const list = (fs: string[]) => `${fs.slice(0, 3).join(', ')}${fs.length > 3 ? ` +${fs.length - 3}` : ''}`;
      const parts: string[] = [];
      if (wrote.length) parts.push(`${runningNow ? 'writing' : 'wrote'} ${list(wrote)}`);
      if (onlyEdited.length) parts.push(`${runningNow ? 'editing' : 'edited'} ${list(onlyEdited)}`);
      s = parts.join(', ') || `${runningNow ? 'editing' : 'edited'} ${plural(n, 'file')}`;
      break;
    }
    case 'bash': {
      const cmd = stripLead(first, ['ran', 'running', 'run', 'Bash']);
      s = n > 1 ? `${runningNow ? 'running' : 'ran'} ${n} commands` : `${runningNow ? 'running' : 'ran'} ${cmd || 'a command'}`;
      break;
    }
    case 'web':
      s = `${runningNow ? 'searching' : 'searched'} the web${n > 1 ? ` ${n}×` : ''}`;
      break;
    case 'codex':
      s = `codex: ${stripLead(first, ['codex']) || (runningNow ? 'working' : 'done')}`;
      break;
    case 'chat':
      s = `${runningNow ? 'messaging' : 'messaged'} another chat`;
      break;
    case 'comment':
      s = `${runningNow ? 'commenting' : 'commented'}${n > 1 ? ` ${n}×` : ''}`;
      break;
    case 'undo':
      s = `${runningNow ? 'undoing' : 'undid'} changes`;
      break;
    default: {
      const name = c[0].name;
      s = first.toLowerCase().startsWith(name.toLowerCase()) || VERBISH.test(first) ? first : `${name} ${first}`.trim();
    }
  }
  if (failed) s += ' · failed';
  return runningNow ? `${s}…` : s;
}

/** All node ids an activity line touched. */
export const actNodes = (it: ActItem) => [...new Set(it.calls.flatMap((x) => x.nodes))];
export const actCost = (it: ActItem) => it.calls.reduce((a, x) => a + x.cost, 0);

// ---- chips -----------------------------------------------------------------------------

const TAG: Record<string, string> = {
  image: 'img', video: 'video', audio: 'audio', text: 'text', note: 'note', frame: 'frame', generation: 'gen',
  model3d: '3d', document: 'pdf', vector: 'svg', group: 'group',
};

const r2 = (v: number) => (Math.round(v * 100) / 100).toFixed(2);

/** `[img hero-2]`, `[spot hero-2 0.42,0.31]`, `[frame clip-1 @3.24s box]`, … */
export function chipLabel(c: Chip, node?: { name: string; kind: string } | null, chatName?: string): string {
  if (c.kind === 'chat') return `#${slug(chatName ?? c.chat)}`;
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
