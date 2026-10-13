# UX v1: chats, agents, layers

Builds on [UI-SPEC.md](UI-SPEC.md) (mono terminal look, minimal chrome) and [CHAT-PROTOCOL.md](CHAT-PROTOCOL.md). Research: [Brilliant chat UX](research/brilliant-chat-ux.md), [Paper](research/paper-ux.md).

**The principle:** chats are the work units, and the canvas shows what they are doing. Each chat has **one color**, used for its tab, its agents' cursors, its activity outlines and the highlight on the nodes it touches. That color is the only exception to the monochrome UI. Use a muted palette of 8 desaturated hues, assigned per chat.

## 1. Start a chat from what you're looking at
- **Selection prompt (`[>]`):**
  - Selecting one or more nodes shows a small `[>]` at the selection's top-right corner. Click it, or press `/` with a selection, to open an **inline prompt** that follows the selection.
  - The prompt is pre-filled with the selection as chips: image thumbnails, `[note …]`, `[frame …]`.
  - **Enter** starts a **new chat**: the panel opens on it and the director runs. **⌘Enter** sends to the current chat instead. **Tab** ("open in chat") moves the draft into the panel.
  - A draft is kept per selection.
- **Composer input:**
  - Drop or paste images and files onto the chat input. They're uploaded as canvas nodes, placed near the viewport, and attached as chips.
  - You can also drag a canvas node onto the chat.
  - `@` autocompletes nodes by name; `#` autocompletes other chats.
- **Chips:**
  - Image chips show a 24px thumbnail; others are bracketed text. Chips stay on the sent message; hovering shows a preview, clicking pans to the node.
  - With a selected image, Alt-click or Alt-drag still adds spot and region chips.
- **Nothing is attached implicitly.** The host sends the director a short canvas outline on the first turn.

## 2. Running chats: overview and switching
- **Chat tabs** run along the top of the chat panel, one per open chat:
  - Each tab shows its color swatch, title, `*` while running, and `•` for unread.
  - ⌘1–9 badges show when ⌘ is held.
  - Tabs can be closed (archived) and reordered by drag.
  - `+` (⌘N) starts a new chat. A `≡` opens the full chat list: search, archived, all chats with state and cost.
- **Activity bars:** a thin bar in the bottom-left corner of the canvas, one colored segment per running chat. Clicking a segment opens that chat. This is the always-visible "what's running" indicator, even with the panel collapsed.
- ⌘⇧[ / ⌘⇧] cycle chats; ⌘\ collapses the panel.
- **Clicking an agent cursor** on the canvas opens its chat, scrolled to that agent.

## 3. Transcript
- **User messages** are shown as `> text` with their chips. An edit affordance on your own message rewinds: it drops later turns and re-runs from there.
- **Director text** streams as plain text. Light markdown is rendered minimally: lists and code; no raw `**`.
- **Activity lines:** one line per tool, with **past-tense verbs**, merged when consecutive and of the same kind. Each expands to show details. For example:
  - `· generated 4 images (flux/schnell) → a1 b2 c3 d4   $0.01`
  - `· looked at 3 nodes`
  - `· wrote storyboard (8 notes)`
  - `· thought for 12s`

  Housekeeping calls (session, set_status, title, knowledge, file reads) are hidden. Node ids are clickable.
- **Subagents:**
  - Running subagents appear in a **swarm strip** above the input, one row each: `● generator-2  generating hero shot 3/4  [stop]`, with the dot in the chat color.
  - Clicking a row opens **that subagent's own transcript** in the panel, with a `← main chat` row to go back.
  - A subagent's tool lines go only into its own transcript. The director's transcript shows a one-line card when it starts and a summary card when it finishes (status, tools, duration, nodes).
- **Messages while a run is going** queue above the input: `queued: …` (editable, ×). They're injected after the next tool call.
- **Revert:** each finished turn ends with `· done → nodes   [revert]`. Revert undoes everything that turn's agents changed, in one step, and redo is available.
- **Scrolling:** follow output only while scrolled to the bottom; otherwise show a `↓ latest` button.
- **No duplicate final text:** the done line never repeats the director's message.

## 4. Agent activity on the canvas (from Paper)
- The host emits activity per agent session: **read**, **write**, **edit** and **delete**, with node ids and a timestamp. Entries expire after 30 s, or when the agent calls `finish_working(ids?)`.
- **Engine overlay,** in the chat's color. Plain 1px outlines only: no sweeps, glows, fills or easing. One-shot outlines hold, then step down in three discrete levels (≤1 s):
  - a steady outline on the frame or group currently being worked on;
  - **read:** a dashed outline;
  - **write:** a solid outline, then child outlines appear in sequence (capped);
  - **edit:** a short solid outline;
  - **delete:** a dashed outline where the node was.
- **Cursors:** a small arrow in the chat color with the agent's task label (`generator-2 · hero shot`). Dimmed while thinking, solid while writing. Fades out after 10 s idle.
- A "reduce motion" setting turns the stepped outlines into static ones.
- **Agents never move the user's camera.** Only an explicit click pans.

## 5. Layers and properties (left panel, ⌘⇧\)
- **Layers:**
  - Every node is a layer in a tree that follows the doc tree, z-ordered with the top of the list on top.
  - Each row has a type glyph (`▣` frame, `▢` group, `▧` image, `▶` video, `♪` audio, `¶` note/text, `◌` generation), the name, and a colored dot if a chat created or is touching it.
  - Selection is synced both ways. Double-click renames; drag reorders or reparents.
  - Collapsible; filter with ⌘F in the panel.
  - Hovering a row highlights the node on the canvas.
- **Grouping** follows Paper and Figma: **⌘G** wraps the selection in a group, **⌘⌥G** frames the selection, ⌘⇧G ungroups.
- **Properties** sit below the layers, for the current selection only. Compact mono fields:
  - all nodes: x, y, w, h, rotation, locked;
  - media: source (prompt, model, seed, parents, cost) with actions `[copy prompt] [rerun] [variations] [use as reference]`. "Use as reference" adds the node as a chip to the current chat.
  - notes: text size;
  - generation: status and `[cancel]`.
  - Multi-select shows the shared fields.

## 6. Generation placeholders
- Placed in **empty space next to the selection** or next to the referenced nodes; never on top of existing content.
- Label: `<model> · <elapsed> · <progress>`, shown on hover over the LED-mark tile (UI-SPEC, generating state).
- On completion the result keeps its lineage, so `[rerun]` and `[variations]` work from the properties panel.

## 7. Connect your agent
- **Opened from** the ⌘K palette ("connect agent") and from the empty-state hint on an empty canvas. It's a modal with vertical tabs: Claude Code, Codex, Cursor, Gemini, OpenCode, Other.
- **Each tab has three steps:**
  1. **Connect:** a one-click `[add to Claude Code]` that calls the host's register endpoint, which writes the config: preserving format, atomically, idempotently, and refusing broken files. A manual snippet sits in a collapsible.
  2. **Restart your agent:** shown as a note.
  3. **Try a prompt:** example prompts with copy buttons, such as "In fal-os, make a moodboard for a neon Tokyo night shoot".
- **Checkmarks come from real events:** step 1 ticks when that client first connects (its session appears), step 3 when it first writes. On the first write the dialog closes, the canvas zooms to the result, and a one-line toast says it worked.
- **Host:**
  - Identify agents by MCP client name.
  - A stdio relay (`falos-host mcp`) for clients that prefer stdio.
  - Show external agents in the chat tab strip as `ext:` pseudo-chats, so their activity is visible too: read-only transcripts of their tool calls.
