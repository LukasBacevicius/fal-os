import assert from "node:assert/strict";
import { test } from "node:test";
import { answerFrom } from "../src/drivers.ts";
import { Channel, doneSummary, friendlyError, generatedSummary, pastTense, scrubError } from "../src/events.ts";
import { ChatRun, promptText, type RunFrame } from "../src/run.ts";

const frame = (over: Partial<RunFrame> = {}): RunFrame => ({
  chat: "c_test",
  canvas: "main",
  text: "make a moodboard",
  context: [],
  history: [],
  project: "/tmp/falos-test-project",
  ...over,
});

test("past-tense summaries", () => {
  assert.equal(generatedSummary(["image", "image", "image", "image"], "fal-ai/flux/schnell"), "generated 4 images (flux/schnell)");
  assert.equal(generatedSummary(["video", "image"], "fal Agent"), "generated 1 video + 1 image (fal Agent)");
  assert.equal(doneSummary("edit", { text: 'note "a"\nnote "b"' }, { created: ["a1b2c3", "d4e5f6"], touched: ["a1b2c3", "d4e5f6"] }, ["a1b2c3", "d4e5f6"]), "wrote 2 notes");
  assert.equal(pastTense("Generating images"), "Generated images");
  assert.equal(pastTense("Running flux"), "Ran flux");
  assert.equal(doneSummary("get_context", { chips: [{}, {}, {}] }, null, undefined), "looked at 3 nodes");
  assert.equal(doneSummary("generate_batch", { model: "fal-ai/flux/schnell", count: 200 }, { total: 200 }, []), "generating 200 outputs (flux/schnell)");
});

test("SDK errors are mapped and scrubbed", () => {
  assert.equal(friendlyError("error_during_execution", 500), "the director stopped mid-step");
  assert.match(friendlyError("error_max_turns", 500), /500 steps/);
  assert.equal(scrubError("error_during_execution: [ede_diagnostic] result_type=user"), "the director stopped mid-step");
  assert.equal(scrubError("Claude Code returned an error result: [ede_diagnostic] x"), "the director stopped mid-step");
  assert.match(scrubError("Error: Claude Code returned an error result: Failed to authenticate: OAuth session expired and could not be refreshed"), /isn't signed in/);
});

test("channel yields pushed items until closed", async () => {
  const c = new Channel<number>();
  c.push(1);
  setTimeout(() => {
    c.push(2);
    c.close();
    c.push(3);
  }, 5);
  const got: number[] = [];
  for await (const x of c) got.push(x);
  assert.deepEqual(got, [1, 2]);
});

test("fal Agent answers from the composer", () => {
  const q = { kind: "clarification", questions: [{ id: "q1", text: "Model?", options: [{ id: "o1", label: "Kling" }, { id: "o2", label: "Veo" }] }] };
  assert.deepEqual(answerFrom(q, "2").answer.answers[0], { question_id: "q1", selected_option_ids: ["o2"] });
  assert.deepEqual(answerFrom(q, "let's use veo please").answer.answers[0], { question_id: "q1", selected_option_ids: ["o2"] });
  assert.equal(answerFrom({ kind: "approval", prompt: "Spend $1?" }, "yes").answer.decision, "approve");
  assert.equal(answerFrom({ kind: "approval", prompt: "Spend $1?" }, "no thanks").answer.decision, "reject");
});

test("queue: enqueue, unqueue, injected frames, nextQueued", async () => {
  const frames: any[] = [];
  const run = new ChatRun(frame(), () => {}, async () => null, (m) => frames.push(m));
  assert.ok(run.enqueue({ id: "q_1", text: "also blue" }));
  assert.ok(run.enqueue({ id: "q_2", text: "drop me" }));
  run.unqueue("q_2");
  const q = await run.nextQueued(100);
  assert.equal(q?.id, "q_1");
  assert.deepEqual(frames, [{ type: "injected", chat: "c_test", id: "q_1", text: "also blue" }]);
  // A waiter gets the next message as soon as it arrives.
  const waiting = run.nextQueued(1000);
  run.enqueue({ id: "q_3", text: "answer: 2" });
  assert.equal((await waiting)?.text, "answer: 2");
  run.closeQueue();
  assert.equal(run.enqueue({ id: "q_4", text: "late" }), false);
});

test("events are attributed to agents and their parents; sessions are reported", () => {
  const events: any[] = [];
  const frames: any[] = [];
  const run = new ChatRun(frame(), (e) => events.push(e), async () => null, (m) => frames.push(m));
  const on = (m: any) => (run as any).onMessage(m);
  on({ type: "assistant", parent_tool_use_id: null, message: { content: [{ type: "tool_use", id: "A1", name: "Agent", input: { subagent_type: "generator", description: "hero shots" } }] } });
  on({ type: "assistant", parent_tool_use_id: "A1", message: { content: [{ type: "tool_use", id: "T1", name: "mcp__falos__session", input: { agentName: "Generator" } }, { type: "text", text: "On it." }] } });
  on({ type: "user", parent_tool_use_id: "A1", message: { content: [{ type: "tool_result", tool_use_id: "T1", content: '{"sessionId":"s_1"}' }] } });
  run.emit({ kind: "cost", usd: 1 });
  const started = events.find((e) => e.kind === "agent" && e.state === "started");
  assert.deepEqual([started.agent, started.parent], ["generator", "director"]);
  const tool = events.find((e) => e.kind === "tool" && e.name === "session" && e.state === "running");
  assert.deepEqual([tool.agent, tool.parent], ["generator", "director"]);
  const text = events.find((e) => e.kind === "text");
  assert.deepEqual([text.agent, text.parent, text.delta], ["generator", "director", "On it.\n"]);
  assert.equal(events.at(-1).agent, "director");
  assert.deepEqual(frames.at(-1), { type: "session.agent", chat: "c_test", session: "s_1", agent: "generator", parent: "director" });
  // Thinking blocks merge into one "thought for Ns" event.
  on({ type: "stream_event", parent_tool_use_id: null, event: { type: "content_block_start", content_block: { type: "thinking" } } });
  on({ type: "stream_event", parent_tool_use_id: null, event: { type: "content_block_stop" } });
  const t = events.at(-1);
  assert.equal(t.kind, "thinking_done");
  assert.equal(t.agent, "director");
  assert.ok(t.seconds >= 1);
});

test("prompt carries the outline on the first turn and the driver switch", () => {
  const p = promptText(frame({ outline: "canvas `main` · 2 nodes", history: [{ role: "user", text: "earlier" }], previous_driver: "fal" }), { resume: false, notes: [] });
  assert.match(p, /Canvas outline/);
  assert.match(p, /earlier turns ran on the fal driver/);
  const r = promptText(frame({ outline: "x" }), { resume: true, notes: [] });
  assert.doesNotMatch(r, /Canvas outline/);
});
