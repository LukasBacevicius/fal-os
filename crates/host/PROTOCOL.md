# fal-os host protocol

One loopback HTTP server (`127.0.0.1`, port 7317 or the next free one) serves:

| Path | What |
|---|---|
| `POST/GET /mcp` | Canvas MCP server, Streamable HTTP (agents) |
| `GET /sync/{project}` → WebSocket | Canvas sync of a project (the canvas UI, running falos-doc in wasm); `/sync/{canvas}` in single-project mode |
| `GET/HEAD/PUT /assets/{sha256}?project=<id>` | Content-addressed project media (`<project>/assets/<hex>.<ext>`) |
| `POST /import/url?project=<id>` | The host downloads a remote media URL into the assets (`{url, sha256?}`) |
| `GET /workspace` → WebSocket | Projects list + `project.*` ops (see docs/CHAT-PROTOCOL.md → Projects) |
| `GET /runtime` → WebSocket | The agent runtime (one for the workspace; frames carry `project`) |

## Workspace and projects
`falos-host serve --workspace <dir>` (default `~/fal-os-projects`; `--project <dir>` = single
project). Each subfolder with a `project.json` is a project; its id is the folder name. Projects
load lazily and run concurrently, each with its own canvas, sessions, claims, event bus, chats
and turns. `/sync/<project>` (first canvas, `?canvas=` for another) — `/sync/<canvas>` still works
(single-project mode, or a canvas of the default project). Assets take `?project=<id>`.
Deleted projects move to `<workspace>/.trash/`. `project.json` keeps unknown fields; the host adds
`fal_project` (linked fal Agent project) and `touched` (ms). `host.json` gains `workspace`.
MCP sessions are dropped after 6 h idle (rmcp's default 5 min made long renders fail with
`404 Session not found`).

## Assets
`{sha256}` is the hex digest, optionally prefixed `sha256:` (the doc's asset id form).
- `PUT` streams the body to disk, verifies the digest (`400` on mismatch), caps it at 2 GiB
  (`413`), stores the content type as the file extension; `201` when written, `200` when it
  already existed (dedupe: clients `HEAD` first).
- `GET`/`HEAD` serve the file with its content type, `ETag`, immutable caching and
  `Accept-Ranges: bytes`; a single `Range: bytes=a-b | a- | -n` gets `206` + `Content-Range`
  (`416` if unsatisfiable), so `<video src>` can seek. GET/HEAD also accept `?token=` because
  media elements can't set headers.
- CORS: allowed origins (below) get `Access-Control-Allow-Origin` echoed; `OPTIONS` preflight
  on `/assets/*` and `/import/url` needs no token.
- `POST /import/url` `{url, sha256?}`: downloads an http(s) file (the page can't: CDNs such as
  Figma's signed image URLs send no CORS headers) and stores it like a `PUT`. Only media
  (`image/* video/* audio/*`, sniffed from the bytes when the server says octet-stream; else
  `415`), at most 512 MiB (`413`), upstream errors → `502`. With `sha256` an existing asset
  answers at once (`200`, no download) and a download that hashes differently is `400`.
  Answers `{id: "sha256:<hex>", mime, size, file, existing, width, height}` (`201` when written,
  `200` when it existed; `width`/`height` from the PNG/JPEG/GIF/WebP header, else null). The
  canvas uses it for Figma pastes (`apps/web/src/figma.ts`).

## Discovery and auth
- `~/.fal-os/token` (0600): per-install random bearer token. `~/.fal-os/host.json` (0600):
  `{port, url, token, pid, project}` written on every `serve`. `FALOS_HOME` overrides `~/.fal-os`.
- Every request needs `Authorization: Bearer <token>`. `/sync/*` also accepts `?token=` because
  browsers can't set headers on WebSocket upgrades. Missing/bad token → `401`.
- `Host` must be loopback (`localhost`, `127.0.0.1`, `[::1]`, any port), else `403` (DNS rebinding).
- `Origin`, when present, must be loopback http(s), `tauri://localhost` or `http(s)://tauri.localhost`,
  else `403`.

## Sync WebSocket
`ws://127.0.0.1:<port>/sync/<canvas id or name>?token=<t>&actor=<id>&name=<display name>`

Each connection is a human session (actor `actor`, default `human`; suffixed `-2`, `-3`… if
already in use). Two frame kinds:

- **Binary frames = Loro bytes**, both directions.
  - server → client: first a full snapshot (right after `hello`), then incremental updates.
    A client that falls behind the event bus gets a fresh snapshot. Imports are idempotent.
  - client → server: Loro updates from the UI replica (`Canvas::export_updates(vv_before)`).
    The host imports them, rebroadcasts to every other client and emits a `doc` event. The UI
    must make its edits through falos-doc's `Canvas::apply`/`apply_edit` so ops are
    attributed (peer → actor map).
- **Text frames = JSON**, `{"type": …}`.

### server → client
```jsonc
{"type":"hello","canvas":"main","version":"12@345,…","session":{SessionInfo},"sessions":[SessionInfo],"claims":[Claim]}
{"type":"doc","canvas":"main","actor":"claude-code","version":"…","created":["881s9r"],"touched":["881s9r"]}
//   sent right after the matching binary update (not echoed to the connection it came from)
{"type":"presence","session":{SessionInfo}}
{"type":"session_closed","session":"s_…"}
{"type":"claims","canvas":"main","claims":[Claim]}            // full list for the canvas
{"type":"claim_revoked","canvas":"main","node":"881s9r","actor":"codex","by":"lukas"}
{"type":"annotation_assigned","canvas":"main","annotation":"yflw1y","node":"881s9r","assignee":"codex","by":"lukas"}
{"type":"job","canvas":"main","node":"b59dlc","model":"fal-ai/flux/dev","status":"queued|running|done|failed","queue_position":3,"error":"…"}
{"type":"batch","canvas":"main","batch":{"kind":"batch","id":"batch_…","model":"fal-ai/flux/schnell","total":200,"done":37,"failed":0,"running":120,"finished":false,"frame":"b…","nodes":null}}
{"type":"activity","canvas":"main","session":"s_…","agent":"generator-2","chat":"c_…"|null,"color":"#7393c2","op":"read"|"write"|"edit"|"delete","nodes":["…"],"at":1760000000000}
{"type":"activity.clear","session":"s_…","nodes":["…"]|null}           // null = everything (finish_working without ids, session end)
{"type":"agent.connected","client":"claude","clientName":"claude-code","agentName":"Claude Code","session":"s_…","chat":"ext:claude-1","transport":"http"|"stdio"}
{"type":"agent.first_write","client":"claude","session":"s_…","canvas":"main","nodes":["…"]}
{"type":"agent.registered","client":"claude","ok":true,"path":"/Users/me/.claude.json","changed":true}   // reply to agent.register; on failure ok:false, path:null, error
{"type":"error","message":"…"}
```
`SessionInfo = {id, agentName, actor, kind: "human"|"agent", color, presence: {status, canvas?, cursor?: [x,y], viewport?: [x,y,w,h], last_activity}, chat, agent, parent?, label, state, client?}`
- `chat`: the chat the session works for (`c_…` for the runtime's agents, `ext:<client>-<n>` for external agents), else null. `color` is then the chat's color.
- `agent`: the runtime's subagent label (`generator-2`, set via the runtime's `session.agent`) or the actor id; `parent`: its parent agent.
- `label`: what the cursor shows: the `set_status` text, else the agent name. `state`: `thinking` (reading) | `writing` (changing) | `idle` (10 s without activity).
- `client`: the normalized MCP client of external agents (`claude`, `codex`, `cursor`, `gemini`, `opencode`, else a slug of `clientInfo.name`).

**Activity.** Every agent session has a working set of nodes it read or changed. Tool calls
emit `activity`: `read`/`get_context`/`get_annotation` → `read`; `edit` → `write` for created
nodes, `edit` for changed ones, `delete` for removed ones; `annotate` → `edit`;
`generate`/`generate_batch`/`import_file` → `write`. Entries expire after 30 s (→
`activity.clear` with the expired ids) or when the agent calls `finish_working`.

**External agents and the connect flow.** An MCP session whose requests don't carry
`X-Falos-Chat` is an external agent: on `session` the host creates its pseudo-chat
`ext:<client>-<n>` (see CHAT-PROTOCOL.md), emits `agent.connected` (client from
`clientInfo.name`), and `agent.first_write` on the session's first mutation.

Agent presence is implicit: when an MCP session reads a subtree (`root`), edits, annotates,
claims or generates, the host moves its `cursor` to the center of the last node it touched
(world coordinates; child positions are parent-relative) and its `viewport` to that node plus
a margin, then emits `presence`. `set_status` sets the status line (and may override them).

`Claim = {node, session, actor, expires}` (unix ms). Canvas-scoped events are only sent
for the connection's canvas; presence events are global.

### client → server
```jsonc
{"type":"presence","cursor":[x,y],"viewport":[x,y,w,h],"status":"…"}   // all optional
{"type":"revoke_claim","node":"881s9r"}                                // human override
{"type":"selection","nodes":["881s9r"]}                                 // the user's selection (get_selection, placement)
{"type":"agent.register","client":"claude"|"codex"|"cursor"|"gemini"|"opencode"}  // → agent.registered
```
`agent.register` writes the fal-os MCP server into that client's own config (same code as
`falos-host register`): format-preserving, atomic, idempotent, refuses files it can't parse
(e.g. JSONC with comments), new files 0600. Paths: `~/.claude.json`, `~/.codex/config.toml`,
`~/.cursor/mcp.json`, `~/.gemini/settings.json`, `~/.config/opencode/opencode.json(c)`.
Chat frames (`chat.*`) are in CHAT-PROTOCOL.md.

## Stdio relay
`falos-host mcp` is a stdio MCP server that relays to the running host's `/mcp` (URL and token
from `~/.fal-os/host.json`), adding `X-Falos-From: stdio`. For clients that prefer stdio:
`{"command": "falos-host", "args": ["mcp"]}`. Logs go to stderr.

## Request headers (MCP)
- `X-Falos-Chat: c_…`: sent by the agent runtime on its agents' connections; sessions created
  there join that chat (color, activity, turn tracking for revert).
- `X-Falos-From: stdio`: set by the relay.
- `X-Falos-Project: <id or name>`: the project for `session` calls without `project` (the runtime's agents).

## MCP tools
All tools except `session` and `get_knowledge` require `sessionId` (no shared fallback).
Every mutation runs as the session's actor (attribution + per-actor undo). Results that
contain canvas text or comments wrap it in `<untrusted-canvas-data>…</untrusted-canvas-data>`.
Pending notices for the session (claim revoked, annotation assigned) are appended to any
tool result under `notices`.

| Tool | Hints | Notes |
|---|---|---|
| `session(agentName, project?, canvas?)` | | → `sessionId`, actor, project `{id, name}`, canvases, guide, knowledge keys. `project` = id or name (else `X-Falos-Project`, else the most recently used project); every later call with that `sessionId` runs in that project. External agents' `ext:` chats live in that project. Creates `canvas` if new. |
| `list_projects()` | read-only | `[{id, name, updated, running}]`, newest first. |
| `create_project(name)` | | → `{id, name}` (then `session(project=id)`). |
| `read(sessionId, canvas, root?, depth?, filter?, detail?)` | read-only | Text projection + `version`; `detail:"blurry"` = depth 1, no annotations. Records the read version. |
| `edit(sessionId, canvas, text, expectedVersion?, requestId?)` | destructive | `apply_edit`; → `version, created, touched, warnings, stale, preview`. Baseline defaults to the last read. `requestId` idempotent for 2 min. |
| `undo(sessionId, canvas, steps?)` | destructive | Own transactions only. |
| `revert_since(sessionId, canvas, version)` | destructive, idempotent | |
| `list_annotations(sessionId, canvas, status?, assignee?)` | read-only | `assignee:"me"`. |
| `get_annotation(sessionId, canvas, id)` | read-only | Anchor, thread, node row, asset lineage; + rendered media via `ContextRenderer` (TODO). |
| `reply(sessionId, canvas, annotationId, text, status?, outcome?)` | | |
| `annotate(sessionId, canvas, nodeId, text, anchor?, assignee?)` | | Anchor in text syntax (`box(…)`, `frame(t) pt(…)`, …). |
| `claim(sessionId, canvas, ids, ttlSecs?)` / `release(…, ids?)` | idempotent | Soft leases (default 120 s). Others get warnings, humans can revoke. |
| `set_status(sessionId, text, canvas?, cursor?, viewport?)` | idempotent | Presence. |
| `generate(sessionId, canvas, model, input, placement?)` | open-world | Returns the `generation` node id at once; fal queue job runs async and converts the node (same id) into the media node with an asset + lineage. Needs `FAL_KEY` (env or `~/.fal-os/fal.env`) or `falos-host login`. Without x/y, `placement` (`near?`) puts it in free space next to `near`, else the user's selection, never overlapping. |
| `generate_batch(sessionId, canvas, model?, input?, count?, variations?, jobs?, parent?, near?, label?)` | open-world | Many outputs at once: all placeholders in one transaction in a grid in free space (frame "<label> ×N" for 8+), all jobs submitted in parallel (64 in flight), one shared poller (500 ms → 2 s backoff), downloads ×16, conversions coalesced every 150 ms. Returns `{batch, frame, total, nodes}` immediately; progress as `batch` events. Max 500 outputs. |
| `await_batch(sessionId, batch, timeoutSecs?)` | read-only | Waits for a batch; → `{done, failed, finished, nodes, errors, seconds}`. |
| `finish_working(sessionId, ids?)` | idempotent | Ends the activity highlight on `ids` (all when omitted) → `activity.clear`. |
| `get_selection(sessionId)` | read-only | `{selections: [{actor, name, canvas, nodes: [{id, type, name, row}]}]}`: what the humans have selected. |
| `find_space(sessionId, canvas, w, h, parent?, near?)` | read-only | A free `{x, y}` next to `near` / the selection. |
| `chat_list` / `chat_new` / `chat_send` / `chat_read` / `chat_stop` | | Drive the in-app chats from an external agent (CHAT-PROTOCOL.md → External control). |
| `get_knowledge(keys)` | read-only | `format`, `annotations`, `generation`, `collaboration`. |
