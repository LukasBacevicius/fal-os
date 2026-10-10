// End-to-end driver: optionally seeds a "user" product photo (uploaded like the UI does: PUT
// /assets/{sha256} + an image node), then sends a chat over /sync and logs every chat event
// until the turn is done.
//
//   tsx scripts/e2e.ts --host http://127.0.0.1:7420 --token <t> --out <dir> \
//     [--seed-photo "prompt"] [--photo file.png] "OPUS please direct me an ad for the headphones"
import { createHash } from "node:crypto";
import { appendFileSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { extname, join } from "node:path";
import WebSocket from "ws";
import { findMedia, runModel } from "../src/fal.ts";
import { HostMcp } from "../src/hostMcp.ts";

const args = process.argv.slice(2);
const opt = (k: string) => {
  const i = args.indexOf(k);
  return i >= 0 ? args.splice(i, 2)[1] : undefined;
};
const hostUrl = opt("--host") ?? "http://127.0.0.1:7420";
const token = opt("--token") ?? process.env.FALOS_TOKEN ?? "";
const out = opt("--out") ?? ".";
const canvas = opt("--canvas") ?? "main";
const seedPrompt = opt("--seed-photo");
/** Reference another chat with a `chat` chip (repeatable as comma list). */
const chatChips = (opt("--chat-chip") ?? "").split(",").filter(Boolean);
/** Follow every chat (cross-chat runs), finishing when all are idle. */
const followAll = args.includes("--follow-all") ? (args.splice(args.indexOf("--follow-all"), 1), true) : false;
let photo = opt("--photo");
const text = args.join(" ") || "OPUS please direct me an ad for the headphones";
mkdirSync(out, { recursive: true });
const logFile = join(out, "events.jsonl");
const t0 = Date.now();
const ts = () => `${((Date.now() - t0) / 1000).toFixed(1).padStart(6)}s`;

async function seed(): Promise<string | undefined> {
  if (seedPrompt && !photo) {
    console.log(`${ts()} seeding a product photo with fal-ai/flux-pro/v1.1 …`);
    const { data } = await runModel("fal-ai/flux-pro/v1.1", { prompt: seedPrompt, image_size: "landscape_4_3" }, () => {});
    const m = findMedia(data)[0];
    const bytes = Buffer.from(await (await fetch(m.url)).arrayBuffer());
    photo = join(out, `product${m.mime === "image/png" ? ".png" : ".jpg"}`);
    writeFileSync(photo, bytes);
  }
  if (!photo) return undefined;
  const bytes = readFileSync(photo);
  const hex = createHash("sha256").update(bytes).digest("hex");
  const mime = extname(photo).toLowerCase() === ".png" ? "image/png" : "image/jpeg";
  const put = await fetch(`${hostUrl}/assets/${hex}`, { method: "PUT", headers: { authorization: `Bearer ${token}`, "content-type": mime }, body: bytes });
  if (!put.ok) throw new Error(`PUT /assets: ${put.status} ${await put.text()}`);
  const host = new HostMcp(`${hostUrl}/mcp`, token);
  const s = await host.call("session", { agentName: "Lukas (upload)", canvas });
  const e = await host.call("edit", {
    sessionId: s.sessionId,
    canvas,
    text: `image "my headphones (product photo)" p(0,-900) s(800,600) asset(sha256:${hex})`,
  });
  console.log(`${ts()} uploaded user photo → node ${e.created[0]} (asset sha256:${hex.slice(0, 12)}…, no uri yet)`);
  return e.created[0];
}

const productNode = await seed();
const ws = new WebSocket(`${hostUrl.replace(/^http/, "ws")}/sync/${canvas}?token=${token}&actor=lukas&name=Lukas`);
let chat: string | undefined;
let docEvents = 0;
const created = new Set<string>();
const sessions = new Map<string, string>();
let cost = 0;
let lastList = "";
const states = new Map<string, string>();
let idleTimer: NodeJS.Timeout | undefined;

ws.on("message", (data, isBinary) => {
  if (isBinary) return;
  const m = JSON.parse(String(data));
  if (m.type === "hello") {
    const context = [...(productNode ? [{ kind: "node", node: productNode }] : []), ...chatChips.map((c) => ({ kind: "chat", chat: c }))];
    ws.send(JSON.stringify({ type: "chat.send", text, context }));
    console.log(`${ts()} → chat.send ${JSON.stringify(text)}${productNode ? ` + chip ${productNode}` : ""}`);
  } else if (m.type === "doc") {
    docEvents++;
    for (const c of m.created ?? []) if (!created.has(c)) {
      created.add(c);
      console.log(`${ts()}   + node ${c} by @${m.actor}`);
    }
  } else if (m.type === "presence" && m.session?.kind === "agent") {
    const s = m.session;
    if (s.presence?.status && sessions.get(s.id) !== s.presence.status) console.log(`${ts()}   ◉ ${s.agentName}: ${s.presence.status}`);
    sessions.set(s.id, s.presence?.status ?? "");
  } else if (m.type === "chat.list") {
    const line = m.chats.map((c: any) => `${c.title} [${c.state}${c.agents?.length ? ` · ${c.agents.map((a: any) => a.agent).join(",")}` : ""}] $${c.cost}`).join(" | ");
    if (line !== lastList) console.log(`\n${ts()} LIST ${line}`);
    lastList = line;
  } else if (m.type === "chat.event") {
    chat ??= m.chat;
    appendFileSync(logFile, JSON.stringify({ t: Date.now() - t0, ...m }) + "\n");
    const e = m.event;
    if (m.chat !== chat) {
      if (!followAll) return;
      process.stdout.write(`\n${ts()} «${m.chat}» `);
      if (e.kind === "status") states.set(m.chat, e.state);
    }
    switch (e.kind) {
      case "text":
        process.stdout.write(e.delta);
        break;
      case "tool":
        if (e.state !== "running") console.log(`\n${ts()} [${e.agent}] ${e.name} ${e.state}${e.summary ? ` — ${e.summary}` : ""}${e.nodes ? ` → ${e.nodes.join(",")}` : ""}`);
        else console.log(`\n${ts()} [${e.agent}] ${e.name} …${e.summary ? ` ${e.summary}` : ""}`);
        break;
      case "agent":
        console.log(`\n${ts()} ⟡ ${e.agent} ${e.state}${e.status ? `: ${e.status}` : ""}`);
        break;
      case "plan":
        console.log(`\n${ts()} PLAN ${e.steps.map((s: any) => `[${s.state}] ${s.title}`).join(" | ")}`);
        break;
      case "cost":
        cost = e.usd;
        console.log(`\n${ts()} $ fal spend so far: $${e.usd}`);
        break;
      case "done":
        console.log(`\n${ts()} DONE nodes=${JSON.stringify(e.nodes)}\n${e.summary}`);
        break;
      case "status":
        console.log(`\n${ts()} status ${e.state}${e.error ? ` (${e.error})` : ""}`);
        states.set(m.chat, e.state);
        if (!followAll) {
          if (m.chat === chat && (e.state === "idle" || e.state === "error")) finish(e.state === "error" ? 1 : 0);
        } else {
          // Done when every chat we've seen is idle for a while (cross-chat replies may follow).
          clearTimeout(idleTimer);
          if ([...states.values()].every((s) => s !== "running")) idleTimer = setTimeout(() => finish(0), 45_000);
        }
        break;
    }
  }
});
ws.on("error", (e) => {
  console.error(e);
  process.exit(1);
});

function finish(code: number) {
  const summary = { chat, seconds: (Date.now() - t0) / 1000, docEvents, createdNodes: created.size, agentSessions: [...sessions.keys()].length, falSpendUsd: cost };
  writeFileSync(join(out, "summary.json"), JSON.stringify(summary, null, 2));
  console.log(`\n${ts()} summary ${JSON.stringify(summary)}`);
  setTimeout(() => process.exit(code), 500);
}

process.on("SIGINT", () => {
  if (chat) ws.send(JSON.stringify({ type: "chat.stop", chat }));
  setTimeout(() => finish(130), 4000);
});
