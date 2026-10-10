# fal-os agent runtime

The swarm behind the chat panel. You type "OPUS please direct me an ad for the headphones". An Opus 5.5 **Director** plans the piece, then runs subagents. They work live on the canvas, each with its own fal-os session, cursor, status line and attribution:

| Agent | Model | Does |
|---|---|---|
| Director | `claude-opus-5-5` | Reads the canvas and the attached references (it looks at images and video frames itself). Lays out the Brief / Storyboard / Assets / Motion / Final frames, writes the brief, plans, delegates, and keeps you posted in chat. |
| storyboard | `claude-opus-5-5` | Writes the script and the shot list as notes in the Storyboard frame. |
| generator (one or more, in parallel) | `claude-opus-5-5` | Makes stills, video clips and music with **fal** into the Assets frame. Placeholders appear at once and turn into the media. |
| motion | `claude-opus-5-5` (+ **Codex**) | Builds the edit in **Remotion** (motion graphics as code) from the canvas assets, renders a 1080p MP4 and imports it into the Final frame. |
| qa | `claude-opus-5-5` | Runs ffprobe on the render, looks at extracted frames, pins annotations, and sends motion back once if needed. |
| Codex | Codex CLI (your ChatGPT sign-in) | A coding worker the motion agent uses to write and iterate the composition. It has its own fal-os session. |

```
host ──ws /runtime──► runtime (this) ── Claude Agent SDK query per chat turn
                                        ├─ MCP falos  → http://127.0.0.1:<port>/mcp (canvas: session/read/edit/set_status/import_file/get_context/…)
                                        └─ MCP swarm  (in-process): fal_agent · fal_model · fal_model_schema · media_url · codex · chat_list/read/send/title
```

## Run
```sh
pnpm --dir apps/agent-runtime install
pnpm --dir apps/agent-runtime/motion-template install   # Remotion; the first render downloads chrome-headless-shell
cargo run -p falos-host -- serve --project ~/fal-os-projects/demo   # spawns + supervises this runtime
```
- The host auto-detects `apps/agent-runtime`. You can also pass `--runtime <dir>` or set `FALOS_RUNTIME`. Use `--no-runtime` to disable it.
- When the host spawns the runtime it passes `FALOS_HOST_URL`, `FALOS_TOKEN` (canvas MCP bearer), `FALOS_RUNTIME_TOKEN` (for the `/runtime` socket) and `FALOS_PROJECT_DIR`.
- To run it by hand, set `FALOS_RUNTIME_TOKEN` to the same value for both the host and `pnpm start`.

## Requirements and auth
- **Claude**: the SDK spawns your locally installed `claude` CLI and uses its login. Override the path with `FALOS_CLAUDE_PATH`. No API key is needed for local use.
- **fal**: `FAL_KEY` with the **AGENT** permission preset. It is read from the environment, else from `$FALOS_HOME/fal.env`, else from `~/.fal-os/fal.env` (a single `FAL_KEY=…` line, mode 0600). The key stays in the runtime and is never passed to agents or clients.
  - If the fal Agent API rejects the key (for example, the key lacks the AGENT preset), the runtime says so in chat once. Generators then fall back to direct fal model calls (`fal_model`, the same key on the queue API). These are still real generations.
- **Codex** (optional): `@openai/codex-sdk` bundles the Codex CLI. It uses `codex login` (ChatGPT sign-in, `~/.codex/auth.json`) or `OPENAI_API_KEY`. If neither is present, chat shows "run `npx codex login` to enable Codex" and Claude does the code work.
- `ffmpeg`/`ffprobe` on PATH: used for frames, crops, QA, and the host's media probing.

## Config (env)
| Var | Default | |
|---|---|---|
| `FALOS_DIRECTOR_MODEL` | `claude-opus-5-5` | Director model |
| `FALOS_WORKER_MODEL` | `claude-opus-5-5` | Subagent model |
| `FALOS_MAX_CONSECUTIVE_FAILURES` | `3` | Circuit breaker: generation tools refuse after this many failed fal tasks in a row |
| `FALOS_MAX_FAL_TASKS` | `100` | Runaway guard: swarm fal tasks (`fal_agent` / `fal_model`) per chat run; a `generate_batch` call counts as none |
| `FALOS_MAX_TURNS` | `500` | Director API round-trips per turn |

## Budgets and spend
- There is **no spend cap**. Chat shows the fal spend as it happens (`cost` events, cumulative per turn):
  - fal Agent tasks report `usage.cost` (settled, else estimated).
  - Direct model calls use a list-price estimate.
- The circuit breaker and the runaway guard stop loops, not spending. Stopping a chat (`chat.stop`) interrupts the Claude query and cancels running fal Agent responses (`responses.cancel`).

## User media
Anything on the canvas can be a reference or a direct model input: generated media and your own uploads (the UI `PUT`s them to `/assets/{sha256}`).
- **Context chips** attached to a message are resolved by the host:
  - you get the row, lineage, local file and pixel crop;
  - images, regions and video frames are passed to the Director as image blocks, so it sees what you mean.
- `media_url(nodes)` and `fal_model(mediaInputs: {"image_url": <node>})` turn canvas media into public URLs:
  1. If the asset has no `uri`, the file is uploaded to fal storage once (`fal.storage.upload`).
  2. The URL is cached on the asset via the host's `set_asset_uri`.
  3. `fal_agent(references: [...])` attaches the media to the fal Agent task (`input_image` / `input_file`).

## Swarm tools (in-process MCP `swarm`)
- `fal_agent(sessionId, task, outputs[], parent?, references?)`: a fal Agent SDK task (`agent.stream`).
  - Creates one `generation` placeholder per expected output, inside `parent`.
  - Maps `fal.activities` to the caller's status line and to chat `agent` events.
  - Auto-answers questions and approvals (approve; first option) and posts the decision in chat.
  - Downloads the artifacts and lands them via `import_file(replace: placeholder)`. Lineage records the task, the fal response id, the models used and a cost share.
- `fal_model(sessionId, model, input, mediaInputs?, name, kind, parent?)`: one exact endpoint on the fal queue, with a placeholder, lineage and a cost estimate.
- `fal_model_schema(model)`: input fields of an endpoint, from fal's public OpenAPI.
- `media_url(sessionId, nodes)`: public URLs for canvas media.
- `codex(task, dir?)`: a Codex thread (`workspace-write`, no approvals) in the chat workspace. It has its own "Codex" session and status.

## Drivers, queue, errors (UX v1)
- **Drivers** per chat (`chat.send` `driver`): `opus` (this swarm), `fal` (talk to the fal Agent directly: plan, activities, its own cursor, artifacts imported with lineage, questions answered in the composer, multi-turn via `previous_response_id`), `codex` (a Codex thread with the fal-os MCP configured). See `src/drivers.ts`.
- **Queued messages**: the host sends `queue` frames while a chat runs; the runtime injects them into the SDK query at the next tool call (streaming input, `priority: "next"`) and reports `injected`.
- **Attribution**: every event has `agent` (+ `parent` for subagents); subagent text is forwarded (`forwardSubagentText`); `session.agent` frames tell the host which subagent owns a fal-os session. All MCP connections send `X-Falos-Chat`.
- **Errors**: SDK result errors are logged, shown briefly, and the run auto-resumes once in the same session; then `status: error` with `actions: ["continue"]` (`chat.continue`).
- **Batches**: agents use the host's `generate_batch` for many outputs (one call, parallel jobs).

## Multiple chats
- Chats run concurrently, one SDK query each. A follow-up to a busy chat queues behind its current turn.
- Every Director and subagent can use these swarm tools:
  - `chat_list`, `chat_read(chat, last_n)`: see what other chats are doing.
  - `chat_send(chat, text)`: hand work to another chat or report back.
  - `chat_title(title)`: Director only; names its own chat.
- The host delivers `chat_send` as a user message from `chat:<id>` and starts a run in that chat. That chat's Director reports back with `chat_send`.
- A loop guard limits each chat pair to 5 messages per minute.
- A `#chat` chip in the user's message arrives as a digest of that chat.
- See `docs/CHAT-PROTOCOL.md` → Multiple chats.

## Files
- `<project>/chats/<chat>.jsonl`: chat events, written by the host.
- `<project>/work/<chat>/`: the run's workspace.
  - `motion/`: the Remotion copy of `motion-template` with linked `node_modules`, `public/` for assets, and `out/ad.mp4`.
  - `downloads/`: fal outputs before import.
  - `context/`, `qa/`: chip images and QA frames.
  - `claude-session.json`: the SDK session, resumed on follow-ups.

## Motion template
`motion-template/` is a Remotion 4 project for 1920×1080 at 30 fps:
- `Ad` composition: hero reveal with light sweep → kinetic feature callouts with Ken Burns cards → clip montage → end card with CTA. Music fades in and out.
- Props are listed in `src/props.ts`. Scene lengths scale with `durationInSeconds` (6–60 s).
- The motion agent (or Codex) adapts the code for each piece.

To render by hand:
```sh
npx remotion render src/index.ts Ad out/ad.mp4 --props=props.json
```

## Dev scripts
- `scripts/e2e.ts`: seeds a "user" product photo (PUT `/assets` + image node), sends a chat over `/sync` and logs every event until the turn ends.
- `scripts/fal-key-check.ts`: shows whether the key can use the Agent API and the queue API.
- `scripts/fal-agent-smoke.ts`: a single fal Agent task, printing the snapshot shapes.
