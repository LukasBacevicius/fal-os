// Smoke test for the in-process swarm MCP server: a cheap Claude query that calls swarm tools
// (share_files, chat_title) and prints every emitted chat event.
//   tsx scripts/swarm-smoke.ts <project dir> <file inside it> [--resume]
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { query } from "@anthropic-ai/claude-agent-sdk";
import { config } from "../src/config.ts";
import { HostMcp } from "../src/hostMcp.ts";
import { createSwarmTools } from "../src/swarmTools.ts";

const [projectDir, file] = process.argv.slice(2);
const resume = process.argv.includes("--resume");
const workDir = join(projectDir, "work", "smoke");
const events: any[] = [];
const swarm = createSwarmTools({
  host: new HostMcp(),
  canvas: "main",
  workDir,
  signal: new AbortController().signal,
  emit: (e) => {
    events.push(e);
    console.log("EVENT", JSON.stringify(e).slice(0, 300));
  },
  agentFor: () => "director",
  addCost: () => {},
  chat: "c_smoke",
  request: async () => [],
  projectDir,
});
const sessionFile = join(projectDir, "smoke-session.json");
const q = query({
  prompt: `Call the swarm tool share_files with files ["${file}"], then call chat_title with "Smoke". Reply "ok".`,
  options: {
    model: "claude-haiku-4-5",
    cwd: projectDir,
    permissionMode: "bypassPermissions",
    allowDangerouslySkipPermissions: true,
    settingSources: [],
    strictMcpConfig: true,
    tools: [],
    pathToClaudeCodeExecutable: config.claudePath,
    mcpServers: { swarm },
    ...(resume && existsSync(sessionFile) ? { resume: JSON.parse(readFileSync(sessionFile, "utf8")).sessionId } : {}),
  },
});
for await (const m of q) {
  if (m.type === "system" && m.subtype === "init") writeFileSync(sessionFile, JSON.stringify({ sessionId: m.session_id }));
  if (m.type === "user") for (const b of (m as any).message?.content ?? []) if (b.type === "tool_result") console.log("RESULT", JSON.stringify(b.content).slice(0, 300));
  if (m.type === "result") console.log("DONE", (m as any).subtype);
}
console.log(`files events: ${events.filter((e) => e.kind === "files").length}`);
