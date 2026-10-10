# fal-os host protocol

One loopback HTTP server (`127.0.0.1`, port 7317 or the next free one) serves two endpoints:

| Path | What |
|---|---|
| `POST/GET /mcp` | Canvas MCP server, Streamable HTTP (agents) |
| `GET /sync/{canvas}` → WebSocket | Canvas sync (the canvas UI, running falos-doc in wasm) |

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
{"type":"error","message":"…"}
```
`SessionInfo = {id, agentName, actor, kind: "human"|"agent", color, presence: {status, canvas?, cursor?: [x,y], viewport?: [x,y,w,h], last_activity}}`
`Claim = {node, session, actor, expires}` (unix ms). Canvas-scoped events are only sent
for the connection's canvas; presence events are global.

### client → server
```jsonc
{"type":"presence","cursor":[x,y],"viewport":[x,y,w,h],"status":"…"}   // all optional
{"type":"revoke_claim","node":"881s9r"}                                // human override
```

## MCP tools
All tools except `session` and `get_knowledge` require `sessionId` (no shared fallback).
Every mutation runs as the session's actor (attribution + per-actor undo). Results that
contain canvas text or comments wrap it in `<untrusted-canvas-data>…</untrusted-canvas-data>`.
Pending notices for the session (claim revoked, annotation assigned) are appended to any
tool result under `notices`.

| Tool | Hints | Notes |
|---|---|---|
| `session(agentName, project?, canvas?)` | | → `sessionId`, actor, canvases, guide, knowledge keys. Creates `canvas` if new. |
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
| `generate(sessionId, canvas, model, input, placement?)` | open-world | Returns the `generation` node id at once; fal queue job runs async and converts the node (same id) into the media node with an asset + lineage. Needs `FAL_KEY`. Future: MCP Tasks. |
| `get_knowledge(keys)` | read-only | `format`, `annotations`, `generation`, `collaboration`. |
