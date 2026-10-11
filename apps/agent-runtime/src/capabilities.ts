// What the swarm can use right now. Probed at runtime start and before every chat turn (cheap
// file checks every time; network checks cached), emitted as a `capabilities` chat event and
// written into the director's and subagents' system prompts so they only plan with tools
// that work.
import { existsSync } from "node:fs";
import { join } from "node:path";
import { codexAuthAvailable, config, falKey, remotionChrome, which } from "./config.ts";
import { falAgentAvailable } from "./fal.ts";
import { HostMcp } from "./hostMcp.ts";

export type CapName = "claude" | "fal-agent" | "fal-models" | "codex" | "remotion" | "hyperframes" | "ffmpeg" | "remotion-skills" | "hyperframes-skills";

export type Capabilities = {
  available: CapName[];
  missing: { name: CapName; fix: string }[];
  /** Local plugin dirs with agent skills (for the SDK's `plugins` option). */
  plugins: { name: string; path: string }[];
};

const NET_TTL_MS = 5 * 60 * 1000;
let hostOk: { at: number; ok: boolean } | null = null;

/** The host's MCP endpoint answers on a fresh session (what Codex will connect to). */
async function hostMcpWorks(): Promise<boolean> {
  if (hostOk && Date.now() - hostOk.at < NET_TTL_MS && hostOk.ok) return true;
  const ok = config.token ? await new HostMcp().ping() : false;
  hostOk = { at: Date.now(), ok };
  return ok;
}

export function skillPlugin(name: "remotion" | "hyperframes"): string | undefined {
  const dir = join(config.skillsDir, name);
  return existsSync(join(dir, ".claude-plugin", "plugin.json")) && existsSync(join(dir, "skills")) ? dir : undefined;
}

export async function probeCapabilities(opts: { refreshNet?: boolean } = {}): Promise<Capabilities> {
  const available: CapName[] = ["claude"];
  const missing: Capabilities["missing"] = [];
  const has = (name: CapName, ok: boolean, fix: string) => (ok ? available.push(name) : missing.push({ name, fix }));

  const key = Boolean(falKey());
  if (key) {
    const fa = await falAgentAvailable(opts.refreshNet);
    has("fal-agent", fa.ok, fa.ok ? "" : `the fal key lacks the AGENT preset (${fa.reason}); create one with the AGENT preset at fal.ai/dashboard/keys`);
  } else has("fal-agent", false, "put FAL_KEY=… (AGENT preset) in ~/.fal-os/fal.env");
  has("fal-models", key, "put FAL_KEY=… in ~/.fal-os/fal.env");

  const codexAuth = codexAuthAvailable();
  const mcp = codexAuth ? await hostMcpWorks() : false;
  has("codex", codexAuth && mcp, codexAuth ? "the host's MCP endpoint isn't reachable for Codex (restart falos-host)" : "run `npx codex login`");

  const ffmpeg = Boolean(which("ffmpeg") && which("ffprobe"));
  has("ffmpeg", ffmpeg, "install ffmpeg (`brew install ffmpeg`)");

  const remotionDeps = existsSync(join(config.motionTemplate, "node_modules", "remotion"));
  const chrome = remotionChrome();
  has(
    "remotion",
    remotionDeps && Boolean(chrome),
    !remotionDeps
      ? "run `pnpm --dir apps/agent-runtime/motion-template install`"
      : "run `npx remotion browser ensure` in apps/agent-runtime/motion-template",
  );
  const hfDeps = existsSync(join(config.hyperframesTemplate, "node_modules", "hyperframes")) && existsSync(join(config.hyperframesTemplate, "node_modules", "gsap"));
  has(
    "hyperframes",
    hfDeps && Boolean(chrome) && ffmpeg,
    !hfDeps ? "run `pnpm --dir apps/agent-runtime/hyperframes-template install --ignore-workspace`" : !chrome ? "install Remotion's browser first (it renders with the same chrome-headless-shell)" : "install ffmpeg",
  );

  const plugins: Capabilities["plugins"] = [];
  for (const name of ["remotion", "hyperframes"] as const) {
    const p = skillPlugin(name);
    has(`${name}-skills` as CapName, Boolean(p), "run `apps/agent-runtime/scripts/install-skills.sh`");
    if (p) plugins.push({ name, path: p });
  }
  return { available, missing, plugins };
}

const LINES: Record<CapName, string> = {
  claude: "Claude (you and your subagents): reasoning, writing, code (Read/Write/Edit/Bash).",
  "fal-agent": "fal Agent (swarm `fal_agent`): multi-step media tasks; it picks models.",
  "fal-models": "fal models (swarm `fal_model`, fal-os `generate` / `generate_batch`): exact endpoints.",
  codex: "Codex (swarm `codex`): OpenAI's coding agent — only when the user asks for Codex.",
  remotion: "Remotion (React → MP4) in work/motion: the default renderer for edits with footage, stills and music.",
  hyperframes: "HyperFrames (HTML + GSAP → MP4) in work/hyperframes: for typography-led kinetic pieces and motion graphics.",
  ffmpeg: "ffmpeg / ffprobe: frames, trims, probes, loudness, QA.",
  "remotion-skills": "Remotion's official agent skills (Skill tool: remotion:remotion-best-practices, remotion:remotion-markup, …).",
  "hyperframes-skills": "HyperFrames' official agent skills (Skill tool: hyperframes:hyperframes, hyperframes:motion-graphics, …).",
};

/** Capability block for system prompts. */
export function capabilityPrompt(c: Capabilities): string {
  const yes = c.available.map((n) => `- ${LINES[n]}`).join("\n");
  const no = c.missing.length ? `\nNOT available right now — never call or plan with these:\n${c.missing.map((m) => `- ${m.name} (fix: ${m.fix})`).join("\n")}` : "";
  return `## Tools available in this run\n${yes}${no}`;
}

/** The `capabilities` chat event. */
export function capabilityEvent(c: Capabilities) {
  return { kind: "capabilities", available: c.available, missing: c.missing.map(({ name, fix }) => ({ name, fix })) };
}
