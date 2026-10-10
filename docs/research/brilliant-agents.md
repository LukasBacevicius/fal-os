# Brilliant teardown — agent integration

Sources: live read-only calls to the running MCP server (tools/list, init, read, get_knowledge…), bundled `.claude-prod/` prompts/specs, Dart strings. **[I]** = inference.

## One tool server, five doors
Prompts for all doors are composed from one file (`.claude-prod/agent.md`, blocks tagged per door, golden-tested).

| Door | Who | How it writes |
|---|---|---|
| mcp | User's Claude Code, Codex, Cursor, VS Code, Zed, Windsurf, Gemini CLI, Antigravity, OpenCode, Amp | `edit` tool |
| hosted | In-app chat spawning local `claude` CLI (`--input/--output-format stream-json`, `--include-partial-messages`, `--resume`) | `<edit>` blocks in assistant text, applied while streaming |
| http | In-app chat on API keys (Anthropic, OpenAI Responses, Google, OpenRouter, Ollama…) or hosted "Brilliant AI" (Gemini Flash) | `<edit>` blocks; own harness (bash, read/write, glob, rg-backed grep, web fetch, sub-agents) |
| codex | `codex app-server` JSON-RPC (thread/start, turn/start, item/*) | same |
| subagent | sub-agents of in-app chat | `edit` tool |

Cursor is driven over **ACP** (`session/new`, `session/prompt`, `session/update`, `session/request_permission`, `fs/*`). Each backend has an event mapper into one chat UI.

## MCP server
`mcp_dart`, Streamable HTTP on `127.0.0.1:3333` (→3334→3335), protocol 2025-06-18, tools only (no resources/prompts/annotations), stateless; identity is a `sessionId` argument. Loopback Host check, **no auth token**.

### Tools (24, ~10 live, 5 retired stubs)
- `init(agentName, project, vision)` → `sessionId` (`mcp:<hex>`), paths, design-system catalog, knowledge index, gives the agent a **labelled canvas cursor**.
- `edit(text, sessionId, designSystem, requestId, response_mode)` — **the only write door**. Text holds fences, each starting with `file(path)`: ```html (HTML+inline CSS → native elements; default), ```bl (Blueprint rows, streamed, first bad row halts), ```ds, ```bm. `requestId` = 2-min idempotency. `// label` lines = undo checkpoints. `preview(#ref)` returns a screenshot in the result.
- `read(paths|ids|query|textContent|type|fillColor|componentName, format, depth, limit)` — returns exactly the fence `edit` accepts (symmetric).
- `get_knowledge(keys)` — ~100 small docs with `assumes:` dependencies, loaded on demand instead of giant tool descriptions.
- `get_selection`, `export` (png/svg/pdf/html/react/mp4), `list_projects`, `convert_html` (dry run), `execute_commands` (100+ app commands by id, through UI path, undoable, act on ids not live selection).
- Paid generative: `generate_image` (Google), `generate_svg`/`vectorize_image` (Quiver), `remove_background`, `add_cutout_outline`.
- Self-documentation: `capture_ui`, `render_ui`, `list_stagers`.

## Concurrency
- No locks. Per-session refs, design-system state, undo stack (rebased), cursor + optional live "thinking snippet".
- Writes annotated: "changed since your last read (by someone else)", "(changed by @…)".
- Per-turn Revert; separate AI undo stack from user's.
- Circuit breakers ("consecutive no-progress tool cycles… protect the user's spend"), no-op edit warnings.
- Untrusted-content taint: canvas text wrapped `[[untrusted:…]]`; after reading it, bash/write are refused for the rest of the turn.
- Project exclusivity across sessions; headless-binding notice.

## Install
Settings → MCP Connections writes a `brilliant` entry into each client's own config (format-preserving JSON/JSONC/TOML), rewritten every launch to follow the port; auto-adds to Codex if `~/.codex` exists. Web editor uses an npm stdio bridge (`claude mcp add brilliant -- npx -y @brilliant-hq/mcp`). External MCP agents unlimited on Free; built-in chat metered.

## Lessons
**Copy:** symmetric read/write in one format with an easy tier (HTML) and expert tier (DSL); knowledge on demand; explicit session id + agent name; presence; staleness reports instead of locks; per-session undo + checkpoints + revert-turn; screenshot in write result (text fallback for no-vision); idempotency; spend breakers; untrusted-content taint; config-file-is-truth installer; streaming text edits for in-house agent; refusals with alternatives.

**Avoid:** novel-length tool prose and huge init payload; stub tools; no annotations; **no async jobs/progress** (fal generations need this); unauthenticated local endpoint; optional sessionId silently falling back to shared; two write protocols; re-implementing a coding-agent harness; **no computer use at all**.
