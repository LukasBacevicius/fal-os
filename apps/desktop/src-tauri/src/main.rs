//! fal-os desktop shell: a Tauri v2 window (WKWebView on macOS) hosting the web UI.
//!
//! Spike/dev env overrides:
//! - `FALOS_URL=http://127.0.0.1:8789/?bench` loads an explicit URL instead of devUrl / the bundled frontend.
//! - `FALOS_PROBE=<endpoint>` injects a script that POSTs WebGPU availability, then page state and the
//!   bench result (5 s / 16 s after load), as JSON to `<endpoint>` (e.g. `http://127.0.0.1:8789/bench`,
//!   which web/serve.py appends to web/bench-results.jsonl).
//! - `FALOS_ON_TOP=1` keeps the window above others: an occluded WKWebView reports
//!   `visibilityState = hidden` and stops requestAnimationFrame, which stalls unattended benches.

use tauri::{WebviewUrl, WebviewWindowBuilder};

const PROBE: &str = r#"
addEventListener('load', async () => {
  const post = (r) => fetch(__ENDPOINT__, { method: 'POST', body: JSON.stringify({ origin: location.origin, ...r }) });
  const r = { probe: 'webgpu', has_gpu: !!navigator.gpu, secure: isSecureContext,
              webcodecs: typeof VideoDecoder === 'function' };
  try {
    const a = navigator.gpu && await navigator.gpu.requestAdapter();
    r.adapter = !!a;
    if (a) {
      r.features = [...a.features];
      r.max_texture_2d = a.limits.maxTextureDimension2D;
      r.device = !!(await a.requestDevice());
    }
  } catch (e) { r.error = String(e); }
  post(r);
  for (const t of [5, 16]) setTimeout(() => post({
    probe: 'state', t, visibility: document.visibilityState,
    stats: document.getElementById('stats')?.textContent, bench: window.__falosBench ?? null }), t * 1000);
});
"#;

mod sidecar;

use std::sync::Arc;

use sidecar::Sidecar;
use tauri::{Manager, RunEvent, State};

type Side<'a> = State<'a, Arc<Sidecar>>;

/// The local fal-os host's discovery info (`$FALOS_HOME/host.json`, default `~/.fal-os`):
/// `{port, token, project}` for the web app's sync connection, or null when no host runs.
/// In the packaged app this is the host the app started (it waits for it to come up).
#[tauri::command(async)]
fn host_info(side: Side) -> Option<serde_json::Value> {
    side.host_info()
}

/// First-run setup state: claude / codex / ffmpeg / fal key / motion tools / disk.
#[tauri::command(async)]
fn setup_status(side: Side) -> serde_json::Value {
    side.status()
}

#[tauri::command(async)]
fn save_fal_key(side: Side, key: String) -> Result<&'static str, String> {
    side.save_key(&key)
}

#[tauri::command(async)]
fn forget_fal_key(side: Side) -> Result<(), String> {
    side.forget_key()
}

/// Whether a key (the given one, else the stored one) works for the fal Agent and queue APIs.
#[tauri::command(async)]
fn check_fal_key(side: Side, key: Option<String>) -> Result<serde_json::Value, String> {
    side.check_key(key.as_deref())
}

#[tauri::command(async)]
fn install_extras(side: Side) -> Result<(), String> {
    side.install()
}

#[tauri::command(async)]
fn restart_host(side: Side) -> Result<(), String> {
    side.restart()
}

fn main() {
    let side = Arc::new(Sidecar::new());
    tauri::Builder::default()
        .manage(side.clone())
        .invoke_handler(tauri::generate_handler![
            host_info,
            setup_status,
            save_fal_key,
            forget_fal_key,
            check_fal_key,
            install_extras,
            restart_host
        ])
        .setup(|app| {
            // Packaged app: start the host (and through it the agent runtime) off the main thread.
            let s = app.state::<Arc<Sidecar>>().inner().clone();
            std::thread::spawn(move || {
                if let Err(e) = s.start() {
                    eprintln!("fal-os: {e}");
                }
            });
            let url = match std::env::var("FALOS_URL") {
                Ok(u) => WebviewUrl::External(u.parse()?),
                Err(_) => WebviewUrl::default(),
            };
            let mut win = WebviewWindowBuilder::new(app, "main", url)
                .title("fal-os")
                .inner_size(1440.0, 900.0);
            if let Ok(endpoint) = std::env::var("FALOS_PROBE") {
                let endpoint = format!("{endpoint:?}"); // Debug-quoting yields a valid JS string literal
                win = win.initialization_script(PROBE.replace("__ENDPOINT__", &endpoint));
            }
            if std::env::var_os("FALOS_ON_TOP").is_some() {
                win = win.always_on_top(true).focused(true);
            }
            win.build()?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building fal-os")
        .run(move |_, event| {
            if let RunEvent::Exit = event {
                side.stop();
            }
        });
}
