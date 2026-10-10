# Brilliant: chat and agent UX patterns

Sources:
- Brilliant's own UI docs written for its agent (`Resources/.claude-prod/knowledge/reference/ui/*.md`, `reference/ai/*.md`, `canvas/window-modes.md`, `mcp-connections/*.md`)
- strings in the Dart binary
- read-only renders of its chat components

## Starting chats and attaching references
- **Typing in the bottom toolbar input starts a new chat.** Enter creates the session (titled from the prompt) and opens the chat panel. `/` or `.` focuses the input; ⌘N there starts a new chat.
- **From a selection:** a ✦ sparkle appears at the selection's top-right corner. Clicking it (or pressing `/`) opens an **inline prompt that follows the selection, with the selected elements already attached as chips**.
  - Enter opens the chat panel on a new session; "Open in chat" moves the draft there without sending.
  - Drafts are kept per selection.
- **Ways to attach:** `@` mentions canvas elements by name; paste, drag-and-drop or the paperclip add images and files; pasting an element works like `@`.
  - Attachment types: element, image, file, area.
- **Chips sit above the input** (image and element chips look different) and **stay on the sent message**. Hover shows a preview; click zooms to the element.
- **Nothing is attached implicitly.** The first message carries only a short canvas outline.

## Message rendering
- **Each activity is one quiet line**, with past-tense verbs: "Thought for …", "Looked at", "Created & updated", "Generating image…". Consecutive similar calls are merged ("Loading skills [a][b][c]"). Each line expands to show details.
- **Undo:**
  - "Undo all changes from this message on" and "Undo all changes in this message".
  - A **Revert** card per stopped turn, with Redo.
  - Editing one of your own messages **rewinds** the conversation; there is no branch tree.
- **Scrolling:** the sent message is pinned near the top and the reply streams below. The view follows only while you're at the bottom, and a "Jump to latest" button shows a streaming indicator.
- **Thinking:** shown as "Thought for Ns", with a thinking-level picker.
- **Cost and context:** cost is not shown inline (`/cost` shows it); context use appears only above 80%.
- **Duplicates:** a final assistant message that repeats streamed text is dropped.
- **Plans and questions:** a plan is a single card that ticks off steps. Multiple-choice questions can be answered by number key.

## Multiple chats and agents
- **Sessions:**
  - Session tabs show topic, a spinner while running, and context %.
  - ⌘1–9 / ⌘0 jump to a session, ⌘⇧[ / ⌘⇧] cycle, ⌘W closes.
  - A Chat Explorer sidebar (⌘⇧A) adds search, archive and auto-archive; collapsed, it becomes a strip.
- **Background strip above the input:** running subagents and shell commands, each with Stop.
  - Clicking a subagent opens **its conversation inside the same panel**, with a "Main chat" row to return.
  - A finished subagent becomes a card (status, tool count, tokens, duration).
  - The chat auto-resumes when background work finishes.
- **Messaging mid-run:** a message to the agent is read after the next tool call. Messages sent during a run wait **queued** above the input (editable or cancellable) and are injected mid-turn.
- **Project tabs** shimmer while an agent works there.

## Agent presence on the canvas
- **Cursors:** a named cursor per agent, **tinted in its session color**, dimmed while thinking and solid while editing.
  - Agents name themselves after their task, with a fallback name like "Nova" until then.
  - An optional 2–4 line thinking caption types out next to the cursor.
  - Clicking the cursor or its card opens that agent's chat.
- **Read sweep:** a scan animation over elements the agent is reading.
- **Toolbar:** one colored activity bar per running session (`agent_rain_indicator`).

## Layers
A plain tree with no marking of which items agents made. Not worth copying.

## Connecting agents
- **Settings → MCP Connections:** a checkbox per client, reflecting whether the client's own config file contains Brilliant. A refresh button, plus a note: "restart the tool afterwards".
- **In-chat setup checklist:** steps tick off when the agent's first call arrives.
- **Commands:** a palette command per client.

## Patterns for fal-os, in priority order
1. **Start a chat from a selection** with the ✦ inline prompt, pre-filled with chips. Make attaching an image one gesture (drop or paste on the composer, or drag a node onto the chat). Chips stay on sent messages.
2. **Persistent running-chat switcher:** title, spinner, unread marker and ⌘1–9 badges. **One color per chat**, reused everywhere: tab, toolbar bar, canvas cursor, and the highlight on nodes it's working on.
3. **Swarm strip:** one row per subagent with Stop. Click to open its conversation in the panel. A finished subagent collapses to a card. Don't interleave every subagent's output into the director's transcript.
4. **One merged line per tool call,** with past-tense verbs, expandable. Hide housekeeping calls; show "Thought for Ns".
5. **Queued messages and messaging subagents.**
6. **Revert per turn,** plus edit-and-rewind.
7. **Canvas presence:** session-colored cursor, task name, optional caption, click to open the chat, read sweep.
8. **Scrolling:** pin the sent message, follow only at the bottom, "Jump to latest".
9. **Smaller items:** number-key answers, `/` commands plus recent chats, prompt history, and an MCP clients settings list.
