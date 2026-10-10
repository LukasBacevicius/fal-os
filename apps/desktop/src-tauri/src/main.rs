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

/// The local fal-os host's discovery file (`~/.fal-os/host.json`, `$FALOS_HOME/host.json`):
/// `{port, token, project}` for the web app's sync connection, or null when no host runs.
#[tauri::command]
fn host_info() -> Option<serde_json::Value> {
    let home = std::env::var_os("FALOS_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".fal-os")))?;
    let v: serde_json::Value =
        serde_json::from_slice(&std::fs::read(home.join("host.json")).ok()?).ok()?;
    Some(serde_json::json!({
        "port": v.get("port")?,
        "token": v.get("token")?,
        "project": v.get("project"),
    }))
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![host_info])
        .setup(|app| {
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
        .run(tauri::generate_context!())
        .expect("error while running fal-os");
}
