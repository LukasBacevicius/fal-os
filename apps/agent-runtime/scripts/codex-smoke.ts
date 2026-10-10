// Smoke test for the Codex worker: one tiny task in a temp dir, printing the event stream.
import { mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Codex } from "@openai/codex-sdk";
import { codexAuthAvailable } from "../src/config.ts";

console.log("codex auth available:", codexAuthAvailable());
const dir = mkdtempSync(join(process.env.SMOKE_DIR ?? tmpdir(), "codex-smoke-"));
const thread = new Codex().startThread({ workingDirectory: dir, skipGitRepoCheck: true, sandboxMode: "workspace-write", approvalPolicy: "never", modelReasoningEffort: "low" });
const { events } = await thread.runStreamed("Create a file hello.ts exporting `export const hello = (n: string) => `Hello, ${n}!`;`. Nothing else.");
for await (const ev of events) {
  const it: any = (ev as any).item;
  console.log(ev.type, it ? `${it.type}${it.command ? ` $ ${it.command}` : ""}${it.text ? `: ${String(it.text).slice(0, 100)}` : ""}` : (ev as any).message ?? "");
}
console.log("hello.ts:", readFileSync(join(dir, "hello.ts"), "utf8"));
