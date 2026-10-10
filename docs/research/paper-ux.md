# Paper (paper.design) 0.5.15: agent UX teardown

Sources:
- the Electron shell's source, which ships as unminified TypeScript
- web UI bundles recovered from the local cache
- a read-only probe of the MCP server (`initialize` and `tools/list` only)

## Architecture
- **Shell and UI:** the shell loads `https://app.paper.design` (React with a MobX-style store; activity overlays drawn on a 2D HUD canvas). Backend: `sync.paper.design` (websocket) and `api`/`assets`; login through WorkOS.
- **MCP server:**
  - Streamable HTTP at `127.0.0.1:29979/mcp`. It rejects requests that carry an Origin header and checks Host.
  - A CLI at `~/.paper/bin/paper mcp` relays stdio to it, and every client config uses that stdio command.
  - Each MCP session is one "agent", identified by the client name (claude-code, codex-mcp-client, …).
  - Tool definitions live in the web app, not the shell. The main process forwards each call into the page.
- **Routing:**
  - An optional `fileId` per call; otherwise the file the user is viewing.
  - `pageId` lets an agent work without disturbing you; agents can never switch your page.
  - Background tabs stay alive while an agent works in them.
- **36 tools,** all with read-only or destructive hints:
  - Reading: selection, node info, tree summary, screenshot, `get_jsx`, computed styles, `find_nodes`.
  - Writing: `write_html` (insert or replace), `update_styles`, `duplicate_nodes` (returns old→new id map), `move_nodes`, `set_text_content`.
  - Other: tokens, comments, export (including mp4/webm), and **`finish_working_on_nodes`**.
- **Undo:** agent edits are kept out of the user's undo stack (`transactionWithoutUndo`).

## Patterns
- **No in-app agent chat.** The conversation happens in Claude, Codex, Cursor and so on. Context is pulled: the agent calls `get_selection`, so whatever you have selected is the reference. Comments serve as a task queue that agents list, fix and resolve.
- **Image prompt box** (⌘⇧I):
  - Selected images become thumbnail chips and automatically switch it to image-to-image. Selected text becomes chips.
  - It shows the original prompt, with "Start over from this prompt" and "Copy prompt".
  - Model picker, including a **Variety pack** that runs one prompt through four models.
  - Placeholders read "Generating with <model>", are placed in empty space next to the selection, and the camera pans to them. Lineage is recorded.
- **Agent activity on the canvas:**
  - Each agent tracks which nodes it read or wrote and when. Entries expire after 40 s or when it calls `finish_working_on_nodes`.
  - On screen:
    - a pulsing outline around the artboard being worked on, plus a "rain" of animated dots by the artboard label;
    - a **read** sweep upward;
    - a **write** reveal: a downward sweep, then up to 6 child outlines appear in sequence;
    - an **edit** highlight;
    - a **delete** fade.
  - A "Reduce agent animations" setting.
  - Activity icons appear on pages and on desktop tabs.
- **Presence:** agents appear in the avatar bar with the tool's icon tinted in the owner's color, and are broadcast to collaborators. There's no list of chats and no follow-agent.
- **Layers:** every HTML element from `write_html` becomes a layer, named with `layer-name`; renamed layers keep their name. There's no group type: grouping means frames with flex layout (frame selection ⇧F, ungroup ⌘⇧G, wrap in flex ⇧A).
- **Inspector:** sections are composed from what the selection supports. With nothing selected, it shows an MCP panel with "Connect your agent".
- **"Connect your agent":** a vertical tab per agent (Claude, Codex, Cursor, OpenCode, Antigravity, Copilot, VS Code, Other), each with three steps:
  1. Install.
  2. One-click "Add to X", with a manual-snippet fallback.
  3. Example prompts.

  Step checkmarks are driven by **real events**: the first connection, then the first write. On the first write the dialog closes, the canvas zooms to the result, and a toast says it worked.

  Config writes are comment-preserving, atomic, idempotent, refuse broken files, and create new files as 0600.

## For fal-os
Copy:
- canvas-side activity (sweeps, outline on the frame being worked, expiry plus an explicit finish signal);
- agents in presence;
- selection as visible chips, with image-to-image switched on automatically;
- background work that never steals focus;
- generation placeholders near the selection that keep their prompt;
- an onboarding flow whose progress comes from real events;
- comments as a task queue.

**Where we beat it:** a built-in chat and thread list whose tool rows link to those canvas highlights, and per-turn undo for agent edits.
