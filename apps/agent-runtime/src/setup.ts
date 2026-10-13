// First-run setup for the packaged desktop app (also usable from a checkout):
//
//   node dist/setup.mjs doctor      → JSON: claude / codex / ffmpeg / fal key / motion tools / disk
//   node dist/setup.mjs check-key   → JSON: whether FAL_KEY (env) works for the Agent API and the queue API
//   node dist/setup.mjs install     → installs the motion tools into $FALOS_HOME:
//                                     templates/{motion,hyperframes}-template (+ npm install),
//                                     Remotion's chrome-headless-shell, skills/{remotion,hyperframes}
//   node dist/setup.mjs ffmpeg      → a pinned, sha256-checked static ffmpeg + ffprobe into $FALOS_HOME/bin
//   node dist/setup.mjs codex       → the Codex CLI with the bundled npm into $FALOS_HOME/npm
//
// Paths come from the same env the runtime uses (FALOS_MOTION_TEMPLATE, FALOS_HYPERFRAMES_TEMPLATE,
// FALOS_SKILLS_DIR); `install` copies the template sources shipped next to this file
// (`../motion-template`, `../hyperframes-template`). Progress goes to `$FALOS_HOME/setup-state.json`
// and stdout. Secrets are never printed.
import { execFileSync, spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { chmodSync, cpSync, existsSync, mkdirSync, readFileSync, renameSync, rmSync, statfsSync, writeFileSync } from "node:fs";
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
  let out: string;
  try {
    out = execFileSync(path, ["auth", "status", "--json"], { encoding: "utf8", timeout: 15_000, stdio: ["ignore", "pipe", "ignore"] });
  } catch (e: any) {
    // Signed out exits 1 but still prints the JSON.
    out = String(e?.stdout ?? "");
  }
  try {
    const j = JSON.parse(out);
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
    ffmpegPath: which("ffmpeg") ?? null,
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

// ---- ffmpeg without Homebrew ----------------------------------------------------------------
// Martin Riedl's static macOS arm64 release builds (ffmpeg.martin-riedl.de): Developer ID signed
// and notarized, GPL (x264/x265). Downloaded on the user's machine into $FALOS_HOME/bin, never
// bundled (docs/PACKAGING.md §2.7). Bump the version, URLs and hashes together.
const FFMPEG_VERSION = "9.0.2";
const FFMPEG_BASE = "https://ffmpeg.martin-riedl.de/download/macos/arm64/1789931890_9.0.2";
const FFMPEG_ZIPS: Record<"ffmpeg" | "ffprobe", string> = {
  ffmpeg: "c8ed4c4e6978a03c485edbfe4e0a5dc2380f8a30bba5150531b31b094492d924",
  ffprobe: "fcbe839537485eaee7a7a8bc5cbc0f90d53617e80943e8a5b2e31cb851197ea6",
};

async function download(url: string, label: string): Promise<Buffer> {
  const r = await fetch(url, { signal: AbortSignal.timeout(600_000) });
  if (!r.ok || !r.body) throw new Error(`download ${url}: HTTP ${r.status}`);
  const total = Number(r.headers.get("content-length")) || 0;
  const chunks: Buffer[] = [];
  let got = 0;
  let shown = -1;
  for await (const c of r.body as unknown as AsyncIterable<Uint8Array>) {
    chunks.push(Buffer.from(c));
    got += c.length;
    const pct = total ? Math.floor((got / total) * 100) : -1;
    if ((pct >= 0 && (pct >= shown + 10 || (pct === 100 && shown < 100))) || (pct < 0 && got - shown > 5e6)) {
      shown = pct < 0 ? got : pct;
      console.log(total ? `${label} ${pct}% of ${Math.round(total / 1e6)} MB` : `${label} ${Math.round(got / 1e6)} MB`);
    }
  }
  return Buffer.concat(chunks);
}

async function installFfmpeg() {
  if (process.platform !== "darwin" || process.arch !== "arm64") throw new Error("the ffmpeg download is for Apple Silicon Macs; install ffmpeg with your package manager");
  const bin = join(home, "bin");
  const tmp = join(home, "bin", ".ffmpeg.tmp");
  rmSync(tmp, { recursive: true, force: true });
  mkdirSync(tmp, { recursive: true });
  try {
    for (const [name, sha] of Object.entries(FFMPEG_ZIPS)) {
      const zip = await download(`${FFMPEG_BASE}/${name}.zip`, `downloading ${name} ${FFMPEG_VERSION}`);
      const got = createHash("sha256").update(zip).digest("hex");
      if (got !== sha) throw new Error(`${name}.zip: sha256 mismatch (got ${got}, want ${sha})`);
      console.log(`${name}: sha256 ok`);
      const z = join(tmp, `${name}.zip`);
      writeFileSync(z, zip);
      execFileSync("/usr/bin/unzip", ["-o", "-q", z, name, "-d", tmp]);
      rmSync(z);
      chmodSync(join(tmp, name), 0o755);
      const v = execFileSync(join(tmp, name), ["-hide_banner", "-version"], { encoding: "utf8", timeout: 30_000 }).split("\n")[0];
      console.log(v);
    }
    for (const name of Object.keys(FFMPEG_ZIPS)) renameSync(join(tmp, name), join(bin, name));
    console.log(`installed ffmpeg + ffprobe ${FFMPEG_VERSION} into ${bin}`);
  } finally {
    rmSync(tmp, { recursive: true, force: true });
  }
}

/** The Codex CLI into a user prefix ($FALOS_HOME/npm), with the bundled npm: no sudo, no global npm. */
async function installCodex() {
  const prefix = join(home, "npm");
  mkdirSync(prefix, { recursive: true });
  const [npm, pre] = npmCli();
  console.log(`npm install -g --prefix ${prefix} @openai/codex`);
  await run(npm, [...pre, "install", "-g", "--prefix", prefix, "--no-audit", "--no-fund", "--loglevel=error", "@openai/codex"], prefix);
  const codex = join(prefix, "bin", "codex");
  if (!existsSync(codex)) throw new Error(`npm finished but ${codex} is missing`);
  console.log(execFileSync(codex, ["--version"], { encoding: "utf8", timeout: 30_000 }).trim());
}

const cmd = process.argv[2];
if (cmd === "doctor") console.log(JSON.stringify(await doctor()));
else if (cmd === "check-key") console.log(JSON.stringify(await checkKey()));
else if (cmd === "install") await install();
else if (cmd === "ffmpeg" || cmd === "codex") {
  try {
    await (cmd === "ffmpeg" ? installFfmpeg() : installCodex());
  } catch (e: any) {
    console.error(`error: ${String(e?.message ?? e).slice(0, 500)}`);
    process.exitCode = 1;
  }
} else {
  console.error("usage: setup <doctor|check-key|install [skills|remotion|chrome|hyperframes…]|ffmpeg|codex>");
  process.exitCode = 2;
}
