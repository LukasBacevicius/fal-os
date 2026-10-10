# fal-os desktop (Tauri v2)

A minimal Tauri v2 shell (WKWebView on macOS) around the web UI in `../../web`. The Rust side in `src-tauri` is its own Cargo workspace, so the root `cargo build` is unaffected. Only the Xcode Command Line Tools are needed; tauri-cli is optional.

## Run (dev: loads devUrl http://127.0.0.1:8789)
```sh
./web/build.sh                       # build the wasm into web/pkg
python3 -I web/serve.py 8789 &       # devUrl; also collects POST /bench into web/bench-results.jsonl
cargo run --manifest-path apps/desktop/src-tauri/Cargo.toml
```

## Run with the bundled frontend (tauri://localhost, no server)
```sh
cargo run --release --features custom-protocol --manifest-path apps/desktop/src-tauri/Cargo.toml
```
To make a `.app`: `cargo install tauri-cli --version "^2" --locked`, then run `cargo tauri build` in `apps/desktop/src-tauri`.

## Bench / probe env vars
- `FALOS_URL=http://127.0.0.1:8789/?bench` loads any URL. `tauri://localhost/?bench` also works with `custom-protocol`.
- `FALOS_PROBE=http://127.0.0.1:8789/bench` POSTs WebGPU availability, page state and the bench result to serve.py.
- `FALOS_ON_TOP=1` keeps the window on top. An occluded WKWebView pauses rAF, which stalls unattended benches.

```sh
FALOS_ON_TOP=1 FALOS_PROBE=http://127.0.0.1:8789/bench FALOS_URL="http://127.0.0.1:8789/?bench" \
  apps/desktop/src-tauri/target/release/falos-desktop
```
Results are in `docs/spike-results.md` under "Tauri / WKWebView".
