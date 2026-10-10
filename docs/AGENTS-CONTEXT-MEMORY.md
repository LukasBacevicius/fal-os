# Agents, context, feedback & memory

> fal-os is **Figma on acid**: a creative platform where nearly all the work happens by prompting agents. That puts **feedback and context** at the center of the product. The canvas is where you give feedback, and it is also how the agents see the work.

## The loop
```
   ┌──────────── context ────────────┐
   │ selection · annotations · brief │
   ▼ lineage · memory · renders      │
 AGENT ──► edits / generations ──► CANVAS ──► human feedback
   ▲                                         (annotate · rate · pick · reject)
   └────────── memory (learned taste, decisions, recipes) ◄──┘
```
Every part of the product serves one of these four: **context in**, **work out**, **feedback back**, **memory over time**.

## 1. Harness: build on the Claude Agent SDK and Codex SDK, don't write our own
Agent harnesses are hard: tool loops, compaction, subagents, permissions, skills, hooks, session resume. Claude Code and Codex have already solved them. We host them.

**Agent Runtime** is a TypeScript sidecar process, launched and supervised by the Rust host. It talks to the host over the canvas MCP and a local socket.

| Driver | SDK | Auth / billing | Role |
|---|---|---|---|
| `claude` | Claude Agent SDK (`@anthropic-ai/claude-agent-sdk`) | API key, or Bedrock/Vertex billed through fal. A claude.ai login needs Anthropic's approval. | Default in-app agent. Gets the canvas MCP in-process, plus skills, subagents and hooks. |
| `codex` | Codex SDK / `codex app-server` | "Sign in with ChatGPT", which uses the user's own plan | Second in-app agent; strong at code and computer use |
| `fal` | fal Agent SDK (`@fal-ai/client` agent) | fal key (AGENT scope) | Media specialist: plans and runs multi-model generation pipelines |
| external | the user's own Claude Code, Codex, Cursor or Gemini over localhost MCP | the user's own | Bring your own agent |

All drivers emit one normalized event stream: `message`, `thinking`, `tool`, `plan`, `edit`, `job`, `question`, `done`. The UI and presence render that stream the same way for every driver, Brilliant-style, with one event mapper per backend.

**Agent definitions** are composable (Claude Agent SDK `agents` / Codex profiles). Each is a role plus a system prompt, tool scope, model, budget and review mode:
- "Art Director": decomposes the brief, critiques, assigns work.
- "Generator": runs fal models.
- "Editor": assembles timelines, cuts, grades.
- "Copywriter".
- "QA": checks consistency, brand and specs.

A swarm is several roles working on one canvas, coordinated through the canvas: claims, annotations and a shared task board.

**Tool surface** is the same canvas MCP for internal and external agents, so there is a single place to improve. See [ARCHITECTURE.md](ARCHITECTURE.md#how-agents-connect-v1).

## 2. Context engine (the core IP)
Agents produce good work only when they get the right context. The **Context Engine** lives in the Rust host. It compiles canvas state into **context packs** suited to each model's limits and modalities.

**Sources**
- **Focus:** the current selection, the annotation being answered, and the viewport.
- **Feedback:** the annotation thread, anchored media (crop, exact frame, audio clip, 3D view), and earlier attempts on the same anchor together with why they were rejected.
- **Lineage:** the parent assets, the prompt and model chain behind each asset, and seeds, so an agent can make "the same but…" changes reproducibly.
- **Project brief and style:** the brief doc, style references, characters, palettes, LUTs, fonts, and brand rules (as `.ds`-like token docs whose comments are prompts).
- **Memory:** relevant entries from the project, user and org memory repos (§4).
- **Activity:** what other agents are doing now and their claims, so agents avoid duplicating work.

**Rendering of each modality into a pack:** every node type has a `describe()` (text) and a `render()` (media the model can read).

| Modality | Text | Visual / audio sample |
|---|---|---|
| Image | caption, dimensions, lineage | downscaled render, with the annotated region cropped and outlined |
| Video | duration, cuts, transcript | contact sheet, exact annotated frames, short clip if the model accepts video |
| Audio | transcript, BPM/key, loudness | clip of the annotated range, spectrogram image |
| 3D | stats, materials | turntable sheet (4–8 views), view from the annotation camera |
| Text / doc | content (ranged) | — |
| Canvas region | the "blurry" scene list (ids, types, bounds, labels) | `render_viewport` screenshot |

**Budgeting:** packs are tiered as blurry, focused and full. Agents pull more through `read` and `get_annotation`, so prompts stay small and agents get detail on demand.

## 3. Feedback, first-class
Feedback is data with an anchor, not chat text.

- **Annotations** on every modality, with exact anchors (see ARCHITECTURE).
- **Quick reactions:** 👍 / 👎 / ⭐ / "more like this" / "never this" on any node.
- **Picks:** choosing 1 of N variants is strong preference data, so record which ones lost.
- **Compare mode:** A/B side by side, where the choice is recorded.
- **Assign and route:** `@agent` or drag an annotation onto an agent's avatar.
- **Outcome linking:** each attempt links back to its feedback, giving a "try history" per anchor.
- Every feedback event is written to a **feedback log** (project-local JSONL plus the doc). That log is what the memory "dreaming" process reads (§4).

## 4. Memory: Agent Memory Repo standard
We adopt [Agent Memory Repo](https://cognition.com/agent-memory-repo) ([spec](https://github.com/AgentMemoryRepo/agentmemoryrepo)) as-is.
- It is a git-versioned folder of plain files.
- `MEMORY.md` is the entry point, loaded every session, and ends with an `## Index`.
- Entries are one-line bullets with `[source: …; added: YYYY-MM-DD]` metadata and `[[path]]` links.
- `swarms/<task>/` folders coordinate multi-agent work.

**Scopes, each in its own repo:**
- `project/.fal-os/memory/`: decisions, brief evolution, recipes that worked (model + params + prompt patterns), characters, things to avoid.
- `~/.fal-os/memory/` (user): taste ("prefers warm grade, 35mm, no lens flares"), workflow habits, favorite models.
- `org/` (optional, shared remote): brand rules, approved models, house style.

**How agents use it:**
- In-app drivers load `MEMORY.md` of every scope into the context pack, and grep or follow links on demand. Our harness exposes the memory folders to the Claude/Codex SDK as plain files, which they are good at.
- External agents get the AMR skill plus a `memory` MCP resource and tools.
- Writes are attributed commits, e.g. `memory: … [source: falos://project/p/canvas?annotation=…]`, so every fact links back to the canvas moment it came from.
- **Dreaming:** a periodic background agent reads the feedback log, picks and rejections, and recent sessions. It distills taste and recipes, merges duplicates, drops stale entries, and resolves contradictions using sources. It runs on a cheap model and is budgeted.
- **Swarms:** a multi-agent task gets `swarms/<task>/` (README, findings, questions, per-agent notes), mirrored on the canvas as a task board.
- **Memory is visible:** a Memory panel shows each entry with a link to its source, and you can edit or delete it. Taste is never a hidden black box.

## 5. What this means for the build order
1. Engine + doc + host (spike → core).
2. Canvas MCP + context engine v0 (blurry/focused packs, `render_viewport`, `get_annotation` with crops and frames).
3. Annotations + feedback log.
4. Agent Runtime sidecar with the `claude` driver first, then `codex` and `fal`.
5. Memory repos + dreaming.
6. Swarm roles + task board.
