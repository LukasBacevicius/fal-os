# Brilliant 1.0.0-beta.102: stability, polish and UX direction

Static teardown of `brilliant-1.0.0-beta.102-Installer.dmg`: a plain drag-to-Applications DMG, `design.brilliant`, `CFBundleVersion 1.0.0.102`, built with Xcode 27 / SDK macOS 27, min macOS 14. Nothing was executed.

**Sources**
- **[K]** Brilliant's bundled agent knowledge (`Resources/.claude-prod/knowledge/reference/**`, 345 files), cited by key, e.g. [K canvases/auto-save].
- **[D]** Dart source paths recovered from the AOT snapshot (930 `package:brilliant/…` URIs), cited as e.g. [D managers/boot_watchdog].
- **[S]** user-facing and log strings in `App.framework/App` and `libbrilliant_native.dylib`.
- **[A]** bundle assets (Info.plist, `flutter_assets`, `motion/`, `playground/*.jsonl`).

Read with [renderer](brilliant-renderer.md), [document model](brilliant-document-model.md), [agents](brilliant-agents.md), [canvas interaction](brilliant-canvas-interaction.md) and [chat UX](brilliant-chat-ux.md). This doc covers only what those miss.

## 0. What changed since our last teardown
- **It is the same build.** The earlier docs were also written against beta.102, so this DMG has no product delta. The new material comes from areas we hadn't read: stability engineering, notifications, onboarding, updater, telemetry, media/import, agent error handling and MCP binding details.
- **Corrections and additions to earlier findings:**
  - **MCP binding.** Each session binds to the project **on screen** by default. `init(project)` rebinds the whole session; every canvas tool also takes `project` for a single call. Unknown or ambiguous names are refused with the candidates listed. A project that isn't open is **opened in the background**: the user is told an agent is working there and offered to open it [A agent.md "Working across projects"]. Some tools refuse with "acts on the project open on screen … ask the user to open it" [S].
  - **Claude Desktop is not supported.** Brilliant only registers remote URLs, and Claude Desktop needs stdio [K mcp-connections/enabling]. We already cover it via our stdio relay.
  - **Port behaviour.** After sleep/wake the server rebinds the **same** port. If all of 3333–3335 are taken, a port-unavailable notice with Retry appears [K mcp-connections/server].
  - **Onboarding demo.** It replays recorded sessions: `assets/playground/*.jsonl` are normalized stream events (`user_message`, `thinking_delta`, `tool_input_delta`, `tool_result`, `usage`…) with timestamps, recorded on `claude-opus-5-5` [A].
  - **Telemetry and crash reporting are in-house.** There is no Sentry, PostHog or Crashlytics. Brilliant posts to its own `/events`, `/feedback` and `/feedback/crash-dump` endpoints [S].

## 1. Canvas interaction and motion (new beyond the interaction spec)
- **Input tuning.** Pan speed and zoom speed are settings (100% default; 50–200 from the dropdown, 25–400 typed). They scale trackpad pan (including its momentum) and pinch / modifier-scroll zoom. Space and middle-mouse drags stay 1:1, and there's an "Invert zoom direction" toggle for the mouse wheel [K ui/window-modes].
- **Canvas switch never flashes.**
  - A fast switch shows nothing.
  - Only when the first paint is late does the canvas area *dim with a subtle shimmer* (rather than keep showing the old canvas), clearing the instant content is up [K canvases/managing].
  - A slow project switch raises "Switching to {project}…", which clears itself on landing or is replaced by the failure [K ui/top-toolbar].
  - Engine side: a first-visit fit hold times out at **1500 ms**: *"a pathological load must never blank the canvas"* [S].
- **Only slow operations show progress.**
  - "Pasting…" with a progress bar and its stages ("Fetching images…", "Uploading assets…") appears only if the paste takes real time, and ends with a brief "Pasted".
  - An instant paste shows nothing, and a failed one shows only its error, never "Pasted" on top.
  - The stated goal: Cmd+V never looks like it did nothing, so nobody presses it twice [K ui/top-toolbar; D managers/paste_progress_controller].
- **Agents only read committed geometry.** "Reads report committed geometry; a gesture in progress commits when it ends" [A agent.md], so an agent never sees a half-dragged node.
- **Detailed hover highlight** (opt-in): outlines the hovered frame's children (dashed) and its parent (dotted) [K ui/window-modes].
- **Window frame persistence** [K ui/window-modes]:
  - The frame is saved as you move/resize and again on quit.
  - Changes in the first moments of a launch are ignored, so tiling window managers (yabai, AeroSpace) don't overwrite it.
  - A frame on a missing or resized display is pulled back inside a live work area.
  - Fullscreen is never persisted.
  - First run opens at about 80% of the work area, capped at 1400×900; the minimum is 800×600. A "Center Window on Screen" command exists for a lost window.
- **Clipboard details** [K editing/clipboard, paste]:
  - Cut copies first and only then removes.
  - If the system clipboard write fails, a toast says so and the internal copy still pastes.
  - Copying one plain image writes its raw bytes.
  - Paste checks in order: internal if the system clipboard is unchanged, then image, SVG, Figma, Blueprint, HTML, plain text.
- **Generation and loading visuals** are shimmer/sweep painters (`_ShimmerSweepPainter`, `RecordingShimmerPainter`) [S]. We replaced these with LED tiles. The pattern worth keeping is the **shimmer watchdog** (`_ensureShimmerWatchdogRunning`, `_finalizePersistentShimmerRemoval`) [S]: an activity indicator can never stay stuck after its owner dies. The tab shimmer "clears the moment the agent finishes; nothing lingers" [K ui/top-toolbar].

## 2. Agent UX
**Errors are named, with one next step** [K ai/errors]:
- The card names the provider and the cause in plain words, gives one action button, and keeps the raw text behind "Details". For example: "Gemini rejected the request (invalid API key). Update the key in Settings to continue." with [Update API key].
- **Automatic recovery:**
  - A connection that drops before the reply starts is retried once, with a visible row saying so.
  - A cut-off reply continues by itself up to 3 times ("Reply cut off; continuing…"). The 4th cut ends the turn.
  - A fault in how Brilliant built the request retries once silently ("Something went wrong on our side. Retrying…").
- **Busy upstream:** an in-place waiting row ("…not responding on Google side right now; Brilliant is fine, retrying (5 s)"). When the wait budget runs out it becomes a stop card with [Send again] and the real wait ("Send again in 28 s").
- **Circuit breakers name the cause,** e.g. "made 4 tool calls in a row that did nothing, so the turn was stopped. Send again to continue."
- **Failed tool rows** settle to their outcome ("Image 2 of 3 could not be generated (busy). The other 2 are on the canvas.").
- **Transcript persistence failures** are told once, in words: "Chat history is not being saved to disk (disk full)…".

**Held messages, never lost** [K ai/limits]:
- A send blocked by a wall (sign-in, quota, missing key) is **held**. It continues by itself once the wall clears, with a single line saying so ("Signed in. Picking up where we left off.").
- Dismissing the wall puts the message back in the composer as an editable draft.
- Running out of quota mid-answer: the turn in flight finishes first.

**Self-review loop** [K ai/context]:
- After each edit block the agent gets that block's result and a screenshot of just the changed elements.
- After **two blocks in a row that change nothing**, Brilliant asks the agent to wrap up.
- A selection of more than 1,000 elements reaches the model as a summary: counts, bounds, the first 100 ids.

**Presence labels.** The agent emits `<agent>Meditation App</agent>` (1–3 words) for its cursor label, plus a separate `<title>` for the chat. Both tags are stripped from the transcript [A agent.md].

**External agent onboarding** [K mcp-connections/*, ai-setup/*]:
- **Config file is the truth.**
  - There is one flat list of clients with checkboxes, and no install detection: you can enable a client that isn't installed yet.
  - Brilliant rewrites the entry to follow the current port on every start. It auto-adds to Codex only if `~/.codex` exists.
- **Expectations are set up front:**
  - Restart the tool after connecting.
  - Then "nothing happens until a chat starts", because Cursor and Codex spawn MCP servers on the first message. The checklist ticks its last step on that first call.
  - The web editor polls for about 10 minutes, then offers "Show the command again".
- **CLI detection for in-app chats:**
  - Runs under the user's **login shell** environment (PATH, proxy, CA), with a 15 s probe timeout and `BRILLIANT_{CLAUDE,CODEX,CURSOR_AGENT}_PATH` overrides.
  - Re-checks the first time you click into the chat.
  - Status rows are precise: "Installed · login unverified, chat still works", "Signed in · plan not eligible", "Not connected" (CLI missing) vs "Not signed in".
  - On failure it names the likely cause (proxy, certificate, permission, sign-in, outdated) and offers Retry.
  - Cursor errors separate "could not launch", "version mismatch" and "timeout" from a login problem.
- **Copy element id** (context menu and a command, with a "Copied element id" toast) puts the id on the clipboard to paste into a Claude Code prompt [K ui/context-menus].
- **Share links** carry the selection *and* the typed inline prompt: the recipient lands with the same selection and draft [K ai/input-bar].

**Feedback from agents** [K feedback/*, skills/feedback]:
- `send_feedback` only *opens* a pre-filled card; only the user's click sends it.
- An agent may offer feedback at most once per session and once per class (hit a wall, missing capability, frustration), never mid-gesture.
- Context (logs, screenshots) is opt-in per item at the draft preview.
- Reports are capped at a few per day, and the draft is kept when the cap is hit.

## 3. Media handling
Brilliant has no video, audio or 3D nodes; images are paints. So this section is about patterns.

- **Formats** [K canvases/import, previews]:
  - PNG, JPEG, GIF, BMP and WebP everywhere; TIFF, HEIC, HEIF and AVIF through the OS codecs (NSImage). SVG is converted to native vectors.
  - Unknown files are *sniffed*: text opens in the editor, binary shows an "unsupported" placeholder.
  - Text over 5 MB opens read-only behind a banner.
  - EXIF orientation is honoured (`_findExifInJpeg`, `_buildHttpOrientation`).
  - Images sent inline to an agent are downscaled to fit the inline budget (`autoDownscaleForInlineBudget`, "served downscaled to …") [S].
- **Asset hygiene** [K canvases/previews, recovery]:
  - Renaming updates every reference.
  - Deleting warns if the asset is in use and moves it to `.brilliant/trash/` (undoable).
  - "Clean Up Unused Assets" is undoable.
  - The file watcher reflects external changes within about 1 s; a `git checkout` burst applies as one batch; regaining focus triggers a rescan.
- **Non-destructive operations with honest outcomes** (remove background) [K ai/images]:
  - "No subject found in Texture." and nothing is charged.
  - "…was not applied: the image changed." when the input changed mid-run.
  - "2 hidden or locked layers skipped."
  - "You're offline…"
  - "Restore Background" works in any later session, not only through undo.
- **Uploads count as saves.** The save cue stays dim until every image has reached the cloud. A failed upload is a sync error that names the file. Uploads resume on the next launch, and a dropped connection re-sends by itself [K canvases/collaboration, ui/top-toolbar].
- **Drag/drop/paste:** drop onto the canvas or the explorer; `desktop_drop` and `super_native_extensions` are present [A Frameworks]. Paste lands at the cursor and import at the viewport centre (already in the interaction spec).

## 4. Stability engineering

### 4.1 Autosave, crash recovery and data safety
**Autosave basics** [K canvases/auto-save; S "the 500ms auto-save debounce caps … 120 writes/min"]:
- About 500 ms debounce, written off the main thread, no Cmd+S.
- The dirty cue is the canvas name's opacity.

**When a save fails:**
- The canvas stays queued and retries on a widening interval.
- A held notice says the edits haven't reached disk yet.
- Nothing is ever shown as saved when it isn't.
- Quit and workspace switch flush every dirty canvas, including ones whose earlier save failed.

**A failed load blocks saves** on that canvas, so good data is never overwritten with an empty state [K; S `save_arm_refused_load_failed`].

**Partial-load recovery:**
- A file with a few damaged lines opens without them, with a banner saying how many lines and which.
- Autosave **pauses** for that file and the original is never touched.
- Choices: "Save repaired copy" (written next to the original) or Dismiss.
- A file that is mostly unreadable is treated as unreadable: saves blocked, original kept.

**Truncation detection** [K editor-open-failures/one-canvas]:
- "The end of this file is missing" pauses autosave and offers "Restore from checkpoint" / "Save a copy of the damaged file".
- If a good local copy exists, it is restored and the damaged file moved to `.archive/`.

**Crash-orphaned temp files are adopted:** "Adopted crash-orphaned save temp for canvas …". A `save_episode_open.json` marker lets the next launch tell that a save was cut off mid-way [S].

**Save-collapse guard** (`save_collapse_guard`, `save_collapse_resolved` [S]):
- A save that would drop most of a canvas is refused unless it was intended.
- If a synced canvas rewritten on disk "loses much of its content", the user chooses "Keep disk version / Restore synced version". The replaced version goes to the recovery folder first [K canvases/previews].

**External edits are reconciled, never clobbered:**
- With no unsaved edits, reload silently (selection and camera kept) and show a passive notice.
- With unsaved edits, pause autosave and ask **Reload / Keep mine**.
- Reload first copies unsaved work to the recovery folder.

**Nothing is hard-deleted.**
- Deletes go to `.brilliant/trash/` with an undo entry.
- The explorer has its own undo stack.
- Deleting the last canvas creates an empty one.

**Quit:**
- Quit runs through ordered phases with a ledger: `quit_phases`, `quit_phase_ledger`, `quit_data_guard`, `quit_flush_legs`, plus telemetry `quit_unsaved` and `quit_total_ms`.
- Child agent processes are reaped (`child_process_reaper`, `process_tree_kill`) and a single instance is enforced (`instance_lock`) [D].

### 4.2 Startup, renderer and UI watchdogs
**Boot watchdog** [D managers/boot_watchdog, boot_trace, launch_readiness; S]:
- Each boot stage has a stall budget: `BRILLIANT_BOOT_STAGE_STALL`, `BOOT_FIRST_PAINT_MS`, `BOOT_WINDOW_UP_MS`.
- If first paint overruns, a **degraded window is forced up** ("first-paint watchdog overran, degraded window forced up (stalled at …)") instead of the app hanging invisibly.
- The card "Still getting the window ready" offers **Retry** and **Open without last canvas** (also an automatic "boot repair") [K editor-open-failures/desktop-startup].

**Renderer crash-loop guard:**
- After the renderer crashes several times in a row, or on a known-bad driver, Brilliant doesn't start it automatically: "paused starting it so you are not stuck in the same crash. Your work is safe." A **Try anyway** button starts it for this session only.

**Device loss** [K editor-open-failures/desktop]:
- "Restoring the canvas…" keeps the **last frame on screen** while the renderer recovers.
- Three resets, each within 30 minutes of the last, stop automatic restarts for the session. The block clears after an hour without a reset, a driver update, or a week.

**Engine health checks** [S native]:
- `bn_health_v1` reports `LOCK_WEDGED` if it can't take the engine lock within its time budget.
- The render thread that "did not park within budget" is leaked rather than hanging the app.
- A downscale watchdog force-restores full resolution.
- Shader pause uses a wall-time budget with backoff.

**UI stall watchdog on a separate isolate** (`ui_stall_watchdog`, `BRILLIANT_UI_STALL_FELT_MS`, `_TICK_MS`):
- Records `ui_stall stalled_ms` into a **stall journal** (`stall_journal_io`).
- Kept "flight reports" upload later [S].
- `canvas_paint_watchdog` emits `canvas_blank_detected`; `churn_watchdog` and `sync_jam_watchdog` also exist.

**Failure cards** [K editor-open-failures/*]:
- **Every failure card names what happened;** the canvas is never blank.
- "Your work is safe" is a promise: nothing is saved over the file while the card is up.
- **Report** copies a short private diagnostics block (version, system, GPU backend, reason).
- **Send logs** sends the renderer's recent logs (no designs, no file names), once per failure.
- When an update is ready, the card's *primary* button becomes **Update to <version>** [K ui/top-toolbar].

### 4.3 Document versioning and migrations
- The file header carries the format version (`bp:v1`).
- Legacy YAML content migrates to Blueprint **on its next save**. Old extensions are renamed, with the originals kept under `.archive/` [K canvases/workspaces].
- **Files written by a newer version** [K canvases/auto-save; S]:
  - They open with every element visible.
  - Unknown properties are preserved in the file and autosave is paused.
  - "Save repaired copy" strips the unknown properties only on request.
  - A file that can't be opened says "written by a newer version of Brilliant. Update to open it."
- **Migration modules exist per domain:** `legacy_migration`, `extension_migration`, `component_ref_migration`, `dsl/startup_migration`, `chat_migration` [D].
- Settings migrations are logged by name, e.g. "Thinking default: migrating the pre-update 'medium' default to 'off'" [S].

### 4.4 Sync and reconnect
We deliberately avoid Brilliant's two sync lanes; these are the UX rules worth keeping.
- **Sync health is reported only when something is wrong** [K ui/top-toolbar]:
  - There is no standing sync indicator.
  - On trouble, *one* notification per project is raised, updated in place and never stacked.
  - It **cannot be dismissed while the failure lasts**, and the dot is red exactly as long as sync is broken.
  - On recovery it flips to "Back in sync" and fades.
- **Server backpressure:** "The server asked Brilliant to slow down" pauses all requests together, then resumes. Saves made meanwhile go out right after.
- **Losing write access is its own state.** "Changes are safe on this device"; no re-login loop and no retry spinner that can't succeed.
- **Live session** [K canvases/collaboration]:
  - When the connection drops, editing continues and saves go direct.
  - "Catching up on history" is reassuring and clears itself.
  - If the canvas you're on is deleted remotely, you land on another canvas with a notice.
- **Conflict noise is suppressed for one user** [K canvases/collaboration]:
  - A solo owner never sees conflict notices.
  - The same account on two machines converges quietly.
  - Checkpoints and notices are reserved for merges involving someone else's work.
- **Offline local edits** are kept in a recovery folder if the cloud moved on.
- **A restore wins,** but the displaced local bytes go to sync trash with a notice naming the file.
- **Presence cap** of 25 cursors; past it your cursor is hidden for others, with a notice.

### 4.5 Updater, logging and telemetry
**Updater** [D managers/update_manager; S; K ui/top-toolbar]:
- Checks run in the background for the whole session (`checkForUpdatesIfNeeded`, `LastUpdateCheckTime`), so an app left open for days still gets the notification.
- The update is a CTA notification with **Update** / **What's new**, shows progress in place, and names a newer version if one ships while it's up.
- **Install steps:**
  - The download is verified against its checksum ("Checksum valid! Installing update"; "did not match expected length and checksum").
  - The staged bundle is verified with `codesign --deep --strict`, then the quarantine attribute is stripped.
  - It reuses a downloaded installer, and the UI promises "Installs and relaunches in about 15 seconds".
  - On failure it says so on the next launch: "<version> did not install - your current version kept running. Try again."
- A "Disable Update Checks" command exists.
- Our [PACKAGING.md](../PACKAGING.md) already records the DMG / `ditto` / `RENAME_SWAP` flow.

**Logging:**
- The log lives at `~/.config/brilliant/logs/brilliant.log`, with classed log lines (`classed_log_line`, `log_file_sink`).
- Startup probes log one line each (e.g. `chrome_font_probe`).
- A notification button opens the logs folder [K ui/top-toolbar; D].

**Telemetry** [S; D managers/analytics_manager, telemetry_spool]:
- Anonymous telemetry is always on; "detailed analytics" tied to the account is opt-in.
- Events go to a **persisted outbox** (`analytics.outbox.v1`, durable spool replay) and labels are hashed with an install salt (`analytics.labelSalt.v1`).
- `BRILLIANT_TELEMETRY_MUTE` turns it off.
- The event set is a useful reference:
  - timings: `first_paint_ms`, `canvas_load_ms`, `save_ms`, `first_token_ms`, `mcp_tool_ms`, `quit_total_ms`, `ui_stalled_ms`;
  - activation funnel: `first_canvas`, `first_ai_prompt`, `first_element_created`, `first_export`, `first_save`;
  - failures: `crash_loop`, `engine_degraded_open`, `save_collapse_guard`, `save_failed`, `canvas_blank_detected`, `sync_jam`.

**Crash reports:**
- The next launch asks **once** per crash ("Send a crash report?"): a technical snapshot with no document content. Declining is final for that crash.
- It detects injected mods ("These mods patch other apps' code and are a known cause of this kind of crash").
- After a rough session it offers a calm "Something went wrong last time?" at most once per release [K feedback/facts; S].

### 4.6 Performance budgets (stated in strings)
| Budget | Value | Evidence |
|---|---|---|
| Autosave debounce | 500 ms (≤120 writes/min per canvas) | [S] |
| Thumbnail producer debounce | 5 s (≤12 renders/min; 0 for an unchanged canvas) | [S] |
| First-visit fit hold | 1500 ms, then present anyway | [S] |
| MCP handshake / provider connect | 10 s / 30 s | [S] |
| CLI turn settle after interrupt | 10 s, then force-recycle; previous child exit wait 12 s | [S] |
| CLI detection probe | 15 s | [S] |
| Settings sync | "a couple of seconds after an edit" and on focus | [K ui/window-modes] |
| Inline `/` double-press | 2 s; explorer `gg` 500 ms; nudge coalesce 1 s | [K bottom-toolbar, explorer, undoable] |
| "Copied!" tooltip | 1 s | [K ui/top-toolbar] |

Every optimization still has a kill-switch env var (`BRILLIANT_DISABLE_*`, 108 `BN_*`).

## 5. Onboarding
- **No macOS permission prompts at all.** Info.plist has no `NS*UsageDescription` keys. Accessibility is requested only for the opt-in overlay global hotkey [A Info.plist; K ui/window-modes].
- **No sign-in before value.** The first messages send on included usage without an account. The sign-in wall *holds* the prompt and continues it after sign-in [K ai/limits].
- **First run** [K ui/web-platform, ai-setup/demo-mode]:
  - One guided step: pick a starter prompt and watch a **recorded** session build real, editable elements, costing nothing.
  - When it finishes, the canvas **glides to centre the result**.
  - There is no skip button, but ⌘N leaves the onboarding.
  - Using the inline prompt suppresses it for good. "Reset onboarding" re-arms it.
- **Empty and home states** [K ui/home, canvases/workspaces]:
  - With no folder open there's a Scratch workspace.
  - "No projects yet" plus "+ New"; "No projects match." for empty search.
  - Tiles never fake thumbnails: a plain gray cell is shown until the real one is backfilled, on-screen tiles first.

## 6. Polish details
- **Notification rail** [K ui/top-toolbar]:
  - Successes fade after a few seconds.
  - **Failures and warnings stay until dismissed.**
  - CTA notifications (with buttons) never expire.
  - Notifications the user opened themselves stay in view; ones the app raised tuck away and leave the dot lit.
  - "Clear all" and the agent's `dismiss_notifications` spare failures.
  - A later result updates a held failure's wording without removing it.
  - The dot is red only while something is actually failing.
- **Honest refusals.** A view-only edit raises a notice ("never a silent dead drag"). While access is still unknown it shows "Checking your access…", never a confidently wrong verdict [K canvases/collaboration].
- **Error-copy formula:** `<who> <what happened> (<cause>). <one next step>.` + [action] + Details. Agent-facing refusals include a corrected example [S, e.g. "rot() takes 1 value (degrees). Example: rot(45)."].
- **Palette:**
  - Commands with a checkbox keep the palette open.
  - Commands that open another surface leave you on it.
  - Keyboard shortcuts are fully customizable, with "Active when" contexts shown read-only, a conflict triangle and a "show conflicts" filter [K ui/command-palette, shortcuts/*].
- **Context menu** starts with "Select layer" (every layer under the cursor), then "Copy element id" and "Copy as" (PNG @1/2/4x, SVG, HTML, React, CSS, Blueprint) [K ui/context-menus].
- **Esc in a menu closes only the menu;** typing closes it and types [K ui/context-menus].
- **Combos:** user macros of command sequences with a key [K ui/combos].
- **`render_ui` / `list_stagers`:** agents render *real* UI widgets in a declared state to explain the app [K ui-walkthroughs].

## 7. Recommendations for fal OS
Where we stand today (read from the code):
- **Saving.** `crates/doc/src/project.rs` writes temp + rename with no fsync and no backup. The 400 ms autosave in `crates/host/src/host.rs` only logs failures (`tracing::error`), and a failed save isn't retried until the next edit.
- **Loading.** A corrupt `project.json` makes the whole project fail to open.
- **Host supervision.** The packaged host (`apps/desktop/src-tauri/src/sidecar.rs`) is not restarted if it crashes; `restart_host` exists but nothing calls it.
- **Toasts.** `toast()` in `apps/web/src/store.ts` is one auto-expiring slot (3.2 s, or 8 s with actions), so failures vanish.
- **Logs and updates.** Logs append forever, and there's no updater yet.
- **Already fine:** sync reconnect has jittered backoff.

### P0 stability
1. **Durable saves.**
   - *Brilliant:* atomic background saves, a crash-orphan temp adopted at launch, a marker for an interrupted save, a guard against saves that drop most of the content, and a widening retry with a held "not on disk yet" notice.
   - *Us:*
     - In `write_atomic`, `sync_all` the temp file (F_FULLFSYNC on macOS) before the rename, and fsync the directory.
     - Keep a small ring of last-good snapshots per canvas (`canvases/.bak/<id>.<n>.loro`).
     - Refuse a save whose node count collapses (say >50% at once) unless a delete op explains it.
     - On failure, keep the canvas dirty, retry with backoff (1 → 2 → 4 → … 30 s) and raise a held notice.
2. **Never overwrite after a failed load; degrade per canvas.**
   - *Brilliant:* saves are blocked on a canvas whose load failed, partial files open with a banner, and "Save repaired copy" leaves the original untouched.
   - *Us:*
     - If a canvas fails to load, open the rest of the project and show a card on that canvas ("couldn't open X; your file is untouched") with **Restore last good** (from the backup ring) and **Reveal file**. Autosave for that canvas is off until resolved.
     - A bad `project.json` is rebuilt from the `canvases/*.loro` files and the original kept as `project.json.broken`.
     - *Difference:* Loro snapshots aren't line-oriented, so recovery is backup-based rather than line-by-line.
3. **Supervise the host with a crash-loop guard.**
   - *Brilliant:* renderer crash-loop guard ("paused starting it so you are not stuck in the same crash"), "Try anyway", and failure cards with Report / Send logs.
   - *Us:*
     - The sidecar watches the host child and restarts it on an unexpected exit (backoff; at most 3 in 10 minutes).
     - The web UI keeps its last state and shows `reconnecting` only past ~1 s.
     - After the limit, show a card: `host stopped: <reason>`, with [restart] [copy report] [open logs].
     - Agents' stdio relays already re-initialize after a host restart.
4. **A notification rail with held failures** (replaces the single toast).
   - *Brilliant:* successes fade; failures persist; CTAs never expire; one notification per ongoing condition, updated in place, undismissable while active, then "back in sync" and fade.
   - *Us:*
     - Keep the mono look: a stack of dim one-line notices in a corner.
     - Successes fade after about 3 s. Failures (failed save, generation batch failure, a host or runtime down, fal auth or balance) stay until dismissed.
     - An ongoing condition (host offline, saves failing) is one undismissable line that turns into `ok` and fades.
     - "Clear all" spares failures.
5. **Boot watchdog: never a blank or hung window.**
   - *Brilliant:* per-stage stall budgets force a degraded window up, with Retry / "Open without last canvas".
   - *Us:*
     - Time the boot stages (window up → host found → project snapshot → first engine frame).
     - When a stage overruns (e.g. 5 s), show a plain card naming the stalled stage, with [retry] and [open without last project].
     - Log a `boot_phases` line every launch.
6. **Never lose the canvas to a GPU problem.**
   - *Brilliant:* device loss keeps the last frame ("Restoring the canvas…"), auto-recovers, and stops after 3 resets within 30 minutes; a blank-canvas detector exists.
   - *Us:*
     - Handle WebGPU `device.lost` in the engine (WKWebView): keep the last presented frame (or a CSS snapshot), recreate the device and re-upload by viewport distance.
     - Apply the same 3-in-30-minutes limit with a card offering [restart rendering].
7. **On-disk format versioning and migrations.**
   - *Brilliant:* `bp:v1` header; migrate on next save with originals archived; a newer-version file opens with saves paused.
   - *Us:*
     - Add `format: N` to `project.json` and to canvas root metadata.
     - On load, migrate in memory and back up the original to `.archive/` before the first save.
     - Refuse to save files with a newer `format` ("made by a newer fal OS; update to edit").
     - This matters now because the node schema changes weekly.
8. **Local diagnostics before telemetry.**
   - *Brilliant:* UI-stall watchdog, stall journal, flight reports, a "Report" diagnostics block, "Send logs" once per failure, and crash-report consent asked once.
   - *Us:*
     - Rotate `host.log` and `setup.log` (e.g. 10 MB × 3).
     - Install a Rust panic hook that writes `logs/crash-<ts>.txt` (backtrace, version, last 200 log lines).
     - On the next launch, show one line: `fal OS crashed last time [copy report] [dismiss]`.
     - A web main-thread stall probe (rAF gap > 250 ms) logs `ui_stall`.
     - Internal app, so nothing is uploaded; `copy report` is enough.

### P1 polish
9. **Indicator watchdog.** *Brilliant:* the shimmer watchdog and "nothing lingers after the agent finishes". *Us:* every agent outline, cursor, `*` tab marker and LED generation tile has an owner and a TTL. When its session or job disappears (host restart, crash, cancel), the engine clears it. Add a sweep on reconnect.
10. **Slow-only progress and no-flash switches.** *Brilliant:* "Pasting…" and "Switching…" appear only when slow, never a "done" over an error, and the old canvas never shows during a slow load. *Us:* import, Figma paste, project/canvas switch and export show progress only after about 300 ms. Hold the previous frame until the new canvas's first frame. Never show `imported` when anything failed; say `3 of 5 imported` and name the failures.
11. **Agent error cards and auto-recovery.** *Brilliant:* provider/cause/next step with Details; one retry on an early drop; cut-off continuation up to 3; breakers name the cause. *Us:* standardize chat error lines as `<agent/model> <what> (<cause>). <next step>` + one action, with raw text behind `[details]`. Retry once silently on a connection drop before first token. Breakers say *why* (`4 tool calls changed nothing`).
12. **Hold, don't drop.** *Brilliant:* a message blocked by a wall is held and auto-continues; dismissing returns it to the composer. *Us:* when a send hits a missing fal key, an exhausted balance or a down runtime, keep the message queued (`held: no fal key`) and send it automatically when resolved. Any failed send restores the draft.
13. **Agent-connect copy and detection.** *Brilliant:* "restart, then nothing happens until you start a chat", login-shell detection, re-check on focus, precise status rows. *Us:* add those two sentences to the Connect modal. Re-check status when the modal gains focus. Distinguish `not installed` / `installed, not signed in` / `signed in, can't verify plan`. We already resolve the login-shell PATH; also honour `FALOS_CLAUDE_PATH` and `FALOS_CODEX_PATH`.
14. **Point an external agent at a node.** *Brilliant:* "Copy element id" (context menu, command and a toast) and share links carrying selection + prompt. *Us:* `copy id` in the node context menu and palette (one id per line). `falos://project/<p>/canvas/<c>?sel=…&prompt=…` deep links, which "Continue in my agent" can also include.
15. **Agents see committed state and get a review image.** *Brilliant:* reads are committed geometry only; a screenshot of the changed elements after each edit block; stop after two no-op blocks. *Us:* make sure `read` never returns mid-drag positions, and add a cheap preview of the touched nodes to `edit`/`arrange` results (optional, off for text-only clients).
16. **Background project work is announced.** *Brilliant:* an agent touching a project that isn't open opens it in the background, and the user is told and offered to open it. *Us:* a notice line `codex is working in <project> [open]` instead of a silent change, consistent with "agents never move the camera".
17. **Input settings and window persistence.** *Brilliant:* pan/zoom speed, invert zoom, and window frame rules (ignore launch-time moves, pull back onto a live display, no fullscreen persist, 800×600 minimum). *Us:* add pan/zoom speed and invert-wheel to settings (kept in the ≡ menu, no new chrome), and adopt the window rules in the Tauri shell.
18. **Updater, when we add `tauri-plugin-updater`.** *Brilliant:* periodic checks for long sessions, a CTA with progress, signature/codesign verification, "did not install" reported on the next launch, update offered on failure cards. *Us:* the same behaviour on a signed `latest.json`. Re-check every few hours while open. Show `fal OS <v> ready [restart] [what's new]` as a held line, and never restart while agent runs or generations are in flight (reuse `request_quit`'s check).
19. **Honest operation outcomes.** *Brilliant:* "No subject found", "not applied: the image changed", "2 locked layers skipped". *Us:* `transform` / `rerun` / `compose` report skips and races the same way in chat and tool results, and drop results whose input changed mid-run unless lineage allows it.

### P2 nice-to-have
20. **Recorded event streams as regression fixtures** (Brilliant uses them as an onboarding demo; we don't: real runs only). Record normalized chat events from real runs and replay them in tests to check the transcript, activity lines and canvas application without spending.
21. **Shortcut customization** with "active when" contexts and conflict display (we already have a full click-path list in SHORTCUTS.md).
22. **First-run glide:** after a user's *first real* generation finishes, ease the camera to frame the result once. This is the single exception to "agents never move the camera", triggered by the user's own first prompt.
23. **Tiles never fake thumbnails** on the projects home (plain cell until the real still exists, backfilled for on-screen tiles first).
24. **Crash-report consent** and "something went wrong last time?" only once we have external users; as an internal tool, local reports suffice.
25. **Feedback discipline for in-app agents:** offer at most once per session and once per class, never send without the user's click.

**Where our use case differs**
- **Media:** we're media-first with long-running async generations (video/audio/3D), so job state, LED tiles and batch results need the same "held failure / indicator TTL / honest partial" rules as saves.
- **Sync:** one authority (host + Loro) means none of Brilliant's file-lane/live-lane cutover machinery. Copy its reporting rules, not its lanes.
- **Visuals:** we keep the mono terminal look and LED aesthetic; adopt Brilliant's state rules, not its shimmer visuals.
- **Accounts:** no account walls or plans to copy; the "hold the message" pattern maps onto fal key and balance.
