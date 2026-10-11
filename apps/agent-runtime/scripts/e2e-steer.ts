// Workspace + steering end-to-end: lists projects over /workspace, creates (or reuses) a
// project, starts a chat over /sync/<project>, then steers it mid-run (`chat.steer`), logging
// every chat event until the turn ends.
//
//   tsx scripts/e2e-steer.ts --host http://127.0.0.1:7421 --token <t> --out <dir> \
//     [--project "Steer test"] [--after 20] [--steer "…"] [--agent motion] "first request"
import { appendFileSync, mkdirSync } from "node:fs";
import { join } from "node:path";
import WebSocket from "ws";

const args = process.argv.slice(2);
const opt = (k: string) => {
  const i = args.indexOf(k);
  return i >= 0 ? args.splice(i, 2)[1] : undefined;
};
const hostUrl = opt("--host") ?? "http://127.0.0.1:7421";
const token = opt("--token") ?? process.env.FALOS_TOKEN ?? "";
const out = opt("--out") ?? ".";
const projectName = opt("--project") ?? "Steer test";
const after = Number(opt("--after") ?? 20);
const steer = opt("--steer");
const agent = opt("--agent");
const driver = opt("--driver");
const timeout = Number(opt("--timeout") ?? 900);
const text = args.join(" ");
mkdirSync(out, { recursive: true });
const logFile = join(out, "events.jsonl");
const t0 = Date.now();
const ts = () => `${((Date.now() - t0) / 1000).toFixed(1).padStart(6)}s`;
const wsUrl = (p: string) => `${hostUrl.replace(/^http/, "ws")}${p}${p.includes("?") ? "&" : "?"}token=${token}`;

function open(path: string): Promise<WebSocket> {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(wsUrl(path));
    ws.once("open", () => resolve(ws));
    ws.once("error", reject);
  });
}

function next(ws: WebSocket, pred: (m: any) => boolean): Promise<any> {
  return new Promise((resolve) => {
    const on = (d: any) => {
      if (typeof d !== "string" && !(d instanceof Buffer)) return;
      let m: any;
      try {
        m = JSON.parse(String(d));
      } catch {
        return;
      }
      if (pred(m)) {
        ws.off("message", on);
        resolve(m);
      }
    };
    ws.on("message", on);
  });
}

const w = await open("/workspace");
const first = await next(w, (m) => m.type === "projects");
console.log(`${ts()} projects: ${first.projects.map((p: any) => `${p.id} (${p.name}, running ${p.running}${p.fal_project ? `, fal ${p.fal_project}` : ""})`).join(" · ")}`);
let id = first.projects.find((p: any) => p.name === projectName)?.id;
if (!id) {
  w.send(JSON.stringify({ type: "project.create", name: projectName }));
  id = (await next(w, (m) => m.type === "project.created")).id;
  console.log(`${ts()} created project ${id}`);
}
w.send(JSON.stringify({ type: "project.touch", id }));

const s = await open(`/sync/${encodeURIComponent(id)}?actor=lukas&name=Lukas`);
await next(s, (m) => m.type === "chat.list");
s.send(JSON.stringify({ type: "chat.send", text, ...(driver ? { driver } : {}) }));
let chat = "";
let steered = false;
let done: () => void;
const finished = new Promise<void>((r) => (done = r));
s.on("message", (d) => {
  let m: any;
  try {
    m = JSON.parse(String(d));
  } catch {
    return;
  }
  if (m.type !== "chat.event") return;
  if (!chat && m.event.kind === "user") chat = m.chat;
  if (m.chat !== chat) return;
  appendFileSync(logFile, `${JSON.stringify({ t: Date.now() - t0, ...m })}\n`);
  const e = m.event;
  const who = e.agent && e.agent !== "director" ? `[${e.agent}] ` : "";
  if (e.kind === "text") process.stdout.write(e.delta);
  else if (e.kind === "tool") e.state !== "running" && console.log(`\n${ts()} ${who}· ${e.name}: ${e.summary ?? ""} (${e.state})`);
  else if (e.kind === "agent") console.log(`\n${ts()} ${who}${e.state}${e.role ? ` (${e.role})` : ""}: ${e.status ?? ""}`);
  else if (e.kind === "user") console.log(`\n${ts()} > ${e.steer ? "[steer] " : ""}${e.agent ? `→${e.agent} ` : ""}${e.text}`);
  else if (e.kind === "capabilities") console.log(`${ts()} capabilities: ${e.available.join(", ")}${e.missing.length ? ` · missing: ${e.missing.map((x: any) => x.name).join(", ")}` : ""}`);
  else if (e.kind === "status") {
    console.log(`\n${ts()} status ${e.state}${e.error ? `: ${e.error}` : ""}`);
    if (e.state !== "running" && steered) done();
  } else if (e.kind === "dequeued") console.log(`${ts()} dequeued ${e.id} (${e.reason})`);
});
if (steer) {
  setTimeout(() => {
    console.log(`\n${ts()} ---- steering now ----`);
    s.send(JSON.stringify({ type: "chat.steer", chat, text: steer, ...(agent ? { agent } : {}) }));
    steered = true;
  }, after * 1000);
} else steered = true;
setTimeout(() => {
  console.log(`\n${ts()} timeout`);
  done();
}, timeout * 1000);
await finished;
console.log(`${ts()} chat ${chat} in project ${id}; events in ${logFile}`);
process.exit(0);
