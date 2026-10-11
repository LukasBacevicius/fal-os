// Assets v1 on the runtime side: capability detection from the host's tools/list, prompts that
// only mention tools the host has, file cards, export QA and the OTIO writer.
import assert from "node:assert/strict";
import { test } from "node:test";
import { assetCapsFrom, capabilityEvent, capabilityPrompt, type Capabilities } from "../src/capabilities.ts";
import { doneSummary, filesOf, runningSummary } from "../src/events.ts";
import { agents, assetRules, directorPrompt } from "../src/prompts.ts";
import { ChatRun, type RunFrame } from "../src/run.ts";
import { specIssues } from "../src/swarmTools.ts";
// @ts-expect-error plain ESM script without types
import { toOtio } from "../scripts/otio.mjs";

const OLD_HOST = ["session", "read", "edit", "generate", "generate_batch", "import_file", "get_context", "get_knowledge"].map((name) => ({ name, inputSchema: { properties: { lineage: { $ref: "#/$defs/LineageArgs" } }, $defs: { LineageArgs: { properties: { model: {}, params: {}, parents: {} } } } } }));
const NEW_HOST = [
  ...OLD_HOST.filter((t) => !["generate", "import_file"].includes(t.name)),
  { name: "generate", inputSchema: { properties: { mediaInputs: { type: "object" } } } },
  { name: "import_file", inputSchema: { $defs: { LineageArgs: { properties: { kind: {}, op: {}, recipe: {}, parents: {} } } } } },
  ...["list_ops", "inspect", "transform", "compose", "rerun", "export", "package", "search_assets", "get_asset", "place_asset"].map((name) => ({ name })),
];

test("asset capabilities come from the host's tools/list", () => {
  assert.deepEqual(assetCapsFrom(OLD_HOST), { ops: false, export: false, compose: false, rerun: false, package: false, library: false, mediaInputs: false, provenance: false });
  assert.deepEqual(assetCapsFrom(NEW_HOST), { ops: true, export: true, compose: true, rerun: true, package: true, library: true, mediaInputs: true, provenance: true });
  // inspect alone isn't the op registry
  assert.equal(assetCapsFrom([{ name: "inspect" }]).ops, false);
});

test("capability block and event name the missing asset tools with their fix", () => {
  const caps: Capabilities = {
    available: ["claude", "ffmpeg"],
    missing: [{ name: "asset-export", fix: "this falos-host build has no `export` yet" }],
    plugins: [],
    hostTools: NEW_HOST.map((t) => t.name),
    assets: assetCapsFrom(NEW_HOST),
  };
  caps.available.push("asset-ops");
  const p = capabilityPrompt(caps);
  assert.match(p, /Asset ops \(fal-os `list_ops`/);
  assert.match(p, /Also `compose`/);
  assert.match(p, /NOT available[\s\S]*asset-export \(fix: this falos-host build has no `export` yet\)/);
  assert.deepEqual(capabilityEvent(caps).missing, [{ name: "asset-export", fix: "this falos-host build has no `export` yet" }]);
});

test("prompts on an old host: ffmpeg + import_file lineage, ffmpeg deliverables, never the absent tools", () => {
  const assets = assetCapsFrom(OLD_HOST);
  const o = { canvas: "main", workDir: "/p/work/c", falAgent: false, codex: false, assets, projectDir: "/p", qa: "/q.mjs" };
  const d = directorPrompt({ ...o, falAgent: { ok: false, reason: "x" } });
  for (const absent of ["`transform`", "`search_assets`", "`deliver(", "`inspect`", "role(final)"]) assert.ok(!d.includes(absent), `director mentions ${absent}`);
  assert.match(d, /Protected inputs/);
  assert.match(d, /"model":"ffmpeg"/);
  assert.match(d, /\/p\/deliverables\//);
  assert.match(d, /share_files/);
  assert.match(d, /youtube-1080`\) \+ 9:16 \(`reel`\) \+ 1:1 \(`square`\)/);
  const a = agents({ ...o, worker: "m", hostTools: OLD_HOST.map((t) => t.name) });
  assert.ok(a.editor && a.finisher);
  for (const def of Object.values(a)) for (const t of def.tools ?? []) assert.ok(!/__(transform|inspect|export|package|search_assets|list_ops)$|__deliver$/.test(t), `${t} listed on an old host`);
  assert.ok(a.finisher.tools?.includes("mcp__swarm__share_files"));
  assert.match(a.editor.prompt, /no ops yet[\s\S]*import_file/);
  assert.match(a.motion.prompt, /otio\.mjs|ad\.otio/);
  assert.match(a.motion.prompt, /--size <W>x<H>/);
});

test("prompts on an Assets v1 host: ops, provenance lineage, roles, library first, deliver", () => {
  const assets = assetCapsFrom(NEW_HOST);
  const r = assetRules({ assets });
  assert.match(r, /`transform`/);
  assert.match(r, /"kind":"derive"/);
  assert.match(r, /role\(rejected\)/);
  assert.match(r, /`search_assets`/);
  assert.match(r, /mediaInputs/);
  const a = agents({ canvas: "main", workDir: "/p/work/c", falAgent: true, codex: false, worker: "m", assets, projectDir: "/p", hostTools: NEW_HOST.map((t) => t.name) });
  assert.ok(a.editor.tools?.includes("mcp__falos__transform"));
  assert.ok(a.finisher.tools?.includes("mcp__swarm__deliver") && a.finisher.tools?.includes("mcp__falos__export"));
  assert.ok(a.generator.tools?.includes("mcp__falos__search_assets"));
  assert.match(a.finisher.prompt, /deliver\(sessionId, nodes, presets\)/);
  assert.match(a.motion.prompt, /"kind":"render"/);
  // an OTIO skill, when installed, replaces the script
  const b = agents({ canvas: "main", workDir: "/w", falAgent: true, codex: false, worker: "m", assets: { ...assets, otioSkill: "remotion:remotion-opentimeline" } });
  assert.match(b.motion.prompt, /Skill `remotion:remotion-opentimeline`/);
  assert.ok(b.motion.tools?.includes("Skill"));
});

test("files from export / package results; summaries", () => {
  const exp = { files: [{ path: "/p/deliverables/a_reel_1080x1920_v1.mp4", node: "abc123", preset: "reel", w: 1080, h: 1920, bytes: 10, sha256: "f" }], dir: "/p/deliverables" };
  assert.deepEqual(filesOf(exp).files[0], { path: exp.files[0].path, name: "a_reel_1080x1920_v1.mp4", node: "abc123", preset: "reel", w: 1080, h: 1920, bytes: 10, sha256: "f" });
  assert.equal(filesOf({ zip: "/p/exports/x.zip", bytes: 5 }).files[0].name, "x.zip");
  assert.equal(filesOf({ zip: "/p/exports/x.zip" }).dir, "/p/exports");
  assert.deepEqual(filesOf(null).files, []);
  assert.equal(doneSummary("export", { presets: ["reel"] }, exp, undefined), "exported 1 file (reel)");
  assert.equal(doneSummary("transform", { op: "reframe", inputs: ["a"] }, { node: "zzz111" }, ["zzz111"]), "reframed → 1 node");
  assert.equal(doneSummary("transform", { op: "crop" }, null, undefined), "cropped");
  assert.equal(runningSummary("transform", { op: "concat", inputs: ["a", "b"] }), "concatenating clips (2 inputs)");
  assert.equal(runningSummary("transform", { op: "ffmpeg" }), "running ffmpeg");
  assert.equal(runningSummary("inspect", { what: ["probe", "contact_sheet"] }), "inspecting probe, contact_sheet");
});

test("export QA: preset dimensions, empty files, loudness", () => {
  assert.deepEqual(specIssues({ preset: "reel", bytes: 9 }, { w: 1080, h: 1920, vcodec: "h264", pix_fmt: "yuv420p" }, -14.2), []);
  assert.match(specIssues({ preset: "reel", bytes: 9 }, { w: 1920, h: 1080 }, null)[0], /preset reel is 1080×1920/);
  assert.deepEqual(specIssues({ preset: "reel", bytes: 0 }, null, null), ["empty file", "not readable by ffprobe"]);
  assert.match(specIssues({ preset: "square", bytes: 9 }, { w: 1080, h: 1080 }, -20).join(), /-20 LUFS/);
});

test("file cards: the host posts export cards, the runtime posts package zips; finals tracked", () => {
  const events: any[] = [];
  const f: RunFrame = { chat: "c", canvas: "main", text: "x", context: [], history: [], project: "/tmp/p" };
  const run = new ChatRun(f, (e) => events.push(e), async () => null) as any;
  // export: the host already appended the `files` event (ops/export.rs) → no duplicate
  run.onToolUse({ id: "t1", name: "mcp__falos__export", input: { presets: ["reel"] } }, null);
  run.onToolResult({ tool_use_id: "t1", content: [{ type: "text", text: JSON.stringify({ files: [{ path: "/tmp/p/deliverables/a.mp4", preset: "reel", bytes: 3, sha256: "s" }], dir: "/tmp/p/deliverables" }) }] });
  assert.equal(events.filter((e) => e.kind === "files").length, 0);
  // …unless the host says it didn't
  run.onToolUse({ id: "t2", name: "mcp__falos__export", input: {} }, null);
  run.onToolResult({ tool_use_id: "t2", content: [{ type: "text", text: JSON.stringify({ emitted: false, files: [{ path: "/tmp/p/deliverables/a.mp4", preset: "reel", bytes: 1 }], dir: "/tmp/p/deliverables" }) }] });
  const files = events.find((e) => e.kind === "files");
  assert.equal(files?.files[0].preset, "reel");
  assert.equal(files?.dir, "/tmp/p/deliverables");
  // package: one card for the zip
  events.length = 0;
  run.onToolUse({ id: "t4", name: "mcp__falos__package", input: { kind: "deliverables" } }, null);
  run.onToolResult({ tool_use_id: "t4", content: [{ type: "text", text: JSON.stringify({ path: "/tmp/p/exports/2026-10-11/x_deliverables_1.zip", bytes: 9, sha256: "z", kind: "deliverables" }) }] });
  assert.equal(events.find((e) => e.kind === "files")?.files[0].name, "x_deliverables_1.zip");
  run.onToolUse({ id: "t3", name: "mcp__falos__edit", input: { text: "abc123 role(final)\nzzz999 role(rejected)" } }, null);
  run.onToolResult({ tool_use_id: "t3", content: [{ type: "text", text: "{}" }] });
  assert.deepEqual(run.finalNodes, ["abc123"]);
});

test("OTIO writer: tracks, gaps, file URLs, render metadata", () => {
  const doc = toOtio(
    {
      name: "Ad",
      fps: 30,
      render: "out/ad.mp4",
      tracks: [
        { kind: "Video", clips: [{ name: "hero", file: "/w/public/hero.png", start: 0, duration: 2 }, { file: "/w/public/clip1.mp4", start: 3, duration: 1.5, in: 4, node: "abc123" }] },
        { kind: "Audio", clips: [{ file: "/w/public/music.mp3", start: 0, duration: 4.5 }] },
      ],
    },
    "/w",
  );
  assert.equal(doc.OTIO_SCHEMA, "Timeline.1");
  const [v, au] = doc.tracks.children;
  assert.deepEqual(v.children.map((c: any) => c.OTIO_SCHEMA), ["Clip.2", "Gap.1", "Clip.2"]);
  assert.equal(v.children[1].source_range.duration.value, 30);
  assert.equal(v.children[2].source_range.start_time.value, 120);
  assert.equal(v.children[2].media_references.DEFAULT_MEDIA.target_url, "file:///w/public/clip1.mp4");
  assert.deepEqual(v.children[2].metadata, { falos: { node: "abc123" } });
  assert.equal(au.kind, "Audio");
  assert.equal(doc.metadata.falos.render, "/w/out/ad.mp4");
});

test("subagents resumed by SendMessage keep the session open and their label", () => {
  const events: any[] = [];
  const f: RunFrame = { chat: "c", canvas: "main", text: "x", context: [], history: [], project: "/tmp/p" };
  const run = new ChatRun(f, (e) => events.push(e), async () => null) as any;
  run.onToolUse({ id: "a1", name: "Agent", input: { subagent_type: "qa", description: "check" } }, null);
  run.onMessage({ type: "system", subtype: "task_notification", task_id: "t1", tool_use_id: "a1", status: "completed", summary: "PASS" });
  // the director resumes qa later (SendMessage): a background task the old code didn't track
  run.onMessage({ type: "system", subtype: "background_tasks_changed", tasks: [{ task_id: "t2", task_type: "local_agent", description: "qa" }] });
  run.onMessage({ type: "system", subtype: "task_started", task_id: "t2", tool_use_id: "s1", subagent_type: "qa", is_backgrounded: true, description: "re-check" });
  assert.equal(run.liveTasks.size, 1);
  assert.equal(run.labels.get("s1"), "qa");
  assert.ok(events.some((e) => e.agent === "qa" && e.status === "resumed"));
  run.onMessage({ type: "system", subtype: "task_notification", task_id: "t2", tool_use_id: "s1", status: "completed", summary: "done" });
  assert.equal(run.liveTasks.size, 0);
});
