// First-run setup for the packaged desktop app (also usable from a checkout):
//
//   node dist/setup.mjs doctor      → JSON: claude / codex / ffmpeg / fal key / motion tools / disk
//   node dist/setup.mjs check-key   → JSON: whether FAL_KEY (env) works for the Agent API and the queue API
//   node dist/setup.mjs install     → installs the motion tools into $FALOS_HOME:
//                                     templates/{motion,hyperframes}-template (+ npm install),
//                                     Remotion's chrome-headless-shell, skills/{remotion,hyperframes}
//
// Paths come from the same env the runtime uses (FALOS_MOTION_TEMPLATE, FALOS_HYPERFRAMES_TEMPLATE,
// FALOS_SKILLS_DIR); `install` copies the template sources shipped next to this file
// (`../motion-template`, `../hyperframes-template`). Progress goes to `$FALOS_HOME/setup-state.json`
// and stdout. Secrets are never printed.
import { execFileSync, spawn } from "node:child_process";
import { cpSync, existsSync, mkdirSync, readFileSync, renameSync, rmSync, statfsSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join } from "node:path";
import { codexAuthAvailable, config, falKey, remotionChrome, which } from "./config.ts";
import { falAgentAvailable } from "./fal.ts";

const home = process.env.FALOS_HOME || join(homedir(), ".fal-os");
const shipped = (name: string) => new URL(`../${name}/`, import.meta.url).pathname;
const statePath = join(home, "setup-state.json");

function state(s: Record<string, unknown>) {
  mkdirSync(home, { recursive: true });
  const v = { ...s, at: new Date().toISOString() };
  writeFileSync(statePath, JSON.stringify(v));
  console.log(JSON.stringify(v));
}

function run(cmd: string, args: string[], cwd: string): Promise<void> {
  return new Promise((resolve, reject) => {
    const p = spawn(cmd, args, { cwd, stdio: ["ignore", "inherit", "inherit"], env: process.env });
    p.on("error", reject);
    p.on("exit", (code) => (code === 0 ? resolve() : reject(new Error(`${cmd} ${args.join(" ")} exited with ${code}`))));
  });
}

function claudeStatus() {
  const path = config.claudePath;
  if (!path) return { path: null, loggedIn: false };
  try {
    const j = JSON.parse(execFileSync(path, ["auth", "status", "--json"], { encoding: "utf8", timeout: 15_000 }));
    return { path, loggedIn: Boolean(j.loggedIn), authMethod: j.authMethod ?? null, subscription: j.subscriptionType ?? null };
  } catch {
    // Older CLIs have no `auth status`: assume the login works if the binary runs.
    return { path, loggedIn: null };
  }
}

function extras() {
  return {
    motion: existsSync(join(config.motionTemplate, "node_modules", "remotion")),
    hyperframes: existsSync(join(config.hyperframesTemplate, "node_modules", "hyperframes")),
    chrome: Boolean(remotionChrome()),
    skills: ["remotion", "hyperframes"].every((n) => existsSync(join(config.skillsDir, n, ".claude-plugin", "plugin.json"))),
  };
}

function diskFreeGb(): number | null {
  try {
    const s = statfsSync(home.startsWith("/") && existsSync(home) ? home : homedir());
    return Math.round(((s.bavail * s.bsize) / 1e9) * 10) / 10;
  } catch {
    return null;
  }
}

async function doctor() {
  let last: unknown = null;
  try {
    last = JSON.parse(readFileSync(statePath, "utf8"));
  } catch {}
  const codexPath = config.codexPath ?? which("codex") ?? null;
  return {
    claude: claudeStatus(),
    codex: { path: codexPath, signedIn: codexAuthAvailable() },
    ffmpeg: Boolean(which("ffmpeg") && which("ffprobe")),
    node: process.version,
    falKey: Boolean(falKey()),
    extras: extras(),
    install: last,
    diskFreeGb: diskFreeGb(),
  };
}

async function checkKey() {
  const key = falKey();
  if (!key) return { ok: false, agent: { ok: false, reason: "no key" }, models: { ok: false, reason: "no key" } };
  const agent = await falAgentAvailable(true);
  // The queue API without spending anything: a status lookup for a request id that doesn't exist
  // answers 404 for a valid key and 401/403 for a bad one.
  let models: { ok: boolean; reason?: string };
  try {
    const r = await fetch("https://queue.fal.run/fal-ai/flux/requests/00000000-0000-0000-0000-000000000000/status", {
      headers: { Authorization: `Key ${key}` },
      signal: AbortSignal.timeout(20_000),
    });
    models = r.status === 401 || r.status === 403 ? { ok: false, reason: `${r.status} key rejected` } : { ok: true };
  } catch (e: any) {
    models = { ok: false, reason: String(e?.message ?? e).slice(0, 200) };
  }
  return { ok: models.ok, agent, models };
}

/** `NAME_COMMIT=…` pins from install-skills.sh (the one place they're maintained). */
function skillPins(): Record<string, string> {
  const sh = readFileSync(new URL("../scripts/install-skills.sh", import.meta.url), "utf8");
  const pin = (n: string) => sh.match(new RegExp(`^${n}_COMMIT=([0-9a-f]{40})$`, "m"))?.[1] ?? "";
  return { remotion: pin("REMOTION"), hyperframes: pin("HYPERFRAMES") };
}

/** Fetch a GitHub repo at a commit as a tarball (no git needed) and keep `paths` (all if empty). */
async function fetchSkills(name: string, repo: string, commit: string, paths: string[]) {
  const dest = join(config.skillsDir, name);
  const tmp = join(config.skillsDir, `.${name}.tmp`);
  rmSync(tmp, { recursive: true, force: true });
  mkdirSync(tmp, { recursive: true });
  const r = await fetch(`https://codeload.github.com/${repo}/tar.gz/${commit}`, { signal: AbortSignal.timeout(120_000) });
  if (!r.ok) throw new Error(`download ${repo}@${commit}: HTTP ${r.status}`);
  const tgz = join(tmp, "src.tgz");
  writeFileSync(tgz, Buffer.from(await r.arrayBuffer()));
  const include = paths.flatMap((p) => ["--include", `*/${p}`, "--include", `*/${p}/*`]);
  execFileSync("tar", ["-xzf", tgz, "-C", tmp, "--strip-components", "1", ...include]);
  rmSync(tgz);
  writeFileSync(join(tmp, "SOURCE"), `${repo}@${commit}\n`);
  rmSync(dest, { recursive: true, force: true });
  renameSync(tmp, dest);
}

function npmCli(): [string, string[]] {
  // The packaged app ships npm next to node (node/lib/node_modules/npm); a checkout uses PATH.
  const bundled = join(dirname(process.execPath), "..", "lib", "node_modules", "npm", "bin", "npm-cli.js");
  return existsSync(bundled) ? [process.execPath, [bundled]] : ["npm", []];
}

async function installTemplate(name: string, dir: string) {
  const src = shipped(name);
  mkdirSync(dir, { recursive: true });
  for (const f of ["src", "package.json", "tsconfig.json", "remotion.config.ts", "index.html"])
    if (existsSync(join(src, f))) cpSync(join(src, f), join(dir, f), { recursive: true });
  const [npm, pre] = npmCli();
  await run(npm, [...pre, "install", "--no-audit", "--no-fund", "--loglevel=error"], dir);
}

async function install() {
  const steps: [string, () => Promise<void>][] = [
    ["skills", async () => {
      const pins = skillPins();
      await fetchSkills("remotion", "remotion-dev/claude-code-plugin", pins.remotion, []);
      await fetchSkills("hyperframes", "heygen-com/hyperframes", pins.hyperframes, [".claude-plugin", "skills", "LICENSE"]);
    }],
    ["remotion", () => installTemplate("motion-template", config.motionTemplate)],
    ["chrome", () => run(join(config.motionTemplate, "node_modules", ".bin", "remotion"), ["browser", "ensure"], config.motionTemplate)],
    ["hyperframes", () => installTemplate("hyperframes-template", config.hyperframesTemplate)],
  ];
  const only = process.argv.slice(3);
  for (const [step, f] of steps) {
    if (only.length && !only.includes(step)) continue;
    state({ running: true, step });
    try {
      await f();
    } catch (e: any) {
      state({ running: false, step, error: String(e?.message ?? e).slice(0, 500) });
      process.exitCode = 1;
      return;
    }
  }
  state({ running: false, done: true, extras: extras() });
}

const cmd = process.argv[2];
if (cmd === "doctor") console.log(JSON.stringify(await doctor()));
else if (cmd === "check-key") console.log(JSON.stringify(await checkKey()));
else if (cmd === "install") await install();
else {
  console.error("usage: setup <doctor|check-key|install [skills|remotion|chrome|hyperframes…]>");
  process.exitCode = 2;
}
