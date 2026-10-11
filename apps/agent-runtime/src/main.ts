// fal-os agent runtime: connects to the host's /runtime socket (one for the whole workspace)
// and runs one Claude Agent SDK swarm per chat turn (see docs/CHAT-PROTOCOL.md and README.md).
// Every frame carries `project`; runs are keyed by (project, chat), so agents of different
// projects never mix.
import WebSocket from "ws";
import { probeCapabilities } from "./capabilities.ts";
import { config } from "./config.ts";
import { ChatRun, type Queued, type RunFrame, type Steer } from "./run.ts";

if (!config.token || !config.runtimeToken) {
  console.error("FALOS_TOKEN and FALOS_RUNTIME_TOKEN are required (the host passes them when it spawns the runtime)");
  process.exit(2);
}

const runs = new Map<string, ChatRun>();
const queued = new Map<string, RunFrame[]>();
let ws: WebSocket | null = null;
const outbox: string[] = [];

/** (project, chat) key. Older hosts send no project id. */
const keyOf = (project: unknown, chat: string) => `${typeof project === "string" ? project : ""}\u0000${chat}`;

function send(msg: unknown) {
  const s = JSON.stringify(msg);
  if (ws?.readyState === WebSocket.OPEN) ws.send(s);
  else outbox.push(s);
}

let nextReq = 1;
const waiting = new Map<number, { resolve: (v: any) => void; reject: (e: Error) => void }>();

/** Ask the host something about `project` (cross-chat ops); resolves with `result` or rejects. */
export function hostRequest(project: string | undefined, op: string, args: Record<string, unknown>): Promise<any> {
  const id = nextReq++;
  return new Promise((resolve, reject) => {
    waiting.set(id, { resolve, reject });
    send({ type: "request", id, op, project, ...args });
    setTimeout(() => {
      if (waiting.delete(id)) reject(new Error(`host request ${op} timed out`));
    }, 30_000);
  });
}

function log(message: string) {
  console.log(`[runtime] ${message}`);
}

/** The project id of a run frame (older hosts put the folder in `project`). */
const projectId = (f: RunFrame) => (f.project_dir ? f.project : undefined);

async function start(frame: RunFrame) {
  const key = keyOf(projectId(frame), frame.chat);
  if (runs.has(key)) {
    // One turn at a time per chat; follow-ups run after the current turn.
    queued.set(key, [...(queued.get(key) ?? []), frame]);
    return;
  }
  const project = projectId(frame);
  const tag = (m: Record<string, unknown>) => (project ? { ...m, project } : m);
  const run = new ChatRun(
    frame,
    (event) => send(tag({ type: "event", chat: frame.chat, event })),
    (op, args) => hostRequest(project, op, args),
    (m) => send(tag(m)),
  );
  runs.set(key, run);
  log(`run ${project ?? ""}/${frame.chat}: ${frame.text.slice(0, 80)}`);
  try {
    await run.run();
  } catch (e: any) {
    send(tag({ type: "event", chat: frame.chat, event: { kind: "status", state: "error", error: String(e?.message ?? e) } }));
  } finally {
    runs.delete(key);
    log(`run ${project ?? ""}/${frame.chat} finished`);
    const next = queued.get(key)?.shift();
    if (next) void start(next);
  }
}

function connect() {
  const url = config.hostUrl.replace(/^http/, "ws") + "/runtime";
  ws = new WebSocket(url, { headers: { authorization: `Bearer ${config.token}`, "x-falos-runtime": config.runtimeToken } });
  ws.on("open", () => {
    log(`connected to ${url}`);
    while (outbox.length) ws!.send(outbox.shift()!);
  });
  ws.on("message", (data) => {
    let msg: any;
    try {
      msg = JSON.parse(String(data));
    } catch {
      return;
    }
    const key = msg.chat ? keyOf(msg.project, msg.chat) : "";
    if (msg.type === "reply") {
      const w = waiting.get(msg.id);
      if (w) {
        waiting.delete(msg.id);
        msg.ok ? w.resolve(msg.result) : w.reject(new Error(msg.error ?? "host request failed"));
      }
    } else if (msg.type === "run") void start(msg as RunFrame);
    // A message sent while the chat runs: injected after the next tool call. When the run
    // is already wrapping up it's dropped here and the host re-delivers it as a new run.
    else if (msg.type === "queue") runs.get(key)?.enqueue(msg as Queued);
    else if (msg.type === "unqueue") runs.get(key)?.unqueue(msg.id);
    // Deliver now: interrupt the current step (or tell the host there's nothing running).
    else if (msg.type === "steer") {
      const run = runs.get(key);
      if (run) run.steer(msg as Steer);
      else send({ type: "steer.failed", project: msg.project, chat: msg.chat, id: msg.id, text: msg.text, author: msg.author, chips: msg.chips ?? [] });
    } else if (msg.type === "stop") {
      queued.delete(key);
      runs.get(key)?.stop();
    }
  });
  ws.on("close", () => {
    log("host connection closed; reconnecting");
    setTimeout(connect, 1000);
  });
  ws.on("error", (e) => log(`socket error: ${e.message}`));
}

for (const sig of ["SIGINT", "SIGTERM"] as const)
  process.on(sig, () => {
    for (const r of runs.values()) r.stop();
    setTimeout(() => process.exit(0), 1500);
  });

// Exit when the host that spawned us is gone (e.g. it was SIGKILLed), instead of lingering.
const hostPid = Number(process.env.FALOS_HOST_PID);
if (hostPid > 0)
  setInterval(() => {
    try {
      process.kill(hostPid, 0);
    } catch {
      log("host process gone; exiting");
      for (const r of runs.values()) r.stop();
      setTimeout(() => process.exit(0), 1500);
    }
  }, 2000).unref();

connect();
// What's usable right now (logged; each chat turn re-probes and tells its agents).
setTimeout(() => {
  probeCapabilities()
    .then((c) => log(`capabilities: ${c.available.join(", ")}${c.missing.length ? ` · missing: ${c.missing.map((m) => `${m.name} (${m.fix})`).join("; ")}` : ""}`))
    .catch((e) => log(`capability probe failed: ${e?.message ?? e}`));
}, 1500);
