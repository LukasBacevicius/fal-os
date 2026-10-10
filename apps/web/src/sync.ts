// Live sync with the local fal-os host (crates/host/PROTOCOL.md):
//   ws://<host>/sync/<canvas>?token=…&actor=…&name=…
// Binary frames are Loro bytes both ways (first one = full snapshot), text frames are JSON
// events. Also: agent presence (smoothed cursors that fade when idle), claims, job progress,
// toasts for claim revocations and assignments, and asset upload/download over /assets.

import { discoverHost, explicitHost, type HostInfo } from './host';
import { getState, setState, toast } from './store';
import type { CanvasApp } from './wasm';
import type { NodeView, Presence } from './types';

export type ConnState = 'local' | 'connecting' | 'connected' | 'reconnecting';

export interface SessionInfo {
  id: string;
  agentName: string;
  actor: string;
  kind: 'human' | 'agent';
  color: string;
  presence: { status: string; canvas?: string; cursor?: [number, number]; viewport?: [number, number, number, number]; last_activity: number };
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

  constructor(
    private app: CanvasApp,
    private host: HostInfo,
    private canvas: string,
    private actor: { id: string; name: string },
  ) {}

  get info() { return this.host; }
  get connected() { return this.ws?.readyState === WebSocket.OPEN && this.bootstrapped; }

  start() {
    // The wasm client calls `send(kind, payload)` after local transactions and reads
    // `connected` to decide whether to send at all.
    const self = this;
    this.app.setTransport({
      send: (kind: string, payload: unknown) => self.sendFromApp(kind, payload),
      get connected() { return self.connected; },
    });
    this.connect();
  }

  stop() {
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
    const ws = new WebSocket(`${this.base('ws')}/sync/${encodeURIComponent(this.canvas)}?${q}`);
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
        // First contact: the host's canvas replaces whatever was local.
        this.app.loadSnapshot(bytes, true);
        this.bootstrapped = true;
      } else {
        // Resync after a reconnect: merge (CRDT), then offline edits flow out.
        this.app.importUpdates(bytes);
      }
      this.retry = 0;
      setState({ conn: 'connected', canvasName: this.app.canvasName() });
      this.app.tick();
      this.syncAssets(getState().nodes);
    } catch (e) {
      toast(`Sync failed: ${e}`);
      this.ws?.close();
    }
  }

  private onEvent(m: any) {
    switch (m.type) {
      case 'hello':
        this.me = m.session;
        this.sessions = new Map((m.sessions as SessionInfo[]).map((s) => [s.id, s]));
        this.onClaims(m.claims ?? []);
        this.publishPresence();
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
      case 'annotation_assigned':
        toast(`@${m.by} assigned feedback on ${m.node} to @${m.assignee}`);
        break;
      case 'job':
        setState((s) => ({ jobs: { ...s.jobs, [m.node]: m as JobInfo } }));
        if (m.status === 'failed') toast(`Generation failed: ${m.error ?? m.model}`);
        break;
      case 'error':
        toast(`Host: ${m.message}`);
        break;
    }
  }

  private onClaims(claims: { node: string; session: string; actor: string }[]) {
    const color = (session: string) => this.sessions.get(session)?.color ?? '#8b5cf6';
    const list = claims.map((c) => ({ node: c.node, actor: c.actor, color: color(c.session) }));
    this.app.setClaims(list);
    setState({ claims: list });
  }

  // ---- presence ----------------------------------------------------------------------

  /** Other sessions on this canvas → store + engine cursors (smoothed, idle ones fade). */
  private publishPresence(changed?: string) {
    const now = performance.now();
    const others = [...this.sessions.values()].filter((s) => s.id !== this.me?.id);
    setState({ sessions: others });
    for (const s of others) {
      const c = s.presence.cursor;
      if (!c || (s.presence.canvas && s.presence.canvas !== this.canvas)) continue;
      const cur = this.cursors.get(s.id);
      if (!cur) this.cursors.set(s.id, { x: c[0], y: c[1], tx: c[0], ty: c[1], seen: now });
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
      list.push({
        actor: s.actor, name: s.agentName, kind: s.kind, color: s.color,
        cursor: [c.x, c.y], status: s.presence.status || null, opacity,
      });
    }
    // The engine draws cursors (full opacity); labels fade in the DOM layer.
    this.app.setPresence(list.filter((p) => (p.opacity ?? 1) > 0.5));
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
    return `${this.base('http')}/assets/${hex}${withToken ? `?token=${encodeURIComponent(this.host.token)}` : ''}`;
  }

  private auth() { return { Authorization: `Bearer ${this.host.token}` }; }

  /** Uploads a locally imported asset unless the host already has it (dedupe by sha256). */
  async upload(id: string) {
    if (!id.startsWith('sha256:') || this.uploaded.has(id)) return;
    this.uploaded.add(id);
    const blob = this.app.assetBlob(id);
    if (!blob) return;
    try {
      const head = await fetch(this.assetUrl(id), { method: 'HEAD', headers: this.auth() });
      if (head.ok) return;
      const r = await fetch(this.assetUrl(id), {
        method: 'PUT',
        headers: { ...this.auth(), 'Content-Type': blob.type || 'application/octet-stream' },
        body: blob,
      });
      if (!r.ok) throw new Error(`${r.status} ${await r.text()}`);
    } catch (e) {
      this.uploaded.delete(id);
      toast(`Upload to host failed: ${e}`);
    }
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
      if (n.kind === 'image' || n.kind === 'vector') {
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

/** Connects the canvas to the host if one is found. Returns whether a host was found. */
export async function connectHost(app: CanvasApp, canvas: string, actor: { id: string; name: string }): Promise<boolean> {
  const host = await discoverHost();
  if (!host) {
    setState({ conn: 'local' });
    return false;
  }
  current?.stop();
  current = new HostSync(app, host, canvas, actor);
  setState({ hostAddr: host.addr });
  current.start();
  return true;
}
