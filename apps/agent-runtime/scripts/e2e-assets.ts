// Assets v1 end-to-end ("bring it in, generate, take it out"): imports local files as the
// user's inputs into a fresh project, asks the swarm for a piece with deliverables, logs every
// chat event, then verifies: inputs untouched, derivatives carry lineage back to the inputs,
// deliverable files exist with the right sizes, and a `files` event was posted.
//
//   tsx scripts/e2e-assets.ts --host http://127.0.0.1:7480 --token <t> --out <dir> \
//     --workspace <dir> --inputs a.jpg,b.jpg,clip.mp4 [--project "Assets e2e"] [--expect 1920x1080,1080x1920] "request"
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { appendFileSync, existsSync, mkdirSync, readdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { basename, join } from "node:path";
import WebSocket from "ws";
import { HostMcp } from "../src/hostMcp.ts";

const args = process.argv.slice(2);
const opt = (k: string) => {
  const i = args.indexOf(k);
  return i >= 0 ? args.splice(i, 2)[1] : undefined;
};
const hostUrl = opt("--host") ?? "http://127.0.0.1:7480";
const token = opt("--token") ?? process.env.FALOS_TOKEN ?? "";
const out = opt("--out") ?? ".";
const projectName = opt("--project") ?? `Assets e2e ${new Date().toISOString().slice(11, 19)}`;
const inputs = (opt("--inputs") ?? "").split(",").filter(Boolean);
const expect = (opt("--expect") ?? "1920x1080,1080x1920").split(",").map((s) => s.split("x").map(Number));
const timeout = Number(opt("--timeout") ?? 2400);
/** `<projectId>/<chat>`: a follow-up turn in an existing chat (e.g. after a host restart). */
const resume = opt("--resume");
const workspace = opt("--workspace") ?? process.env.FALOS_WORKSPACE ?? "";
const text = args.join(" ") || "make a 10s product teaser from my clip and photos, deliver 16:9 and 9:16";
mkdirSync(out, { recursive: true });
const logFile = join(out, "events.jsonl");
const t0 = Date.now();
const ts = () => `${((Date.now() - t0) / 1000).toFixed(1).padStart(6)}s`;
const wsUrl = (p: string) => `${hostUrl.replace(/^http/, "ws")}${p}${p.includes("?") ? "&" : "?"}token=${token}`;
const sha = (f: string) => createHash("sha256").update(readFileSync(f)).digest("hex");

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

// ---- project + the user's inputs ----------------------------------------------------------
const w = await open("/workspace");
await next(w, (m) => m.type === "projects");
let id: string;
if (resume) id = resume.split("/")[0];
else {
  w.send(JSON.stringify({ type: "project.create", name: projectName }));
  id = (await next(w, (m) => m.type === "project.created")).id;
}
w.send(JSON.stringify({ type: "project.touch", id }));
const host = new HostMcp(`${hostUrl}/mcp`, token, { "x-falos-project": id });
const s0 = await host.call("session", { agentName: "Lukas (import)", canvas: "main" });
const sid = s0.sessionId as string;
const projectDir = workspace ? join(workspace, id) : "";
const imported: { node: string; file: string; sha: string; name: string; row?: string }[] = [];
let frame: string;
if (resume) {
  // Follow-up turn in an existing chat: the inputs are already in the "My inputs" frame.
  const all = await host.callText("read", { sessionId: sid, canvas: "main" });
  frame = all.match(/^\s*([a-z0-9]{6,12}) frame "My inputs"/m)?.[1] ?? "";
  for (const f of inputs) {
    const node = all.match(new RegExp(`^\\s*([a-z0-9]{6,12}) (?:image|video|audio) "${basename(f).replace(/\./g, "\\.")}"`, "m"))?.[1];
    if (node) imported.push({ node, file: f, sha: sha(f), name: basename(f) });
  }
} else {
  frame = (await host.call("edit", { sessionId: sid, canvas: "main", text: `frame "My inputs" p(0,-1400) s(2200,1000)` })).created[0];
  for (const [i, f] of inputs.entries()) {
    const r = await host.call("import_file", { sessionId: sid, canvas: "main", path: f, name: basename(f), placement: { parent: frame, x: 40 + i * 720, y: 120, w: 640 } });
    imported.push({ node: r.node, file: f, sha: sha(f), name: basename(f) });
    console.log(`${ts()} imported ${basename(f)} → ${r.node}`);
  }
}
const rowsBefore = await host.callText("read", { sessionId: sid, canvas: "main", root: frame });

// ---- the chat ------------------------------------------------------------------------------
const s = await open(`/sync/${encodeURIComponent(id)}?actor=lukas&name=Lukas`);
await next(s, (m) => m.type === "chat.list");
let chat = resume?.split("/")[1] ?? "";
s.send(JSON.stringify({ type: "chat.send", text, ...(chat ? { chat } : { context: imported.map((x) => ({ kind: "node", node: x.node })) }) }));
console.log(`${ts()} → ${text}`);
const filesEvents: any[] = [];
const capabilities: any[] = [];
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
  else if (e.kind === "files") {
    filesEvents.push(e);
    console.log(`\n${ts()} ${who}FILES ${e.files.map((f: any) => `${f.name} ${f.w ?? "?"}×${f.h ?? "?"}`).join(" · ")} in ${e.dir}`);
  } else if (e.kind === "capabilities") {
    capabilities.push(e);
    console.log(`${ts()} capabilities: ${e.available.join(", ")}${e.missing.length ? ` · missing: ${e.missing.map((x: any) => x.name).join(", ")}` : ""}`);
  } else if (e.kind === "status") {
    console.log(`\n${ts()} status ${e.state}${e.error ? `: ${e.error}` : ""}`);
    if (e.state !== "running") done();
  } else if (e.kind === "done") console.log(`\n${ts()} DONE nodes=${JSON.stringify(e.nodes)}`);
});
setTimeout(() => {
  console.log(`\n${ts()} timeout`);
  done();
}, timeout * 1000);
await finished;

// ---- verify --------------------------------------------------------------------------------
const checks: { name: string; ok: boolean; detail: string }[] = [];
const check = (name: string, ok: boolean, detail: string) => {
  checks.push({ name, ok, detail });
  console.log(`${ok ? "✓" : "✗"} ${name}: ${detail}`);
};
console.log("\n---- verification ----");
// 1. Inputs untouched: the source files, the stored asset bytes, and the input rows.
const v = new HostMcp(`${hostUrl}/mcp`, token, { "x-falos-project": id });
const vs = (await v.call("session", { agentName: "Verifier", canvas: "main" })).sessionId as string;
for (const x of imported) {
  const [c] = await v.call("get_context", { sessionId: vs, canvas: "main", chips: [{ kind: "node", node: x.node }] });
  const stored = c?.file && existsSync(c.file) ? sha(c.file) : "missing";
  check(`input ${x.name} untouched`, stored === x.sha && sha(x.file) === x.sha, `node ${x.node} ${c?.error ? c.error : "present"}, stored sha ${stored.slice(0, 12)} vs ${x.sha.slice(0, 12)}`);
}
const rowsAfter = await v.callText("read", { sessionId: vs, canvas: "main", root: frame });
const inputRows = (t: string) => t.split("\n").filter((l) => imported.some((x) => l.includes(x.node))).map((l) => l.replace(/ upd\([^)]*\)| by\([^)]*\)/g, "").trim());
check("input rows unchanged", JSON.stringify(inputRows(rowsBefore)) === JSON.stringify(inputRows(rowsAfter)), inputRows(rowsAfter).join(" | ").slice(0, 300));
// 2. Derivatives with lineage back to the inputs.
const canvas = await v.callText("read", { sessionId: vs, canvas: "main" });
const media = [...canvas.matchAll(/^\s*([a-z0-9]{6,12}) (image|video|audio) "([^"]*)"/gm)].map((m) => ({ node: m[1], type: m[2], name: m[3] })).filter((n) => !imported.some((x) => x.node === n.node));
const derived: any[] = [];
for (const n of media) {
  const [c] = await v.call("get_context", { sessionId: vs, canvas: "main", chips: [{ kind: "node", node: n.node }] });
  const lin = JSON.stringify(c?.lineage ?? "");
  const fromInputs = imported.filter((x) => lin.includes(x.node) || lin.includes(x.sha.slice(0, 12))).map((x) => x.name);
  derived.push({ ...n, lineage: c?.lineage, fromInputs });
}
writeFileSync(join(out, "derived.json"), JSON.stringify(derived, null, 2));
const withLineage = derived.filter((d) => d.lineage && JSON.stringify(d.lineage).length > 4);
check("new media nodes carry lineage", derived.length > 0 && withLineage.length === derived.length, `${withLineage.length}/${derived.length} (${derived.map((d) => `${d.node} ${d.type} "${d.name}"`).join("; ").slice(0, 400)})`);
check("some derive from the user's inputs", derived.some((d) => d.fromInputs.length), derived.filter((d) => d.fromInputs.length).map((d) => `${d.node} ← ${d.fromInputs.join("+")}`).join("; ").slice(0, 400));
// 3. Deliverables.
const ddir = projectDir ? join(projectDir, "deliverables") : "";
const files = ddir && existsSync(ddir) ? readdirSync(ddir).filter((f) => !f.startsWith(".")).map((f) => join(ddir, f)) : [];
const probed = files.map((f) => {
  try {
    const [wh, dur] = execFileSync("ffprobe", ["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height:format=duration", "-of", "csv=p=0", f], { encoding: "utf8" }).trim().split("\n");
    const [ww, hh] = wh.split(",").map(Number);
    return { file: basename(f), w: ww, h: hh, duration: Number(dur), bytes: statSync(f).size };
  } catch {
    return { file: basename(f), bytes: statSync(f).size };
  }
});
for (const [ew, eh] of expect) check(`deliverable ${ew}×${eh}`, probed.some((p: any) => p.w === ew && p.h === eh), probed.map((p: any) => `${p.file} ${p.w ?? "?"}×${p.h ?? "?"} ${p.duration ? p.duration.toFixed(1) + "s" : ""}`).join("; ") || `nothing in ${ddir}`);
// 4. File cards.
check("files event posted", filesEvents.length > 0, filesEvents.map((e) => e.files.map((f: any) => f.name).join(", ")).join(" | ") || "none");
const report = { project: id, projectDir, chat, seconds: (Date.now() - t0) / 1000, capabilities: capabilities.at(-1), checks, deliverables: probed, filesEvents, derived: derived.map(({ lineage, ...d }) => d) };
writeFileSync(join(out, "report.json"), JSON.stringify(report, null, 2));
console.log(`\n${checks.filter((c) => c.ok).length}/${checks.length} checks passed · report ${join(out, "report.json")}`);
process.exit(0);
