# Stability and polish audit (2026-10-14)

A hands-on pass over the web app at `a95d21a` ("Director agents…"), driven like a user in a
browser, against its own host. The question was "is this half-baked or good?".

**Short answer:** the core is better than "half-baked". Canvas, import, previews, library,
export, sync and host restarts held up under real use. The chat panel and the external-agent
tab are the rough parts. One P0 sat in the most basic path: ⌘A then ⌫ did nothing on any
canvas with a frame. Besides it, 13 P1s showed up, mostly in failure paths that agents never
hit in their own temp setups: stop, an expired login, a host that's late, a window that
shrinks. 14 issues were fixed in the first pass, one small commit each; a second pass fixed
the 3 open P1s and 9 of the P2s. Two P2s stay open (below).

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
| 15 | P1 | External agent tab | Each new MCP session (every Claude Code / Codex restart, every script) | A new `ext:<client>-N` tab each time, with the same title. Dead sessions stay `running · [stop]` forever and **accept messages that queue and are never delivered** → one tab per agent; disconnected sessions marked as such, with the composer disabled | `crates/host/src/chat.rs:631` create_ext allocates a fresh tab per session. Nothing notices an HTTP MCP session going away | Fixed `69840a1`, `b4103db`: one tab per agent identity (`ext:<client>[-<name>]`); a new session reattaches and the history continues; running → idle → detached (connection closed, no session after a restart, or 10 min silent); messages wait for the next session ("waits for next session") and are delivered on reattach; tabs close; old duplicates merge on load. Verified with real Codex runs |
| 16 | P1 | Projects | Create a project, then press N as the empty canvas suggests | "n" is typed into the chat composer instead of making a note. The no-chat draft is shared across projects | `apps/web/src/projects.ts:322` focuses the composer on create. The draft key is `''` (`ChatPanel.tsx`) | Fixed `fe4b5e3`: the canvas gets the keyboard (N makes a note); no-chat drafts are per project |
| 17 | P1 | Projects | Switch to a project with no cached snapshot while the host is slow or down | The **previous project's canvas** shows under the new name until the host answers | `sync.ts:677` switchProject keeps the old scene when nothing is cached | Fixed `0c1a188`: an empty canvas (no hint) until the host answers |
| 18 | P2 | Connect | Open Connect from a dev or test host (other port or `FALOS_HOME`) and click `connect all` | It would write the **real** `~/.claude.json` / `~/.codex/config.toml` with that test host's URL and token, breaking the live connection. It shows "outdated" for the live one | `crates/host/src/connect.rs:101-117`: dev entries are HTTP URL + token, always into the user's home | Fixed `eac3840`: a host that isn't the installed app writes `fal-os-dev` beside `fal-os` (and no skill), and Connect says why |
| 19 | P2 | Canvas | Paste the same text, or ⌘V the same nodes, twice without moving the pointer | Copies land exactly on top of each other → offset the next paste | `actions.ts:203`, `:219` | Fixed `09db4ef`: repeats cascade by 20 px |
| 20 | P2 | Canvas | Paste HTML with links | A plain-text note (links lost) | `actions.ts` paste, text branch | Fixed `09db4ef`: links kept as `label (url)` |
| 21 | P2 | Console | Import any file | A red "Failed to load resource: 404" per file, from the dedupe `HEAD` | `sync.ts:536` | Fixed `a94a470`: `HEAD …?probe=1` answers 204 when missing |
| 22 | P2 | Media | Zoom into an audio tile (~130 %) | Waveform and name are a blurry, low-res texture | `apps/web/src/cardArt.ts:257` (card raster size) | Fixed `28ce631`: audio cards raster at ≥ 3 px per unit (up to 2048) |
| 23 | P2 | Chat | ⌘. without a fal key | Cycles into `fal`, which can't run (dim in the menu, but selectable) | `chatActions.ts:90` setDriver | Fixed `ef5f9d5`: ⌘. skips unavailable drivers (the picker still offers them) |
| 24 | P2 | Chat | 4+ chats | The tab strip scrolls with no indicator; the first tab is cut off at the left | `styles.css:365` `.tabs` | Fixed `8c240ff`: the strip fades at an edge with more tabs past it |
| 25 | P2 | Host | A chat-turn revert | Loro logs `Missing in parent's children` twice (not reproduced in isolation) | Host revert path | Open, to investigate (not reproduced) |
| 26 | P2 | Canvas | Right-click a node, Esc | One Esc closes the menu **and** deselects | keys / ContextMenu | Fixed `8ef0750` |
| 27 | P2 | Layout | 900×600 with default panels | The canvas gets ~300 px; panels don't collapse by themselves | — | Open (design call: auto-collapse rules) |
| 28 | P2 | Import | Open "import files…" and cancel | The hidden `<input type=file>` stays in the DOM | `controller.ts:170` pickFiles | Fixed `d2d673c`: removed on `cancel` (and before the next picker) |

**Counts:**
- P0: 1, fixed.
- P1: 13, all fixed (#15–#17 in a second pass, see below).
- P2: 14. 12 are fixed; 2 are open: #25 (a Loro warning, not reproduced) and #27 (panels
  don't collapse by themselves at 900×600: a design call).

**Second pass (same day):** #15–#24, #26, #28. Checked with `cargo test -p falos-host`
(new: `external_agent_tab_detaches_and_reattaches_with_messages`,
`duplicate_external_tabs_from_older_hosts_merge`, `a_dev_host_registers_beside_the_real_entry`),
`tsc -b`, vitest, and in the browser against a temp host (port 7447, own `FALOS_HOME`, detach
timeout 90 s) with the bundled Codex 0.160.1 connected through `falos-host connect --home <tmp>`
(which wrote `fal-os-dev`):
- Codex run 1 (`session` + `report`) opened one "Codex" tab, which went detached (○) the moment
  Codex quit.
- A message typed into the detached tab showed "waits for next session: …".
- Codex run 2 reattached to the same tab, got the message in its `session` result, reported
  "blue" and detached again: one tab, one log.
- A scripted session that went silent showed working → attached · idle (30 s) → detached (90 s);
  three messages queued meanwhile; its next `inbox` call attached it again and got all three.
- Closing the tab while the agent works keeps it closed; the agent's next session reopens it.
- After a host restart the tab loads detached, without a stale "running".
- A new project from the switcher: N made a note. Switching to an uncached project with the host
  stopped (SIGSTOP) showed an empty canvas with no hint, then the project's note when it resumed.
- Connect on the temp host shows "this host isn't the installed fal OS (a dev build ·
  FALOS_HOME=… · port 7447): it registers as fal-os-dev". Not clicked (it would write the real
  home's configs).
- Pastes cascaded (0,0 → 20,20 → 40,40) and kept a link; Esc on an open context menu kept the
  selection; the dedupe probe answers 204.

Not re-checked in the browser: the audio tile at zoom (#22), the tab-strip fade (#24) and the
picker cancel (#28; a `cancel` event, Chrome 113+ / Safari 16.4+).
`projects_are_isolated_sync_assets_and_mcp` (tests/workspace.rs:284) fails, already at
`038078f`: the most-recent-project default after `create_project`. Not related to these fixes.

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
- **External-agent tab:** **solid after the second pass.**
  - Two-way messaging works: `inbox` got the user's message instantly.
  - Before #9, the agent's reports were effectively invisible.
  - One tab per agent; it knows when the agent is gone and keeps messages for its next session
    (#15).
- **Library ⌘⇧L / ⌘⇧A, export ⌘⇧E:** **solid.**
  - Fast; filters, keyboard and quick look work.
  - Export wrote a file and toasted its path.
- **Connect:** **OK.** The UI is clear. A second host registers as `fal-os-dev` (#18).
- **Setup:** not testable in the browser.
- **Projects, panels, persistence:** **mostly solid.**
  - Create, switch and the delete prompt work.
  - Reloads bring back nodes, chats, camera, panel widths and the driver.
  - Two tabs sync live.
  - #16 and #17 are fixed.
- **Robustness:** **good, now.**
  - A `kill -9` of the host mid-session reconnects cleanly, and offline edits (a note, a move,
    a dropped image) merge.
  - The runtime is respawned when killed.
  - The host restarting on the same port gets picked up.
  - The two real holes, #4 and #5, are fixed.
  - With the error boundaries, a bad chat event no longer takes the app down.

## What still worries me, most important first

1. **Opus untested end to end in this pass.** The login on this machine had expired. A real
   Claude run (with a fal key) should go through the same stop / queue / revert checks
   before a demo. Claude Code as an external agent was not run for real either; its tab logic
   is the same as Codex's (client `claude`).
2. **Silent external agents.** An agent that is alive but makes no fal-os call for 10 min
   shows as detached; its next call attaches it again and takes the waiting messages, but the
   tab said "waits for next session" meanwhile. There's no transport heartbeat to tell
   "quiet" from "gone" for HTTP clients that never close their MCP session.
3. **Real-device paths not exercised here:** drag-out to Finder, the native clipboard, the
   live app iframe, and generation tiles.

## P1 canvas feel and agent polish (follow-up)

From the recommendations in `docs/research/paper-0515.md` (canvas interaction) and
`docs/research/brilliant-b102.md` (§9–12). What changed:

- **Snapping and guides** (`crates/client/src/snap.rs`, engine overlay): edges, centres and
  equal spacing within 4 screen px (spacing 1.5×); resize snaps only the moving edges; ⌘/Ctrl
  moves freely; 1 px `--dim` guides, gaps with end ticks; guides stay ~850 ms after a nudge;
  ⌥ over another node shows distances (insets inside a frame). All in Rust; the only DOM work
  is the ⌥ labels, written directly (no React render).
- **Precision:** threshold 3 px, 1 px after a 500 ms hold; ⇧ axis lock; edges resize along their
  length; no handles under 16 px on screen; Space moves the box mid-resize; Esc puts a resize
  back and keeps the selection; ⌘D repeats the last offset.
- **Undo and keys:** each undo step carries the selection before and after it (kept in step
  with Loro's per-actor undo depth); the Esc ladder; ⏎ / ⇧⏎ walk children / parents (⏎ still
  previews media); `/ ? [ ] \` also match the US physical key.
- **Nothing lingers:** a host watchdog fails generating tiles with no live job after 60 s
  (`interrupted … rerun it`, an `Event::Job` failed goes out); `hello` carries jobs in flight;
  the UI watchdog (2 s) drops activity of gone sessions and past TTL, job progress of tiles that
  stopped generating, claims on deleted nodes; a lost socket clears claims and activity; tabs
  show no `*` while the host is unreachable.
- **Progress and errors:** slow-only progress (paste, import, project switch, export), never a
  done line over an error; one-sentence errors with one next step and `[details]`; one automatic
  retry (a live transient run failure; fal submits that never reached fal, status/result polls);
  held messages behind walls.

**How it was checked:** `cargo test -p falos-client -p falos-doc -p falos-host` (new: snap math,
drag/resize/Esc/Enter/nudge interaction tests, `tests/watchdog.rs`), `tsc -b`, vitest (new:
`hotkeys`, `watchdog`, `errors`). In the browser against a temp host (port 7463, own
`FALOS_HOME` and workspace, `--no-runtime`) and vite on 5193, media muted:

- A real mouse drag of a note landed exactly aligned (0,250 from ~2 px off); ⌘Z put it back with
  its selection. ⌫ on two notes, Esc, ⌘Z: both back and selected.
- Mid-drag (driven through the wasm API) the equal-spacing and alignment guides drew; an edge
  resize snapped to the neighbour's edge; Space mid-resize moved the box at its size.
- Nudges showed the alignment guides for under a second; ⌥ over a node showed `140`, inside a
  frame the four insets.
- ⏎ on a frame → its children, Esc → the frame, Esc → nothing; ⏎ on a note edited it, Esc left
  the editor, ⇧⏎ → the frame.
- Synthetic German keys: ⌘ü (`BracketRight`) reordered, `-` on `Slash` zoomed out, ⇧7 `/`
  opened the ask prompt.
- ⌘D, move the copy 150 down, ⌘D ⌘D: a column at the same spacing.
- A ghost session's activity, a stray job and a claim on a deleted node were swept within 2–7 s;
  a generation node with no job turned `failed` after ~65 s and its hover read
  `interrupted by a host restart · rerun it`.
- Host killed: a sent message showed `held · host unreachable · sends when it is back`; on
  restart it went out by itself within ~1.5 s. With the host SIGSTOPped a project switch showed
  `opening audit two…` after 400 ms; it vanished when the host resumed.
- Error lines rendered as `! fal key missing · [open setup] [details]` and so on; `[details]`
  showed the raw text.

**Not covered:** a real fal generation failing transiently (no key on the test host), real
Claude/Codex login walls clearing through the desktop setup flow (web only here), and a real
⇧/⌘ held mid-drag with the in-app browser's mouse (the logic is unit-tested; modifier changes
re-evaluate the drag through `pointerMove`).
