// What the swarm can use right now. Probed at runtime start and before every chat turn (cheap
// file checks every time; network checks cached), emitted as a `capabilities` chat event and
// written into the director's and subagents' system prompts so they only plan with tools
// that work.
import { existsSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { codexAuthAvailable, config, falKey, remotionChrome, which } from "./config.ts";
import { falAgentAvailable } from "./fal.ts";
import { HostMcp, type HostTool } from "./hostMcp.ts";

export type CapName =
  | "claude"
  | "fal-agent"
  | "fal-models"
  | "codex"
  | "remotion"
  | "hyperframes"
  | "ffmpeg"
  | "remotion-skills"
  | "hyperframes-skills"
  // Assets v1 (docs/ASSETS-V1.md), detected from the host's MCP tools/list:
  | "asset-ops"
  | "asset-export"
  | "asset-library"
  | "media-inputs"
  | "provenance";

/** What the host's asset surface supports (Assets v1 §3/§7); every flag comes from tools/list. */
export type AssetCaps = {
  /** `inspect` + `transform` (+ `list_ops`): the host op registry. */
  ops: boolean;
  export: boolean;
  compose: boolean;
  rerun: boolean;
  package: boolean;
  /** `search_assets` & co. */
  library: boolean;
  /** `generate` / `generate_batch` accept `mediaInputs` (nodes or chips). */
  mediaInputs: boolean;
  /** `import_file` lineage takes `kind`/`op`/`recipe`/parent roles; nodes have `role`/`protected`. */
  provenance: boolean;
  /** An installed agent skill that writes OTIO timelines (e.g. Remotion's opentimeline). */
  otioSkill?: string;
};

export type Capabilities = {
  available: CapName[];
  missing: { name: CapName; fix: string }[];
  /** Local plugin dirs with agent skills (for the SDK's `plugins` option). */
  plugins: { name: string; path: string }[];
  /** The host's MCP tool names (empty when the host MCP isn't reachable). */
  hostTools: string[];
  assets: AssetCaps;
};

const NET_TTL_MS = 5 * 60 * 1000;
const TOOLS_TTL_MS = 60 * 1000;
let hostOk: { at: number; ok: boolean } | null = null;
let toolsCache: { at: number; tools: HostTool[] | null } | null = null;

/** The host's MCP tools (null: not reachable). Cached briefly; a host upgrade shows up within a minute. */
async function hostTools(refresh = false): Promise<HostTool[] | null> {
  if (!refresh && toolsCache && Date.now() - toolsCache.at < TOOLS_TTL_MS) return toolsCache.tools;
  let tools: HostTool[] | null = null;
  if (config.token)
    try {
      tools = await new HostMcp().listTools();
    } catch {
      tools = null;
    }
  toolsCache = { at: Date.now(), tools };
  return tools;
}

const LIBRARY_TOOLS = ["search_assets", "get_asset", "place_asset", "tag_assets", "collect_assets", "show_assets"];

/** Asset capabilities from a host's tools/list (pure; schemas are checked for the new args). */
export function assetCapsFrom(tools: HostTool[]): AssetCaps {
  const by = new Map(tools.map((t) => [t.name, t]));
  const schemaHas = (tool: string, word: string) => by.has(tool) && JSON.stringify(by.get(tool)?.inputSchema ?? {}).includes(word);
  return {
    ops: by.has("inspect") && by.has("transform"),
    export: by.has("export"),
    compose: by.has("compose"),
    rerun: by.has("rerun"),
    package: by.has("package"),
    library: by.has("search_assets"),
    mediaInputs: schemaHas("generate", "mediaInputs") || schemaHas("generate_batch", "mediaInputs"),
    provenance: schemaHas("import_file", "recipe"),
  };
}

/** An installed skill that writes OpenTimelineIO (none of the pinned Remotion skills does yet). */
function otioSkill(plugins: { name: string; path: string }[]): string | undefined {
  for (const p of plugins) {
    const dir = join(p.path, "skills");
    try {
      const hit = readdirSync(dir).find((s) => /otio|opentimeline/i.test(s));
      if (hit) return `${p.name}:${hit}`;
    } catch {}
  }
  return undefined;
}

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

  // Assets v1: only what this host build exposes. Agents never call an absent tool; the asset
  // rules in their prompts fall back to Bash/ffmpeg + `import_file` lineage instead.
  const tools = await hostTools(opts.refreshNet);
  const assets: AssetCaps = { ...assetCapsFrom(tools ?? []), otioSkill: otioSkill(plugins) };
  const hostFix = (what: string) =>
    tools === null ? "the host's MCP endpoint isn't reachable (restart falos-host)" : `this falos-host build has no ${what} yet (Assets v1); update falos-host`;
  has("asset-ops", assets.ops, `${hostFix("`inspect`/`transform` ops")} — agents derive with ffmpeg + \`import_file\` lineage meanwhile`);
  has("asset-export", assets.export, `${hostFix("`export`")} — the finisher encodes deliverables with ffmpeg meanwhile`);
  has("asset-library", assets.library, hostFix("`search_assets` library tools"));
  has("media-inputs", assets.mediaInputs, `${hostFix("`mediaInputs` on `generate`")} — swarm \`fal_model\` mediaInputs work meanwhile`);
  has("provenance", assets.provenance, `${hostFix("provenance lineage (kind/recipe/roles)")} — legacy lineage {model, params, parents} meanwhile`);
  return { available, missing, plugins, hostTools: (tools ?? []).map((t) => t.name), assets };
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
  "asset-ops": "Asset ops (fal-os `list_ops` / `inspect` / `transform`): probe, contact sheets, palette, loudness; crop/resize/reframe/trim/concat/speed/captions/audio mix/remove_bg/upscale/ffmpeg — each result a new node with lineage.",
  "asset-export": "Export (fal-os `export`, swarm `deliver`): nodes/frames/\"deliverables\" → preset files in <project>/deliverables with a manifest and file cards in chat.",
  "asset-library": "Asset library (fal-os `search_assets` / `get_asset` / `place_asset` / `tag_assets` / `collect_assets` / `show_assets`): the workspace + fal library.",
  "media-inputs": "fal-os `generate` / `generate_batch` take `mediaInputs` ({param: node id or chip}): canvas media and chips as model inputs.",
  provenance: "Provenance: `import_file` lineage {kind, op, recipe, parents:[{node, role}]}; node `role(…)` and `protected`.",
};

/** Capability block for system prompts. */
export function capabilityPrompt(c: Capabilities): string {
  const extra: Partial<Record<CapName, string>> = {
    "asset-ops": [c.assets?.compose && "`compose` (grid, contact_sheet, storyboard, moodboard)", c.assets?.rerun && "`rerun` (re-run a recipe with changes / on other nodes)"].filter(Boolean).join(", "),
    "asset-export": c.assets?.package ? "`package` (project or deliverables .zip with a manifest)" : "",
  };
  const yes = c.available.map((n) => `- ${LINES[n]}${extra[n] ? ` Also ${extra[n]}.` : ""}`).join("\n");
  const no = c.missing.length ? `\nNOT available right now — never call or plan with these:\n${c.missing.map((m) => `- ${m.name} (fix: ${m.fix})`).join("\n")}` : "";
  return `## Tools available in this run\n${yes}${no}`;
}

/** The `capabilities` chat event. */
export function capabilityEvent(c: Capabilities) {
  return { kind: "capabilities", available: c.available, missing: c.missing.map(({ name, fix }) => ({ name, fix })) };
}
