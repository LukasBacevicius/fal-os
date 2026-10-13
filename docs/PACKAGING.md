# Packaging and sharing fal-os (internal)

Goal: colleagues at fal can install fal-os on their Mac and use it, without a dev checkout.
This doc covers how comparable apps ship (decoded from their bundles), the options for us,
the recommendation, the proof of concept in this repo (`scripts/package-macos.sh`), and the
steps to sign and notarize the app later.

**TL;DR**
- **This week:** an ad-hoc signed, Apple Silicon only DMG built by `scripts/package-macos.sh`.
  Share it in Slack or Drive. Colleagues drag it to /Applications, run one `xattr` command, and
  follow the setup screen: paste a fal key, click install / sign in for Claude Code, ffmpeg and
  the motion tools download by themselves. No Terminal, no Homebrew. The app uses their own
  `claude` login. Colleague-facing steps: [INSTALL.md](INSTALL.md).
- **Later:** a fal Developer ID certificate and notarization. Build with `cargo tauri build`
  (`externalBin` + `resources`), add `tauri-plugin-updater` with a signed `latest.json` on an
  internal bucket, and optionally a private Homebrew cask.

---

## 1. How comparable apps ship (decoded)

These findings come from static inspection only; none of the bundles was executed.

| | Brilliant 1.0 (Flutter) | Paper (Electron via ToDesktop) | Atlas 1.7 (Swift) |
|---|---|---|---|
| **Signing** | Developer ID, hardened runtime, notarized + stapled. Entitlements: no sandbox, `files.user-selected.read-write`. | Developer ID, hardened runtime, notarized + stapled. Entitlements: `allow-jit`, `allow-unsigned-executable-memory`, `disable-library-validation`, `allow-dyld-environment-variables` (Electron defaults). | Developer ID, hardened runtime, notarized + stapled. Empty entitlements (no sandbox). |
| **Updates** | Custom Dart updater. It calls `/versions/info`, downloads a DMG, runs `hdiutil -nobrowse`, `ditto`s the app to staging, strips quarantine, swaps the app with `RENAME_SWAP` and relaunches. `install_latest` is refused on the agent surface ("dangerous, relaunches the app"). | ToDesktop runtime (`electron-updater`, generic feed `download.todesktop.com/<id>`, channel `latest`). ToDesktop's own prompts are off and the web UI shows "Restart to update". A second layer refreshes the remotely loaded web client. The user agent carries `PaperDesktop/<ver>` for minimum-version gating. | Sparkle 2.9.3. `SUFeedURL` is an appcast on Cloudflare R2, with an EdDSA `SUPublicEDKey`. Stable, beta and alpha channels are switched in code. Default check interval. |
| **Sidecars** | `Resources/rg` (universal, signed). Motion runner `Resources/motion/*.mjs` uses the **user's** `node` (it searches nvm, fnm, mise, volta and `/opt/homebrew/bin`) and system `ffmpeg`. It spawns the user's `claude` / `codex` / `cursor-agent`, with `BRILLIANT_*_PATH` overrides. | Go CLI `paper` in `app.asar.unpacked/dist/cli-bin/<platform>`. Copied byte for byte to `~/.paper/bin` on every launch (atomic rename, skipped if identical), with no PATH editing. The MCP server runs in the main process (fixed port 29979). | `Contents/Helpers/atlas` CLI, a thin client for the app's MCP endpoint (fixed port 50473, bearer token from the library). It can serve stdio (`atlas mcp`). |
| **CLI install** | none | copy to `~/.paper/bin`; MCP configs use the absolute path | symlink into `/usr/local/bin` via `do shell script … with administrator privileges` |
| **MCP registration** | Writes `~/.claude.json`, `claude_desktop_config.json` and Codex `config.toml` directly, and also tries `claude mcp add`. | On request from the web UI (consent screen) it writes stdio entries `<abs path>/paper mcp` for Claude Code, Codex, Copilot, OpenCode and Antigravity. Writes are atomic, keep comments (`jsonc-parser`), and are refused if a file won't parse. Cursor uses a deep link; Claude Desktop gets a `.mcpb`. | Copy-paste only (`claude mcp add --transport http …`, plus `mcp-remote` for Claude Desktop). It writes a Claude Code skill to `~/.claude/skills/atlas/`. |
| **Onboarding / auth** | Google or email login; refresh token in the Keychain. Bring-your-own provider keys are "encrypted in your system keychain". Deep-links to the Screen Recording and Accessibility settings. | WorkOS AuthKit PKCE in the system browser, then a `paper://auth/callback` deep link. The session lives in (unencrypted) Electron cookies. Single-instance lock. | Paywall first: a 7-day server-checked trial with an email, then Gumroad or Polar keys plus a Cloudflare Worker that binds to the hardware UUID. |
| **Telemetry** | First-party `/events`. Crash dumps only with consent. | PostHog (with exception autocapture); `electron-log` to `main.log`. | First-party Worker, with an opt-out toggle. |
| **Size / arch** | 238 MB, universal, macOS 14+ | — / arm64 + x64, macOS 13+ | 61 MB, universal, macOS 14+ |

**What we take from them**
- Everyone ships Developer ID + notarized. Ad-hoc is only acceptable for an internal pilot.
- Nobody bundles the agent CLIs. Brilliant, the closest match to us, spawns the user's `claude`
  and `codex` and relies on their logins. We do the same.
- Sidecars live inside the bundle and are found relative to the executable. GUI apps don't get
  the shell PATH, so look up tools explicitly: Brilliant hardcodes the nvm, volta and Homebrew
  paths, and we read the login shell's PATH.
- MCP registration: stdio entries with an absolute path to a CLI inside the app (Paper, Atlas
  `mcp`) survive port changes better than HTTP URLs with a port in them.
- Sparkle and the ToDesktop updater both boil down to a signed feed plus a signed archive.
  For a Tauri app the equivalent is `tauri-plugin-updater`.

---

## 2. Options

### 2.1 Distribution

| Option | Effort | Colleague experience | Verdict |
|---|---|---|---|
| **Ad-hoc signed DMG** (`codesign -s -`), shared on Slack or Drive | none (POC exists) | Drag to /Applications, then `xattr -dr com.apple.quarantine /Applications/fal-os.app` once per download (or right-click → Open → System Settings → "Open Anyway"). | **This week** |
| **Developer ID + notarized DMG** | Apple Developer Program for fal (org), a Developer ID Application cert, and notarytool in CI | Double-click, no warnings. Keychain items survive updates because the code signature is stable. | **Next** |
| TestFlight for Mac | needs a sandboxed App Store build | — | **No.** fal-os spawns arbitrary CLIs (`claude`, `node`, `ffmpeg`) and writes agent configs, which the sandbox forbids. |
| Private Homebrew cask tap (`fal-ai/homebrew-tap`) | small, once we have stable DMG URLs | `brew install --cask fal-ai/tap/fal-os`. `brew upgrade` handles updates. For unsigned builds `--no-quarantine` skips the xattr step. A private repo needs `HOMEBREW_GITHUB_API_TOKEN`. | Nice later, on top of the notarized DMG |
| **Hosted web (Vercel) + local helper** | medium | Still needs the host, runtime, Node and ffmpeg installed locally. The host must allow the Vercel origin (today it allows only loopback and `tauri://`). Chrome's Private Network Access prompts. Safari/WKWebView is stricter about `https` → `http://127.0.0.1`. | **No** as the main path: it doesn't remove the local install. Possibly later for a read-only viewer or share links. |

### 2.2 Updates

- **Now:** none. The colleague downloads the new DMG and replaces the app. Projects in
  `~/fal-os-projects` and settings in `~/.fal-os` are untouched.
- **Tauri updater (recommended next).** It is the same model as Sparkle and native to our stack.
  - `cargo tauri signer generate -w ~/.tauri/fal-os.key` creates a minisign key pair.
  - The **public** key goes in `tauri.conf.json` → `plugins.updater.pubkey`.
  - The **private** key and its password go in CI secrets (`TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`).
  - `bundle.createUpdaterArtifacts: true` makes `cargo tauri build` emit `fal-os.app.tar.gz` + `.sig`.
  - CI writes `latest.json` (`version`, `notes`, `platforms.darwin-aarch64.{url, signature}`).
  - In the app: `tauri-plugin-updater` checks `endpoints: ["https://<internal-bucket>/fal-os/latest.json"]` at launch and shows "restart to update" in the UI (the Paper pattern).
  - GitHub Releases on a **private** repo don't work as an endpoint without shipping a token. Use a bucket that colleagues can reach instead: R2/GCS behind Cloudflare Access, or a public bucket with unguessable paths.
  - The updater verifies the minisign signature itself, so it works with ad-hoc builds too. But with ad-hoc signing every update changes the code signature, so macOS asks again for Keychain access to the fal key. That is one more reason to get Developer ID first.
- **Sparkle:** proven (Atlas uses it). It needs a Swift/ObjC bridge in a Tauri app and its own EdDSA key and appcast. There is no advantage over the Tauri updater for us.

### 2.3 What to bundle and what to require

| Component | Decision | Why |
|---|---|---|
| Web UI (Vite + wasm) | **bundle** (embedded in the shell binary via `custom-protocol`) | 5 MB |
| `falos-host` | **bundle** as a sidecar (`Contents/MacOS/falos-host`) | 21 MB stripped |
| Agent runtime | **bundle**, esbuild → `dist/main.mjs` (2.6 MB) + `dist/setup.mjs` | no `node_modules`, no tsx, no pnpm at runtime |
| Node | **bundle the official Node 22** (`node/bin/node` + npm/npx, 122 MB, keeps its OpenJS Developer ID signature) | The agents run `npx remotion render`, `node motion-qa.mjs` and `npm install` themselves, so a real Node with npm is needed anyway. `node --experimental-sea-config` (SEA) would still embed the full ~107 MB binary, is CJS-only in Node 22, and provides no `npx`. `bun build --compile` (~60 MB) is a different runtime under the Agent SDK's process handling, and the agents would still need Node. The bundled Node goes first on the runtime's PATH, so renders use a known-good version. |
| `claude` CLI | **the user's own install and login**, installed from the setup screen if missing | The Agent SDK spawns `claude` and uses that user's subscription; no API key is shared and there is no per-seat billing for us. Bundling it would add 219 MB and an update treadmill. The setup screen detects it (`claude auth status --json`), runs Anthropic's official installer (`curl -fsSL https://claude.ai/install.sh \| bash`, everything under `$HOME`, `~/.local/bin/claude`) and `claude auth login` (browser) on a click. |
| Codex | **optional, the user's own `codex`** (`FALOS_CODEX_PATH`, else PATH), or one click: the bundled npm installs `@openai/codex` into `~/.fal-os/npm` | `@openai/codex` is 318 MB. Few people will use the codex driver, so it's never downloaded unasked. |
| ffmpeg | **downloaded on first run** into `~/.fal-os/bin` (setup screen; the user's own Homebrew ffmpeg also works) | A pinned static arm64 build (Martin Riedl's release builds, Developer ID signed + notarized), sha256-checked; ~56 MB of zips, 130 MB on disk. No Homebrew needed. It is GPL (x264/x265), which is fine because it is downloaded and installed on the user's machine, not shipped by us (§2.7). Host, runtime and agents prefer `~/.fal-os/bin` over PATH. |
| Remotion + HyperFrames template deps, chrome-headless-shell | **install on first run** into `~/.fal-os/templates` (Setup → install) | about 0.85 GB on disk (`motion-template` ≈ 600 MB, chrome ≈ 90 MB, HyperFrames ≈ 126 MB). The bundle stays small, and updates don't re-download it. The npm cache is reused. |
| Remotion / HyperFrames agent skills | **fetch on first run** (GitHub tarballs at the commits pinned in `install-skills.sh`, no git needed) | Remotion's skills repo has no LICENSE file, so we fetch them and never redistribute. |

### 2.4 Secrets
- The fal key is entered on the setup screen and checked before it is saved:
  - Queue API: an auth-only status lookup that spends nothing.
  - Agent API: `GET /agent/projects`, to detect whether the key has the AGENT preset.
- It is stored in the **macOS Keychain** (service `ai.fal.os`, account `FAL_KEY`, via the `keyring` crate). The fallback is `~/.fal-os/fal.env` with mode 0600.
- The app passes it in the environment to the host, which passes it to the runtime. It never goes into files we ship, logs, or argv.
- Nothing in the DMG is secret. Each install creates its own host token (`~/.fal-os/token`).
- In packaged mode the runtime reads only `$FALOS_HOME`; it skips the dev fallback to `~/.fal-os`.

### 2.5 First-run onboarding (implemented)
The setup screen opens at launch until the required parts are there: a fal key, Claude Code
installed and signed in, and ffmpeg. It is also available from ⌘K → "setup…" (for example to
change the key). Nothing on it needs Terminal or Homebrew.
1. **fal key**: [open fal.ai/dashboard/keys] opens the keys page in the system browser. Paste
   with ⌘V (the paste itself triggers the check) or [paste] (reads the clipboard only on that
   click; nothing reads it unprompted). The key is validated (models + AGENT preset) and stored
   in the Keychain. Once stored: [check] / [change] / [remove]; [change] saves a new key over the
   old one after the same check.
2. **Claude Code**: path + `loggedIn` + subscription. Not installed → [install] runs Anthropic's
   official installer with streamed output. Not signed in → [sign in] runs `claude auth login`,
   which opens the browser; the screen shows [open the sign-in page] when the CLI prints a URL,
   and [cancel]. While anything required is missing, the screen polls `claude auth status --json`
   every 3 s, so it turns green by itself after the browser sign-in.
3. **Motion tools**: ffmpeg + per-part status (Remotion, browser, HyperFrames, skills), free disk.
   Right after a key is saved, the ffmpeg download and the motion tools install start by
   themselves in the background (progress inline; they keep going if the screen is closed).
   [install] retries what's missing.
4. **Codex (optional)**: [install] (bundled npm → `~/.fal-os/npm`) and [sign in] (`codex login`,
   browser).
5. **ready → [start →]**: when 1–3 (fal key, Claude, ffmpeg) are done, one button closes the
   screen and opens a fresh project (the empty first project, else a new one), chat focused. The
   motion tools may still be finishing in the background. Reopened from ⌘K it says [done].

How it runs: the installs and sign-ins are a fixed list of tasks in the shell (`setup_run`,
`setup_task`, `setup_cancel` in `sidecar.rs`; never an arbitrary command), each in its own process
group with the app's PATH and **without** `FAL_KEY` in its env. Output is kept in memory (last 40
lines, ANSI stripped) for the screen and appended to `~/.fal-os/logs/setup.log`. A finished
install restarts the host, so the runtime sees the new `claude` / `ffmpeg` / `codex`. `ffmpeg` and
`codex` are subcommands of `dist/setup.mjs`.

The runtime's capability fixes point at "fal-os → Setup" when packaged (`FALOS_PACKAGED=1`),
not at dev commands.

### 2.6 Workspace, MCP registration, logs
- Projects: `~/fal-os-projects` (per user). Settings, token, templates, skills and logs: `~/.fal-os`.
  Overrides: `FALOS_HOME`, `FALOS_PROJECTS`.
- MCP registration for Claude Code, Codex, Cursor, Gemini and OpenCode already exists, with consent: Connect modal → "add to …", which calls the host's `register` and keeps other settings.
  - **Caveat:** it registers `http://127.0.0.1:<port>/mcp`, and the port changes when 7317 is busy.
  - **Next:** register the stdio relay with an absolute path instead, as Paper and Atlas do: `/Applications/fal-os.app/Contents/MacOS/falos-host mcp`. It reads `host.json`, so it survives port changes and app updates.
- Logs: `~/.fal-os/logs/host.log` (host + runtime) and `setup.log`. There is no telemetry or crash reporting; colleagues send these logs instead. Later: opt-in Sentry for Rust + JS.

### 2.7 Licensing notes (internal use; check before any external release)
- **Remotion**: free only for individuals and companies of up to 3 people. **fal needs a Remotion Company License** even for internal use of the motion agent; check the current terms at remotion.dev/license. We don't bundle Remotion; it's npm-installed on first run, but the license is about use, not bundling.
- **HyperFrames**: Apache-2.0 (HeyGen). Its skills ship with their LICENSE.
- **ffmpeg**: not bundled. The setup screen downloads Martin Riedl's static macOS arm64 release build (ffmpeg.martin-riedl.de, FFmpeg 9.0.2, `--enable-gpl --enable-version3`, so GPLv3 with x264/x265) on the user's machine, pinned by URL and sha256 in `apps/agent-runtime/src/setup.ts`. We don't distribute it; the user downloads it from its publisher, like a Homebrew install. Because it's GPL, never copy it into the DMG. If ffmpeg is ever bundled: LGPL-only build, dynamic linking or the source offer, no GPL encoders. To bump it: new version, URLs and both hashes together (the site publishes a `.sha256` next to each zip; verify locally with `shasum -a 256`).
- **Node**: MIT, LICENSE included at `Resources/node/LICENSE`. **npm** is Artistic-2.0.
- **Claude Agent SDK**: Anthropic's commercial terms, inlined in `dist/main.mjs`; fine for internal use. **@openai/codex-sdk**: Apache-2.0. **@fal-ai/client**: MIT.
- chrome-headless-shell (Chromium, BSD plus others) is downloaded by Remotion on the user's machine.

### 2.8 Size budget (POC, measured)

| | Size |
|---|---|
| `fal-os.app` | **157 MB** |
| └ `Resources/node` (node 107 MB + npm 15 MB) | 122 MB |
| └ `falos-host` (stripped) | 21 MB |
| └ `falos-desktop` (shell + embedded web/wasm) | 11 MB |
| └ `Resources/runtime` | 2.8 MB |
| **DMG** (APFS, ULFO/lzfse) | **54 MB** |
| First-run install in `~/.fal-os` | ≈ 0.85 GB |

Budget: keep the DMG under 100 MB. Not bundling `claude` (219 MB), `codex` (318 MB) and
Remotion is what keeps it small.

### 2.9 Architecture
**Apple Silicon only** for now:
- `LSMinimumSystemVersion` is 26.0, because the canvas needs WebGPU in WKWebView.
- fal's Macs are Apple Silicon.
- A universal build would need `lipo` for host + shell (+~30 MB) and an x64 Node (+~110 MB), and doubles the testing.

---

## 3. Recommendation

### This week: ad-hoc DMG (built and verified, see §4)
1. `scripts/package-macos.sh` → `fal-os-0.1.0-arm64.dmg` (54 MB).
2. Post it in the internal channel with the install steps from §5.
3. Colleagues need a fal key with the AGENT preset and a Claude account. The setup screen installs Claude Code and ffmpeg and signs in; see [INSTALL.md](INSTALL.md).
4. Updates: post a new DMG.

### Next (1–2 weeks)
1. **Developer ID + notarization** (§6). It removes the xattr step and Keychain re-prompts, and makes the app look trustworthy.
2. **Move the build to `cargo tauri build`:**
   - `bundle.externalBin: ["binaries/falos-host"]` (built as `falos-host-aarch64-apple-darwin`).
   - `bundle.resources` for `node/` and `runtime/`.
   - `bundle.macOS.signingIdentity` / `entitlements`.
   - `APPLE_ID` / `APPLE_PASSWORD` / `APPLE_TEAM_ID` (or an App Store Connect API key) for automatic notarization.
   - The layout matches the POC, so `sidecar.rs` doesn't change. Keep `package-macos.sh` as the no-tauri-cli fallback.
3. **Auto-update:** `tauri-plugin-updater`, `latest.json` on an internal bucket, "restart to update" in the UI. CI on tag: build → sign → notarize → staple → upload DMG + `.app.tar.gz` + `.sig` + `latest.json`.
4. **MCP registration via the stdio relay** at an absolute path inside the app.
5. **Single-instance** (`tauri-plugin-single-instance`). Today two app instances would start two hosts.
6. A private Homebrew cask, and an opt-in crash reporter.

---

## 4. Proof of concept (this repo)

`scripts/package-macos.sh` builds everything and assembles the bundle by hand in Tauri's
`externalBin` layout, so tauri-cli is not needed:

```
fal-os.app/Contents/
  MacOS/falos-desktop        Tauri shell; web/wasm embedded (custom-protocol)
  MacOS/falos-host           sidecar: MCP + sync; spawns + supervises the runtime
  Resources/node/            Node 22 (bin/node, bin/npm, bin/npx, lib/node_modules/npm, LICENSE)
  Resources/runtime/         dist/main.mjs, dist/setup.mjs, scripts/{motion-qa.mjs,install-skills.sh},
                             motion-template/, hyperframes-template/ (sources), package.json
  Resources/icon.icns, Info.plist (ai.fal.os, macOS 26+)
```

**How it runs** (`apps/desktop/src-tauri/src/sidecar.rs`):
- **Startup:** on launch (off the main thread) the app reads the login shell's PATH (`$SHELL -ilc`), plus Homebrew, `~/.local/bin` and volta, with the bundled Node first.
- **Host start:** it starts `falos-host serve --runtime Resources/runtime` in its own process group. The env carries `FALOS_HOME`, `FALOS_PACKAGED=1`, `FALOS_NODE`, the template and skills dirs, `FAL_KEY` from the Keychain, and `FALOS_EXIT_WITH_PARENT=1`. Output goes to `~/.fal-os/logs/host.log`.
- **Runtime start:** when `FALOS_NODE` is set and `dist/main.mjs` exists, the host starts the runtime as `node dist/main.mjs` (`crates/host/src/runtime.rs`).
- **Port:** the host picks 7317 or the next free port and writes `host.json`. The `host_info` command waits until `host.json` is the one *its* child wrote (pid match), so the UI never connects to a stale port or to someone else's host.
- **Quit:** `RunEvent::Exit` SIGTERMs the host's group (and the setup helper's), waits 5 s, then SIGKILLs. The host stops the runtime's group.
- **Crash:** if the app dies, the host notices it was reparented and exits (`FALOS_EXIT_WITH_PARENT`), and the runtime follows its host (`FALOS_HOST_PID`).
- **Setup commands:** `setup_status`, `check_fal_key`, `save_fal_key`, `forget_fal_key`, `install_extras` and `restart_host` call `node dist/setup.mjs doctor|check-key|install` or the Keychain. `setup_run` / `setup_task` / `setup_cancel` run the fixed install and sign-in tasks (§2.5).
- **Dev:** `cargo run` from a checkout spawns nothing and finds your own `falos-host serve` as before.

**Verified** (DMG installed into a temp "Applications" dir; launched with a Finder-like minimal env, `PATH=/usr/bin:/bin:/usr/sbin:/sbin`, and `FALOS_HOME`, `FALOS_PROJECTS` and the Keychain service all pointed at test locations):
- `xattr -dr com.apple.quarantine` clears the quarantine set by a simulated browser download.
- Process tree: `falos-desktop` → `falos-host` (own pgid) → `node dist/main.mjs` (own pgid).
- The host chose **7318**, because the user's host was on 7317 (left untouched), and wrote the temp `host.json`.
- "agent runtime connected". Capabilities found `claude` and `ffmpeg` through the login-shell PATH, and missing items point to "fal-os → Setup".
- The UI loaded from the embedded frontend, connected to the sidecar host, and the **setup screen opened automatically** (no key in the temp home). Claude was detected as signed in.
- Setup helper from the bundle:
  - `check-key` with a bogus key → models `401 key rejected`, agent `401`.
  - `install skills` fetched both skill packs from GitHub tarballs (22 MB, `.claude-plugin/plugin.json` present).
  - `install hyperframes` ran the **bundled npm** (78 packages, 126 MB).
  - After a relaunch the runtime reported `remotion-skills` and `hyperframes-skills` as available.
- Quit (`quit app id "ai.fal.os"`) → host and runtime gone, nothing left running.
- SIGKILL of the app → host logs "parent process gone; shutting down", and the runtime exits.

**Not verified:**
- Typing a real key into the setup screen and the Keychain write from the GUI. No GUI automation was run on the user's desktop, and no real key was used.
- The Remotion template install + `remotion browser ensure` (~700 MB, skipped for disk). It is the same code path as the HyperFrames install.

---

## 5. Install instructions for colleagues (ad-hoc build)

> fal-os is an internal experiment. Apple Silicon Mac with **macOS 26 (Tahoe)** or newer.

The short version for colleagues is [INSTALL.md](INSTALL.md).

1. **Before you start**: a fal account (for a key with the **AGENT** preset) and a Claude account. Nothing to install by hand.
2. Open `fal-os-<version>-arm64.dmg` and drag **fal-os** to **Applications**.
3. This build isn't notarized yet, so tell macOS you trust it (once per download):
   ```sh
   xattr -dr com.apple.quarantine /Applications/fal-os.app
   ```
   Without Terminal: open it once, dismiss the warning, then go to System Settings → Privacy & Security → "fal-os was blocked…" → **Open Anyway**.
4. Open fal-os and follow the setup screen: open the keys page, paste your fal key (stored in your Keychain), click [install] and [sign in] for Claude Code. ffmpeg and the motion tools (~0.9 GB into `~/.fal-os`) download by themselves after the key is saved. When it says "ready", press [start →].
   macOS may ask whether fal-os may use the "ai.fal.os" Keychain item. Click **Always Allow** (it asks again after each update until builds are notarized).
5. Your projects live in `~/fal-os-projects`. To connect Claude Code or Codex to the canvas, use ⌘K → "connect agent".
6. Problems? Send `~/.fal-os/logs/host.log` and `~/.fal-os/logs/setup.log`.

**Update:** quit fal-os, replace the app with the new DMG's copy, and run the `xattr` command again.
**Uninstall:** delete the app, `~/.fal-os` (settings, templates, logs) and, if you want, `~/fal-os-projects`.
The Keychain item is "ai.fal.os" in Keychain Access.

---

## 6. Later: Developer ID signing + notarization

There is no signing identity on the build machine today (`security find-identity -v -p codesigning` → 0).

1. **Get a certificate:**
   - fal enrolls in the Apple Developer Program as an organization, which gives a Team ID.
   - The Account Holder or an Admin creates a **Developer ID Application** certificate (Certificates → +).
   - Install it in the build Mac's keychain, or export a `.p12` for CI (`security import cert.p12 -k build.keychain -P … -T /usr/bin/codesign`).
2. **Sign:**
   ```sh
   SIGN_IDENTITY="Developer ID Application: <fal legal name> (<TEAMID>)" scripts/package-macos.sh
   ```
   With a real identity the script signs with `--options runtime --timestamp`, inside out: node, then falos-host, then the app, then the DMG.
   - **App entitlements** (`apps/desktop/src-tauri/entitlements.plist`): none. WKWebView JITs JS and **wasm** in WebKit's own WebContent process, so the app needs neither `allow-jit` nor `allow-unsigned-executable-memory`. It's not sandboxed.
   - **Node entitlements** (`node-entitlements.plist`): `cs.allow-jit` and `cs.allow-unsigned-executable-memory` for V8, plus `cs.disable-library-validation` for native addons that npm installs on first run.
   - We re-sign node with our identity. Its original OpenJS signature would also notarize, but one team is simpler.
3. **Notarize + staple:**
   ```sh
   # once: store credentials (app-specific password, or an App Store Connect API key)
   xcrun notarytool store-credentials fal-os --apple-id <you@fal.ai> --team-id <TEAMID> --password <app-specific-password>
   #   or: --key AuthKey_<KEYID>.p8 --key-id <KEYID> --issuer <ISSUER-UUID>
   xcrun notarytool submit target/package/fal-os-0.1.0-arm64.dmg --keychain-profile fal-os --wait
   xcrun notarytool log <submission-id> --keychain-profile fal-os   # if it's rejected
   xcrun stapler staple target/package/fal-os-0.1.0-arm64.dmg
   spctl -a -vv -t install target/package/fal-os-0.1.0-arm64.dmg   # → accepted, Notarized Developer ID
   ```
   The DMG contains the signed app, so notarizing the DMG covers both. Staple the DMG; to also
   staple the app, notarize a zip of it first, staple it, then build the DMG.
4. **With `cargo tauri build` instead:**
   - Set `bundle.macOS.signingIdentity`, `bundle.macOS.entitlements` and `bundle.macOS.hardenedRuntime: true` (the default).
   - Export `APPLE_ID`, `APPLE_PASSWORD` and `APPLE_TEAM_ID`, or `APPLE_API_KEY`, `APPLE_API_ISSUER` and `APPLE_API_KEY_PATH`. Tauri signs the `externalBin` sidecars and notarizes automatically.
   - Sign `Resources/node/bin/node` with its entitlements before bundling, or in a `beforeBundleCommand`.
5. **Updater keys** (see §2.2): run `cargo tauri signer generate -w ~/.tauri/fal-os.key`.
   - The public key goes in `tauri.conf.json` under `plugins.updater.pubkey`.
   - The private key and its password go in CI secrets. Never commit them.
   - If the private key is lost, installed apps can't take updates any more: they need a manual reinstall.

---

## 7. Known gaps (POC)
- Ad-hoc signature: Gatekeeper needs the `xattr` step, and the Keychain asks again after each update.
- No auto-update; no single-instance guard. Two app instances start two hosts on different ports, and the last one owns `~/.fal-os/host.json`. The same applies if a colleague also runs a dev `falos-host serve` with the default `FALOS_HOME`.
- MCP registration writes an HTTP URL with the current port (§2.6).
- ffmpeg isn't bundled; it's a ~56 MB first-run download (§2.3). x86 Macs would need `brew install ffmpeg` (the download is arm64 only).
- `claude auth login` from the app is wired and tested with a stub, but a real browser sign-in from the packaged app hasn't been run yet. If the CLI ever insists on a TTY, the screen shows the Terminal command as a fallback.
- Before a key is saved, `host.log` says "run `falos-host login`", which is a dev hint. The host's own `generate` tool picks up `FAL_KEY` from the env once a key is saved.
- The DMG has no background or window layout. It uses the version from `tauri.conf.json` and the git sha as `CFBundleVersion`.
- Node is taken from the build machine (`NODE_DIST`, default: the nvm Node on PATH). CI should download a pinned official tarball and verify its SHASUMS.
