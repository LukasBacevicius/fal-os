// Smoke test for the in-process swarm MCP server: a cheap Claude query that calls swarm tools
// (share_files, chat_title) and prints every emitted chat event.
//   tsx scripts/swarm-smoke.ts <project dir> <file inside it> [--resume]
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { query, type SDKUserMessage } from "@anthropic-ai/claude-agent-sdk";
import { config } from "../src/config.ts";
import { Channel } from "../src/events.ts";
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
// --stream <s>: streaming input like the runtime; a first turn, <s> seconds idle, then the swarm call.
const si = process.argv.indexOf("--stream");
const idle = si >= 0 ? Number(process.argv[si + 1]) : -1;
const ask = `Call the swarm tool share_files with files ["${file}"], then call chat_title with "Smoke". Reply "ok".`;
const input = new Channel<SDKUserMessage>();
const user = (text: string) => ({ type: "user", message: { role: "user", content: [{ type: "text", text }] }, parent_tool_use_id: null }) as SDKUserMessage;
if (idle >= 0) {
  input.push(user(`Call chat_title with "Warmup". Reply "ok".`));
  setTimeout(() => input.push(user(ask)), idle * 1000);
}
// --agent: a background subagent calls swarm tools while the main agent is idle, then the main
// agent calls share_files after it finishes (the swarm pattern that came back "interrupted").
const withAgent = process.argv.includes("--agent");
if (withAgent)
  input.push(
    user(
      `Launch the "helper" subagent with the Agent tool and run_in_background: true, task: "call chat_title with 'Sub', then share_files with files [\\"${file}\\"]". Then end your turn. When you are notified that it finished, call share_files with files ["${file}"] yourself and reply "ok".`,
    ),
  );
let results = 0;
const live = new Set<string>();
const q = query({
  prompt: idle >= 0 || withAgent ? input : ask,
  options: {
    ...(withAgent
      ? {
          agents: { helper: { description: "test helper", prompt: "Do exactly the task with the swarm tools, then reply done.", model: "haiku", tools: ["mcp__swarm__chat_title", "mcp__swarm__share_files"] } },
        }
      : {}),
    model: "claude-haiku-4-5",
    cwd: projectDir,
    permissionMode: "bypassPermissions",
    allowDangerouslySkipPermissions: true,
    settingSources: [],
    strictMcpConfig: true,
    tools: withAgent ? ["Agent"] : [],
    pathToClaudeCodeExecutable: config.claudePath,
    mcpServers: { swarm },
    ...(resume && existsSync(sessionFile) ? { resume: JSON.parse(readFileSync(sessionFile, "utf8")).sessionId } : {}),
  },
});
for await (const m of q) {
  if (m.type === "system" && m.subtype === "init") writeFileSync(sessionFile, JSON.stringify({ sessionId: m.session_id }));
  if (m.type === "user") for (const b of (m as any).message?.content ?? []) if (b.type === "tool_result") console.log("RESULT", JSON.stringify(b.content).slice(0, 300));
  const s: any = m;
  if (s.type === "system" && s.subtype === "task_started") live.add(s.task_id);
  if (s.type === "system" && s.subtype === "task_notification") live.delete(s.task_id);
  if (m.type === "result") {
    console.log("DONE", (m as any).subtype, new Date().toISOString(), `live tasks ${live.size}`);
    results++;
    if (withAgent ? live.size === 0 && results >= 2 : idle < 0 || results === 2) input.close();
  }
}
console.log(`files events: ${events.filter((e) => e.kind === "files").length}`);
