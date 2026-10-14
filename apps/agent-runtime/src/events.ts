// Shared helpers for chat events: friendly past-tense tool summaries, SDK error mapping, and
// a small async channel used for streaming input into the Claude Agent SDK.

/** Push-based async iterable (closed once; items pushed after close are dropped). */
export class Channel<T> implements AsyncIterable<T> {
  private items: T[] = [];
  private wake: (() => void) | null = null;
  private closed = false;

  push(item: T) {
    if (this.closed) return false;
    this.items.push(item);
    this.wake?.();
    return true;
  }

  close() {
    this.closed = true;
    this.wake?.();
  }

  get isClosed() {
    return this.closed;
  }

  async *[Symbol.asyncIterator](): AsyncIterator<T> {
    for (;;) {
      while (this.items.length) yield this.items.shift()!;
      if (this.closed) return;
      await new Promise<void>((r) => (this.wake = r));
      this.wake = null;
    }
  }
}

const clean = (v: unknown) => String(v ?? "").replace(/\s+/g, " ").trim();
const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;
/** `fal-ai/flux/schnell` → `flux/schnell`. */
export const shortModel = (m: unknown) => clean(m).replace(/^fal-ai\//, "");

const IRREGULAR: Record<string, [string, string]> = {
  // imperative → [past, present participle]
  run: ["ran", "running"], make: ["made", "making"], write: ["wrote", "writing"], build: ["built", "building"],
  get: ["got", "getting"], find: ["found", "finding"], read: ["read", "reading"], set: ["set", "setting"],
  put: ["put", "putting"], cut: ["cut", "cutting"], send: ["sent", "sending"], take: ["took", "taking"],
  draw: ["drew", "drawing"], do: ["did", "doing"], see: ["saw", "seeing"], split: ["split", "splitting"],
  begin: ["began", "beginning"], encode: ["encoded", "encoding"], copy: ["copied", "copying"],
};
const VERBS = new Set([
  ...Object.keys(IRREGULAR),
  "render", "extract", "check", "list", "install", "create", "generate", "import", "export", "convert", "download", "upload",
  "probe", "measure", "normalize", "master", "mix", "trim", "crop", "scale", "resize", "compile", "verify", "test", "preview",
  "inspect", "look", "fix", "update", "remove", "delete", "move", "rename", "start", "stop", "kill", "wait", "count", "analyze",
  "detect", "compose", "assemble", "concatenate", "add", "apply", "compare", "sample", "grab", "open", "show", "print", "clean",
  "prepare", "transcode", "typecheck", "lint", "fetch", "search", "load", "save", "edit", "replace", "pull", "push", "link",
]);
const ALREADY = /^(?:[a-z]+ed|ran|wrote|made|built|sent|read|found|got|did|took|saw|drew|began|set|put|cut|split)\b/i;
const ING = /^[a-z]+ing\b/i;

function inflect(verb: string, form: 0 | 1): string {
  const v = verb.toLowerCase();
  if (IRREGULAR[v]) return IRREGULAR[v][form];
  if (form === 0) return v.endsWith("e") ? `${v}d` : /[^aeiou]y$/.test(v) ? `${v.slice(0, -1)}ied` : /^(stop|grab|trim|drop|plan|ship|crop)$/.test(v) ? `${v}${v.at(-1)}ed` : `${v}ed`;
  return v.endsWith("e") && !v.endsWith("ee") ? `${v.slice(0, -1)}ing` : /^(stop|grab|trim|drop|plan|ship|crop)$/.test(v) ? `${v}${v.at(-1)}ing` : `${v}ing`;
}

/**
 * A command's description as an activity phrase with exactly one leading verb:
 * "Build a contact sheet" → "built a contact sheet" (done) / "building a contact sheet"
 * (running); text without a verb gets "ran" / "running". Never "ran ran …".
 */
export function actionPhrase(desc: string, done: boolean): string {
  let d = clean(desc).replace(/[.。]+$/, "");
  if (!d) return done ? "ran a command" : "running a command";
  // Drop leading "Ran"/"Running" the model already wrote.
  d = d.replace(/^(?:ran|run|running)\s+(?=\S)/i, (m) => (VERBS.has(d.split(/\s+/)[1]?.toLowerCase() ?? "") ? "" : m));
  const [first, ...rest] = d.split(/\s+/);
  const lowerRest = rest.join(" ");
  const f = first.toLowerCase();
  if (done ? ALREADY.test(f) : ING.test(f)) return `${f} ${lowerRest}`.trim();
  if (VERBS.has(f)) return `${inflect(f, done ? 0 : 1)} ${lowerRest}`.trim();
  if (ALREADY.test(f) || ING.test(f)) return `${f} ${lowerRest}`.trim();
  // A capitalized plain word starting a short sentence ("Survey the skill …") is an imperative.
  if (/^[A-Z][a-z]{2,}$/.test(first) && !/^(remotion|hyperframes|ffmpeg|ffprobe|node|npx|pnpm|npm|python|codex|claude|chrome|final|music|audio|video|the|this|all|new)$/.test(f) && rest.length && /^[a-z0-9"'`(]/.test(rest[0])) return `${inflect(f, done ? 0 : 1)} ${lowerRest}`.trim();
  return `${done ? "ran" : "running"} ${d}`;
}

/** Human role of a subagent type, never just its label again. */
export const ROLES: Record<string, string> = {
  storyboard: "script & shots",
  generator: "fal media",
  motion: "edit & render",
  qa: "render check",
  editor: "cuts & reframes",
  finisher: "export & delivery",
  codex: "code",
  coder: "code & apps",
};

/** A status line without the agent's own name in front ("motion: Build film" → "Build film"). */
export function stripAgent(status: string, agent: string, role?: string): string {
  let s = clean(status);
  for (const w of [agent, role, agent.replace(/-\d+$/, "")].filter(Boolean) as string[]) {
    const re = new RegExp(`^${w.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\s*[:·\\-–—]\\s*`, "i");
    s = s.replace(re, "");
  }
  return s;
}

/** Present-tense line while a tool runs. */
export function runningSummary(name: string, input: any): string {
  switch (name) {
    case "edit":
      return "editing the canvas";
    case "read":
      return `looking at ${input.root ?? "the canvas"}`;
    case "get_context":
      return `looking at ${plural((input.chips ?? []).length, "node")}`;
    case "generate":
      return `generating (${shortModel(input.model)})`;
    case "generate_batch":
      return `generating ${input.count ?? (input.jobs ?? []).length ?? ""} (${shortModel(input.model ?? input.jobs?.[0]?.model)})`.replace("  ", " ");
    case "await_batch":
      return "waiting for the batch";
    case "import_file":
      return `importing ${clean(input.name ?? String(input.path ?? "").split("/").pop())}`;
    case "set_status":
      return clean(input.text).slice(0, 100);
    case "session":
      return `joining as ${input.agentName}`;
    case "Bash":
      return actionPhrase(String(input.description ?? input.command ?? ""), false).slice(0, 100);
    case "Skill":
      return `loading the ${clean(input.skill ?? input.name ?? "")} skill`.replace("the  skill", "a skill");
    case "Read":
    case "Write":
    case "Edit":
      return `${name.toLowerCase()} ${clean(input.file_path).split("/").slice(-2).join("/")}`;
    case "transform":
      return `${opPhrase(input.op, false)}${inputsPhrase(input.inputs)}`;
    case "inspect":
      return `inspecting ${[input.what ?? []].flat().join(", ") || "media"}`;
    case "compose":
      return `composing a ${clean(input.op).replace(/_/g, " ") || "layout"}`;
    case "rerun":
      return `re-running ${input.node ?? "a recipe"}`;
    case "export":
    case "deliver":
      return `exporting ${[input.presets ?? []].flat().join(", ") || "files"}`;
    case "package":
      return `packaging the ${clean(input.kind) || "project"}`;
    case "share_files":
      return `sharing ${plural((input.files ?? input.paths ?? []).length, "file")}`;
    case "search_assets":
      return `searching the library${input.query?.text ? ` for "${clean(input.query.text).slice(0, 40)}"` : ""}`;
    case "place_asset":
      return "placing a library asset";
    case "create_app":
      return `building ${clean(input.name) || "an app"}`;
    case "write_app_files":
      return `writing ${plural((input.files ?? []).length, "file")} of ${clean(input.app)}`;
    case "read_app":
      return `reading ${clean(input.app)}`;
    case "app_logs":
      return `checking ${clean(input.app)}`;
    default:
      return name.replace(/_/g, " ");
  }
}

/** `reframe` → "reframing" / "reframed"; the ffmpeg escape hatch reads as "ran ffmpeg". */
const OPS: Record<string, [string, string]> = {
  ffmpeg: ["ran ffmpeg", "running ffmpeg"],
  concat: ["concatenated clips", "concatenating clips"],
  speed: ["retimed", "retiming"],
  gif: ["made a gif", "making a gif"],
  burn_captions: ["burned in captions", "burning in captions"],
  audio_mix: ["mixed audio", "mixing audio"],
  audio_normalize: ["normalized loudness", "normalizing loudness"],
  audio_extract: ["extracted audio", "extracting audio"],
  extract_frames: ["extracted frames", "extracting frames"],
  remove_bg: ["removed the background", "removing the background"],
};

function opPhrase(op: unknown, done: boolean): string {
  const o = clean(op).toLowerCase() || "transform";
  if (OPS[o]) return OPS[o][done ? 0 : 1];
  return /^[a-z]+$/.test(o) ? inflect(o, done ? 0 : 1) : `${done ? "ran" : "running"} ${o.replace(/_/g, " ")}`;
}

const inputsPhrase = (inputs: unknown) => {
  const n = Array.isArray(inputs) ? inputs.length : inputs ? 1 : 0;
  return n > 1 ? ` (${n} inputs)` : "";
};

/** Past-tense, human line for a finished tool (`generated 4 images (flux/schnell)`). */
export function doneSummary(name: string, input: any, json: any, nodes: string[] | undefined): string {
  const n = nodes?.length ?? 0;
  switch (name) {
    case "edit": {
      const created = Array.isArray(json?.created) ? json.created.length : 0;
      const touched = Array.isArray(json?.touched) ? json.touched.length : 0;
      const notes = String(input.text ?? "")
        .split("\n")
        .filter((l) => /^\s*note\b/.test(l)).length;
      const deleted = /\bdelete\(/.test(String(input.text ?? ""));
      if (notes && created) return `wrote ${plural(notes, "note")}`;
      if (created) return `created ${plural(created, "node")}`;
      if (deleted) return `deleted ${plural(Math.max(touched, 1), "node")}`;
      return `edited ${plural(touched, "node")}`;
    }
    case "read":
      return input.root ? `looked at ${input.root}` : "looked at the canvas";
    case "get_context":
      return `looked at ${plural((input.chips ?? []).length, "node")}`;
    case "get_annotation":
      return "read a comment";
    case "annotate":
      return "left a comment";
    case "generate":
      return `started generating (${shortModel(input.model)})`;
    case "generate_batch":
      return `generating ${plural(json?.total ?? n, "output")} (${shortModel(input.model ?? input.jobs?.[0]?.model)})`;
    case "await_batch":
      return `batch finished: ${json?.done ?? 0} done${json?.failed ? `, ${json.failed} failed` : ""}`;
    case "import_file":
      return `imported ${clean(input.name ?? String(input.path ?? "").split("/").pop())}`;
    case "claim":
      return `claimed ${plural(json?.granted?.length ?? 0, "node")}`;
    case "finish_working":
      return "finished with its nodes";
    case "get_selection":
      return "looked at your selection";
    case "Bash":
      return actionPhrase(String(input.description ?? input.command ?? ""), true).slice(0, 100);
    case "Skill":
      return `loaded the ${clean(input.skill ?? input.name ?? "")} skill`.replace("the  skill", "a skill");
    case "Read":
      return `read ${clean(input.file_path).split("/").pop()}`;
    case "Write":
      return `wrote ${clean(input.file_path).split("/").pop()}`;
    case "Edit":
      return `edited ${clean(input.file_path).split("/").pop()}`;
    case "transform":
      return `${opPhrase(input.op, true)}${inputsPhrase(input.inputs)}${n ? ` → ${plural(n, "node")}` : ""}`;
    case "inspect":
      return `inspected ${[input.what ?? []].flat().join(", ") || "media"}`;
    case "compose":
      return `composed a ${clean(input.op).replace(/_/g, " ") || "layout"}`;
    case "rerun":
      return `re-ran ${input.node ?? "a recipe"}${n ? ` → ${plural(n, "node")}` : ""}`;
    case "export":
    case "deliver": {
      const files = filesOf(json).files;
      const presets = [...new Set(files.map((f) => f.preset).filter(Boolean))];
      return `exported ${plural(files.length, "file")}${presets.length ? ` (${presets.join(", ")})` : ""}`;
    }
    case "package":
      return `packaged the ${clean(input.kind) || "project"}`;
    case "share_files":
      return `shared ${plural(filesOf(json).files.length, "file")}`;
    case "search_assets": {
      const items = Array.isArray(json?.items) ? json.items.length : Array.isArray(json) ? json.length : undefined;
      return `searched the library${items !== undefined ? ` (${plural(items, "hit")})` : ""}`;
    }
    case "place_asset":
      return "placed a library asset";
    case "create_app":
    case "write_app_files": {
      const errs = Array.isArray(json?.check?.errors) ? json.check.errors.length : null;
      const tail = errs === null ? "" : errs ? ` · ${plural(errs, "error")}` : " · runs clean";
      return `${name === "create_app" ? "built" : "updated"} ${clean(json?.app ?? input.app ?? input.name)} v${json?.version ?? "?"}${tail}`;
    }
    case "read_app":
      return `read ${clean(json?.app ?? input.app)}`;
    case "app_logs":
      return `checked ${clean(json?.app ?? input.app)}${typeof json?.errors === "number" ? (json.errors ? ` · ${plural(json.errors, "error")}` : " · runs clean") : ""}`;
    case "list_ops":
      return "listed the asset ops";
    default:
      return name.replace(/_/g, " ");
  }
}

/** One file card (`files` chat event, docs/ASSETS-V1.md §7). */
export type FileCard = { path: string; name: string; node?: string; preset?: string; w?: number; h?: number; duration?: number; bytes: number; sha256: string };

/** Files in an `export` / `package` result (`{files:[…], dir}`, or one `{path|zip|file}`); [] if none. */
export function filesOf(json: any): { files: FileCard[]; dir?: string } {
  if (!json || typeof json !== "object") return { files: [] };
  const raw: any[] = Array.isArray(json.files) ? json.files : [json.zip ?? json.path ?? json.file].filter((x) => typeof x === "string").map((path) => ({ path, bytes: json.bytes, sha256: json.sha256 }));
  const files = raw
    .map((f) => (typeof f === "string" ? { path: f } : f))
    .filter((f) => typeof f?.path === "string")
    .map((f) => ({
      path: f.path,
      name: f.name ?? String(f.path).split("/").pop(),
      ...(f.node ? { node: f.node } : {}),
      ...(f.preset ? { preset: f.preset } : {}),
      ...(Number.isFinite(f.w) ? { w: f.w } : {}),
      ...(Number.isFinite(f.h) ? { h: f.h } : {}),
      ...(Number.isFinite(f.duration) ? { duration: f.duration } : {}),
      bytes: Number(f.bytes ?? 0),
      sha256: String(f.sha256 ?? ""),
    }));
  const dir = typeof json.dir === "string" ? json.dir : files.length ? commonDir(files.map((f) => f.path)) : undefined;
  return { files, dir };
}

/** Longest common directory of absolute paths. */
export function commonDir(paths: string[]): string {
  const parts = paths.map((p) => p.split("/").slice(0, -1));
  const first = parts[0] ?? [];
  let n = first.length;
  for (const p of parts) {
    let i = 0;
    while (i < n && p[i] === first[i]) i++;
    n = i;
  }
  return first.slice(0, n).join("/") || "/";
}

/** The `files` chat event: file cards (drag out, Reveal, Open in…, Copy). */
export function filesEvent(files: FileCard[], dir: string | undefined, agent?: string) {
  return { kind: "files", ...(agent ? { agent } : {}), files, dir: dir ?? commonDir(files.map((f) => f.path)) };
}

/** `generated 4 images (flux/schnell)` for fal results. */
export function generatedSummary(kinds: string[], model: string): string {
  const counts = new Map<string, number>();
  for (const k of kinds) counts.set(k, (counts.get(k) ?? 0) + 1);
  const parts = [...counts].map(([k, c]) => plural(c, k));
  return `generated ${parts.join(" + ") || "nothing"} (${shortModel(model)})`;
}

/** Short user-facing message for an SDK result error; the raw text only goes to the log. */
export function friendlyError(subtype: string, maxTurns: number): string {
  switch (subtype) {
    case "error_max_turns":
      return `stopped after ${maxTurns} steps (the turn limit, FALOS_MAX_TURNS)`;
    case "error_during_execution":
      return "the director stopped mid-step";
    case "error_max_budget_usd":
      return "hit the budget limit";
    case "error_max_structured_output_retries":
      return "couldn't produce a valid answer";
    default:
      return "the director stopped unexpectedly";
  }
}

/** "Generating images" → "Generated images" (fal Agent activity labels). */
export function pastTense(label: string): string {
  const irregular: Record<string, string> = { running: "ran", making: "made", writing: "wrote", finding: "found", thinking: "thought", getting: "got", building: "built", choosing: "chose" };
  return label.replace(/^\w+ing\b/i, (w) => {
    const lower = w.toLowerCase();
    const past = irregular[lower] ?? lower.replace(/ing$/, "ed");
    return w[0] === w[0].toUpperCase() ? past[0].toUpperCase() + past.slice(1) : past;
  });
}

/** Never show SDK diagnostics (`[ede_diagnostic] …`) to users. */
export function scrubError(msg: string): string {
  // Sign-in problems are the user's to fix: say so instead of "stopped mid-step".
  if (/failed to authenticate|oauth (session|token)|not logged in|please run \/login|invalid api key|authentication_error/i.test(msg))
    return "Claude isn't signed in (the login expired): run `claude` in a terminal and /login, then continue";
  if (/ede_diagnostic|error_during_execution|returned an error result/i.test(msg)) return "the director stopped mid-step";
  return msg.slice(0, 300);
}
