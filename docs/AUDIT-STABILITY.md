# Stability and polish audit (2026-10-14)

A hands-on pass over the web app at `a95d21a` ("Director agents…"), driven like a user in a
browser, against its own host. The question was "is this half-baked or good?".

**Short answer:** the core is better than "half-baked". Canvas, import, previews, library,
export, sync and host restarts held up under real use. The chat panel and the external-agent
tab are the rough parts. One P0 sat in the most basic path: ⌘A then ⌫ did nothing on any
canvas with a frame. Besides it, 13 P1s showed up, mostly in failure paths that agents never
hit in their own temp setups: stop, an expired login, a host that's late, a window that
shrinks. 14 issues are fixed on this branch, one small commit each; 3 P1s stay open (below).

## How it was tested
- **Setup:**
  - A host built from this tree, run as `falos-host serve` on port 7417 with its own
    `FALOS_HOME` and workspace. It had no fal key and spawned the agent runtime.
  - Vite on port 5185, viewed at 1280×800 and 900×600.
  - Real media copied from `~/fal-os-projects`: 3 images, 2 × 5 s videos and one 32 s mp3.
  - Every media element was muted and every AudioContext suspended, so nothing made a sound.
- **Input:** real keyboard and mouse events in the in-app browser. Where the browser tool
  couldn't produce an input (system clipboard, ⌥ held during a drag, OS file dialog), the
  same DOM events were dispatched synthetically: ClipboardEvent, DragEvent with real `File`s,
  PointerEvent with altKey, and the picker's `change`.
- **Agents:** the Codex driver ran for real (tiny prompts: reply, `sleep 20` + queue + stop,
  add a note + revert). An external MCP client was scripted (`session`, `report`, `inbox`).
  An app node was created with `create_app`. A generation was attempted (no key, as expected).
- **Robustness:**
  - The host was killed with `kill -9` mid-session and restarted, with offline edits made
    while it was down.
  - The host was stopped before a page load.
  - The runtime was killed with `kill -9`.
  - Two tabs edited one project.
  - Rapid actions: 50 × ⌘D, then 50 undos and 50 redos.
  - A selection of 614 nodes was dragged.
  - 3,450 wheel pan/zoom events ran over 60 s.
- **Checks:** `tsc -b`, the web vitest suite (22), the runtime tests (27), `falos-client`
  unit tests (19) and the host `ux` tests.

**Not covered** (and why):
- Real fal generations: the test host had no fal key. `generate` fails cleanly ("not signed
  in to fal"), so the LED tile, its progress and its failed state were not seen.
- The opus driver: the Claude Code OAuth session on this machine had expired. That led to
  fix #8.
- Live app iframes: the browser pane blocks the app port (`ERR_BLOCKED_BY_CLIENT`). The app
  itself served and ran fine in a tab of its own, and its still renders on the canvas.
- Desktop-only paths: Setup, drag-out to Finder, the native clipboard, the menu bar, and
  Figma paste with a real Figma clipboard (it's covered by unit tests only).

## Issues

Severity: **P0** broken · **P1** rough (wrong behaviour, data or work lost, misleading) ·
**P2** polish. "Fixed" names this branch's commit.

| # | Sev | Surface | Repro | Observed → expected | Cause (file:line) | Status |
|---|---|---|---|---|---|---|
| 1 | P0 | Canvas | Frame two images (⌘⌥G), then ⌘A, ⌫ (or select a frame plus one of its children, ⌫) | Toast `unknown node …`, **nothing deleted** → delete everything | ⌘A selected children too. Delete sent `DeleteNode` for a child already removed with its frame, which failed the whole batch (`crates/client/src/interact.rs` key_down Delete; `apps/web/src/actions.ts` deleteSelection; `Layers.tsx` ⌫) | Fixed `9a6926c` |
| 2 | P1 | Canvas | Same selection, ⇧→ (or drag it) | Frame children move **twice** (+20 instead of +10) → children follow the frame once | Nudge and move updated every selected id; the child's update was relative to the frame's old origin (`interact.rs` nudge and `Drag::Move` items) | Fixed `9a6926c` |
| 3 | P1 | Canvas | ⌘A on a canvas with a frame | Selects the frame **and** its contents (14 instead of 12) → the siblings at the current level (Figma) | `interact.rs` "a" selected every drawn node | Fixed `9a6926c` |
| 4 | P1 | Sync | Host down (or still starting) when the page loads or a project opens. Make a note or drop an image, then start the host | The canvas is editable, then the host's first snapshot **silently drops** everything made meanwhile → keep it | `apps/web/src/sync.ts` onSnapshot: first contact replaces the local scene | Fixed `ccf8815`: nodes created since the sync started are carried into the host's canvas, with a toast |
| 5 | P1 | Robustness | Any exception while rendering the chat (e.g. an unexpected event from an agent) | The **whole window goes blank**, canvas and GPU surface included → only that panel hides | Only the canvas layers had error boundaries (`App.tsx`, `Stage.tsx`) | Fixed `3bd01de`: Guard around chat, left panel, overlays, activity and status. Verified with an injected bad event |
| 6 | P1 | Chat | While a run is going, queue a message, then stop (Esc or [stop]) | The queued message **vanishes** (not sent, not in the input) → back into the composer | Host `chat.stop` dequeued with reason `cancelled` and the UI ignored it (`crates/host/src/chat.rs` chat.stop) | Fixed `07075e9` (reason `stopped`; the UI restores the text) |
| 7 | P1 | Chat | Stop a run | The turn ends with `· done` like a finished one → `· stopped` | `apps/web/src/chat.ts` transcript turn end | Fixed `07075e9` |
| 8 | P1 | Chat (opus) | Claude Code's login expired, send a message | `! the director stopped mid-step [continue]` within a second; continue can never work → say the login expired | `apps/agent-runtime/src/events.ts:364` scrubError mapped every SDK error result to that text | Fixed `d784bf1` |
| 9 | P1 | External agent tab | An external agent calls `report("…")`, then `inbox` | Its message is folded into the collapsed `▸ 1 tool call`: **the user never sees what it said** → stays visible | `chat.ts` groupTurns: text before more work counted as narration, even after `done` | Fixed `207956c` |
| 10 | P1 | Panels | Widen the chat and layers panels, then shrink the window to 900×600 | **Canvas 66 px wide** → the canvas keeps room | Widths clamped only at load and while dragging (`styles.css` `.chat`, `.layers`, `.library.panel`) | Fixed `dd0fabf`: panels flex-shrink to their minimums; the canvas keeps ≥ 240 px |
| 11 | P1 | Chat | Pick `codex` (e.g. because opus is broken), click `+` | The picker silently **switches back to opus** → keep the choice | The composer synced the driver from the new chat's host default (`ChatPanel.tsx` Composer effect) | Fixed `2a16901` |
| 12 | P2 | Chat | Click `+` twice | Two empty "New chat" tabs → reuse the empty one | `apps/web/src/keys.ts` newChat | Fixed `ede2122` |
| 13 | P2 | Canvas | N (or double-click), then Esc without typing | An empty 240×120 note stays → removed | `TextLayer.tsx` Editor blur | Fixed `34fd619` |
| 14 | P2 | Chat | Any chat with a long title | The placeholder `message <full title>…` wraps over 3 lines | `ChatPanel.tsx` placeholder | Fixed `44600b9` |
| 15 | P1 | External agent tab | Each new MCP session (every Claude Code / Codex restart, every script) | A new `ext:<client>-N` tab each time, with the same title. Dead sessions stay `running · [stop]` forever and **accept messages that queue and are never delivered** → one tab per agent; disconnected sessions marked as such, with the composer disabled | `crates/host/src/chat.rs:631` create_ext allocates a fresh tab per session. Nothing notices an HTTP MCP session going away | Open |
| 16 | P1 | Projects | Create a project, then press N as the empty canvas suggests | "n" is typed into the chat composer instead of making a note. The no-chat draft is shared across projects | `apps/web/src/projects.ts:322` focuses the composer on create. The draft key is `''` (`ChatPanel.tsx`) | Open (design call: focus the canvas, or change the hint) |
| 17 | P1 | Projects | Switch to a project with no cached snapshot while the host is slow or down | The **previous project's canvas** shows under the new name until the host answers | `sync.ts:677` switchProject keeps the old scene when nothing is cached | Open (new nodes made there now survive, via #4; edits to the shown nodes don't) |
| 18 | P2 | Connect | Open Connect from a dev or test host (other port or `FALOS_HOME`) and click `connect all` | It would write the **real** `~/.claude.json` / `~/.codex/config.toml` with that test host's URL and token, breaking the live connection. It shows "outdated" for the live one | `crates/host/src/connect.rs:101-117`: dev entries are HTTP URL + token, always into the user's home | Open (not clicked in this audit) |
| 19 | P2 | Canvas | Paste the same text, or ⌘V the same nodes, twice without moving the pointer | Copies land exactly on top of each other → offset the next paste | `actions.ts:203`, `:219` | Open |
| 20 | P2 | Canvas | Paste HTML with links | A plain-text note (links lost) | `actions.ts` paste, text branch | Open |
| 21 | P2 | Console | Import any file | A red "Failed to load resource: 404" per file, from the dedupe `HEAD` | `sync.ts:536` | Open |
| 22 | P2 | Media | Zoom into an audio tile (~130 %) | Waveform and name are a blurry, low-res texture | `apps/web/src/cardArt.ts:257` (card raster size) | Open |
| 23 | P2 | Chat | ⌘. without a fal key | Cycles into `fal`, which can't run (dim in the menu, but selectable) | `chatActions.ts:90` setDriver | Open |
| 24 | P2 | Chat | 4+ chats | The tab strip scrolls with no indicator; the first tab is cut off at the left | `styles.css:365` `.tabs` | Open |
| 25 | P2 | Host | A chat-turn revert | Loro logs `Missing in parent's children` twice (not reproduced in isolation) | Host revert path | Open, to investigate |
| 26 | P2 | Canvas | Right-click a node, Esc | One Esc closes the menu **and** deselects | keys / ContextMenu | Open |
| 27 | P2 | Layout | 900×600 with default panels | The canvas gets ~300 px; panels don't collapse by themselves | — | Open |
| 28 | P2 | Import | Open "import files…" and cancel | The hidden `<input type=file>` stays in the DOM | `controller.ts:170` pickFiles | Open |

**Counts:**
- P0: 1, fixed.
- P1: 13. 10 are fixed (#2–#11); 3 are open (#15–#17).
- P2: 14. 3 are fixed (#12–#14); 11 are open (#18–#28).

## Verdict per area

- **Canvas** (pan and zoom, select, marquee, move, resize, undo and redo, copy and paste,
  groups and frames, notes, shortcuts): **solid after the fixes.**
  - Every shortcut in SHORTCUTS.md that I drove behaved as documented: V/H, N, Tab/⇧Tab,
    ⌘D, ⌘G/⌘⌥G/⌘⇧G, ⌘]/⌘[, ⇧1/⇧2, ⌘0/⌘±, arrows, ⌘L, /, ?, ⌘K, ⌥-click/⌥-drag chips.
  - Undo and redo stayed exact through 50 rapid duplicates.
  - Dragging 614 nodes held 60 fps (≈17 ms frames, 11 ms commit).
  - 60 s of pan/zoom moved the JS heap from 36 to 37 MB.
  - The P0 (#1) was real and in the most basic path.
- **Import** (drop, paste of text, images and nodes, file picker): **solid.**
  - Uploads dedupe by sha; placeholders and progress lines work.
  - Polish left: stacked pastes and HTML.
- **Previews** (image, video, audio, library quick look): **solid.**
  - The video transport, J/L/K, the focus view with ←/→ and the audio preview all work.
  - Audio tiles look blurry up close.
  - App nodes: the host side (serving, headless check, still) works. The live iframe couldn't
    be checked here.
- **Chat panel:** **rough.**
  - Sending, queueing, stop, revert/redo, history, delete with `[y/n]`, @-mentions, many chips
    (a capped strip) and long messages work with Codex.
  - The failure paths were the weak spot (#6–#8, #11) and are fixed.
- **External-agent tab:** **half-baked.**
  - Two-way messaging works: `inbox` got the user's message instantly.
  - Before #9, the agent's reports were effectively invisible.
  - The tab-per-session model (#15) means stale tabs pile up and swallow messages.
- **Library ⌘⇧L / ⌘⇧A, export ⌘⇧E:** **solid.**
  - Fast; filters, keyboard and quick look work.
  - Export wrote a file and toasted its path.
- **Connect:** **OK.** The UI is clear. Hazard #18 is for anyone running a second host.
- **Setup:** not testable in the browser.
- **Projects, panels, persistence:** **mostly solid.**
  - Create, switch and the delete prompt work.
  - Reloads bring back nodes, chats, camera, panel widths and the driver.
  - Two tabs sync live.
  - Open rough edges: #16 and #17.
- **Robustness:** **good, now.**
  - A `kill -9` of the host mid-session reconnects cleanly, and offline edits (a note, a move,
    a dropped image) merge.
  - The runtime is respawned when killed.
  - The host restarting on the same port gets picked up.
  - The two real holes, #4 and #5, are fixed.
  - With the error boundaries, a bad chat event no longer takes the app down.

## What still worries me, most important first

1. **External-agent tabs (#15).** This is the director workflow the latest commits push. It
   needs one tab per agent identity, a disconnected state, and a composer that refuses
   messages nobody will read.
2. **Opus untested end to end in this pass.** The login on this machine had expired. A real
   Claude run (with a fal key) should go through the same stop / queue / revert checks
   before a demo.
3. **Project switch while the host is slow (#17)**, and the composer stealing N on a new
   project (#16).
4. **Real-device paths not exercised here:** drag-out to Finder, the native clipboard, the
   live app iframe, and generation tiles.
