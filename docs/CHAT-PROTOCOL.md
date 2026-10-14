# Chat + agent swarm protocol (v0)

The product surface is **chat + canvas**, as in Cursor. You talk to a director agent (Opus 5.5). It runs a swarm of subagents that work *on the canvas* where you can see them: each subagent has its own session, cursor, status line and attribution. Context comes from what you select:
- whole assets
- a spot or region on an image
- a frame (plus optional region) of a video
- a time range of audio

```
apps/web (chat panel) ──ws /sync──► falos-host ──ws /runtime──► agent runtime (Node, Claude Agent SDK)
                         ◄── chat events ──┘         ◄── events ──┘   │
                                                                     ├─ MCP: fal-os canvas (http://127.0.0.1:<port>/mcp, bearer)
                                                                     └─ MCP: swarm (in-process): fal Agent SDK (FAL_KEY, AGENT preset), direct fal models,
                                                                        fal storage uploads, Codex SDK worker
```
fal access lives in the runtime: `FAL_KEY` comes from the environment or `~/.fal-os/fal.env` and never reaches agents or clients. See [`apps/agent-runtime/README.md`](../apps/agent-runtime/README.md).

## Context chips
```jsonc
{ "kind": "node", "node": "a1b2c3" }                                   // whole asset
{ "kind": "spot", "node": "a1b2c3", "x": 0.42, "y": 0.31 }             // normalized 0..1 in the node
{ "kind": "region", "node": "a1b2c3", "box": [0.1, 0.2, 0.5, 0.4] }    // x, y, w, h normalized
{ "kind": "frame", "node": "v9", "t": 3.24, "box": [0.1,0.2,0.3,0.3] } // video frame (box optional)
{ "kind": "range", "node": "au1", "t0": 2.0, "t1": 5.5 }               // audio/video time range
```
The host turns chips into context the agent can read. Each chip gets its node's text projection row, its lineage, and a local file path for its media. The runtime passes images and frames to Claude as image blocks: the region is cropped, and the frame is extracted with ffmpeg.

A resolved chip (in `run.context` and in `get_context` results) looks like this:
```jsonc
{ "chip": {…}, "node": "a1b2c3", "type": "image", "name": "…", "row": "<untrusted-canvas-data>…", "lineage": ["asset sha256:… ← model by @actor params {…}"],
  "asset": "sha256:…", "mime": "image/png", "uri": "https://…" /* null when local-only */, "file": "/abs/project/assets/<hex>.png",
  "media": { "width": 1024, "height": 768, "duration": null },
  "crop": { "x": 102, "y": 154, "w": 512, "h": 307 } /* pixels: region/spot/frame box */, "point": {…} /* spot */,
  "frame": { "t": 3.24 }, "range": { "t0": 2, "t1": 5.5 }, "hint": "ffmpeg … crop=…" }
```

**User media** (the UI uploads files via `PUT /assets/{sha256}`) is a first-class reference and model input. When fal needs a public URL for local-only media, the runtime uploads the file to fal storage once and records it on the asset with `set_asset_uri`. Fal needs this for fal Agent attachments and for model inputs such as `image_url` or `video_url`.

## Client ↔ host (JSON text frames on the existing `/sync/<canvas>` socket)
Client → host:
```jsonc
{ "type": "chat.send", "chat": "c_123" /* omit to start a new chat */, "text": "OPUS please direct me an ad for the headphones", "context": [/* chips */], "options": {/* optional, passed to the runtime */} }
{ "type": "chat.stop", "chat": "c_123" }
```
Host → client, broadcast to every client on the chat's canvas:
```jsonc
{ "type": "chat.event", "canvas": "main", "chat": "c_123", "seq": 17, "event": { /* below */ } }
{ "type": "chat.history", "chats": [{ "chat": "c_123", "title": "Headphones ad", "events": [/* events, each with its "seq" */] }] } // after hello + snapshot
```
- A new chat id has the form `c_<hex>`. The client learns it from the first `chat.event`, which is the `user` event.
- Chat ids must match `[A-Za-z0-9_-]{1,64}`. Bad frames get `{"type":"error"}`.
- `seq` is per chat and increases by 1.
- `chat.history` covers the chats of this canvas, with up to 5000 events each.

## Multiple chats
A project can have many chats. They can **run concurrently**: each turn is its own SDK query, every subagent still has its own fal-os session and cursor, and claims prevent collisions on the canvas.

Client → host:
```jsonc
{ "type": "chat.new" }                                   // → reply to the sender: {"type":"chat.created","chat":"c_…"}, then chat.list to everyone
{ "type": "chat.rename", "chat": "c_…", "title": "…" }
{ "type": "chat.archive", "chat": "c_…" }                // hidden from list/history; a running turn is stopped
```
Host → client, sent after `hello` (after `chat.history`) and again whenever anything changes:
```jsonc
{ "type": "chat.list", "canvas": "main", "chats": [{ "chat": "c_…", "canvas": "main", "title": "Headphones ad", "state": "running" | "idle" | "error",
  "updated": 1760000000000, "agents": [{ "agent": "motion", "status": "rendering 0:07/0:15" }], "cost": 0.42 }] }   // newest activity first
```
- **Coalescing.** Changes are broadcast at most every 250 ms per canvas.
- **`agents`** lists the agents working right now: their latest `agent` event is not done or failed.
- **`cost`** is the chat's cumulative fal spend across turns.
- **Titles.** A new chat starts as "New chat".
  - The first message sets the title to its first words.
  - The Director usually sets a short title early, through the swarm tool `chat_title`, which emits `{"kind":"title","text":"…"}`.
  - `chat.rename` fixes the title.
- **Persistence.** Everything is kept in `<project>/chats/index.json` (`chat, canvas, title, created, updated, archived, cost, state`) plus `<chat>.jsonl`.

**Chats talking to each other.** Every Director and subagent has the swarm tools `chat_list()`, `chat_read(chat, last_n)` and `chat_send(chat, text)`. The runtime asks the host over `/runtime`:
```jsonc
{ "type": "request", "id": 7, "op": "chat.list" }
{ "type": "request", "id": 8, "op": "chat.read", "chat": "c_b", "last_n": 30 }         // → {chat, title, digest}
{ "type": "request", "id": 9, "op": "chat.send", "from": "c_a", "chat": "c_b", "text": "…" }
// host → runtime: { "type": "reply", "id": 9, "ok": true, "result": {"delivered": true, "chat": "c_b", "seq": 12} } | { …, "ok": false, "error": "…" }
```
- **Delivery.** `chat.send` appends `{"kind":"user","author":"chat:c_a","from_title":"…","text":"…"}` to the target chat. It also sends a `run` with `"from":{"chat","title"}`. The run starts at once if the target is idle; otherwise it queues after the current turn.
- **Replies.** The target Director is told to report back with `chat_send` when it's done.
- **In the source chat**, the call shows as a normal `tool` event named `chat_send`.
- **Loop guard.** At most 5 messages per minute per (from, to) pair. A chat can't message itself.

**Chat chips.** `{ "kind": "chat", "chat": "c_…" }` references another chat; in the input it is written `#title`. It resolves to `{chip, chat, title, digest}`, where the digest covers the last ~20 events (user messages, Director text, agents, tools with node ids, results), truncated and wrapped as untrusted data.

## UX v1 additions
Implements [UX-V1.md](UX-V1.md). Everything below is additive to the frames above.

### Chat list rows
```jsonc
{ "chat": "c_…", "canvas": "main", "title": "…", "state": "running" | "idle" | "error", "updated": 1760000000000,
  "agents": [{ "agent": "generator-2", "status": "…" }], "cost": 0.42,
  "color": "#7393c2",          // one of 8 muted hues, least-used first, stable per chat (chats/index.json)
  "driver": "opus" | "fal" | "codex",
  "open": true,                 // shown as a tab (chat.close hides it, history keeps it; distinct from archive)
  "external": false, "client": null,   // true / "claude" for an external agent's pseudo-chat
  "turn": "t_…" | null,         // the running turn
  "queued": [{ "id": "q_…", "text": "…" }] }
```
`chat.history` entries also carry `color`. New chats, chats that start a run and chats that
receive a cross-chat message are opened automatically.

### Client → host
```jsonc
{ "type": "chat.send", "chat": "c_…", "text": "…", "context": [], "driver": "opus" | "fal" | "codex" }  // driver optional: switches the chat's driver
{ "type": "chat.unqueue", "chat": "c_…", "id": "q_…" }      // drop a queued message → dequeued (reason "cancelled")
{ "type": "chat.revert", "chat": "c_…", "turn": "t_…" }     // undo everything that turn's agents changed → reverted
{ "type": "chat.redo", "chat": "c_…", "turn": "t_…" }       // re-apply a reverted turn → redone
{ "type": "chat.continue", "chat": "c_…" }                  // resume a run that stopped with an error (actions: ["continue"])
{ "type": "chat.open", "chat": "c_…" } / { "type": "chat.close", "chat": "c_…" }   // tabs
{ "type": "chat.delete", "chat": "c_…" }                    // stop it, delete chats/<id>.jsonl + its index entry; the last chat can be deleted too
```
Host → client: `{"type":"chat.deleted","canvas":"main","chat":"c_…"}` (then `chat.list` without it).
Errors (unknown turn, reverting a running turn, unknown driver) come back as `{"type":"error"}`.

### New chat events
Runtime events always carry `agent` (`director` unless another agent; subagents also carry
`parent`), so the UI can split per-subagent transcripts: a subagent's `text`, `tool`, `agent`
and `thinking_done` events go to its own transcript.
```jsonc
{ "kind": "turn", "turn": "t_…", "state": "start" }                       // host: a run starts (after its user event)
{ "kind": "turn", "turn": "t_…", "state": "end", "nodes": ["…"] }          // host: the run ended (status idle/error); nodes its agents touched
{ "kind": "queued", "id": "q_…", "text": "…", "context": [], "author": "lukas" }   // chat.send while running
{ "kind": "dequeued", "id": "q_…", "reason": "injected" | "cancelled" | "stopped" | "started" | "requeued" }
{ "kind": "user", "text": "…", "author": "lukas", "queued": "q_…" }          // follows dequeued/injected: the message joined the running turn
{ "kind": "reverted", "turn": "t_…", "count": 7, "nodes": ["…"] }
{ "kind": "redone", "turn": "t_…", "count": 7, "nodes": ["…"] }
{ "kind": "thinking_done", "agent": "director", "seconds": 12 }             // "thought for 12s"
{ "kind": "batch", "agent": "generator", "id": "batch_…", "model": "fal-ai/flux/schnell", "total": 200, "done": 37,
  "failed": 0, "running": 120, "finished": false, "frame": "b…", "nodes": null /* all ids once finished */,
  "then": { "running": 3, "done": 0, "failed": 0 } /* only with per-item pipelines */ }   // throttled (≤4/s); finished = every job and every `then` chain
{ "kind": "question", "agent": "fal", "id": "…", "prompt": "…", "approval": false,
  "questions": [{ "id": "q1", "text": "Which model?", "options": [{ "id": "o1", "label": "Kling" }] }] }   // fal driver: answer in the composer
{ "kind": "status", "state": "running", "error": "the director stopped mid-step — continuing…", "recovering": true }  // auto-resume
{ "kind": "status", "state": "error", "error": "the director stopped mid-step.", "actions": ["continue"] }
```
- `tool` events have past-tense `summary` strings once done (`generated 4 images (flux/schnell)`,
  `looked at 3 nodes`, `wrote 8 notes`), present tense while running, and optionally `usd`.
- Raw SDK diagnostics are never shown: they go to the runtime log.

### Turns, revert, redo
- The host starts a turn when it sends a `run` (or when the runtime reports `running` for a
  chat without one) and records every canvas's version. Mutations by the chat's agent sessions
  (connections with `X-Falos-Chat`) are tracked per (canvas, actor).
- `chat.revert` runs falos-doc's per-actor `revert_since(turn start)` for each of them, as one
  step; `chat.redo` re-applies those undo steps. Turns live in memory: after a host restart
  older turns can't be reverted. Reverting an earlier turn also undoes later turns of the same
  actors (actors are per agent name, e.g. `director`).

### Queue
- `chat.send` while a turn runs → `queued` event and `{"type":"queue","chat","id","text","author","from","context":[resolved]}`
  to the runtime. The runtime injects it into the running Claude Agent SDK query at the next
  tool call (streaming input, `priority: "next"`; if no tool call comes, it starts the next
  SDK turn when the current one ends) and replies `{"type":"injected","chat","id","text"}`;
  the host then emits `dequeued` (reason `injected`) and the `user` event.
- The fal driver uses queued messages as answers to its questions; the Codex driver runs them
  as the next turn (it can't take input mid-turn).
- `chat.unqueue` → host drops it (`dequeued`, reason `cancelled`) and sends `{"type":"unqueue","chat","id"}`.
- `chat.stop` drops the whole queue (`dequeued`, reason `stopped`); the UI puts that text back
  into the composer.
- Messages still queued when a turn ends (the runtime never took them) start the next run
  (`dequeued` reason `started`); the rest queue behind it again (`requeued` + a new `queued`).
- Cross-chat `chat_send` to a busy chat uses the same queue.

### Drivers
`driver` per chat (default `opus`): `opus` = Claude Agent SDK director + swarm (Opus 5.5 for
the director and every subagent); `fal` = the fal Agent directly (its plan → `plan`, activities
→ `tool`/`agent` events, its own fal-os session/cursor, artifacts → canvas nodes with lineage,
pending inputs → `question` events, stop → `responses.cancel`, chips → attachments via fal
storage URLs, multi-turn via `previous_response_id`); `codex` = a Codex thread with the fal-os
MCP configured (its own session/cursor via `X-Falos-Chat`). Switching drivers starts the next
run fresh with the transcript as context; the `run` frame says `previous_driver`.

### External agents
- **Agent tabs.** An MCP session without `X-Falos-Chat` (Claude Code, Codex, Cursor, … connected
  directly) works in its agent's tab: one per agent identity per project, `ext:<client>` when the
  agentName is just the client's ("Codex", "Claude Code"), else `ext:<client>-<name>` ("Reviewer"
  from Claude Code → `ext:claude-reviewer`). Title = agentName (renamable), a color,
  `external: true`, `client`, and `seen` (its last tool call, ms) in `chat.list`. A new session
  of the same agent reattaches to the tab (reopened if the user closed it) and its history
  continues there. Its tool calls appear as `tool` events (`running`, then `done`/`error` with a
  summary and `nodes`); `report` adds `text` (+ `done` with `nodes`).
- **Liveness.** `status` events: `running` at a tool call, `idle` after 30 s quiet, `idle` +
  `detached: true` (list `state: "detached"`) when its MCP connection closes, when no session is
  attached (host restart), or after 10 min of silence with no call in flight
  (`FALOS_EXT_DETACH_SECS` overrides). A tool call of a detached session attaches it again.
  Liveness isn't saved: tabs load detached. Duplicate `ext:<client>-<n>` tabs from older hosts
  merge into one on load (events in time order). Tabs can be closed (also while the agent works).
  Chat ids may be `ext:` + `[A-Za-z0-9_-]`; logs are `chats/ext_<…>.jsonl`.
- **Messaging the agent (inbox).** `chat.send` / `chat.steer` to an `ext:` chat queue the text
  for that tab (`queued` event with `author`, `context`; `stop: true` for a stop); whichever
  session of that agent calls next picks it up (`inbox`, appended to any of its tool results as
  "notices from the user in fal OS", or the next session's `session` result as "waiting for you
  from the user") and it becomes `dequeued` + `{"kind":"user","queued":id,"author":<name>}`.
  Undelivered messages are rebuilt from the log after a host restart. The tab shows them as
  "waits for next session" while detached. `chat.stop` queues a stop message (refused when
  detached; a stop left for an earlier session expires when a new one attaches),
  `chat.unqueue` takes a message back. Each session's inbox also gets the human's new comments,
  new nodes (references, notes) and selection changes on that canvas.
- **Binding.** `session` without `project` picks: `cwd` argument / the stdio relay's
  `X-Falos-Cwd` inside a workspace project folder → `FALOS_PROJECT` (`X-Falos-Project`) → the
  project open in fal OS (connected human, latest `focus` / selection / cursor) → the most
  recent project. The result says how (`boundBy`) and carries the overview (`context`).
- **Client frames.** `{"type":"focus"}` (window focus: this project is the one "open in fal OS");
  `{"type":"agent.continue","agent":"claude"|"codex","open"?:bool}` → `{"type":"agent.continue",
  command, copied, opened}` (`cd <project> && claude "continue this fal OS project"`, copied with
  `pbcopy`; `open` runs it in Terminal through a `.command` file).
- **External control.** The fal-os MCP server also has `chat_list(sessionId)`,
  `chat_new(sessionId, text, context?, driver?, canvas?)` → `{chat}`, `chat_send(sessionId, chat, text, context?)`
  → `{delivered, queued}`, `chat_read(sessionId, chat, last_n?)` → compact transcript, and
  `chat_stop(sessionId, chat)`. Their messages show as `{"kind":"user","author":"ext:<client>"}`.

### Host ↔ runtime additions
- Host → runtime `run` gains `turn`, `driver`, `previous_driver`, `continue` (chat.continue: resume,
  no new user message) and `outline` (a short canvas outline, first turn only). New frames:
  `queue`, `unqueue` (above).
- Runtime → host: `{"type":"injected","chat","id","text"}`, and
  `{"type":"session.agent","chat","session":"s_…","agent":"generator-2","parent":"director"}`
  once the runtime knows which subagent a fal-os session belongs to (cursor labels, activity).
- The runtime's MCP connections send `X-Falos-Chat: <chat>`.
- Errors: an SDK result error (e.g. `error_during_execution`) is logged, shown as a short
  message, and the run auto-resumes once in the same SDK session; if that fails too it ends
  with `status: error` + `actions: ["continue"]`.

## Projects, steering, capabilities (v1.1)
Additive to everything above.

### Projects
A **project** is one folder = one canvas = one fal Agent project, with many chats.
`falos-host serve --workspace <dir>` (default `~/fal-os-projects`) serves every project folder
in `<dir>`; `--project <dir>` keeps single-project mode. Project id = folder name; the label is
`project.json` `name`. Existing project folders load unchanged.

`GET /workspace?token=…` → WebSocket. Server → client, on connect and whenever anything changes
(create/rename/duplicate/delete/touch, running counts, recency; polled every 1 s):
```jsonc
{ "type": "projects", "projects": [{ "id": "demo", "name": "demo", "updated": 1760000000000 /* ms: max(touched, project.json, chats/index.json) */,
  "running": 1 /* running chats */, "fal_project": "…" | null, "thumb": "/assets/<sha>?project=demo" | null /* newest image; add &token= */ }] }  // newest first
```
Client → server (errors → `{"type":"error","message"}` to the sender):
```jsonc
{ "type": "project.create", "name": "Launch film" }   // → {"type":"project.created","id":"launch-film"} + a new list
{ "type": "project.rename", "id": "…", "name": "…" }   // id stays
{ "type": "project.duplicate", "id": "…" }             // canvas + assets (hard links) → "<name> copy"; not chats/work/fal_project
                                                       // → {"type":"project.created","id":"<new>","from":"<id>"} + a new list
{ "type": "project.delete", "id": "…" }                // moves the folder to <workspace>/.trash/<id>-<ms> (never rm); its chats stop,
                                                       // its /sync clients get {"type":"error","message":"this project was deleted"} and close
{ "type": "project.touch", "id": "…" }                 // recency (the default project for agents)
```
- **Sync:** `/sync/<project>` (its first canvas; `&canvas=` for another). `/sync/<canvas>` still
  works (single-project mode; in a workspace it means that canvas of the default project).
- **Assets:** `/assets/<sha>?project=<id>`. Without `project`, PUT goes to the default project and
  GET also searches the others.
- **Isolation:** each project has its own canvas, sessions, claims, activity, chats
  (`<project>/chats/`), turns and runtime work dirs (`<project>/work/<chat>/`). Chat events and
  `chat.list` on `/sync/<project>` are only that project's.
- **Default project** (MCP `session` without `project`, older clients): the most recently
  updated / touched one; an empty workspace gets an "Untitled" project.
- **fal Agent project:** `project.json` `fal_project`. The first fal-driver chat in a project
  creates (or reuses, by name) a fal Agent project named after it and reports it
  (`project.fal`, below); every fal-driver chat in the project then runs as a conversation in it
  (shared memory, characters, assets). Duplicates start without one.

### Steering
Client → host:
```jsonc
{ "type": "chat.steer", "chat": "c_…", "id": "q_…" }                      // send a queued message now
{ "type": "chat.steer", "chat": "c_…", "text": "…", "context": [], "agent": "motion" }  // or new text; agent optional
```
"Deliver now": the runtime interrupts the current step and continues the **same session** with
the message as the next user message — also while the director is blocked on a subagent
(foreground subagents and commands move to the background and keep running).
- Host emits (when it came from the queue) `{"kind":"dequeued","id","reason":"steer"}`, then
  `{"kind":"user","text","context","author","queued":"q_…","steer":true,"agent"?:"motion"}`
  (`queued` = it joins the running turn; `agent` = the subagent it was sent to).
- `agent` given and that subagent running: it gets the message in its own context after its next
  tool call (or as it is about to stop); the director gets an FYI. Not running: the director
  gets it as "message for <agent>" and relays it.
- No running turn: an ordinary message (starts a run).
- fal driver: answers a pending fal Agent question, else cancels the running task and continues
  the same conversation with it. Codex driver: aborts the Codex turn and continues the thread.
- Queued messages (`chat.send` while running) now also reach the director while it waits for
  background subagents (it is between turns), not only at its next tool call.

### Capabilities
At the start of every run the runtime emits:
```jsonc
{ "kind": "capabilities", "available": ["claude", "fal-agent", "fal-models", "codex", "remotion", "hyperframes", "ffmpeg", "remotion-skills", "hyperframes-skills"],
  "missing": [{ "name": "codex", "fix": "run `npx codex login`" }] }
```
`claude` always; `fal-agent` = FAL_KEY with the AGENT preset (cached `GET /agent/projects`);
`fal-models` = FAL_KEY; `codex` = Codex sign-in **and** the host's MCP answering on a fresh
session; `remotion` = template deps + chrome-headless-shell; `hyperframes` = its template deps +
that browser + ffmpeg; `*-skills` = the official agent skills installed
(`apps/agent-runtime/scripts/install-skills.sh`). The same list goes into the director's and
every subagent's system prompt; missing tools are never planned with. In the opus driver Opus
writes code itself; Codex only when the user asks (or picks the codex driver).

### Summaries
`tool` `summary` strings and `agent` `status` lines start with exactly one verb and never repeat
the tool or agent name: `built a contact sheet` (not `ran ran Build a contact sheet`), `agent`
`role` is a description (`edit & render`, `fal media`, `script & shots`, `render check`, `code`),
never the label again (not `motion · motion · …`).

### Host ↔ runtime (projects)
- Every frame both ways carries `project` (the project id). Host → runtime `run` also has
  `project_dir`, `project_name`, `fal_project`; older runtimes used `project` as the folder.
  The runtime keys runs by (project, chat); `request` frames carry `project`.
- Host → runtime `{"type":"steer","project","chat","id","text","author","agent","context":[resolved],"chips":[raw]}`.
- Runtime → host `{"type":"steer.failed","project","chat","id","text","author","chips"}` when no run
  was active (the host starts a run with it, without a second `user` event), and
  `{"type":"project.fal","project","fal_project"}` (stored in `project.json`).
- The runtime's MCP connections send `X-Falos-Project: <id>` (with `X-Falos-Chat`); Codex gets
  an explicit MCP config with both headers.

## Runtime events (`event` payload)
```jsonc
{ "kind": "user", "text": "…", "context": [/* chips */], "author": "lukas" }
{ "kind": "status", "state": "running" | "idle" | "error", "error": "…" }
{ "kind": "text", "agent": "director", "delta": "…" }                 // streamed assistant text
{ "kind": "thinking", "agent": "director", "delta": "…" }             // optional, collapsed in the UI
{ "kind": "agent", "agent": "generator-1", "role": "fal generator", "parent": "director",
  "state": "started" | "working" | "done" | "failed", "status": "generating 4 hero shots" }
{ "kind": "tool", "agent": "generator-1", "id": "t1", "name": "generate", "summary": "flux-pro · hero shot 2",
  "state": "running" | "done" | "error", "nodes": ["x1y2z3"] }        // nodes touched → clickable in chat
{ "kind": "plan", "agent": "director", "steps": [{ "title": "Storyboard", "state": "done" }, …] }
{ "kind": "cost", "usd": 0.42, "turn_usd": 0.12 }                     // cumulative fal spend for the chat (the host adds up turns; the runtime reports per turn)
{ "kind": "done", "summary": "…", "nodes": ["final01"] }
{ "kind": "files", "files": [{ "path": "/…/exports/2026-10-11/p_hero_reel_1080x1920_v1.mp4", "name": "…", "node": "a1b2c3", "preset": "reel", "w": 1080, "h": 1920, "bytes": 123, "sha256": "…" }], "dir": "/…" }  // host: an agent session in this chat exported files (file cards)
{ "kind": "title", "text": "Headphones ad" }                          // Director's chat_title ("title" is accepted as the field name too)
{ "kind": "user", "author": "chat:c_a", "from_title": "Logo", "text": "…" } // a message delivered from another chat
```
Agents are labelled by subagent type (`storyboard`, `generator`, `generator-2`, `motion`, `qa`) and `codex`. `director` is the main agent. The runtime emits these event kinds:
- `tool` events for canvas tools, with the touched `nodes`;
- `tool` events for the swarm tools: `fal_agent`, `fal_model`, `codex`; concurrent fal Agent briefs each appear as their own agent (`fal-agent`, `fal-agent-2`, …, `role: "fal Agent"`, `parent` = the caller) with their own fal-os session;
- `cost` events from fal Agent `usage.cost`, or from list-price estimates for direct model calls;
- `files` events (file cards, Assets v1): `{"kind":"files","agent","files":[{path,name,node?,preset?,w?,h?,duration?,bytes,sha256}],"dir"}` : the host appends them for agent `export`s (including the one inside swarm `deliver`); the runtime posts them for swarm `share_files` and fal-os `package` zips (and for an `export` result with `"emitted": false`).

Assets v1 adds the subagents `editor` (cuts & reframes) and `finisher` (export & delivery), and the capabilities `asset-ops`, `asset-export`, `asset-library`, `media-inputs`, `provenance`, probed from the host's MCP `tools/list` (missing ones carry the host-upgrade fix and the fallback the agents use meanwhile).

## Host ↔ runtime (`/runtime` WebSocket)
The runtime connects to the host as a client with two credentials:
- `Authorization: Bearer <per-install token>`;
- `X-Falos-Runtime: <runtime token>`. The host passes it as `FALOS_RUNTIME_TOKEN` when it spawns the runtime (`falos-host serve --runtime <dir>`, auto-detected `apps/agent-runtime`, `--no-runtime`).

Messages:
- Host → runtime:
  - `{"type":"hello","project":…}`, sent first;
  - `{"type":"run","chat":…,"canvas":…,"text":…,"author":…,"context":[resolved chips],"history":[{"role","text"}…],"options":…,"project":<dir>}`;
  - `{"type":"stop","chat":…}`.
- Runtime → host: `{"type":"event","chat":…,"event":{…}}`, where `event` needs a `kind`.

Behaviour:
- Runs sent while no runtime is connected are queued and delivered when one connects. One runtime is active at a time; a new connection replaces the old one.
- If the runtime disconnects, each running chat gets a `status: error` event.
- Chats persist in `<project>/chats/<chat>.jsonl`, one `{"seq","ts","event"}` per line, plus `<chat>.meta.json` with the canvas and title.

## Canvas tools the swarm relies on (fal-os MCP)
- These already existed: `session`, `read`, `edit`, `set_status`, `claim`/`release`, `annotate`, and `generate` (a single model; the fallback path).
- `import_file(sessionId, canvas, path, placement?, name?, lineage?, replace?)` copies a local media file (a Remotion render, a downloaded fal output) into project assets by sha256.
  - It creates the media node, sized from ffprobe.
  - `replace` turns a `generation` placeholder into the media node in place, keeping its id and spot.
  - `lineage` is `{model, params, falRequestId, cost, parents, uri}`.
- `get_context(sessionId, canvas, chips)` resolves chips (shape above), for agents that weren't given them directly.
- `set_asset_uri(sessionId, canvas, asset, uri)` records where an asset's bytes live publicly (https), so they're uploaded once. Asset records are otherwise immutable.

## UI (minimal, in the style of the fal.ai dashboard)
- **Canvas** fills the window. The top bar is minimal: project name, connection dot, signed-in fal account.
- **Right side: the chat panel.**
  - Messages, streamed text and collapsible tool and agent rows. Clicking a node chip pans to it.
  - A swarm strip above the input shows each active subagent with its status. It's live, and fades when they finish.
  - The input has context chips. Selecting assets on the canvas offers "Add to chat". **Alt-click** a spot, or **Alt-drag** a region on an image or video, to add a spot, region or frame chip; for video the frame is the current playhead.
- Removed in v0: the left layers panel, the annotations panel and tool, demo cursors, heavy branding. Cursors show only for real agent sessions that are active.

## App nodes
Live web apps on the canvas ([ARCHITECTURE.md](ARCHITECTURE.md#app-nodes-live-code)).
- **MCP tools** (host):
  - `create_app(sessionId, canvas, name, files?, entry?, placement?)` → `{app, node, name, version, dir, url, files, check}`
  - `write_app_files(sessionId, app, files?[{path, content|base64}], delete?, replace?, check?)` → the same plus `written`
  - `read_app(sessionId, app, paths?)` → `{…, contents, errors}`
  - `app_logs(sessionId, app, rerun?, all?)` → `{version, errors, ok, entries[{level, text, source: check|preview, version, at}], note}`

  `check` = `{ok, errors[], warnings[], note}` from a headless load. `app` takes an app id or the node id.
- **Chat event.** The host appends it for agent sessions in a chat after every create or write. The UI keeps one card per app per turn:
  ```jsonc
  { "kind": "app", "app": "ping-pong", "node": "a1b2c3", "name": "Ping Pong", "version": 3, "files": ["index.html", "main.js"] }
  ```
- **Sync frames.**
  - `hello` gains `"apps": "http://127.0.0.1:<port>"` (the app server) and `"project"`.
  - Host → client `{"type":"app","app","node","version","what":"files"|"logs","errors"?}`: reload on `files`; `errors` = console errors of that version.
- **Context.** A chip on an app node resolves with `"app": {app, dir, url, entry, version, files, errors, hint}`, and the snapshot still as its `asset` / `file` (sent to Claude as an image). Selecting the app and asking for a change is enough.
- **Drivers.**
  - opus: the director builds small apps itself; the `coder` subagent (role "code & apps") handles big ones.
  - codex: its sandbox can write `<project>/apps/` directly. The host versions the change, reloads the node and attributes it to the session that last touched the app.

## Parallel execution

The director plans a job as a dependency graph and runs independent branches at once: parallel
tool calls in one turn, background subagents, `generate_batch` for N items (one call, all
placeholders at once, identical requests packed via the model's `num_images`) and per-item
pipelines (`then` on `generate_batch` / `transform`: each output flows into the next step in the
host as it lands, every step a node with lineage). Nothing blocks on a batch: the runtime watches
the director's batches (`await_batch` in short polls) and injects a `[batch done] …` user message
(nodes, errors, chains) into the running session; concurrent `fal_agent` briefs report the same way
(`[fal Agent done] …`). The session stays open while any of this runs. At turn start the host
uploads the attached media to fal storage in the background (recorded with `set_asset_uri`), so
`mediaInputs` don't wait for an upload.
