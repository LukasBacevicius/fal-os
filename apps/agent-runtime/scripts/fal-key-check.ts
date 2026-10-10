// Checks what the configured FAL_KEY can do (no secrets printed): the Agent API and the queue API.
import { createFalClient } from "@fal-ai/client";
import { falKey } from "../src/config.ts";

const key = falKey();
if (!key) throw new Error("no FAL_KEY (env or ~/.fal-os/fal.env)");
const fal = createFalClient({ credentials: key });
try {
  const r = await fal.agent.models.listAgentModels();
  console.log("agent API: ok", JSON.stringify(r).slice(0, 300));
} catch (e: any) {
  console.log("agent API:", e?.status ?? "", e?.cause?.body?.error?.message ?? e?.message);
}
try {
  const r: any = await fal.subscribe("fal-ai/flux/schnell", { input: { prompt: "a white dot", image_size: "square", num_inference_steps: 1 } });
  console.log("queue API: ok", r?.data?.images?.length, "image(s)");
} catch (e: any) {
  console.log("queue API:", e?.status ?? "", e?.body?.detail ?? e?.message);
}
