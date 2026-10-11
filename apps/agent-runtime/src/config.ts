import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

const env = process.env;

function falosHome(): string {
  return env.FALOS_HOME || join(homedir(), ".fal-os");
}

/** FAL_KEY from the environment, else `$FALOS_HOME/fal.env`, else `~/.fal-os/fal.env` (a `FAL_KEY=…` line). Never logged. */
export function falKey(): string | undefined {
  if (env.FAL_KEY) return env.FAL_KEY.trim();
  for (const dir of [falosHome(), join(homedir(), ".fal-os")]) {
    const p = join(dir, "fal.env");
    if (!existsSync(p)) continue;
    for (const line of readFileSync(p, "utf8").split("\n")) {
      const m = line.match(/^\s*(?:export\s+)?FAL_KEY\s*=\s*["']?([^"'\s]+)["']?\s*$/);
      if (m) return m[1];
    }
  }
  return undefined;
}

function which(bin: string): string | undefined {
  try {
    return execFileSync("which", [bin], { encoding: "utf8" }).trim() || undefined;
  } catch {
    return undefined;
  }
}

export const config = {
  hostUrl: env.FALOS_HOST_URL || "http://127.0.0.1:7317",
  token: env.FALOS_TOKEN || "",
  runtimeToken: env.FALOS_RUNTIME_TOKEN || "",
  projectDir: env.FALOS_PROJECT_DIR || "",
  directorModel: env.FALOS_DIRECTOR_MODEL || "claude-opus-5-5",
  workerModel: env.FALOS_WORKER_MODEL || "claude-opus-5-5",
  /** The locally installed Claude Code CLI (uses its login); the SDK's bundled one otherwise. */
  claudePath: env.FALOS_CLAUDE_PATH || which("claude"),
  /** Circuit breaker: stop generating after this many fal Agent tasks fail in a row. */
  maxConsecutiveFailures: Number(env.FALOS_MAX_CONSECUTIVE_FAILURES || 3),
  /** Runaway guard: fal Agent tasks per chat run. */
  maxFalTasks: Number(env.FALOS_MAX_FAL_TASKS || 100),
  /** Director turn cap (API round trips) per run. */
  maxTurns: Number(env.FALOS_MAX_TURNS || 500),
  motionTemplate: new URL("../motion-template/", import.meta.url).pathname,
  /** HyperFrames (HTML + GSAP → MP4) template: the second renderer, for kinetic typography. */
  hyperframesTemplate: new URL("../hyperframes-template/", import.meta.url).pathname,
  /** Agent skills fetched by `scripts/install-skills.sh` (Remotion's and HyperFrames' official ones). */
  skillsDir: env.FALOS_SKILLS_DIR || new URL("../skills/", import.meta.url).pathname,
  /** QA script the motion and qa agents run on renders. */
  motionQa: new URL("../scripts/motion-qa.mjs", import.meta.url).pathname,
  codexHome: env.CODEX_HOME || join(homedir(), ".codex"),
};

/** Remotion's downloaded chrome-headless-shell (HyperFrames renders with it too). */
export function remotionChrome(): string | undefined {
  const base = join(config.motionTemplate, "node_modules", ".remotion", "chrome-headless-shell");
  for (const p of [
    join(base, "mac-arm64", "chrome-headless-shell-mac-arm64", "chrome-headless-shell"),
    join(base, "mac-x64", "chrome-headless-shell-mac-x64", "chrome-headless-shell"),
    join(base, "linux64", "chrome-headless-shell-linux64", "chrome-headless-shell"),
    join(base, "linux-arm64", "chrome-headless-shell-linux-arm64", "chrome-headless-shell"),
  ])
    if (existsSync(p)) return p;
  return undefined;
}

export { which };

/** Codex can run when signed in with ChatGPT (`codex login`) or given an API key. */
export function codexAuthAvailable(): boolean {
  return Boolean(env.OPENAI_API_KEY || env.CODEX_API_KEY) || existsSync(join(config.codexHome, "auth.json"));
}
