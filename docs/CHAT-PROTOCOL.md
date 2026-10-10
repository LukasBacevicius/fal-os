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
{ "kind": "title", "text": "Headphones ad" }                          // Director's chat_title ("title" is accepted as the field name too)
{ "kind": "user", "author": "chat:c_a", "from_title": "Logo", "text": "…" } // a message delivered from another chat
```
Agents are labelled by subagent type (`storyboard`, `generator`, `generator-2`, `motion`, `qa`) and `codex`. `director` is the main agent. The runtime emits these event kinds:
- `tool` events for canvas tools, with the touched `nodes`;
- `tool` events for the swarm tools: `fal_agent`, `fal_model`, `codex`;
- `cost` events from fal Agent `usage.cost`, or from list-price estimates for direct model calls.

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
