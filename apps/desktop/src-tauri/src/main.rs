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
//! Background layer (docs/PACKAGING.md §4): closing the window (⌘W, red button) only hides it;
//! the host and runtime keep running and the menu bar item (`tray.rs`) shows what agents do.
//! ⌘Q / tray "quit" really quits (confirming when runs are active). `--hidden` (open at login)
//! starts with the window hidden.
//!
//! "Out" commands (docs/ASSETS-V1.md §7): `drag_out`, `copy_files`, `copy_image`,
//! `open_with_list`, `open_with`, `reveal`, `materialize` (see `out.rs`).

mod brand;
mod clip;
mod notify;
mod out;
mod tray;

use std::path::PathBuf;

use tauri::{Emitter, WebviewUrl, WebviewWindowBuilder};
use tauri::menu::{Menu, MenuItem, MenuItemKind};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};

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

/// Move a key an older build kept in the Keychain into `fal.env` (one macOS prompt, on click).
#[tauri::command(async)]
fn migrate_fal_key(side: Side) -> Result<&'static str, String> {
    side.migrate_key()
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

/// Setup screen tasks with streamed output: `claude-install`, `claude-login`, `ffmpeg`,
/// `codex-install`, `codex-login` (a fixed list; see `Sidecar::run_task`).
#[tauri::command(async)]
fn setup_run(side: Side, task: String) -> Result<(), String> {
    side.inner().run_task(&task)
}

#[tauri::command(async)]
fn setup_task(side: Side, task: String) -> serde_json::Value {
    side.task(&task)
}

#[tauri::command(async)]
fn setup_cancel(side: Side, task: String) {
    side.cancel_task(&task)
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

/// Dock icon follows the web app's working state: lit while agents work, dimmed when idle.
#[tauri::command]
fn set_working(app: tauri::AppHandle, working: bool) -> Result<(), String> {
    app.run_on_main_thread(move || brand::set_dock_icon(working)).map_err(|e| e.to_string())
}

/// Show and focus the main window (Dock click, tray, notification, `open -a fal-os`). The
/// webview was only ordered out, so the canvas, its WebGPU device and the sync connections are
/// all still there: no reload, no flash.
pub fn show_main(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// A DOM event in the web app (`window.dispatchEvent(new CustomEvent(name, {detail}))`).
pub fn web_event(app: &tauri::AppHandle, name: &str, detail: &serde_json::Value) {
    if let Some(w) = app.get_webview_window("main") {
        let js = format!("window.dispatchEvent(new CustomEvent({}, {{ detail: {detail} }}))", serde_json::Value::from(name));
        let _ = w.eval(js);
    }
}

pub fn open_chat(app: &tauri::AppHandle, project: &str, chat: &str) {
    show_main(app);
    web_event(app, "falos:open-chat", &serde_json::json!({ "project": project, "chat": chat }));
}

/// ⌘Q / tray "quit": confirm when agent runs or generations would be cut off, then exit (the
/// host's process group gets SIGTERM in `RunEvent::Exit`).
pub fn request_quit(app: &tauri::AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        if let Some(what) = tray::status().as_ref().and_then(tray::active_summary) {
            show_main(&app);
            let quit = app
                .dialog()
                .message(format!("{what}. quit anyway?"))
                .title("quit fal-os")
                .buttons(MessageDialogButtons::OkCancelCustom("quit".into(), "cancel".into()))
                .blocking_show();
            if !quit {
                return;
            }
        }
        app.exit(0);
    });
}

/// The default macOS menu with our own Quit (⌘Q) in place of the predefined one, which would
/// terminate without asking.
fn app_menu(app: &tauri::AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let menu = Menu::default(app)?;
    if let Some(MenuItemKind::Submenu(sub)) = menu.items()?.first() {
        let n = sub.items()?.len();
        if n > 0 {
            sub.remove_at(n - 1)?;
        }
        sub.append(&MenuItem::with_id(app, "app:quit", "Quit fal-os", true, Some("CmdOrCtrl+Q"))?)?;
    }
    Ok(menu)
}

fn main() {
    let side = Arc::new(Sidecar::new());
    let hidden = std::env::args().any(|a| a == "--hidden");
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_autostart::Builder::new()
                .args(["--hidden"])
                .macos_launcher(tauri_plugin_autostart::MacosLauncher::LaunchAgent)
                .build(),
        )
        .menu(app_menu)
        .on_menu_event(|app, e| match e.id().as_ref() {
            "app:quit" => request_quit(app),
            id => tray::on_menu(app, id),
        })
        .on_window_event(|w, e| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = e
                && w.label() == "main"
            {
                api.prevent_close();
                let _ = w.hide();
            }
        })
        .manage(side.clone())
        .invoke_handler(tauri::generate_handler![
            host_info,
            materialize,
            drag_out,
            copy_files,
            copy_image,
            open_with_list,
            open_with,
            reveal,
            setup_status,
            save_fal_key,
            migrate_fal_key,
            forget_fal_key,
            check_fal_key,
            install_extras,
            restart_host,
            setup_run,
            setup_task,
            setup_cancel,
            set_working,
            clip::clipboard_files,
            clip::read_clip_file,
            clip::clipboard_text
        ])
        .setup(move |app| {
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
                .title("fal OS")
                .inner_size(1440.0, 900.0)
                // Let drops reach the page as HTML5 drag events with File objects (Tauri's own
                // handler would swallow them; the canvas does its own drop import).
                .disable_drag_drop_handler()
                .visible(!hidden);
            if let Ok(endpoint) = std::env::var("FALOS_PROBE") {
                let endpoint = format!("{endpoint:?}"); // Debug-quoting yields a valid JS string literal
                win = win.initialization_script(PROBE.replace("__ENDPOINT__", &endpoint));
            }
            if std::env::var_os("FALOS_ON_TOP").is_some() {
                win = win.always_on_top(true).focused(true);
            }
            win.build()?;
            tray::start(app.handle())?;
            let h = app.handle().clone();
            notify::init(move |project, chat| {
                let h2 = h.clone();
                let _ = h.run_on_main_thread(move || open_chat(&h2, &project, &chat));
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building fal-os")
        .run(move |app, event| match event {
            // Dock click / `open -a fal-os` while running.
            RunEvent::Reopen { .. } => show_main(app),
            RunEvent::Exit => side.stop(),
            _ => {}
        });
}
