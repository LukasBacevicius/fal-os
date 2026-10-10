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
      return clean(input.description ?? input.command).slice(0, 100);
    case "Read":
    case "Write":
    case "Edit":
      return `${name.toLowerCase()} ${clean(input.file_path).split("/").slice(-2).join("/")}`;
    default:
      return name.replace(/_/g, " ");
  }
}

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
      return `ran ${clean(input.description ?? input.command).slice(0, 90)}`;
    case "Read":
      return `read ${clean(input.file_path).split("/").pop()}`;
    case "Write":
      return `wrote ${clean(input.file_path).split("/").pop()}`;
    case "Edit":
      return `edited ${clean(input.file_path).split("/").pop()}`;
    default:
      return name.replace(/_/g, " ");
  }
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
  if (/ede_diagnostic|error_during_execution|returned an error result/i.test(msg)) return "the director stopped mid-step";
  return msg.slice(0, 300);
}
