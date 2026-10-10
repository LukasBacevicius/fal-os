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
                                                                     └─ MCP: fal relay (https://mcp.fal.ai/mcp-relay, user's fal OAuth token from the host)
```

## Context chips
```jsonc
{ "kind": "node", "node": "a1b2c3" }                                   // whole asset
{ "kind": "spot", "node": "a1b2c3", "x": 0.42, "y": 0.31 }             // normalized 0..1 in the node
{ "kind": "region", "node": "a1b2c3", "box": [0.1, 0.2, 0.5, 0.4] }    // x, y, w, h normalized
{ "kind": "frame", "node": "v9", "t": 3.24, "box": [0.1,0.2,0.3,0.3] } // video frame (box optional)
{ "kind": "range", "node": "au1", "t0": 2.0, "t1": 5.5 }               // audio/video time range
```
The host turns chips into context the agent can read. Each chip gets its node's text projection row, its lineage, and a local file path for its media. The runtime passes images and frames to Claude as image blocks: the region is cropped, and the frame is extracted with ffmpeg.

## Client ↔ host (JSON text frames on the existing `/sync/<canvas>` socket)
Client → host:
```jsonc
{ "type": "chat.send", "chat": "c_123" /* omit to start a new chat */, "text": "OPUS please direct me an ad for the headphones", "context": [/* chips */] }
{ "type": "chat.stop", "chat": "c_123" }
```
Host → client, broadcast to every client on the canvas:
```jsonc
{ "type": "chat.event", "chat": "c_123", "seq": 17, "event": { /* below */ } }
{ "type": "chat.history", "chats": [{ "chat": "c_123", "title": "Headphones ad", "events": [/*…*/] }] } // sent after hello
```

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
{ "kind": "cost", "usd": 0.42 }                                       // cumulative fal spend for the chat
{ "kind": "done", "summary": "…", "nodes": ["final01"] }
```

## Host ↔ runtime (`/runtime` WebSocket, bearer token)
The runtime connects to the host as a client. The host sends `{"type":"run","chat":…,"text":…,"context":[resolved chips],"history":[…]}` and `{"type":"stop","chat":…}`. The runtime replies with `{"type":"event","chat":…,"event":{…}}`. Chats persist in `<project>/chats/<chat>.jsonl`, one event per line.

## Canvas tools the swarm relies on (fal-os MCP)
- `session`, `read`, `edit`, `set_status`, `claim`/`release`, `generate`: these exist.
- `import_file(sessionId, canvas, path, placement?, name?)`: copies a local file (e.g. a Remotion render) into project assets and creates the media node.
- `get_context(sessionId, canvas, chips)`: resolves chips to rows, lineage, local file paths and crops, for agents that weren't given them directly.

## UI (minimal, in the style of the fal.ai dashboard)
- **Canvas** fills the window. The top bar is minimal: project name, connection dot, signed-in fal account.
- **Right side: the chat panel.**
  - Messages, streamed text and collapsible tool and agent rows. Clicking a node chip pans to it.
  - A swarm strip above the input shows each active subagent with its status. It's live, and fades when they finish.
  - The input has context chips. Selecting assets on the canvas offers "Add to chat". **Alt-click** a spot, or **Alt-drag** a region on an image or video, to add a spot, region or frame chip; for video the frame is the current playhead.
- Removed in v0: the left layers panel, the annotations panel and tool, demo cursors, heavy branding. Cursors show only for real agent sessions that are active.
