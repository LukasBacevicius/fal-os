import assert from "node:assert/strict";
import { test } from "node:test";
import { autoDecide, costFromPrice, findMedia, snapshotOf } from "../src/fal.ts";

test("snapshotOf picks final media artifacts, activities and cost", () => {
  const s = snapshotOf({
    id: "resp_1",
    status: "completed",
    output_text: "done",
    usage: { cost: { currency: "USD", estimated: 0.5, settled: 0.42 } },
    fal: { phase: "finished", activities: [{ id: "a", kind: "tool", label: "Generating", status: "completed", name: "fal-ai/flux/dev" }] },
    artifacts: [{ id: "x", kind: "data" }],
    final_artifacts: [
      { id: "m1", kind: "media", media_type: "image", files: [{ role: "output", url: "https://f/1.png", mime_type: "image/png" }] },
      { id: "m2", kind: "media", media_type: "video", files: [{ role: "thumbnail", url: "https://f/t.jpg", mime_type: "image/jpeg" }, { role: "output", url: "https://f/2.mp4", mime_type: "video/mp4" }] },
    ],
  });
  assert.equal(s.costUsd, 0.42);
  assert.deepEqual(s.artifacts.map((a) => a.url), ["https://f/1.png", "https://f/2.mp4"]);
  assert.equal(s.activities[0].name, "fal-ai/flux/dev");
});

test("autoDecide approves and picks first options", () => {
  const a = autoDecide({ kind: "approval", prompt: "Spend $0.40?", accepted_answers: ["approve", "reject"] });
  assert.deepEqual(a.answer, { kind: "approval", decision: "approve" });
  const q = autoDecide({
    kind: "clarification",
    prompt: "?",
    questions: [
      { id: "q1", text: "Model?", multiple: false, allow_text: false, options: [{ id: "o1", label: "Kling" }, { id: "o2", label: "Veo" }] },
      { id: "q2", text: "Notes?", multiple: false, allow_text: true, options: [] },
    ],
  });
  assert.deepEqual(q.answer.answers[0], { question_id: "q1", selected_option_ids: ["o1"] });
  assert.equal(q.answer.answers[1].selected_option_ids.length, 0);
  assert.ok(q.answer.answers[1].text);
});

test("findMedia and costFromPrice", () => {
  const m = findMedia({ images: [{ url: "https://a/1.png", width: 1920, height: 1080, content_type: "image/png" }], seed: 1 });
  assert.equal(m.length, 1);
  assert.ok(Math.abs(costFromPrice({ unit: "megapixels", usd: 0.04 }, {}, m)! - 0.04 * 2.0736) < 1e-9);
  assert.ok(Math.abs(costFromPrice({ unit: "seconds", usd: 0.056 }, { duration: "5" }, [{}])! - 0.28) < 1e-9);
  assert.equal(costFromPrice({ unit: "audios", usd: 0.2 }, {}, [{}]), 0.2);
  assert.equal(costFromPrice({ unit: "30 seconds", usd: 0.1 }, {}, [{}], 45), 0.2);
  assert.equal(costFromPrice({ unit: "1m tokens", usd: 2.5 }, {}, []), undefined);
  assert.equal(costFromPrice(undefined, {}, []), undefined);
});
