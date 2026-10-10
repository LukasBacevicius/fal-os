# Options research — agents, computer use, multi-agent editing, canvas tech, fal

Research date 2026-10-10. **(2nd-hand)** = secondary sources only. Re-verify billing/terms items before committing.

## 1. How external agents can connect
| Channel | Who | Live state | Our UI? | Runs on user's subscription | Maturity | Priority |
|---|---|---|---|---|---|---|
| Local MCP (stdio shim + localhost Streamable HTTP) | everyone | `subscriptions/listen`, Claude Code Channels | No | Yes | High | **P0** |
| CLI + Agent Skills (`fal-os` CLI + SKILL.md) | any shell agent | poll/watch | No | Yes | High | **P0** |
| Remote MCP + OAuth (+ MCP Apps `ui://`) | claude.ai, ChatGPT, cloud agents | Yes | in-chat views | Yes | High | P1 |
| ACP client (we host agents as subprocesses) | Gemini native, Claude/Codex via adapters | Full | Yes | Codex yes; Claude API key; Gemini API key | Med-high | P1 |
| Codex app-server | Codex | Full | Yes | **Sign in with ChatGPT opened to 3rd parties Sept 2026** | Med ("experimental") | P1 |
| Claude Agent SDK | our agents | Full | Yes | API key (claude.ai login needs Anthropic approval) | High | P1 |
| fal agent runtime | our agents | Full | Yes | fal billing | — | **P0** |
| A2A v1.0 | enterprise agents | tasks | No | n/a | Med | P3 |

**MCP 2026-07-28:** stateless core (no `initialize`, no session id; `Mcp-Method`/`Mcp-Name` headers), single `subscriptions/listen` notification stream, Multi Round-Trip Requests (`input_required`) replace elicitation/sampling, **Tasks extension** (`tasks/get`) for long jobs, CIMD replaces DCR, localhost redirects for desktop apps. **MCP Apps** (SEP-1865, final) render `ui://` HTML in Claude/ChatGPT/VS Code. **Claude Code Channels** (research preview) let an MCP server push events into a running session.

**ACP:** JSON-RPC over stdio; sessions, streamed plans/tool calls/diffs, permission requests, fs/terminal; registry; we pass our canvas MCP in `session/new`.

**Codex app-server:** `thread/*`, `turn/start|steer|interrupt`, approvals, experimental `dynamicTools`, `turn/diff/updated`. Codex Mac app prefers an app's plugin/MCP over computer use.

**Gemini CLI:** consumer access reportedly ended 2026-06-18 → Antigravity **(2nd-hand)**; plan API keys. **Cursor:** MCP + skills only.

## 2. Driving other creative apps — "bridges first, eyes second"
| App | Best surface |
|---|---|
| Premiere Pro | UXP (25.6+), ExtendScript legacy (sunset ~Sept 2026, verify); adb-mcp pattern (UXP plugin ↔ WS) |
| After Effects | ExtendScript; UXP beta; `aerender` |
| Photoshop | UXP (`batchPlay`, DOM) |
| Adobe cloud | "Adobe for creativity" hosted MCP **(2nd-hand)** |
| DaVinci Resolve | Python/Lua `DaVinciResolveScript` (some Studio-only); community MCPs |
| Blender | bpy, headless `-b -P`; official Blender MCP connector |
| Figma | remote MCP `use_figma` (Plugin API JS) |
| Final Cut | FCPXML + Workflow Extensions |
| ComfyUI | Comfy Cloud MCP |

Strategy:
1. **App Bridges**: in-app plugins opening a localhost WS to fal-os; external scripts; file interchange (**OTIO** as internal timeline, FCPXML/EDL/PSD/glTF/USD/EXR); vendor MCP passthrough.
2. **Computer-use fallback**: AX tree first, screenshot+zoom when sparse; background/virtual display per agent; pluggable model (Claude `computer_toolset_20260801`, OpenAI `computer` tool / code-exec harness). Record successful traces → promote to "learned bridges".
3. Everything round-trips through the project asset store with lineage.
4. Safety: per-app grants, "agent driving X" banner, snapshots before runs, isolated sessions.

**Optimizing fal-os for computer use:** ARIA mirror DOM of canvas objects with semantic labels; stable test ids; deterministic layout; command palette + shortcuts for everything; status bar with coords/zoom/selection; deep links `falos://project/…?x=&y=&zoom=&select=`; discoverable MCP endpoint (agents prefer it over CU); Electron `AXManualAccessibility`.

## 3. Multi-agent editing
| Option | Notes |
|---|---|
| **Loro** (recommended) | movable tree (z-order/layers), time travel/checkout, per-peer undo, Rust core + wasm |
| Yjs/Yrs | most mature, awareness, UndoManager `trackedOrigins`; no native move |
| Automerge 3 | versioning, smaller footprint |
| Server-authoritative op log (tldraw sync, Brilliant, Figma) | simple validation & locks; harder offline |

Agents as collaborators: actor identity on every transaction + lineage; presence (cursor, viewport, working box, status, plan); **soft leased claims** on objects/regions (human override, `claim_revoked`); one tool call = one transaction = one undo step; `expectedVersion` optimistic concurrency; per-actor undo / revert-agent; review modes (direct / proposal ghost layer / sandbox frame); cost approval over budget; comments as agent inbox.

Prior art: tldraw agent kit (viewport screenshot + "blurry" shapes + focused detail + typed streamed actions; SDK needs commercial license), Figma `use_figma` (code mode + skill), Excalidraw MCP App, Krea Agent (Sept 2026), Runway Agent + MCP workflows, Flora/Fauna, Figma Weave, ComfyUI MCP, Brilliant.

Synthesis: **three tool tiers** — semantic tools / compact DSL-patch write / sandboxed code mode; reads `get_canvas(detail=blurry|full)` + `render_viewport`.

## 4. Canvas & shell
| Renderer | Verdict |
|---|---|
| tldraw | paid production license, DOM/SVG shapes → reference only |
| Konva | Canvas2D, CPU-bound → no |
| PixiJS v8 | WebGPU/WebGL, fine to bootstrap |
| **Custom WebGPU (+WebGL2 fallback)** | recommended; WebCodecs `VideoFrame` → `importExternalTexture` zero-copy video |
| CanvasKit | heavy |
| Vello | alpha |
| Rust + wgpu → native + wasm | Brilliant's choice; phase-2 option |

Shell: **Electron** (bundled Chromium → consistent WebGPU/WebCodecs/codecs, same as web build). Tauri v2 risky: WKWebView WebGPU only macOS 26+, WebKitGTK codec issues.

## 5. fal for agents today
- Hosted MCP `https://mcp.fal.ai/mcp-relay` (OAuth): search/schema/pricing/recommend/run/submit/check/result/cancel/upload/docs; the connected instance also has assets, collections, entities, lineage, `estimate_cost`, `submit_jobs`/`wait_for_jobs`.
- genmedia CLI + `fal-ai-community/skills`; JS/Python clients (queue, webhooks, realtime WS); Platform APIs (assets, collections, workflows, storage); Serverless.
- No product formally named "fal agent SDK" found publicly — confirm internally.

## Sources
MCP: blog.modelcontextprotocol.io/posts/2026-07-28 · modelcontextprotocol.io/seps/1865-mcp-apps-interactive-user-interfaces-for-mcp · code.claude.com/docs/en/channels-reference
ACP: agentclientprotocol.com · zed.dev/acp · geminicli.com/docs/cli/acp-mode
Claude: code.claude.com/docs/en/agent-sdk/overview · platform.claude.com/docs/en/agents-and-tools/tool-use/computer-use-tool
Codex/OpenAI: learn.chatgpt.com/docs/app-server · learn.chatgpt.com/docs/codex-sdk.md · thenewstack.io/sign-in-with-chatgpt · developers.openai.com/codex/use-cases/use-your-computer-with-codex · developers.openai.com/api/docs/guides/tools-computer-use
Apps: ppro-scripting.docsforadobe.dev · developer.adobe.com/after-effects/uxp · anthropic.com/news/claude-for-creative-work · developers.figma.com/docs/figma-mcp-server/write-to-canvas · github.com/samuelgursky/davinci-resolve-mcp · docs.comfy.org/development/cloud/mcp-server
Canvas/CRDT: tldraw.dev/starter-kits/agent · tldraw.dev/pricing · loro.dev/docs · automerge.org/blog/automerge-3 · pixijs.com/blog/pixi-v8-launches · developer.chrome.com/blog/new-in-webgpu-116 · v2.tauri.app/reference/webview-versions
fal: fal.ai/docs/documentation/setting-up/mcp · fal.ai/docs/documentation/setting-up/genmedia · fal.ai/docs/llms.txt
