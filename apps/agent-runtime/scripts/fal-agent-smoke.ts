// Smoke test for the fal Agent SDK: one small task, prints each snapshot's shape (no secrets).
// Usage: pnpm tsx scripts/fal-agent-smoke.ts "Generate one product photo of ..."
import { createFalClient } from "@fal-ai/client";
import { falKey } from "../src/config.ts";

const key = falKey();
if (!key) throw new Error("no FAL_KEY (env or ~/.fal-os/fal.env)");
const agent = createFalClient({ credentials: key }).agent;
const task = process.argv[2] || "Generate one small square product photo of white over-ear headphones on a grey background.";
let last: any;
for await (const r of agent.stream({ input: task }, { timeoutMs: 10 * 60 * 1000 })) {
  last = r;
  console.log(
    JSON.stringify({
      id: r.id,
      status: r.status,
      phase: r.fal?.phase,
      text: r.output_text?.slice(0, 120),
      activities: r.fal?.activities?.map((a: any) => ({ kind: a.kind, label: a.label, status: a.status, name: a.name })),
      artifacts: r.artifacts.map((a) => ({ id: a.id, kind: a.kind, media_type: a.media_type, files: a.files?.map((f) => f.mime_type), meta: a.metadata })),
      pending: r.pending_inputs.map((p) => ({ kind: p.kind, prompt: p.prompt })),
      cost: r.usage?.cost,
    }),
  );
}
console.log("FINAL", JSON.stringify({ output: last?.output?.map((o: any) => o.type + ":" + (o.kind ?? "")), final: last?.final_artifacts?.length, activities: last?.fal?.activities }, null, 1).slice(0, 4000));
