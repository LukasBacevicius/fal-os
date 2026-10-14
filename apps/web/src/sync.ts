// Live sync with the local fal-os host (crates/host/PROTOCOL.md):
//   ws://<host>/sync/<canvas>?token=…&actor=…&name=…
// Binary frames are Loro bytes both ways (first one = full snapshot), text frames are JSON
// events. Also: agent presence (smoothed cursors that fade when idle), claims, job progress,
// chat (docs/CHAT-PROTOCOL.md: chat.send / chat.stop out, chat.history / chat.event in), and
// asset upload/download over /assets.

import { setUpload } from './dnd';
import { discoverHost, explicitHost, type HostInfo } from './host';
import { getState, setState, toast } from './store';
import { blankSnapshot, type CanvasApp } from './wasm';
import type { NodeView, Presence } from './types';
import { applyEvent, applyHistory, applyList, emptyChat, removeChat, selectChat, type ChatState, type Chip } from './chat';
import { bootProject, loadScene, rememberProject, saveCamera, touchProject } from './projects';
import { onActivity, onActivityClear } from './activity';
import { onAppEvent, setAppsBase } from './apps';
import { clipNodes, restoreNodes } from './actions';

/** A send that gets no chat event back within this time means no agent runtime is attached. */
const RUNTIME_TIMEOUT_MS = 8_000;

export type ConnState = 'local' | 'connecting' | 'connected' | 'reconnecting';

export interface SessionInfo {
  id: string;
  agentName: string;
  actor: string;
  kind: 'human' | 'agent';
  color?: string;
  /** UX-V1: the chat this agent works for, its task label and state (also accepted inside
   *  `presence`). */
  chat?: string | null;
  label?: string;
  state?: 'thinking' | 'writing' | 'idle' | string;
  /** Agent label within its chat (`generator-2`), and its parent agent. */
  agent?: string;
  parent?: string;
  /** Normalized MCP client of external agents (`claude`, `codex`, …). */
  client?: string;
  presence: {
    status: string; canvas?: string; cursor?: [number, number]; viewport?: [number, number, number, number]; last_activity: number;
    chat?: string | null; color?: string; label?: string; state?: string;
  };
}

/** Chat/label/state/color of a session, wherever the host put them. */
export function sessionMeta(s: SessionInfo) {
  return {
    chat: s.chat ?? s.presence?.chat ?? null,
    color: s.presence?.color ?? s.color ?? '',
    label: s.label ?? s.presence?.label ?? '',
    state: s.state ?? s.presence?.state ?? '',
  };
}

/** Stop drops the chat's queued messages: their text goes back into the composer so
 *  nothing the user typed is lost. */
function restoreQueued(chat: string, id: string) {
  setState((s) => {
    const q = s.chat.threads[chat]?.events.find((e: any) => e.kind === 'queued' && String(e.id) === id) as { text?: string } | undefined;
    const text = String(q?.text ?? '').trim();
    if (!text) return {};
    const cur = s.drafts[chat] ?? '';
    return { drafts: { ...s.drafts, [chat]: cur.trim() ? `${cur}\n${text}` : text } };
  });
}

/** `#rrggbb` + alpha byte. */
function dimmed(color: string, alpha: number): string {
  if (!/^#[0-9a-f]{6}$/i.test(color)) return color;
  return color + Math.round(alpha * 255).toString(16).padStart(2, '0');
}

export interface JobInfo { node: string; model: string; status: string; queue_position?: number; error?: string }

/** Cursors fade out this long after an actor's last presence change. */
export const IDLE_MS = 10_000;
const PRESENCE_HZ = 20;

interface Cursor { x: number; y: number; tx: number; ty: number; seen: number }

export class HostSync {
  private ws: WebSocket | null = null;
  private retry = 0;
  private timer = 0;
  private closed = false;
  /** True once a snapshot replaced the local canvas; later snapshots (resyncs) merge. */
  private bootstrapped = false;
  private me: SessionInfo | null = null;
  private sessions = new Map<string, SessionInfo>();
  private cursors = new Map<string, Cursor>();
  private anim = 0;
  private lastPresence = 0;
  private pendingPresence: Record<string, unknown> | null = null;
  private presenceTimer = 0;
  private uploaded = new Set<string>();
  private requested = new Set<string>();
  /** Nodes in the scene when this sync started (a cached snapshot, or the previous
   *  project's canvas while switching): anything else before first contact was made here. */
  private baseIds = new Set<string>();

  constructor(
    private app: CanvasApp,
    private host: HostInfo,
    private canvas: string,
    readonly actor: { id: string; name: string },
    /** The project (`/sync/<project>`, assets `?project=`); null = an older host's single
     *  canvas (`/sync/<canvas>`). */
    readonly project: string | null = null,
  ) {}

  get info() { return this.host; }
  /** Key for per-project memory (camera, caches). */
  get key() { return this.project ?? this.canvas; }
  get connected() { return this.ws?.readyState === WebSocket.OPEN && this.bootstrapped; }

  start() {
    // The wasm client calls `send(kind, payload)` after local transactions and reads
    // `connected` to decide whether to send at all.
    const self = this;
    this.baseIds = new Set((this.app.nodes() as NodeView[]).map((n) => n.id));
    this.app.setTransport({
      send: (kind: string, payload: unknown) => self.sendFromApp(kind, payload),
      get connected() { return self.connected; },
    });
    window.addEventListener('focus', this.onFocus);
    this.connect();
  }

  private onFocus = () => this.focus();

  stop() {
    window.removeEventListener('focus', this.onFocus);
    this.closed = true;
    window.clearTimeout(this.timer);
    cancelAnimationFrame(this.anim);
    window.clearTimeout(this.timer2);
    this.ws?.close();
    this.app.setTransport(null);
  }

  private base(scheme: 'ws' | 'http') { return `${scheme}://${this.host.addr}`; }

  private connect() {
    if (this.closed) return;
    setState({ conn: this.bootstrapped ? 'reconnecting' : 'connecting' });
    const q = new URLSearchParams({ token: this.host.token, actor: this.actor.id, name: this.actor.name });
    const ws = new WebSocket(`${this.base('ws')}/sync/${encodeURIComponent(this.project ?? this.canvas)}?${q}`);
    ws.binaryType = 'arraybuffer';
    this.ws = ws;
    let gotSnapshot = false;
    ws.onmessage = (e) => {
      if (typeof e.data === 'string') return this.onEvent(JSON.parse(e.data));
      const bytes = new Uint8Array(e.data as ArrayBuffer);
      if (!gotSnapshot) {
        gotSnapshot = true;
        this.onSnapshot(bytes);
      } else {
        this.app.receive('update', bytes);
      }
    };
    ws.onclose = () => {
      if (this.ws !== ws) return;
      this.ws = null;
      this.sessions.clear();
      this.publishPresence();
      if (this.closed) return;
      setState({ conn: 'reconnecting' });
      const delay = Math.min(10_000, 500 * 2 ** this.retry) * (0.75 + Math.random() * 0.5);
      this.retry = Math.min(this.retry + 1, 6);
      this.timer = window.setTimeout(() => this.reconnect(), delay);
    };
  }

  private async reconnect() {
    // A restarted host may listen on a new port with a new token.
    if (!explicitHost()) {
      const h = await discoverHost();
      if (h) this.host = h;
    }
    this.connect();
  }

  private onSnapshot(bytes: Uint8Array) {
    try {
      if (!this.bootstrapped) {
        // First contact: the host's canvas replaces whatever was local. The project's last
        // camera comes back; a canvas opened for the first time fits its content. Nodes the
        // user made before the host answered (it was down or still starting) are carried
        // over instead of vanishing with the local canvas.
        const made = (this.app.nodes() as NodeView[]).filter((n) => !this.baseIds.has(n.id)).map((n) => n.id);
        const keep = made.length ? clipNodes(made) : null;
        loadScene(this.app, bytes, this.key);
        this.bootstrapped = true;
        if (keep) {
          const kept = restoreNodes(keep);
          if (kept.length) toast(`kept ${kept.length} node${kept.length === 1 ? '' : 's'} made while the host was unreachable`);
        }
      } else {
        // Resync after a reconnect: merge (CRDT), then offline edits flow out.
        this.app.importUpdates(bytes);
      }
      this.retry = 0;
      setState({ conn: 'connected', canvasName: this.app.canvasName(), camera: this.app.camera() });
      this.app.tick();
      this.syncAssets(getState().nodes);
    } catch (e) {
      toast(`Sync failed: ${e}`);
      this.ws?.close();
    }
  }

  private onEvent(m: any) {
    switch (m.type) {
      case 'hello': {
        this.me = m.session;
        this.sessions = new Map((m.sessions as SessionInfo[]).map((s) => [s.id, s]));
        this.onClaims(m.claims ?? []);
        this.publishPresence();
        setAppsBase(m.apps, m.project ?? this.project);
        break;
      }
      case 'app':
        onAppEvent(m);
        break;
      case 'chat.history':
        setState((s) => ({ chat: applyHistory(s.chat, m.chats ?? []) }));
        break;
      case 'chat.list':
        setState((s) => ({ chat: applyList(s.chat, m.chats ?? []) }));
        break;
      case 'chat.created':
        if (typeof m.chat === 'string')
          setState((s) => ({ chat: s.chat.creating ? { ...selectChat(s.chat, m.chat), creating: false, order: s.chat.order.includes(m.chat) ? s.chat.order : [...s.chat.order, m.chat] } : s.chat }));
        break;
      case 'chat.event':
        if (typeof m.chat === 'string' && m.event) {
          if (m.event.kind === 'dequeued' && m.event.reason === 'stopped') restoreQueued(m.chat, String(m.event.id));
          setState((s) => ({ chat: applyEvent(s.chat, m.chat, m.seq, m.event, typeof m.ts === 'number' ? m.ts : Date.now()) }));
        }
        break;
      case 'chat.deleted':
        if (typeof m.chat === 'string') setState((s) => ({ chat: removeChat(s.chat, m.chat), subView: s.chat.active === m.chat ? null : s.subView }));
        break;
      case 'activity':
        onActivity(m);
        break;
      case 'activity.clear':
        onActivityClear(m);
        break;
      case 'agent.registered':
        setState((s) => ({ connect: { ...s.connect, registered: { ...s.connect.registered, [m.client]: { ok: !!m.ok, path: m.path, error: m.error } } } }));
        break;
      case 'director.status':
        setState({ director: { targets: Array.isArray(m.targets) ? m.targets : [], home: m.home, server: m.server, instance: m.instance, changed: m.changed, error: m.error } });
        break;
      case 'agent.continue':
        if (m.error) toast(`continue: ${m.error}`);
        else toast(m.opened ? `opened Terminal: ${m.command}` : m.copied ? `copied: ${m.command}` : String(m.command));
        break;
      case 'agent.connected':
        setState((s) => ({ connect: { ...s.connect, connected: { ...s.connect.connected, [m.client]: { agentName: String(m.agentName ?? m.client), session: String(m.session ?? '') } } } }));
        break;
      case 'agent.first_write':
        setState((s) => ({ connect: { ...s.connect, firstWrite: { ...s.connect.firstWrite, [m.client]: Array.isArray(m.nodes) ? m.nodes : [] } } }));
        window.dispatchEvent(new CustomEvent('falos:first-write', { detail: m }));
        break;
      case 'presence':
        this.sessions.set(m.session.id, m.session);
        this.publishPresence(m.session.id);
        break;
      case 'session_closed':
        this.sessions.delete(m.session);
        this.cursors.delete(m.session);
        this.publishPresence();
        break;
      case 'claims':
        this.onClaims(m.claims);
        break;
      case 'claim_revoked':
        toast(`@${m.by} took over ${m.node} from @${m.actor}`);
        break;
      case 'job':
        setState((s) => ({ jobs: { ...s.jobs, [m.node]: m as JobInfo } }));
        break;
      case 'error': {
        const msg = String(m.message ?? '');
        // A host without chat routing rejects chat frames: no agent runtime to talk to.
        if (/chat/i.test(msg)) {
          setState((s) => ({ chat: { ...s.chat, creating: false, runtime: s.chat.pending ? 'missing' : s.chat.runtime } }));
        } else {
          toast(`host: ${msg}`);
        }
        break;
      }
    }
  }

  private onClaims(claims: { node: string; session: string; actor: string }[]) {
    const list = claims.map((c) => ({ node: c.node, actor: c.actor }));
    this.app.setClaims(list);
    setState({ claims: list });
  }

  // ---- chat ----------------------------------------------------------------------------

  /** Sends a chat message (`chat` omitted starts a new chat). Returns false when offline. */
  sendChat(text: string, context: Chip[], chat: string | null): boolean {
    if (this.ws?.readyState !== WebSocket.OPEN) return false;
    const msg: Record<string, unknown> = { type: 'chat.send', text, context, driver: getState().driver };
    if (chat) msg.chat = chat;
    this.ws.send(JSON.stringify(msg));
    const at = Date.now();
    setState((s) => ({ chat: { ...s.chat, pending: { text, context, at, chat } } }));
    window.setTimeout(() => {
      const p = getState().chat.pending;
      if (p && p.at === at) setState((s) => ({ chat: { ...s.chat, runtime: 'missing' } }));
    }, RUNTIME_TIMEOUT_MS);
    return true;
  }

  /** Steering: delivers a message to the running turn now (`id` = a queued message, or new
   *  `text`), optionally to one subagent. */
  steerChat(chat: string, m: { id?: string; text?: string; agent?: string | null }): boolean {
    const msg: Record<string, unknown> = { type: 'chat.steer', chat };
    if (m.id) msg.id = m.id;
    if (m.text) msg.text = m.text;
    if (m.agent) msg.agent = m.agent;
    if (!this.sendJson(msg)) return false;
    if (m.text) {
      const at = Date.now();
      setState((s) => ({ chat: { ...s.chat, pending: { text: m.text!, context: [], at, chat } } }));
    }
    return true;
  }

  stopChat(chat: string) {
    this.sendJson({ type: 'chat.stop', chat });
  }

  /** Asks the host for a new chat; `chat.created` makes it the active one. */
  newChat() {
    setState((s) => ({ chat: { ...selectChat(s.chat, null), creating: this.sendJson({ type: 'chat.new' }) } }));
  }

  renameChat(chat: string, title: string) {
    this.sendJson({ type: 'chat.rename', chat, title });
  }

  archiveChat(chat: string) {
    this.sendJson({ type: 'chat.archive', chat });
  }

  deleteChat(chat: string) {
    this.sendJson({ type: 'chat.delete', chat });
  }

  /** Tabs: open / close (a closed chat stays in the history). */
  openChat(chat: string) {
    this.sendJson({ type: 'chat.open', chat });
  }

  closeChat(chat: string) {
    this.sendJson({ type: 'chat.close', chat });
  }

  /** Resumes a chat whose director stopped mid-step (error with `actions: ["continue"]`). */
  continueChat(chat: string) {
    this.sendJson({ type: 'chat.continue', chat });
  }

  unqueue(chat: string, id: string) {
    this.sendJson({ type: 'chat.unqueue', chat, id });
  }

  revertTurn(chat: string, turn: string) {
    this.sendJson({ type: 'chat.revert', chat, turn });
  }

  redoTurn(chat: string, turn: string) {
    this.sendJson({ type: 'chat.redo', chat, turn });
  }

  /** "Connect your agent": the host writes the client's MCP config (`agent.registered`). */
  registerAgent(client: string): boolean {
    return this.sendJson({ type: 'agent.register', client });
  }

  /** Continue this project in the user's own agent: the host copies the command (and with
   *  `open` runs it in Terminal); the reply is `agent.continue`. */
  continueInAgent(agent: 'claude' | 'codex', open: boolean): boolean {
    return this.sendJson({ type: 'agent.continue', agent, open });
  }

  /** This window has focus: external agents bind to the project open here. */
  focus() {
    this.sendJson({ type: 'focus' });
  }

  /** Use fal-os from the user's own Claude Code / desktop app / Codex (`director.status`). */
  director(type: 'status' | 'connect' | 'disconnect', targets: string[] = []): boolean {
    return this.sendJson({ type: `director.${type}`, targets });
  }

  private selTimer = 0;
  private selLast = '';
  /** The user's selection, so agents can see what you're looking at (throttled). */
  sendSelection(nodes: string[]) {
    window.clearTimeout(this.selTimer);
    this.selTimer = window.setTimeout(() => {
      const key = nodes.join(',');
      if (key === this.selLast) return;
      if (this.sendJson({ type: 'selection', nodes })) this.selLast = key;
    }, 150);
  }

  private sendJson(m: unknown): boolean {
    if (this.ws?.readyState !== WebSocket.OPEN) return false;
    this.ws.send(JSON.stringify(m));
    return true;
  }

  // ---- presence ----------------------------------------------------------------------

  /** Other sessions on this canvas → store + engine cursors (smoothed, idle ones fade). */
  private publishPresence(changed?: string) {
    const now = performance.now();
    const others = [...this.sessions.values()].filter((s) => s.id !== this.me?.id);
    setState({ sessions: others });
    for (const s of others) {
      const c = s.presence.cursor;
      // Only agents get cursors (UI-SPEC: real agent sessions with recent activity).
      // Per-project sockets only relay their own project's sessions.
      if (!c || s.kind !== 'agent' || (!this.project && s.presence.canvas && s.presence.canvas !== this.canvas)) continue;
      const cur = this.cursors.get(s.id);
      // A cursor first seen on (re)connect is as old as the session's last activity.
      const age = s.id === changed ? 0 : Math.max(0, Date.now() - (s.presence.last_activity || 0));
      if (!cur) this.cursors.set(s.id, { x: c[0], y: c[1], tx: c[0], ty: c[1], seen: now - age });
      else if (s.id === changed || cur.tx !== c[0] || cur.ty !== c[1]) {
        cur.tx = c[0];
        cur.ty = c[1];
        cur.seen = now;
      }
    }
    if (changed) {
      const cur = this.cursors.get(changed);
      if (cur) cur.seen = now;
    }
    if (!this.anim) this.anim = requestAnimationFrame(this.step);
  }

  private step = () => {
    this.anim = 0;
    const now = performance.now();
    let moving = false;
    const list: Presence[] = [];
    for (const [id, c] of this.cursors) {
      const s = this.sessions.get(id);
      if (!s) { this.cursors.delete(id); continue; }
      const idle = now - c.seen;
      if (idle > IDLE_MS + 600) continue;
      const k = 0.18;
      c.x += (c.tx - c.x) * k;
      c.y += (c.ty - c.y) * k;
      if (Math.abs(c.tx - c.x) + Math.abs(c.ty - c.y) > 0.5) moving = true;
      else { c.x = c.tx; c.y = c.ty; }
      const opacity = idle < IDLE_MS ? 1 : Math.max(0, 1 - (idle - IDLE_MS) / 600);
      const meta = sessionMeta(s);
      list.push({
        actor: s.actor, name: s.agent || s.agentName, kind: s.kind, session: s.id,
        cursor: [c.x, c.y], status: s.presence.status || null, opacity,
        chat: meta.chat, color: meta.color || undefined, label: meta.label || undefined, state: meta.state || undefined,
      });
    }
    // The engine draws cursors (full opacity, dimmed while thinking); labels fade in the DOM.
    this.app.setPresence(
      list.filter((p) => (p.opacity ?? 1) > 0.5).map((p) => ({ ...p, color: p.color ? dimmed(p.color, p.state === 'thinking' ? 0.5 : 1) : '' })),
    );
    setState({ presence: list });
    const fading = list.some((p) => (p.opacity ?? 1) < 1);
    const pendingFade = [...this.cursors.values()].some((c) => now - c.seen <= IDLE_MS + 600);
    if (moving || fading) this.anim = requestAnimationFrame(this.step);
    else if (pendingFade) this.timer2 = window.setTimeout(() => { if (!this.anim) this.anim = requestAnimationFrame(this.step); }, 1000);
  };
  private timer2 = 0;

  /** The local user's cursor / viewport (world units), throttled to ~20 Hz. */
  sendPresence(p: { cursor?: [number, number]; viewport?: [number, number, number, number]; status?: string }) {
    this.pendingPresence = { ...(this.pendingPresence ?? {}), ...p };
    const wait = 1000 / PRESENCE_HZ - (performance.now() - this.lastPresence);
    if (wait <= 0) this.flushPresence();
    else if (!this.presenceTimer) this.presenceTimer = window.setTimeout(() => this.flushPresence(), wait);
  }

  private flushPresence() {
    this.presenceTimer = 0;
    if (!this.pendingPresence || this.ws?.readyState !== WebSocket.OPEN) return;
    this.ws.send(JSON.stringify({ type: 'presence', ...this.pendingPresence }));
    this.pendingPresence = null;
    this.lastPresence = performance.now();
  }

  revokeClaim(node: string) {
    if (this.ws?.readyState === WebSocket.OPEN) this.ws.send(JSON.stringify({ type: 'revoke_claim', node }));
  }

  private sendFromApp(kind: string, payload: unknown) {
    if (this.ws?.readyState !== WebSocket.OPEN) return;
    if (kind === 'update') this.ws.send(payload as Uint8Array);
    else if (kind === 'presence') this.ws.send(JSON.stringify({ type: 'presence', ...(payload as object) }));
  }

  // ---- assets ------------------------------------------------------------------------

  private assetUrl(id: string, withToken = false) {
    const hex = id.replace(/^sha256:/, '');
    const q = new URLSearchParams();
    if (this.project) q.set('project', this.project);
    if (withToken) q.set('token', this.host.token);
    const qs = q.toString();
    return `${this.base('http')}/assets/${hex}${qs ? `?${qs}` : ''}`;
  }

  private auth() { return { Authorization: `Bearer ${this.host.token}` }; }

  /** A tokened URL of a stored asset (downloads, `DownloadURL` drags, `<img>`). */
  assetHref(id: string) { return this.assetUrl(id, true); }

  /** Uploads a locally imported asset unless the host already has it (dedupe by sha256). */
  async upload(id: string) {
    if (!id.startsWith('sha256:') || this.uploaded.has(id)) return;
    this.uploaded.add(id);
    const blob = this.app.assetBlob(id);
    if (!blob) return;
    try {
      const head = await fetch(this.assetUrl(id), { method: 'HEAD', headers: this.auth() });
      if (head.ok) return;
      // XHR for upload progress (a 1px bar under the node while it uploads).
      setUpload(id, 0);
      await new Promise<void>((resolve, reject) => {
        const x = new XMLHttpRequest();
        x.open('PUT', this.assetUrl(id));
        for (const [k, v] of Object.entries({ ...this.auth(), 'Content-Type': blob.type || 'application/octet-stream' })) x.setRequestHeader(k, v as string);
        x.upload.onprogress = (e) => e.lengthComputable && setUpload(id, e.loaded / e.total);
        x.onload = () => (x.status >= 200 && x.status < 300 ? resolve() : reject(new Error(`${x.status} ${x.responseText}`)));
        x.onerror = () => reject(new Error('network error'));
        x.send(blob);
      });
      setUpload(id, null);
    } catch (e) {
      setUpload(id, null);
      this.uploaded.delete(id);
      toast(`Upload to host failed: ${e}`);
    }
  }

  /** Has the host download a remote media URL into the project's assets (pages can't fetch
   *  most CDNs: no CORS). Resolves to the stored asset. */
  async importUrl(url: string, digest: { sha256?: string; sha1?: string } = {}): Promise<{ id: string; mime: string; width: number | null; height: number | null; size: number }> {
    const q = this.project ? `?project=${encodeURIComponent(this.project)}` : '';
    const r = await fetch(`${this.base('http')}/import/url${q}`, {
      method: 'POST',
      headers: { ...this.auth(), 'Content-Type': 'application/json' },
      body: JSON.stringify({ url, ...digest }),
    });
    if (!r.ok) throw new Error(`${r.status} ${(await r.text()).slice(0, 200)}`);
    return await r.json();
  }

  /** The bytes of an asset stored on the host. */
  async assetBlob(id: string): Promise<Blob> {
    const r = await fetch(this.assetUrl(id), { headers: this.auth() });
    if (!r.ok) throw new Error(`asset ${r.status}`);
    return await r.blob();
  }

  /** Marks an asset as present on the host (no upload attempt for it). */
  markUploaded(id: string) {
    this.uploaded.add(id);
  }

  /** Makes assets referenced by nodes available: local blobs are uploaded, missing ones loaded. */
  syncAssets(nodes: NodeView[]) {
    if (!this.connected) return;
    const local = new Map((this.app.assets() as { id: string; remote?: boolean }[]).map((a) => [a.id, a]));
    for (const n of nodes) {
      const id = typeof n.props?.asset === 'string' ? (n.props.asset as string) : null;
      if (!id || !id.startsWith('sha256:')) continue;
      const have = local.get(id);
      if (have && !have.remote) { void this.upload(id); continue; }
      if (have || this.requested.has(id)) continue;
      this.requested.add(id);
      void this.fetchAsset(id, n);
    }
  }

  private async fetchAsset(id: string, n: NodeView) {
    try {
      if (n.kind === 'image' || n.kind === 'vector' || n.kind === 'app') {
        const r = await fetch(this.assetUrl(id), { headers: this.auth() });
        if (!r.ok) throw new Error(String(r.status));
        const blob = await r.blob();
        this.app.registerAsset(id, blob, blob.type, n.name);
      } else {
        // Video, audio, 3D, …: stream by URL (byte ranges), nothing held in memory.
        const r = await fetch(this.assetUrl(id), { method: 'HEAD', headers: this.auth() });
        if (!r.ok) throw new Error(String(r.status));
        this.app.registerRemoteAsset(id, this.assetUrl(id, true), r.headers.get('content-type') ?? '', n.name, Number(r.headers.get('content-length') ?? 0));
      }
    } catch (e) {
      // Not on the host (yet): e.g. a generation still downloading. Retry on a later change.
      window.setTimeout(() => this.requested.delete(id), 3000);
      if (String(e) !== 'Error: 404') console.warn('asset fetch failed', id, e);
    }
  }
}

let current: HostSync | null = null;
export const hostSync = () => current;

function startSync(s: HostSync) {
  current = s;
  if (import.meta.env.DEV) Object.assign(window as any, { falosSync: s, falosSwitch: switchProject }); // debugging
  s.start();
}

/** Connects the canvas to the host if one is found: the project from `/workspace` (URL,
 *  last used or most recent), else the host's single canvas. Returns whether a host was
 *  found. */
export async function connectHost(app: CanvasApp, canvas: string, actor: { id: string; name: string }): Promise<boolean> {
  const host = await discoverHost();
  if (!host) {
    setState({ conn: 'local' });
    return false;
  }
  current?.stop();
  setState({ hostAddr: host.addr, conn: 'connecting' });
  const project = await bootProject(host);
  setState({ project });
  if (project) {
    rememberProject(project);
    touchProject(project);
  }
  startSync(new HostSync(app, host, canvas, actor, project));
  return true;
}

/** In-memory per-project caches so switching back is instant: the last canvas snapshot and
 *  chat state (the host's snapshot and chat history replace them a moment later). */
const snapshots = new Map<string, Uint8Array>();
const chatCache = new Map<string, ChatState>();

/** Swaps the open project: same GPU surface and app, new scene + chats + socket. The new
 *  project's last camera comes back (or it fits its content the first time). */
export function switchProject(project: string): boolean {
  const s = getState();
  const app = s.app;
  const prev = current;
  if (!app || !prev) return false;
  if (prev.project === project) return true;
  if (prev.project) {
    saveCamera(prev.key, app.camera() as { x: number; y: number; zoom: number });
    try {
      snapshots.set(prev.key, app.exportSnapshot());
    } catch {
      /* nothing to cache */
    }
    chatCache.set(prev.key, s.chat);
  }
  const host = prev.info;
  prev.stop();
  current = null;
  app.setSelection([]);
  app.setPresence([]);
  app.setClaims([]);
  app.setHighlight([]);
  // The project's last scene from this session, else an empty canvas until the host
  // answers: never the previous project's nodes under the new name.
  const cached = snapshots.get(project) ?? blankSnapshot();
  if (cached) {
    try {
      loadScene(app, cached, project);
    } catch (e) {
      console.warn('cached snapshot failed', e);
    }
  }
  setState((st) => ({
    project,
    chat: chatCache.get(project) ?? { ...emptyChat(), tabOrder: st.chat.tabOrder },
    chatView: 'chat',
    subView: null,
    selection: [],
    editing: null,
    promptOpen: false,
    confirmDelete: null,
    sessions: [],
    claims: [],
    jobs: {},
    activity: [],
    presence: [],
    conn: 'connecting',
    camera: app.camera(),
  }));
  rememberProject(project);
  touchProject(project);
  startSync(new HostSync(app, host, project, prev.actor, project));
  return true;
}
