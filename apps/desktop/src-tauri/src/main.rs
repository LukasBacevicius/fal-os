//! fal-os desktop shell: a Tauri v2 window (WKWebView on macOS) hosting the web UI.
//!
//! Spike/dev env overrides:
//! - `FALOS_URL=http://127.0.0.1:8789/?bench` loads an explicit URL instead of devUrl / the bundled frontend.
//! - `FALOS_PROBE=<endpoint>` injects a script that POSTs WebGPU availability, then page state and the
//!   bench result (5 s / 16 s after load), as JSON to `<endpoint>` (e.g. `http://127.0.0.1:8789/bench`,
//!   which web/serve.py appends to web/bench-results.jsonl).
//! - `FALOS_ON_TOP=1` keeps the window above others: an occluded WKWebView reports
//!   `visibilityState = hidden` and stops requestAnimationFrame, which stalls unattended benches.
//!
//! "Out" commands (docs/ASSETS-V1.md §7): `drag_out`, `copy_files`, `copy_image`,
//! `open_with_list`, `open_with`, `reveal`, `materialize` (see `out.rs`).

mod out;

use std::path::PathBuf;

use tauri::{Emitter, WebviewUrl, WebviewWindowBuilder};

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

/// `materialize(sha|node, name?)` → absolute path of a human-named APFS clone in
/// `<project>/.out/`. With only a node id the host resolves it (`POST /api/materialize`).
#[tauri::command]
async fn materialize(
    sha: Option<String>,
    node: Option<String>,
    name: Option<String>,
    project: Option<String>,
    canvas: Option<String>,
) -> Result<String, String> {
    let path = match (sha, node) {
        (Some(sha), _) => out::materialize(&sha, name.as_deref(), project.as_deref())?,
        (None, Some(node)) => out::materialize_via_host(
            &serde_json::json!({"node": node, "name": name, "canvas": canvas}),
            project.as_deref(),
        )?,
        (None, None) => return Err("materialize needs sha or node".into()),
    };
    Ok(path.to_string_lossy().into_owned())
}

/// Starts a native drag of files out of the window (drag-rs: file URLs). Must be called while
/// the mouse button is still down (the UI calls it when the pointer leaves the window during
/// a node drag, from the selection grip, or from a chat file card), with paths prepared by
/// `materialize` beforehand. `icon`: an image file for the preview; default: the file itself
/// (images) or its Finder icon.
#[tauri::command]
async fn drag_out(
    app: tauri::AppHandle,
    window: tauri::Window,
    paths: Vec<String>,
    icon: Option<String>,
) -> Result<(), String> {
    let files = out::checked_paths(&paths)?;
    let (tx, rx) = std::sync::mpsc::channel();
    let emitter = app.clone();
    app.run_on_main_thread(move || {
        let preview = icon
            .map(PathBuf::from)
            .filter(|p| p.is_file())
            .and_then(|p| out::drag_icon(std::slice::from_ref(&p), 128.0))
            .or_else(|| out::drag_icon(&files, 128.0))
            .unwrap_or_else(|| include_bytes!("../icons/drag.png").to_vec());
        let r = drag::start_drag(
            &window,
            drag::DragItem::Files(files),
            drag::Image::Raw(preview),
            move |result, _pos| {
                let dropped = matches!(result, drag::DragResult::Dropped);
                let _ = emitter.emit("falos://drag-end", serde_json::json!({ "dropped": dropped }));
            },
            drag::Options::default(),
        );
        let _ = tx.send(r.map_err(|e| e.to_string()));
    })
    .map_err(|e| e.to_string())?;
    rx.recv().map_err(|e| e.to_string())?
}

/// File URLs on the general pasteboard: ⌘V pastes the files in Finder, Slack, Premiere, Mail.
#[tauri::command]
fn copy_files(paths: Vec<String>) -> Result<(), String> {
    out::copy_files(&out::checked_paths(&paths)?)
}

/// The image as PNG (+ TIFF) on the general pasteboard.
#[tauri::command]
fn copy_image(path: String) -> Result<(), String> {
    let p = out::checked_paths(std::slice::from_ref(&path))?;
    out::copy_image(&p[0])
}

/// Apps that can open `path`: pinned creative apps first (when installed), then what Launch
/// Services offers; `default` marks Finder's choice.
#[tauri::command]
fn open_with_list(path: String) -> Result<Vec<out::AppInfo>, String> {
    let p = out::checked_paths(std::slice::from_ref(&path))?;
    Ok(out::open_with_list(&p[0]))
}

#[tauri::command]
fn open_with(path: String, app_path: Option<String>, bundle_id: Option<String>) -> Result<(), String> {
    let p = out::checked_paths(std::slice::from_ref(&path))?;
    let app = app_path.or(bundle_id).ok_or("open_with needs app_path or bundle_id")?;
    out::open_with(&p[0], &app)
}

/// Reveal in Finder (tauri-plugin-opener).
#[tauri::command]
fn reveal(path: String) -> Result<(), String> {
    let p = out::checked_paths(std::slice::from_ref(&path))?;
    tauri_plugin_opener::reveal_item_in_dir(&p[0]).map_err(|e| e.to_string())
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            host_info,
            materialize,
            drag_out,
            copy_files,
            copy_image,
            open_with_list,
            open_with,
            reveal
        ])
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
