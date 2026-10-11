import assert from "node:assert/strict";
import { createServer } from "node:http";
import { test } from "node:test";
import { assetCapsFrom, capabilityEvent, capabilityPrompt } from "../src/capabilities.ts";
import { actionPhrase, doneSummary, runningSummary, stripAgent } from "../src/events.ts";
import { HostMcp } from "../src/hostMcp.ts";
import { agents, directorPrompt } from "../src/prompts.ts";
import { ChatRun, projectDir, type RunFrame, steerText } from "../src/run.ts";

const frame = (over: Partial<RunFrame> = {}): RunFrame => ({
  chat: "c_test",
  canvas: "main",
  text: "make an ad",
  context: [],
  history: [],
  project: "demo",
  project_dir: "/tmp/falos-test-ws/demo",
  ...over,
});

test("summaries never repeat the verb or the agent", () => {
  assert.equal(actionPhrase("Build a contact sheet", true), "built a contact sheet");
  assert.equal(actionPhrase("Build a contact sheet", false), "building a contact sheet");
  assert.equal(actionPhrase("Ran Render the film", true), "rendered the film");
  assert.equal(actionPhrase("ran ffprobe on out/ad.mp4", true), "ran ffprobe on out/ad.mp4");
  assert.equal(actionPhrase("Rendered the film", true), "rendered the film");
  assert.equal(actionPhrase("npx remotion render src/index.ts", true), "ran npx remotion render src/index.ts");
  assert.equal(actionPhrase("Extract 6 frames", true), "extracted 6 frames");
  assert.equal(doneSummary("Bash", { description: "Build a contact sheet" }, null, undefined), "built a contact sheet");
  assert.equal(runningSummary("Bash", { command: "ffmpeg -i a.mp4" }), "running ffmpeg -i a.mp4");
  assert.doesNotMatch(doneSummary("Bash", { description: "Run the QA script" }, null, undefined), /^ran ran/);
  assert.equal(stripAgent("motion: Build Remotion launch film", "motion"), "Build Remotion launch film");
  assert.equal(stripAgent("Motion · Build film", "motion"), "Build film");
  assert.equal(stripAgent("generator: 4 stills", "generator-2", "generator"), "4 stills");
});

test("run frames: project id + folder (older hosts: folder in `project`)", () => {
  assert.equal(projectDir(frame()), "/tmp/falos-test-ws/demo");
  assert.equal(projectDir({ ...frame(), project_dir: undefined, project: "/old/project" }), "/old/project");
  const run = new ChatRun(frame(), () => {}, async () => null);
  assert.equal(run.workDir, "/tmp/falos-test-ws/demo/work/c_test");
  assert.deepEqual(run.hostHeaders(), { "x-falos-chat": "c_test", "x-falos-project": "demo" });
});

test("steer: without a running query → steer.failed; drivers take it via steerHandler", () => {
  const frames: any[] = [];
  const run = new ChatRun(frame(), () => {}, async () => null, (m) => frames.push(m));
  assert.equal(run.steer({ id: "q_1", text: "darker", author: "lukas" }), false);
  assert.deepEqual(frames.at(-1), { type: "steer.failed", chat: "c_test", id: "q_1", text: "darker", author: "lukas", chips: [] });
  const got: any[] = [];
  run.steerHandler = (s) => got.push(s);
  assert.equal(run.steer({ id: "q_2", text: "use red", agent: "motion" }), true);
  assert.equal(got[0].agent, "motion");
  run.closeQueue();
  assert.equal(run.steer({ id: "q_3", text: "late" }), false);
  assert.equal(frames.at(-1).type, "steer.failed");
});

test("steer: a waiting fal question takes it as the answer, without an injected frame", async () => {
  const frames: any[] = [];
  const run = new ChatRun(frame(), () => {}, async () => null, (m) => frames.push(m));
  const answer = run.nextQueued(5000);
  assert.equal(run.enqueueAnswer({ id: "q_9", text: "Kling" }), true);
  assert.equal((await answer)?.text, "Kling");
  assert.equal(frames.length, 0);
  assert.equal(run.enqueueAnswer({ id: "q_10", text: "x" }), false);
});

test("steer text: interrupt for the director, relay for a subagent", () => {
  assert.match(steerText({ id: "q", text: "make it darker", author: "lukas" }), /^\[steer\].*interrupts you mid-run[\s\S]*make it darker/);
  assert.match(steerText({ id: "q", text: "red logo" }, "motion"), /message for motion — relay it/);
});

test("capabilities: prompt block and event", () => {
  const caps = { available: ["claude", "fal-models", "remotion", "ffmpeg"] as any, missing: [{ name: "codex" as const, fix: "run `npx codex login`" }], plugins: [], hostTools: [], assets: assetCapsFrom([]) };
  const p = capabilityPrompt(caps);
  assert.match(p, /Remotion/);
  assert.match(p, /NOT available[\s\S]*codex \(fix: run `npx codex login`\)/);
  assert.deepEqual(capabilityEvent(caps), { kind: "capabilities", available: ["claude", "fal-models", "remotion", "ffmpeg"], missing: [{ name: "codex", fix: "run `npx codex login`" }] });
  const d = directorPrompt({ canvas: "main", workDir: "/w", falAgent: { ok: false, reason: "x" }, codex: true, caps: p });
  assert.match(d, /ONLY when the user asks for Codex/);
  assert.match(d, /NOT available/);
});

test("motion agent: renderers, skills and the QA gate follow the capabilities", () => {
  const a = agents({ canvas: "main", workDir: "/w", falAgent: true, codex: false, worker: "m", remotion: true, hyperframes: true, remotionSkills: true, hyperframesSkills: true, qa: "/q.mjs" });
  assert.deepEqual(a.motion.skills, ["remotion:remotion-best-practices", "hyperframes:hyperframes"]);
  assert.ok(a.motion.tools?.includes("Skill") && !a.motion.tools?.includes("mcp__swarm__codex"));
  assert.match(a.motion.prompt, /HyperFrames/);
  assert.match(a.motion.prompt, /node \/q\.mjs <render\.mp4> --master/);
  assert.match(a.motion.prompt, /-14 LUFS/);
  assert.match(a.qa.prompt, /node \/q\.mjs/);
  const b = agents({ canvas: "main", workDir: "/w", falAgent: true, codex: false, worker: "m", remotion: true, hyperframes: false });
  assert.equal(b.motion.skills, undefined);
  assert.doesNotMatch(b.motion.prompt, /HyperFrames \(HTML/);
});

test("host MCP: a dropped session (404) is re-initialized and the call retried", async () => {
  let sessions = 0;
  let calls = 0;
  const srv = createServer((req, res) => {
    let body = "";
    req.on("data", (d) => (body += d));
    req.on("end", () => {
      const m = JSON.parse(body);
      if (m.method === "initialize") {
        sessions++;
        res.writeHead(200, { "content-type": "application/json", "mcp-session-id": `s${sessions}` });
        return res.end(JSON.stringify({ jsonrpc: "2.0", id: m.id, result: {} }));
      }
      if (!m.id) return res.writeHead(202).end();
      calls++;
      // The first session expires after one call.
      if (req.headers["mcp-session-id"] === "s1" && calls > 1) return res.writeHead(404).end("Session not found");
      res.writeHead(200, { "content-type": "application/json" });
      res.end(JSON.stringify({ jsonrpc: "2.0", id: m.id, result: { content: [{ type: "text", text: `{"ok":${calls}}` }] } }));
    });
  });
  await new Promise<void>((r) => srv.listen(0, "127.0.0.1", r));
  const port = (srv.address() as any).port;
  const h = new HostMcp(`http://127.0.0.1:${port}/mcp`, "t", { "x-falos-project": "demo" });
  assert.deepEqual(await h.call("read", {}), { ok: 1 });
  assert.deepEqual(await h.call("read", {}), { ok: 3 });
  assert.equal(sessions, 2);
  srv.close();
});
