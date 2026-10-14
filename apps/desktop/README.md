# fal-os desktop (Tauri v2)

A minimal Tauri v2 shell (WKWebView on macOS) around the canvas app in `../web` (devUrl `http://127.0.0.1:5173`, frontendDist `apps/web/dist`). The engine spike in `/web` can still be loaded with `FALOS_URL`. The Rust side in `src-tauri` is its own Cargo workspace, so the root `cargo build` is unaffected. Only the Xcode Command Line Tools are needed; tauri-cli is optional.

## Run (dev: loads devUrl http://127.0.0.1:5173)
```sh
pnpm --dir apps/web dev &            # wasm + Vite dev server on 5173
cargo run --manifest-path apps/desktop/src-tauri/Cargo.toml
```
The bundled build below needs `pnpm --dir apps/web build` first (frontendDist = `apps/web/dist`).

Engine spike instead: `./web/build.sh && python3 -I web/serve.py 8789 &`, then
`FALOS_URL=http://127.0.0.1:8789/ cargo run --manifest-path apps/desktop/src-tauri/Cargo.toml`.

## Run with the bundled frontend (tauri://localhost, no server)
```sh
cargo run --release --features custom-protocol --manifest-path apps/desktop/src-tauri/Cargo.toml
```
To make a `.app`: `cargo install tauri-cli --version "^2" --locked`, then run `cargo tauri build` in `apps/desktop/src-tauri`.

## Packaged app (DMG for colleagues)
`scripts/package-macos.sh` builds `fal-os.app` and an ad-hoc signed DMG. The bundle contains the shell with the embedded frontend, `falos-host` as a sidecar, a Node 22 runtime and the esbuild-bundled agent runtime. Install steps, signing and notarization are in [docs/PACKAGING.md](../../docs/PACKAGING.md).

In a packaged bundle, `src/sidecar.rs` does the following:
- Starts `falos-host serve` (and through it the agent runtime) on launch, and stops its process group on quit.
- Exposes setup commands for the web UI's first-run screen: `setup_status`, `check_fal_key`, `save_fal_key` (Keychain, fallback `$FALOS_HOME/fal.env`), `forget_fal_key`, `install_extras` and `restart_host`.

`cargo run` from a checkout spawns nothing (no bundled runtime next to the binary), and `host_info` reads `host.json` as before.

Env overrides:
- `FALOS_HOME`: settings, token, templates and logs.
- `FALOS_PROJECTS`: workspace folder instead of `~/fal-os-projects`.
- `FALOS_KEYCHAIN_SERVICE`: Keychain service name.
- `FALOS_NO_KEYCHAIN=1`: file fallback only.
- `FALOS_NO_SIDECAR=1`: don't start the host.
- `FALOS_HOST_BIN=<falos-host>`: a checkout build starts and supervises that host (no agent runtime), e.g. to test restarts with `FALOS_PORT` / `FALOS_PROJECTS` / `FALOS_HOME` of your own.
- `FALOS_TEST_PANIC_APP=1`: panic at start (checks the crash report path).

The packaged host is supervised: restarted when it dies (≤ 3 times in 10 minutes), then the UI shows `host stopped · [restart] [copy report] [open logs]`. Logs live in `$FALOS_HOME/logs` (`host.log` rotated 5 × 5 MB, `app.log`, `setup.log`, `crash-*.txt`); "View Logs" in the app menu opens it.

## Bench / probe env vars
- `FALOS_URL=http://127.0.0.1:8789/?bench` loads any URL. `tauri://localhost/?bench` also works with `custom-protocol`.
- `FALOS_PROBE=http://127.0.0.1:8789/bench` POSTs WebGPU availability, page state and the bench result to serve.py.
- `FALOS_ON_TOP=1` keeps the window on top. An occluded WKWebView pauses rAF, which stalls unattended benches.

```sh
FALOS_ON_TOP=1 FALOS_PROBE=http://127.0.0.1:8789/bench FALOS_URL="http://127.0.0.1:8789/?bench" \
  apps/desktop/src-tauri/target/release/falos-desktop
```
Results are in `docs/spike-results.md` under "Tauri / WKWebView".
