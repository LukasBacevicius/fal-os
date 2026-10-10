// fal-os agent runtime: connects to the host's /runtime socket and runs one Claude Agent SDK
// swarm per chat turn (see docs/CHAT-PROTOCOL.md and README.md).
import WebSocket from "ws";
import { config } from "./config.ts";
import { ChatRun, type RunFrame } from "./run.ts";

if (!config.token || !config.runtimeToken) {
  console.error("FALOS_TOKEN and FALOS_RUNTIME_TOKEN are required (the host passes them when it spawns the runtime)");
  process.exit(2);
}

const runs = new Map<string, ChatRun>();
const queued = new Map<string, RunFrame[]>();
let ws: WebSocket | null = null;
const outbox: string[] = [];

function send(msg: unknown) {
  const s = JSON.stringify(msg);
  if (ws?.readyState === WebSocket.OPEN) ws.send(s);
  else outbox.push(s);
}

function log(message: string) {
  console.log(`[runtime] ${message}`);
}

async function start(frame: RunFrame) {
  if (runs.has(frame.chat)) {
    // One turn at a time per chat; follow-ups run after the current turn.
    queued.set(frame.chat, [...(queued.get(frame.chat) ?? []), frame]);
    return;
  }
  const run = new ChatRun(frame, (event) => send({ type: "event", chat: frame.chat, event }));
  runs.set(frame.chat, run);
  log(`run ${frame.chat}: ${frame.text.slice(0, 80)}`);
  try {
    await run.run();
  } catch (e: any) {
    send({ type: "event", chat: frame.chat, event: { kind: "status", state: "error", error: String(e?.message ?? e) } });
  } finally {
    runs.delete(frame.chat);
    log(`run ${frame.chat} finished`);
    const next = queued.get(frame.chat)?.shift();
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
    if (msg.type === "run") void start(msg as RunFrame);
    else if (msg.type === "stop") {
      queued.delete(msg.chat);
      runs.get(msg.chat)?.stop();
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

connect();
