# Paper 0.5.15 (DMG build 261001): stability and polish teardown

Static inspection only; nothing was run.
- **Desktop shell:** the DMG's `app.asar` ships unminified TypeScript under `src/`.
- **Stdio relay:** a Go binary, `dist/cli-bin/darwin-arm64/paper`; read with `strings` only.
- **Web app:** `app.paper.design`, the cached bundle `main-DwnQW7Hr.js` (4 MB), plus `MCPHandlers-*.js`, `rain-indicator-*.js` and `index-*.css`, recovered from the local Chromium cache.

Paths below are relative to the asar, or name the bundle. Identifiers are minified names and will change between builds.

Earlier teardowns: [paper-ux.md](paper-ux.md) (agent UX), [paper-brilliant-io.md](paper-brilliant-io.md) (assets in and out). This doc covers only what those missed.

## 0. What's new since the last teardown
The last teardown was also labelled 0.5.15, because Paper's CI stamps every build between releases with the same patch number (README "Versioning"). This build is `CFBundleVersion 261001…`. Newly visible:

- **Tabbed, multi-window shell:** `window/paper-window.ts` and `paper-tab.ts`.
  - Tabs are lazily restored and unloaded after 2 days of inactivity, but never while an agent is mid-session.
  - Tabs can be torn off into a new window in a single painted frame.
- **Headless MCP mode** (`PAPER_HEADLESS_MCP`): a hidden window serves agents with a bearer token (`auth/auth-through-mcp.ts`). Agent-initiated windows always prefer headless, so they never steal focus.
- **Many more install paths for agents:**
  - a Claude Code plugin marketplace entry (`paper-design/agent-plugins`, `paper-desktop@paper`);
  - a Claude Desktop `.mcpb` extension (`mcp/install-claude-extension.ts`);
  - Cursor Marketplace and a `vscode:mcp/install?…` deep link;
  - config writers for Copilot, ChatGPT (shares Codex's TOML), Antigravity and OpenCode (`mcp/update-harness-config.ts`).
- **A relay that works without the app:** the stdio relay answers `initialize` and `tools/list` from a cached or remote tool manifest when the app is down (§3).
- **Workarounds for Claude Code's MCP client** (§3).
- **Gates on tool calls:** a 60 s timeout, refusal while offline, an edit-access check and a weekly quota (§3).
- **Crash and health telemetry for the renderer and GPU** (`lib/render-health.ts`).
- **Recovery screens:** an offline splash that retries by itself, and an auth gate that keeps your tabs (§4).
- **Two kinds of update:** a reload for a new web build, a restart for a new shell (§4).

## 1. Canvas interaction
Paper's canvas is real DOM under one camera transform. All UI (outlines, handles, guides, caret) is drawn on one 2D overlay canvas.

- **Picking:** works like Figma by default: a click picks the top-level child (or a sibling of the current selection), and ⌘-click picks the deepest node. "Deep selection" is a user setting. Double-click (≤500 ms, ≤5 px) drills one level down, edits text with everything selected, or renames an artboard label.
- **Marquee** (`findNodesInBrush`):
  - It starts after 5 px and selects what it touches.
  - A fully enclosed artboard selects only top-level nodes. A partly covered one is entered and its children tested, respecting clipping.
  - ⇧ toggles items against the selection that existed before the marquee started.
  - **Space moves the marquee rectangle.**
  - The marquee is excluded from undo; the result is committed as one selection change.
- **Drag:**
  - The threshold is 5 screen px, **dropping to 1 px once the button has been held for 500 ms**. That allows precise micro-moves without accidental drags.
  - ⌥ duplicates during the drag, toggled live, with its own cursor. Repeating ⌘D keeps the last offset.
  - ⇧ locks to the dominant axis.
  - Space suppresses reparenting. A node is never reparented to the root if the press started outside its parent.
- **Snapping:**
  - Threshold 4 screen px ÷ zoom; equal-spacing snaps use 1.5× that.
  - Snap points are the corners and centre of the selection bounds. Ctrl turns snapping off; pixel-grid rounding is a setting.
  - **Guides stay 850 ms after a nudge.** Displayed values round according to zoom.
  - Holding ⌥ shows distance lines.
- **Resize:**
  - The visual handle is a 7 px square, but the **hit radius is 9 screen px**. Rotation is a 6 px ring outside each corner.
  - Edges can be grabbed along their whole length, and the corner zone is capped at 12 % of the side.
  - **Handles are hidden below 8 px on screen.**
  - ⇧ keeps the aspect ratio, ⌥ resizes from the centre, **Space moves the box mid-resize**, and ⇧ snaps rotation to 15°.
- **Keys:**
  - **Nudging:** 1 px, ⇧ for 8 px (configurable in `nudgeAmounts`). ⌘-arrows resize. Inside a flex parent, arrows reorder.
  - **Escape ladder:** blur the input → leave vector edit → back to the Move tool → select the parent. Ctrl+Esc clears the selection.
  - **Hierarchy:** Tab / ⇧Tab move between siblings; Enter selects children; ⇧Enter or `\` selects the parent.
- **Keyboard layouts:** each hotkey has overrides for each layout (AZERTY, QWERTZ and so on), loaded from `/static/keyboard-layouts/…/layouts.json`, and the layout is auto-detected from keydown `code` → `key` pairs.
- **Zoom:**
  - **Steps:** fixed, from 2 % to 25,600 %. Keyboard zoom jumps to a step instantly.
  - **Smoothing:** the camera follows its target with exponential smoothing (0.6 per 1/120 s).
  - **Pinch and mouse wheel:** a pinch arrives as a ctrl-wheel event. Deltas over 100 count as a mouse wheel and apply without smoothing.
  - **Trackpad or mouse:** decided per gesture. A new gesture starts after an 80 ms gap or a sign flip.
  - **Settings:** wheel-zooms, invert zoom, zoom centres on the selection, right-click pans.
- **Undo** (`undoManager`):
  - **History:** 250 steps. Edits within 250 ms are grouped.
  - **Gestures:** drag, resize and rotate are each grouped as one step (`startGesture` / `endGesture`).
  - **Restored state:** each entry stores the selection, the page and the vector-edit state, and restores them on undo and redo.
  - **Text:** a text node that ends up empty is deleted without leaving an undo step.
- **Text and focus:** text is edited in a ProseMirror overlay. The caret is drawn on the overlay and blinks every 525 ms. Pressing on the canvas blurs the active element. Hotkeys are skipped while the target is editable (inputs, contentEditable, `data-scrub-area`).
- **Performance:**
  - **Frame loop:** one rAF tick drives everything, with dt clamped to 1/30 s. A ResizeObserver hook runs measurement and the overlay before paint in the same frame.
  - **Overlay:** redrawn only when marked dirty. The camera transform is written through a paused Web Animations effect rather than style writes.
  - **Loading:** nodes load in chunks of ≤4000, viewport first.
  - **Pausing:** shaders pause when offscreen or when the tab is hidden.
  - **Layers panel:** virtualized, with `content-visibility:auto`.
  - **Browser zoom** is locked to 100 % (`paper-tab.ts`), and the View menu drops the zoom roles because they break DOM measurement.

## 2. Drag and drop, paste
- **Paste classifier, in priority order:**
  1. SVG-looking text becomes `pasted.svg`.
  2. `image/*` files.
  3. HTML carrying Paper's own markers: `<!--<paper-paste-start data-embed>` (a schema-validated node tree), `paper-theme-start` (tokens), `<x-paper-html>`, or `!--(figma)` (a Figma clipboard).
  4. Text that parses as HTML markup becomes design nodes.
  5. Anything else becomes a Text node, coloured for contrast with the canvas.
- **Formats read:** `text/plain`, `text/uri-list` and `text/html`.
- **Where things land:** drops at the cursor; pastes at the selection.
- **Drop target:** a drop is accepted only on the HUD canvas element.
- **Native file paths:** the preload exposes no `webUtils.getPathForFile`. Everything comes in as web `File` objects and is uploaded.
- **Limits:** 25 MB on Free and 100 MB on Pro, with a message that names the file and its size.
- **HEIC:** converted to JPEG (q .92) on the client by a lazily loaded `heic-to`, with a "Converting…" toast.
- **URLs:** `data:`, localhost and `paper-asset://` URLs are fetched and re-uploaded; remote http images are referenced directly.
- **`paper-asset://`** is a scheme for local files:
  - It's registered as **non-standard**, so Chromium doesn't treat the first path segment as a host. An e2e test guards Windows drive paths.
  - It's marked `secure`, with streaming and fetch support so range requests work.
  - It rejects UNC paths to prevent NTLM credential leaks (`deep-links/url-to-file-path.ts`, `tests/local-images.e2e.ts`).
- **Drag-out:** none, as noted in the last teardown.

## 3. Agent UX and MCP
- **Presence:**
  - Agents are stored per MCP client as `{harness, activities:{nodeId:{readAt,writtenAt}}}`, mapped from `clientInfo.name`. Recognised names include claude-code, `codex-mcp-client`, cursor-vscode, copilot-cli and antigravity-client.
  - Presence is broadcast as `UserAgentsChange`, throttled to 100 ms.
  - The avatar strip shows at most **2 agents, then 4 humans, then +N**.
  - **There are no agent cursors.**
- **Activity effects:**
  - **Durations and peak opacity:** read 1500 ms at 0.7, sweeping up; write 1500 ms at 1, sweeping down; edit 1000 ms; delete 800 ms.
  - **Rain dots:** 32×2 columns along the top edge of each artboard with activity in the last 40 s, fading over 400 ms.
  - **Pulsing outline:** 2 px, on a 3 s cycle.
- **New detail:** written nodes stay `visibility:hidden` until the downward sweep is halfway, so content never pops in before its reveal. The tint is the ancestor's average fill, and the effect is clipped to the artboard.
- **Reduce agent animations:** turns off only the reveals; the rain and the outline stay.
- **Tool-call contract** (`MCPHandlers-*.js`):
  - **Timeout:** each call races a **60 s timeout** that returns "Tool call timed out. Please check your file then try again." and fires `mcp__toolcall_timeout`.
  - **Refusals:** `gatedMCPToolCall` refuses writes while the socket is disconnected ("…Wait until the connection is restored then try again"), refuses without edit access, and enforces the weekly quota. The quota message gives the reset time.
  - **Envelope:** every result for a file starts with `{file:{id,name}, contentHash}`.
  - **Size warning:** when the doc is at its size limit (47 MB, recomputed on idle), "Warning: Your file is too large…" is appended to *every* result.
  - **Error wording names the next tool**, e.g. `Node not found … Use get_tree_summary or get_children`. Bad arguments get "try once more, then ask the user to restart their MCP connection".
  - **Batches:** batch tools return partial `errors:[{nodeId,error}]` and `ignoredStyles` instead of failing the whole call.
- **Screenshots:** JPEG at q .92, longest side ≤2000 px, at most 4.5 MB of base64.
  - If it's too big, the quality steps down (.92 → .75 → .4) and then the size halves, up to 8 tries.
  - Images get 5 s to settle, and the result notes it when they haven't ("Some images did not finish loading").
- **Generation:** `paper-gen://model?prompt=…` in a style starts a generation.
  - At most 10 per call, rejected whole if exceeded.
  - A credit check runs first. Declined generations are rewritten to `paper-gen://declined`.
  - There is no approval UI.
- **Routing** (`mcp/bridge.ts`):
  - There's no sticky session. Every call resolves its tab fresh: an explicit `fileId` routes to the window that already has the file, or opens it in a background tab; otherwise the call goes to the active tab.
  - A `pageId` only seeds a *new* tab, so an open file never switches pages under the user.
  - If a file-targeted tab died, the call errors out rather than falling back to the active tab ("would silently run a write against whatever the user is viewing").
  - `removeAgent` never runs against a fallback tab.
  - Background throttling is held off for each call with a refcount (`window/lib/background-throttling.ts`). On Linux the tab captures itself to keep frames coming (`mcp/self-capture-hold.ts`).
- **Server hardening** (`mcp/server.ts`):
  - Sessions are **re-created on any POST**, not just `initialize`, so clients survive an app restart.
  - The `Accept` header is forced to `application/json, text/event-stream`, because Claude Code omits it and then misreads the 406 as an auth failure.
  - `/.well-known/oauth-*` returns a bare 404.
  - Error bodies avoid `error` / `error_description` field names, which make Claude Code start OAuth discovery.
  - Unknown routes return a hint: "The MCP endpoint is /mcp. Try restarting your agent and Paper."
  - On `EADDRINUSE` it logs a readable cause.
- **Stdio relay** (the Go `paper mcp`):
  - **Tool list without the app:** it answers `initialize` and `tools/list` itself, from the live config at `/mcp/desktop/config.json` or from a remote copy. The agent therefore always sees the tools.
  - **App not running:** calls return "Paper Desktop isn't running. Tell the user to open it, then end your turn. Don't launch Paper Desktop yourself."
  - **Failed discovery:** "You MUST tell the user … try restarting Paper and their agent harness."
- **Onboarding agents:**
  - **One "Add to Claude" button** writes the Claude Code config *and* hands a `.mcpb` to Claude Desktop. It reports the combined outcome: "Added to Claude, Claude Code install failed, follow the steps…". If Claude isn't found, that half is skipped.
  - **Config writes** are atomic: temp file plus rename, keeping the file's mode (0600 for new files). JSONC edits keep comments. A TOML entry is appended as its own table, with a full re-serialize only for a replacement.
  - **Encodings:** UTF-16 and BOM-encoded files are read correctly.
  - **Broken configs** are refused, with the line and column sent to telemetry but never the file body.
  - **CLI install:** `~/.paper/bin/paper` is replaced atomically, retrying with backoff when antivirus/EDR holds the file; if it can't, the old binary is kept.
  - **First successful write:** the camera zooms to it, and a toast says "<Harness> successfully ran your first prompt! You're all set." for 8 s.

## 4. Stability engineering
- **Error boundaries:** react-error-boundary. The fallback says "An error occurred. We log and review these errors.", gives an email address, and offers Back to dashboard (Reload on desktop).
- **Stale bundles:** `vite:preloadError` / ChunkLoadError shows a "Refresh to update" banner ("Restart to update" on desktop).
- **Sync** (websocket, JSON, no CRDT):
  - **Heartbeat:** ping every 15 s; no pong within 5 s means reconnect.
  - **Backoff:** `min(n,5) + rand·min(n,5)` seconds, so ≤10 s.
  - **Circuit breaker:** 20 attempts in 2 min shows "We couldn't connect to this file after several attempts…". It resets after 30 s of stable connection.
  - **Close codes:** 4401, 4403 and 4404 each have their own UI.
  - **Edit queue:** `pendingFileEditQueue` numbers each edit; batches over 4 MB are split.
- **Conflict handling:** if the server file differs from the local one after a reconnect, the page reloads ("Changes were made while offline, reloading"). A **reload-loop breaker** fails the session as `desync_loop` after 3 reloads in 60 s, tracked in sessionStorage. There's no offline queue in IndexedDB.
- **Migrations:** `fileState.data.schema` is compared with the latest version, and the chained steps (about 17) run in `requestIdleCallback`.
- **Unsaved-work guard:** a `beforeunload` guard, with a 30 s self-expiry, wraps uploads, generations, migrations and background removal.
- **Crash telemetry** (`lib/render-health.ts`):
  - **Process deaths:** renderer and child-process deaths are logged, skipping clean exits. Each event records free and total system memory, which separates machine-wide memory pressure from one huge file, and the seconds since the last suspend, resume or unlock (GPU surface eviction on wake is a suspect).
  - **Hangs:** unresponsive and responsive events are paired, capped at 5 per view.
  - **Session marker:** records GPU vendor and device and whether compositing runs on the GPU. It gives the crash counts a baseline of sessions without incidents.
- **Blank-window workarounds** (`paper-window.ts`):
  - It forces a repaint on window focus (`invalidate()`) for a Chromium occlusion bug that leaves windows blank or white.
  - First-run windows are pre-sized to the work area of the display under the cursor, so they never flash at the default size before maximizing.
  - Fullscreen is set before show.
  - **On a warm tab switch, the old view stays until the new one has painted** (double rAF, 250 ms cap).
- **Offline:** `window/load-url.ts` shows a local splash ("You are offline / When connected Paper will load automatically."). It polls with a HEAD request every 3 s, where any HTTP response counts as reachable. Superseded loads (`ERR_ABORTED -3`) never show the splash.
- **Logging:** electron-log writes to `logs/main.log`, recording uncaught exceptions, unhandled rejections and a one-line startup banner (version, environment, platform). Help → **View Logs** reveals the file. EPIPE on stdout is swallowed. MCP server logs are also mirrored into the renderer console.
- **Telemetry:**
  - **Main process:** PostHog with exception autocapture. Every event carries app version, arch, OS, locale, session id and uptime. Without a known user, events go out without a person profile rather than being dropped.
  - **Identity:** revalidated against `/auth/me` on launch. A 401 clears it; a 5xx or being offline keeps it.
  - **Web:** a persisted metrics queue in localStorage (24 h TTL, ≤1000 entries, batches of 50, backoff up to 30 s, flushed with keepalive on `pagehide`). Session recording excludes the canvas.
- **Updates:**
  - The shell updater never prompts on its own (`showInstallAndRestartPrompt:'never'`). The web app shows one "Update available" button that covers both a newer web build (reload) and a newer shell (restart).
  - The user agent carries `PaperDesktop/<ver>`, so the server can enforce a minimum version with a dialog that can't be dismissed ("Update required").
  - Menu accelerators aren't registered for undo, copy and so on (`registerAccelerator:false`); the editor owns those keys.
- **Window and tab state:**
  - Saved as one zod-validated JSON file per window; files that no longer parse are deleted.
  - A window is persisted only once the user has seen it.
  - Only normal (unmaximized) bounds are saved.
  - A dev-only env var unloads tabs fast, for testing.

## 5. Onboarding and first run
- **Sign-in:** PKCE in the system browser (password managers work), with the code returned to a 127.0.0.1 listener. The sealed session is stored as a cookie, so the desktop app behaves like the web app.
- **Single instance:** the app holds a single-instance lock (multiple windows are still allowed). A second launch focuses the existing one.
- **Expired session:** raises an **auth gate**. Home is revealed, tab switching locks and tab titles freeze. After re-auth the restored tabs reload, so no work context is lost.
- **Permissions:** no macOS permission prompts at startup. Camera and microphone entitlements exist but are requested only on use.

## 6. Polish details
- **Motion tokens:**
  - **Easings:** `--ease-out-fast cubic-bezier(.45,1.005,0,1.005)`, `--ease-out-smooth (.685,.89,.315,.995)`, `--ease-in-slow (.375,.015,.545,.455)`.
  - **Durations:** mostly 100, 200, 250 and 500 ms. **Button press is 60 ms.** A 2 px navigation progress bar scales with `ease-in-slow`.
- **Toasts:** one at a time, no stack.
  - **Timing:** 4 s, plus 2 s after hover-out.
  - **Variants:** an "until-cleared" variant has a spinner; an optional action button dismisses on click.
  - **Clearing:** cleared when the page or selection changes.
  - **Position:** above the prompt box or banners.
- **Tooltips:**
  - **Delay:** 600 ms by default (800 for the star).
  - **Fade-out:** 100 ms.
  - **Instant mode:** `data-instant` shows them with no delay.
- **Type:** UI text is 10, 12 and 13 px. Fonts: system/Inter for the UI, Matter for headings, PaperMono for code, and 12 px sans / 11 px mono on the canvas HUD.
- **Reduced motion:** `prefers-reduced-motion` affects only the lightbox and zoom transitions. The canvas uses the separate agent-animation setting.
- **Node context menu, in order:**
  - Select layer…, Select parent / children, Copy link (a `?node=` deep link).
  - Paste / **Paste on top / Paste to replace**, Duplicate, Copy / Paste styles.
  - Frame selection, Ungroup, z-order, Show / hide, Lock.
  - **Adjust text…** (± size, weight, line height, letter spacing), Copy as… (PNG, SVG, Tailwind, React).
  - Image-only: Copy prompt, Vectorize, Extract colors, Remove background, Open in new tab.
- **Native context menus** are built from a template the renderer sends, and they resolve with the chosen id.
  - The renderer keeps the "active" state while the menu is open, because the promise resolves only on close.
  - After the menu closes, the window is invalidated to avoid a blank app bar on slow machines.
- **Cursors:** a full custom set (arrow-duplicate, arrow-x/y, position-absolute, hand open/closed, zoom in/out, eyedropper, and more). The rotate and resize cursors are generated with the current angle applied.

## 7. Recommendations for fal OS
Format: **Paper does → we should**. *Differs* notes where our case changes the answer. We're ahead on four points:
- the CRDT (Loro) where Paper reloads on divergence;
- per-turn revert for agents where Paper has no agent undo;
- a built-in chat;
- a host that runs without the UI, so we need no headless window.

### P0: stability
1. **The relay always lists the tools.** Paper's relay answers `initialize` / `tools/list` from a cached manifest when the app is down, and tool calls say "isn't running, tell the user, end your turn, don't launch it" → `falos-host mcp` should write `~/.fal-os/mcp-manifest.json` on every host start and serve it when the host is unreachable. Today an agent started before fal OS gets an error at `initialize` and runs with no tools until it restarts (`crates/host/src/relay.rs` `NOT_RUNNING`). Reword the error the same way.
2. **Defensive MCP endpoint against Claude Code's quirks.** Paper forces `Accept`, returns bare 404s on `/.well-known/oauth-*`, never uses `error` / `error_description` keys, re-creates a session on any POST, and returns a hint for unknown routes → audit `crates/host/src/server.rs` and `mcp.rs` for the same; it matters most for dev-mode HTTP clients, which have no relay.
3. **Gate and time-box tool calls.** Paper has a 60 s timeout with an instructive `isError`, refuses writes while disconnected, and adds a doc-health warning to every result → give every host tool a deadline (except `await_batch` / `inbox`, which already long-poll), and refuse with "wait until … then retry" when the canvas isn't loaded or the project is read-only. Prefix results with `{project, canvas, version}` so agents notice when they're bound to the wrong project. That reinforces our zero-setup binding.
4. **Webview crash and hang recovery with health telemetry.** Paper logs renderer and GPU deaths with memory and time since wake, pairs hangs, and keeps a session marker → in the Tauri shell, handle WKWebView's web-content-process termination by reloading the view and restoring the camera and selection from the host. Log it to `~/.fal-os/logs/app.log` with free memory and seconds since wake. Add **View logs** to the `[≡]` menu. *Differs:* local-only logs; no external telemetry unless opted in.
5. **Circuit breakers on sync and reload.** Paper: 15 s ping / 5 s pong, jittered backoff ≤10 s, a breaker at 20 attempts / 2 min, and `desync_loop` after 3 reloads / 60 s → `apps/web/src/sync.ts` already backs off (≤10 s). Add a heartbeat (a half-open socket looks connected forever), a breaker with a plain message (`host unreachable · [retry]`), and a reload-loop guard around snapshot / version mismatches.
6. **Detect a stale UI.** Paper shows "Refresh to update" on chunk-load errors and a minimum-version gate via the user agent → send `{hostVersion, protocolVersion}` in the sync handshake. A web UI that doesn't match the host shows one line, `ui out of date · [reload]`, instead of failing in odd ways after a host update.
7. **Guard quitting during work.** Paper wraps uploads, generations and migrations in a `beforeunload` guard that expires after 30 s → on ⌘Q (window close already only hides), list in-flight uploads and imports that would be lost. Don't block for host-side generations, which survive.
8. **Run migrations on idle, with a version.** Paper stores an explicit `schema` and runs chained migrations in `requestIdleCallback` → put a schema version in the Loro doc root and `project.json`, run migrations once in the host before serving, back up first, and never migrate in the UI.
9. **Webview throttling while hidden.** Paper holds background throttling off for each agent call, with a refcount, so offscreen captures still render → our window *hides* on close, and WKWebView stops rAF when hidden or occluded. Any agent tool that needs the webview (viewport render, app snapshot) must either render in the host, or briefly keep the view alive and say so. Verify `render_viewport` with the window hidden.

### P1: polish
1. **Snapping and guides.** 4 screen px ÷ zoom; corners and centre; equal spacing at 1.5×; Ctrl disables; guides stay 850 ms after a nudge; ⌥ shows distances → we have none (`crates/client/src/interact.rs`). Draw them in the engine overlay as 1 px `--dim` lines with mono labels.
2. **Drag precision.** Paper's threshold is 5 px, dropping to 1 px after a 500 ms hold; ⇧ locks the axis → our `DRAG_SLOP_PX = 3` is fixed. Add the hold-to-precise rule and the ⇧ axis lock.
3. **Duplicate on drag.** ⌥-drag duplicates, toggled live → *differs:* our ⌥-click / ⌥-drag adds spot and region chips. Use ⌥ to duplicate only when the drag starts on an *already selected* node and passes the threshold, or keep ⌘D with "repeat last offset", which Paper also has.
4. **Resize hit areas larger than the handles.** Paper's handles are 7 px but grab at 9 px; edges grab along their whole length; handles are hidden below 8 px on screen; Space moves mid-resize; ⌥ resizes from the centre → our handles are hit at 7 px (`HANDLE_PX * 0.5 + 3`). Add edge resize along the whole length, hide handles on tiny tiles (so small nodes can still be dragged), and add Space to move during a resize.
5. **Marquee extras.** ⇧ toggles against the selection from before the marquee, Space moves the marquee, and the marquee stays out of undo.
6. **Undo restores the selection.** Each entry stores the selection and page and restores them; 250 ms auto-grouping; gestures are one step → make sure host undo steps carry the selection, so ⌘Z puts it back.
7. **Escape ladder and hierarchy keys.** Leave the text field → leave the group → select the parent; Enter selects children; ⇧Enter / `\` select the parent → we have double-click and Esc for groups. Add Enter / ⇧Enter for a fully keyboard-driven flow, which fits our perf/UX bar.
8. **Hotkeys that follow the keyboard layout.** Match on `code`, and learn the layout from `code`→`key` pairs → this matters for `/`, `[`, `]`, `\` and `?` on non-US layouts.
9. **Tool errors that say what to do next.** Name the next tool, return partial errors for batch items, and size-cap screenshots with a quality ladder (≤2000 px, ≤4.5 MB) → audit `mcp.rs` error strings, and cap `get_context` / `render` images the same way.
10. **One "Add to Claude" button.** It covers Claude Code and Claude desktop with a combined result, plus marketplace / plugin routes and an 8 s first-success toast → our `connect.rs` already writes both. Report per-target outcomes in one line (`claude code ✓ · claude app ✗ (not found)`). Consider publishing a Claude Code plugin (marketplace), so the skill and MCP entry update with the plugin rather than with file writes.
11. **Toasts and tooltips.** One toast, 4 s, +2 s after hover-out, cleared on selection change; tooltips after 600 ms, then instant while moving between controls → adopt these timings. Keep our 120 ms fades; a 60 ms press state fits the terminal look.
12. **Context menu additions.** Copy link (a `falos://…?node=` deep link), Paste to replace, Paste on top, and Select parent / children.
13. **Paste classifier order.** SVG text → image files → our own clipboard marker → HTML → text → add an `x-falos` marker in `text/html` so copy and paste between canvases and projects keeps lineage, and detect SVG text.

### P2: nice to have
1. **No pop-in before the reveal.** Paper hides written nodes until the sweep is halfway → hold new agent nodes until their first stepped outline (≤150 ms). Consistent with "never flash".
2. **Unload idle background canvases and app iframes after inactivity, except while an agent is active on them.** This mirrors Paper's 2-day tab unload with an agent exception.
3. **Presence cap.** At most 2 agents + 4 humans, then +N, in any avatar or agent strip.
4. **Configurable nudge** (`[1,10]` by default), plus settings for wheel-zooms and invert zoom.
5. **Fixed zoom steps** for keyboard zoom (2 % … 25,600 %), with instant jumps; keep smoothing only for wheel and trackpad. Classify trackpad vs mouse per gesture (80 ms gap, sign flip).
6. **Doc health warnings** in tool results (e.g. "canvas has 9k nodes / 40 GB of media: performance will degrade"), like Paper's 47 MB warning.
7. **Hardened local asset scheme.** If we ever serve local files to the webview by a custom scheme: register it as non-standard, reject UNC and `..` paths, and support range requests.
8. **Anonymous-first metrics, if we add them:** a persisted queue that flushes on `pagehide`; events without a person profile until sign-in.

Not worth copying: Paper's reload on divergence (Loro merges), the weekly MCP quota (our analogue is spend budgets with approvals), and Electron-specific workarounds (the Wayland self-capture, the Windows BOM / UTF-16 handling). The *idea* behind the BOM handling, reading user config files tolerantly, still applies to `connect.rs`.
