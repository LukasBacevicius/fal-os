// fal access for the swarm, owned by the runtime (FAL_KEY never reaches the agents):
// - fal Agent SDK tasks (agent.stream) with activities, artifacts, questions/approvals, cancel
// - direct model calls on the queue (fallback / precise control)
// - fal storage uploads for local media, and public model schemas
import { readFile } from "node:fs/promises";
import { basename, extname } from "node:path";
import { createFalClient, type FalClient } from "@fal-ai/client";
import { falKey } from "./config.ts";

let client: FalClient | null = null;
export function fal(): FalClient {
  if (client) return client;
  const key = falKey();
  if (!key) throw new Error("no FAL_KEY: put `FAL_KEY=…` in ~/.fal-os/fal.env (key with the AGENT preset)");
  client = createFalClient({ credentials: key });
  return client;
}

export type AgentAvailability = { ok: true } | { ok: false; reason: string };
let availability: Promise<AgentAvailability> | null = null;

let availabilityAt = 0;
/** Re-check a cached "ok" after this long; a failure is re-checked on every refresh. */
const AVAILABILITY_TTL_MS = 10 * 60 * 1000;

/** Whether the fal Agent API accepts our key (AGENT preset): a cached `GET /agent/projects`. */
export function falAgentAvailable(refresh = false): Promise<AgentAvailability> {
  if (refresh && availability) {
    // Keep a fresh "ok"; re-check failures and stale results.
    availability = availability.then((a) => (a.ok && Date.now() - availabilityAt < AVAILABILITY_TTL_MS ? a : check()));
    return availability;
  }
  availability ??= check();
  return availability;
}

async function check(): Promise<AgentAvailability> {
  availabilityAt = Date.now();
  try {
    await fal().agent.projects.list({ timeoutMs: 20_000 });
    return { ok: true };
  } catch (e: any) {
    const msg = e?.cause?.body?.error?.message ?? e?.body?.detail ?? e?.message ?? String(e);
    return { ok: false, reason: `${e?.status ?? ""} ${typeof msg === "string" ? msg : JSON.stringify(msg)}`.trim().slice(0, 200) };
  }
}

// ---- fal Agent projects ---------------------------------------------------------------------------

const projectLinks = new Map<string, Promise<string>>();

/** fal Agent project names are 1–40 characters. */
export const falProjectName = (name: string) => (name.trim() || "fal-os project").slice(0, 40).trim();

/**
 * The fal Agent project linked to fal-os project `key`: `existing` when set, else one with the
 * same name (reused), else a new one. One in-flight lookup per project, so parallel chats
 * don't create duplicates.
 */
export function ensureFalProject(key: string, name: string, existing?: string | null): Promise<string> {
  if (existing) return Promise.resolve(existing);
  let p = projectLinks.get(key);
  if (!p) {
    p = (async () => {
      const projects = fal().agent.projects;
      const want = falProjectName(name);
      const found = (await projects.list({ timeoutMs: 20_000 })).find((x: any) => x.name === want);
      if (found) return found.id as string;
      const created = await projects.create({ name: want }, { timeoutMs: 20_000 });
      return created.id as string;
    })();
    projectLinks.set(key, p);
    p.catch(() => projectLinks.delete(key));
  }
  return p;
}

/** A new conversation inside a fal Agent project (one per fal-driver chat). */
export async function createFalConversation(project: string, title: string): Promise<string> {
  const c = await fal().agent.projects.conversations.create(project, { title: title.slice(0, 80) || null }, { timeoutMs: 20_000 });
  return c.id as string;
}

export const MIME: Record<string, string> = {
  ".png": "image/png", ".jpg": "image/jpeg", ".jpeg": "image/jpeg", ".webp": "image/webp", ".gif": "image/gif",
  ".mp4": "video/mp4", ".mov": "video/quicktime", ".webm": "video/webm",
  ".mp3": "audio/mpeg", ".wav": "audio/wav", ".m4a": "audio/mp4", ".ogg": "audio/ogg", ".flac": "audio/flac",
};

export const EXT: Record<string, string> = Object.fromEntries(
  Object.entries(MIME).map(([e, m]) => [m, e]).concat([["image/jpg", ".jpg"], ["audio/mp3", ".mp3"], ["audio/x-wav", ".wav"]]),
);

/** Upload a local file to fal storage; returns its public URL. */
export async function uploadFile(path: string): Promise<string> {
  const bytes = await readFile(path);
  const type = MIME[extname(path).toLowerCase()] ?? "application/octet-stream";
  const blob = new File([bytes], basename(path), { type });
  return fal().storage.upload(blob);
}

// ---- fal Agent ---------------------------------------------------------------------------------

export type AgentSnapshot = {
  id: string;
  status: string;
  phase?: string;
  text: string;
  activities: { id: string; label: string; status: string; kind: string; name?: string; args?: unknown }[];
  artifacts: { id: string; media_type?: string; url: string; mime: string; metadata?: unknown }[];
  costUsd: number;
  /** The agent's plan, when it shares one. */
  plan?: { title: string; state: string }[];
};

export function snapshotOf(r: any): AgentSnapshot {
  const c = r?.usage?.cost;
  const artifacts = ((r.final_artifacts?.length ? r.final_artifacts : r.artifacts) ?? [])
    .filter((a: any) => a.kind === "media" || a.media_type)
    .flatMap((a: any) => {
      const f = (a.files ?? []).find((f: any) => f.role === "output" || f.role === "primary") ?? a.files?.[0];
      return f ? [{ id: a.id, media_type: a.media_type, url: f.url, mime: f.mime_type, metadata: a.metadata }] : [];
    });
  return {
    id: r.id,
    status: r.status,
    phase: r.fal?.phase,
    text: r.output_text ?? "",
    activities: (r.fal?.activities ?? []).map((a: any) => ({ id: a.id, label: a.label, status: a.status, kind: a.kind, name: a.name, args: a.arguments })),
    artifacts,
    costUsd: Number(c?.settled ?? c?.estimated ?? c?.reserved ?? 0) || 0,
    plan: planOf(r.fal?.plan ?? r.plan),
  };
}

function planOf(p: any): { title: string; state: string }[] | undefined {
  const steps = Array.isArray(p) ? p : Array.isArray(p?.steps) ? p.steps : null;
  if (!steps?.length) return undefined;
  const state = (s: string) => (/complete|done/.test(s ?? "") ? "done" : /progress|running|active/.test(s ?? "") ? "running" : "pending");
  return steps.map((s: any) => ({ title: String(s.title ?? s.label ?? s.description ?? s.text ?? ""), state: state(String(s.status ?? s.state ?? "")) }));
}

export type InputDecision = { summary: string; answer: any };

/**
 * Run one fal Agent task to completion, answering questions/approvals via `decide`.
 * `onSnapshot` sees every snapshot; `signal` cancels the task server-side too.
 */
export async function runAgentTask(opts: {
  task: string;
  attachments?: { url: string; mime?: string }[];
  onSnapshot: (s: AgentSnapshot) => void;
  decide: (pending: any) => InputDecision | Promise<InputDecision>;
  onDecision?: (d: InputDecision, pending: any) => void;
  signal?: AbortSignal;
  /** Continue a conversation (multi-turn chats on the fal driver, older chats). */
  previousResponseId?: string;
  /** Run inside this conversation (of a fal Agent project): shared project memory. */
  conversation?: string;
}): Promise<AgentSnapshot> {
  const agent = fal().agent;
  const content: any[] = [{ type: "input_text", text: opts.task }];
  for (const a of opts.attachments ?? []) {
    if (a.mime?.startsWith("image/")) content.push({ type: "input_image", image_url: a.url });
    else content.push({ type: "input_file", file_url: a.url, mime_type: a.mime });
  }
  const request: any = { input: content.length > 1 ? [{ role: "user", content }] : opts.task };
  // `conversation` and `previous_response_id` are exclusive.
  if (opts.conversation) request.conversation = opts.conversation;
  else if (opts.previousResponseId) request.previous_response_id = opts.previousResponseId;
  let responseId: string | undefined;
  const onAbort = () => {
    if (responseId) agent.responses.cancel(responseId).catch(() => {});
  };
  opts.signal?.addEventListener("abort", onAbort, { once: true });
  let last: any;
  try {
    let stream: AsyncIterable<any> = agent.stream(request, {
      timeoutMs: 30 * 60 * 1000,
      signal: opts.signal,
      onAccepted: (r) => (responseId = r.id),
    });
    for (let round = 0; round < 20; round++) {
      for await (const r of stream) {
        last = r;
        responseId = r.id;
        opts.onSnapshot(snapshotOf(r));
      }
      if (!last) throw new Error("fal Agent returned no response");
      const pending = (last.pending_inputs ?? []).filter((p: any) => p.status === "pending");
      if (last.fal?.phase !== "waiting_for_input" || pending.length === 0) break;
      for (const p of pending) {
        const d = await opts.decide(p);
        opts.onDecision?.(d, p);
        await agent.responses.answer(last.id, { input_request_id: p.id, answer: d.answer }, { idempotencyKey: `${last.id}:${p.id}` });
      }
      stream = agent.responses.stream(last.id, { timeoutMs: 30 * 60 * 1000, signal: opts.signal });
    }
  } finally {
    opts.signal?.removeEventListener("abort", onAbort);
  }
  const s = snapshotOf(last);
  if (last.status === "failed" || last.error) throw Object.assign(new Error(last.error?.message ?? "fal Agent task failed"), { snapshot: s });
  if (last.status === "cancelled") throw Object.assign(new Error("fal Agent task was cancelled"), { snapshot: s });
  return s;
}

/** Auto-answer: approve within the run (spend isn't capped), pick the first option for questions. */
export function autoDecide(p: any): InputDecision {
  if (p.kind === "approval") {
    const decision = (p.accepted_answers ?? ["approve"]).includes("approve") ? "approve" : "reject";
    return { summary: `${decision}d: ${p.prompt}`, answer: { kind: "approval", decision } };
  }
  const answers = (p.questions ?? []).map((q: any) =>
    q.options?.length
      ? { question_id: q.id, selected_option_ids: [q.options[0].id] }
      : { question_id: q.id, selected_option_ids: [], text: "Use your best judgment for a polished premium product ad." },
  );
  const picked = (p.questions ?? []).map((q: any) => `${q.text} → ${q.options?.[0]?.label ?? "best judgment"}`).join("; ");
  return { summary: `answered: ${picked || p.prompt}`, answer: { kind: "answers", answers } };
}

// ---- direct model calls -----------------------------------------------------------------------

/** Run a fal endpoint on the queue; `onStatus` gets queue/progress updates. */
export async function runModel(
  endpoint: string,
  input: Record<string, unknown>,
  onStatus: (s: string) => void,
  signal?: AbortSignal,
): Promise<{ requestId: string; data: any }> {
  const r: any = await fal().subscribe(endpoint, {
    input,
    logs: true,
    abortSignal: signal,
    onQueueUpdate: (u: any) => {
      if (u.status === "IN_QUEUE") onStatus(`queued${u.queue_position != null ? ` (#${u.queue_position})` : ""}`);
      else if (u.status === "IN_PROGRESS") onStatus(u.logs?.at(-1)?.message?.slice(0, 80) || "running");
    },
  } as any);
  return { requestId: r.requestId, data: r.data };
}

/** All `{url, content_type?}` media objects in a model result, depth-first. */
export function findMedia(v: any, out: { url: string; mime?: string; width?: number; height?: number }[] = []) {
  if (Array.isArray(v)) v.forEach((x) => findMedia(x, out));
  else if (v && typeof v === "object") {
    if (typeof v.url === "string" && v.url.startsWith("http")) out.push({ url: v.url, mime: v.content_type, width: v.width, height: v.height });
    else Object.values(v).forEach((x) => findMedia(x, out));
  }
  return out;
}

/** Input schema of a fal endpoint (public OpenAPI), condensed for an agent. */
export async function modelSchema(endpoint: string): Promise<string> {
  const res = await fetch(`https://fal.ai/api/openapi/queue/openapi.json?endpoint_id=${encodeURIComponent(endpoint)}`);
  if (!res.ok) throw new Error(`no schema for ${endpoint} (${res.status})`);
  const d: any = await res.json();
  const schemas = d?.components?.schemas ?? {};
  const input: any = Object.entries(schemas).find(([k]) => k.endsWith("Input"))?.[1];
  if (!input) throw new Error(`no input schema for ${endpoint}`);
  const lines = Object.entries<any>(input.properties ?? {}).map(([k, p]) => {
    const t = p.enum ? p.enum.join("|") : p.type ?? (p.anyOf ? p.anyOf.map((x: any) => x.type ?? x.$ref?.split("/").pop()).join("|") : "?");
    return `- ${k}${(input.required ?? []).includes(k) ? " (required)" : ""}: ${t}${p.default !== undefined ? ` = ${JSON.stringify(p.default)}` : ""}${p.description ? ` — ${String(p.description).slice(0, 140)}` : ""}`;
  });
  return `${endpoint} input:\n${lines.join("\n")}`;
}

export type UnitPrice = { unit: string; usd: number };
const prices = new Map<string, Promise<UnitPrice | undefined>>();

/** fal's live unit price for an endpoint (platform pricing API), cached per process. */
export function unitPrice(endpoint: string): Promise<UnitPrice | undefined> {
  let p = prices.get(endpoint);
  if (!p) {
    p = (async () => {
      try {
        const res = await fetch(`https://api.fal.ai/v1/models/pricing?endpoint_id=${encodeURIComponent(endpoint)}`, {
          headers: { authorization: `Key ${falKey()}` },
        });
        if (!res.ok) return undefined;
        const d: any = await res.json();
        const e = d?.prices?.find((x: any) => x.endpoint_id === endpoint) ?? d?.prices?.[0];
        return e ? { unit: String(e.unit), usd: Number(e.unit_price) } : undefined;
      } catch {
        return undefined;
      }
    })();
    prices.set(endpoint, p);
  }
  return p;
}

/**
 * USD estimate for a direct call from its unit price, the outputs and their duration
 * (reporting only; fal bills the actual units). Unknown units → undefined.
 */
export function costFromPrice(
  price: UnitPrice | undefined,
  input: any,
  media: { width?: number; height?: number }[],
  durationS?: number,
): number | undefined {
  if (!price || !Number.isFinite(price.usd)) return undefined;
  const n = Math.max(1, media.length);
  const unit = price.unit.toLowerCase();
  const secs = Number(durationS ?? input?.duration ?? input?.seconds_total ?? input?.duration_seconds ?? 5);
  if (/megapixel/.test(unit))
    return media.reduce((s, m) => s + price.usd * Math.max(0.25, ((m.width ?? 1024) * (m.height ?? 1024)) / 1e6), 0) || price.usd;
  if (/^(image|images|video|videos|audio|audios|generation|generations|request|requests|run|runs)$/.test(unit)) return price.usd * n;
  const per = unit.match(/^(\d+)\s*seconds?$/);
  if (per) return price.usd * Math.max(1, Math.ceil(secs / Number(per[1])));
  if (/^seconds?$/.test(unit)) return price.usd * secs;
  if (/^minutes?$/.test(unit)) return price.usd * Math.max(1, secs / 60);
  return undefined;
}
