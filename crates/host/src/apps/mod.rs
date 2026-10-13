//! App nodes: live web apps on the canvas (docs/ARCHITECTURE.md "App nodes").
//!
//! - **Storage.** An app is a folder of plain web files, `<project>/apps/<app>/` (`index.html`
//!   plus ES modules, CSS, assets; CDN imports with pinned versions). No build step.
//! - **Versions.** Every committed change copies the folder to `apps/.history/<app>/<n>/` and
//!   sets the node's `version` prop to `n` as the writing agent's actor. Undo / turn revert
//!   rolls the prop back like any other doc change; [`reconcile`] then restores that
//!   snapshot's files. Redo works the same way.
//! - **Serving.** A second loopback listener (no token: the origin has no privileges) serves
//!   `/<project>/<app>/<path>` with no caching and CORS `*` (opaque-origin iframes load ES
//!   modules cross-origin). HTML gets a small guard script: audio starts muted, console errors
//!   and uncaught exceptions are posted back (`app_logs`), Esc is forwarded to the canvas.
//! - **Live reload.** Writes through the tools, and files changed by hand or by Codex (a
//!   500 ms poll), commit a version and broadcast `{"type":"app"}` to the project's clients.
//! - **Check.** After a change a headless Chrome loads the app (muted), collects console
//!   errors and exceptions and takes a screenshot: the node's `asset` (the still shown when
//!   zoomed out or offscreen).

pub mod cdp;
pub mod serve;
pub mod tools;

use std::collections::{HashMap, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};
use falos_doc::{Actor, Command, Node, NodeKind, Props};
use serde::Serialize;
use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};

use crate::host::{Event, Host, now_ms};
use crate::mcp::PlacementArgs;

/// Limits per write: files, bytes per file, bytes in total.
pub const MAX_FILES: usize = 300;
pub const MAX_FILE_BYTES: usize = 4 << 20;
pub const MAX_APP_BYTES: u64 = 40 << 20;
const MAX_LOGS: usize = 300;
const POLL: Duration = Duration::from_millis(500);
/// An agent session that touched an app owns external edits (Codex writing files) this long.
const OWNER_TTL_MS: i64 = 30 * 60 * 1000;
/// Actor of the derived snapshot still (kept off the agents' undo stacks).
const SNAPSHOT_ACTOR: &str = "fal-os";

static BASE: Mutex<Option<String>> = Mutex::new(None);

/// The app server's base URL (`http://127.0.0.1:<port>`), once it listens.
pub fn base_url() -> Option<String> {
    BASE.lock().unwrap().clone()
}

pub fn set_base_url(url: String) {
    *BASE.lock().unwrap() = Some(url);
}

/// `http://127.0.0.1:<port>/<project>/<app>/<entry>`.
pub fn app_url(host: &Host, app: &str, entry: &str) -> Option<String> {
    Some(format!("{}/{}/{app}/{}", base_url()?, host.id, entry.trim_start_matches('/')))
}

/// One console line / exception from a running app.
#[derive(Clone, Debug, Serialize)]
pub struct LogEntry {
    /// `error` | `warning` | `log` | `exception`.
    pub level: String,
    pub text: String,
    /// `preview` (a canvas / preview iframe, an opened tab) or `check` (headless Chrome).
    pub source: String,
    pub version: u64,
    pub at: i64,
}

impl LogEntry {
    pub fn is_error(&self) -> bool {
        matches!(self.level.as_str(), "error" | "exception")
    }
}

#[derive(Default)]
struct AppState {
    /// The version whose files are on disk.
    version: u64,
    /// Fingerprint of the files on disk (paths, sizes, mtimes).
    sig: u64,
    logs: VecDeque<LogEntry>,
    /// (session, actor, ms): the agent that last worked on the app.
    owner: Option<(String, String, i64)>,
    /// Version the last headless check ran on, and its note (no browser, timeout, …).
    checked: u64,
    note: Option<String>,
}

/// Per-project app registry (on [`Host`]).
#[derive(Default)]
pub struct Apps {
    state: Mutex<HashMap<String, AppState>>,
    /// Serializes file commits (tool writes, the poller, reverts).
    write: tokio::sync::Mutex<()>,
    /// One headless check per app at a time; a change during a check queues one more.
    checks: Mutex<HashMap<String, bool>>,
    started: std::sync::OnceLock<()>,
}

// ---- paths ------------------------------------------------------------------------------

pub fn valid_app_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 48
        && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !id.starts_with('-')
}

pub fn apps_dir(host: &Host) -> PathBuf {
    host.dir().join("apps")
}

pub fn app_dir(host: &Host, id: &str) -> PathBuf {
    apps_dir(host).join(id)
}

fn history_dir(host: &Host, id: &str) -> PathBuf {
    apps_dir(host).join(".history").join(id)
}

/// Snapshot stills (served like assets; kept out of `assets/` and the library).
pub fn snapshots_dir(dir: &Path) -> PathBuf {
    dir.join("apps").join(".snapshots")
}

/// A relative file path inside an app: no absolute paths, `..`, hidden segments or `node_modules`.
pub fn safe_rel(path: &str) -> Result<PathBuf> {
    let p = path.trim().trim_start_matches("./");
    if p.is_empty() || p.len() > 240 || p.contains('\\') || p.contains('\0') {
        bail!("bad file path `{path}`");
    }
    let mut out = PathBuf::new();
    for c in Path::new(p).components() {
        match c {
            Component::Normal(s) => {
                let s = s.to_string_lossy();
                if s.starts_with('.') || s == "node_modules" {
                    bail!("`{path}`: hidden files and node_modules aren't app files");
                }
                out.push(s.as_ref());
            }
            _ => bail!("`{path}` must be a relative path inside the app"),
        }
    }
    Ok(out)
}

fn walk(dir: &Path, rel: &Path, depth: usize, out: &mut Vec<(String, u64, u128)>) {
    if depth > 10 || out.len() > 2000 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || name == "node_modules" {
            continue;
        }
        let Ok(md) = e.metadata() else { continue };
        let r = rel.join(&name);
        if md.is_dir() {
            walk(&e.path(), &r, depth + 1, out);
        } else if md.is_file() {
            let mtime = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            out.push((r.to_string_lossy().replace('\\', "/"), md.len(), mtime));
        }
    }
}

/// Files of an app folder: (relative path, size, mtime ns), sorted.
pub fn files_of(dir: &Path) -> Vec<(String, u64, u128)> {
    let mut out = vec![];
    walk(dir, Path::new(""), 0, &mut out);
    out
}

fn fingerprint(dir: &Path) -> u64 {
    let mut h = Sha256::new();
    for (p, size, mtime) in files_of(dir) {
        h.update(p.as_bytes());
        h.update(size.to_le_bytes());
        h.update(mtime.to_le_bytes());
    }
    u64::from_le_bytes(h.finalize()[..8].try_into().unwrap())
}

fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for (rel, _, _) in files_of(src) {
        let to = dst.join(&rel);
        if let Some(p) = to.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::copy(src.join(&rel), &to).with_context(|| format!("copying {rel}"))?;
    }
    Ok(())
}

/// Atomically exchange two paths (macOS `RENAME_SWAP`, Linux `RENAME_EXCHANGE`); false when
/// the platform or file system can't.
fn exchange(a: &Path, b: &Path) -> bool {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let (Ok(ca), Ok(cb)) = (CString::new(a.as_os_str().as_bytes()), CString::new(b.as_os_str().as_bytes())) else {
            return false;
        };
        #[cfg(target_os = "macos")]
        let r = unsafe { libc::renamex_np(ca.as_ptr(), cb.as_ptr(), libc::RENAME_SWAP) };
        #[cfg(target_os = "linux")]
        let r = unsafe { libc::syscall(libc::SYS_renameat2, libc::AT_FDCWD, ca.as_ptr(), libc::AT_FDCWD, cb.as_ptr(), libc::RENAME_EXCHANGE) } as i32;
        r == 0
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (a, b);
        false
    }
}

/// Replace `live` by `staged` (a sibling folder): one atomic exchange where the OS has it (a
/// reload never sees a missing folder), else two renames.
fn swap_in(live: &Path, staged: &Path) -> Result<()> {
    if live.exists() && exchange(staged, live) {
        let _ = std::fs::remove_dir_all(staged);
        return Ok(());
    }
    let old = live.with_file_name(format!(
        ".{}.old{}",
        live.file_name().unwrap().to_string_lossy(),
        rand::random::<u32>()
    ));
    if live.exists() {
        std::fs::rename(live, &old)?;
    }
    if let Err(e) = std::fs::rename(staged, live) {
        let _ = std::fs::rename(&old, live);
        return Err(e.into());
    }
    let _ = std::fs::remove_dir_all(&old);
    Ok(())
}

fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.trim().to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out: String = out.trim_end_matches('-').chars().take(32).collect();
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() { "app".into() } else { out }
}

// ---- lookup -----------------------------------------------------------------------------

/// The app node for `key` (an app id or a node id), on `canvas` or any canvas of the project.
pub fn find(host: &Host, key: &str, canvas: Option<&str>) -> Result<(String, Node)> {
    let canvases: Vec<String> = match canvas {
        Some(c) => vec![host.resolve_canvas(c)?],
        None => host.project().canvases.iter().map(|c| c.id.clone()).collect(),
    };
    for cv in canvases {
        let hit = host.read(&cv, |c| {
            c.node(key).filter(|n| n.kind == NodeKind::App).or_else(|| {
                c.nodes()
                    .into_iter()
                    .find(|n| n.kind == NodeKind::App && prop_str(n, "app") == Some(key))
            })
        })?;
        if let Some(n) = hit {
            return Ok((cv, n));
        }
    }
    bail!("no app `{key}` (pass an app id or its node id; create_app makes one)")
}

fn prop_str<'a>(n: &'a Node, k: &str) -> Option<&'a str> {
    n.props.get(k).and_then(Json::as_str)
}

pub fn app_id(n: &Node) -> Option<String> {
    prop_str(n, "app").filter(|a| valid_app_id(a)).map(str::to_string)
}

pub fn entry_of(n: &Node) -> String {
    prop_str(n, "entry")
        .filter(|e| safe_rel(e).is_ok())
        .unwrap_or("index.html")
        .to_string()
}

fn version_of(n: &Node) -> u64 {
    n.props.get("version").and_then(Json::as_f64).unwrap_or(0.0).max(0.0) as u64
}

fn session_chat(host: &Host, session: &str) -> Option<String> {
    host.with_sessions(|ss| ss.get(session).and_then(|s| s.chat.clone()))
        .filter(|c| crate::chat::valid_chat_id(c))
}

impl Apps {
    fn with<T>(&self, id: &str, f: impl FnOnce(&mut AppState) -> T) -> T {
        f(self.state.lock().unwrap().entry(id.to_string()).or_default())
    }

    fn set_owner(&self, id: &str, session: &str, actor: &str) {
        self.with(id, |s| s.owner = Some((session.into(), actor.into(), now_ms())));
    }

    /// Log entries for an app (newest last); `from` = only this version and later.
    pub fn logs(&self, id: &str, from: u64) -> Vec<LogEntry> {
        self.with(id, |s| s.logs.iter().filter(|l| l.version >= from).cloned().collect())
    }

    pub fn version(&self, id: &str) -> u64 {
        self.with(id, |s| s.version)
    }

    fn push_logs(&self, id: &str, entries: impl IntoIterator<Item = LogEntry>) {
        self.with(id, |s| {
            for e in entries {
                // Collapse repeats (a game loop throwing every frame).
                if s.logs.iter().rev().take(20).any(|l| l.text == e.text && l.version == e.version && l.source == e.source) {
                    continue;
                }
                s.logs.push_back(e);
                while s.logs.len() > MAX_LOGS {
                    s.logs.pop_front();
                }
            }
        });
    }
}

// ---- writes -----------------------------------------------------------------------------

/// One file to write: `content` (UTF-8 text) or `base64` (binary).
#[derive(Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct FileArg {
    /// Relative path inside the app, e.g. "index.html", "src/game.js".
    pub path: String,
    /// Text content.
    pub content: Option<String>,
    /// Binary content (base64), instead of `content`.
    pub base64: Option<String>,
}

fn decode(files: &[FileArg]) -> Result<Vec<(PathBuf, Vec<u8>)>> {
    use base64::Engine as _;
    if files.len() > MAX_FILES {
        bail!("at most {MAX_FILES} files per write");
    }
    files
        .iter()
        .map(|f| {
            let rel = safe_rel(&f.path)?;
            let bytes = match (&f.content, &f.base64) {
                (Some(c), _) => c.clone().into_bytes(),
                (None, Some(b)) => base64::engine::general_purpose::STANDARD
                    .decode(b.trim())
                    .with_context(|| format!("`{}`: bad base64", f.path))?,
                (None, None) => bail!("`{}`: pass content or base64", f.path),
            };
            if bytes.len() > MAX_FILE_BYTES {
                bail!("`{}` is over {} MB", f.path, MAX_FILE_BYTES >> 20);
            }
            Ok((rel, bytes))
        })
        .collect()
}

/// Write files into an app atomically: a staged copy of the folder gets the changes and is
/// swapped in with a rename, so a reload never sees half a write.
fn stage_write(dir: &Path, files: &[(PathBuf, Vec<u8>)], delete: &[PathBuf], replace: bool) -> Result<()> {
    let parent = dir.parent().context("app dir")?;
    std::fs::create_dir_all(parent)?;
    let staged = parent.join(format!(
        ".{}.new{}",
        dir.file_name().unwrap().to_string_lossy(),
        rand::random::<u32>()
    ));
    let r = (|| -> Result<()> {
        if !replace && dir.exists() {
            copy_tree(dir, &staged)?;
        } else {
            std::fs::create_dir_all(&staged)?;
        }
        for d in delete {
            let p = staged.join(d);
            if p.is_dir() {
                std::fs::remove_dir_all(&p)?;
            } else if p.exists() {
                std::fs::remove_file(&p)?;
            }
        }
        for (rel, bytes) in files {
            let p = staged.join(rel);
            if let Some(par) = p.parent() {
                std::fs::create_dir_all(par)?;
            }
            std::fs::write(&p, bytes)?;
        }
        let total: u64 = files_of(&staged).iter().map(|f| f.1).sum();
        if total > MAX_APP_BYTES {
            bail!("the app would be {} MB (max {} MB)", total >> 20, MAX_APP_BYTES >> 20);
        }
        swap_in(dir, &staged)
    })();
    if r.is_err() {
        let _ = std::fs::remove_dir_all(&staged);
    }
    r
}

/// Snapshot the folder as the next version, point the node at it (as `actor`), broadcast.
fn commit(host: &Host, actor: &Actor, canvas: &str, node: &Node, id: &str) -> Result<u64> {
    let dir = app_dir(host, id);
    let hist = history_dir(host, id);
    // Next free version (history survives reverts: a new write after a revert never reuses one).
    let mut v = version_of(node).max(host.apps.version(id)) + 1;
    while hist.join(v.to_string()).exists() {
        v += 1;
    }
    copy_tree(&dir, &hist.join(v.to_string()))?;
    let mut props = Props::new();
    props.insert("version".into(), json!(v));
    host.mutate(canvas, &actor.id, None, |c| {
        c.apply(actor, None, &[Command::UpdateNode { id: node.id.clone(), props }])
    })?;
    let sig = fingerprint(&dir);
    host.apps.with(id, |s| {
        s.version = v;
        s.sig = sig;
    });
    host.emit(Event::App { app: id.into(), node: Some(node.id.clone()), version: v, what: "files".into(), errors: None });
    Ok(v)
}

fn card(host: &Host, session: Option<&str>, canvas: &str, node: &Node, id: &str, v: u64) {
    let Some(chat) = session.and_then(|s| session_chat(host, s)) else { return };
    let files: Vec<String> = files_of(&app_dir(host, id)).into_iter().map(|f| f.0).take(50).collect();
    let _ = host.chat.append(
        host,
        &chat,
        Some(canvas),
        json!({"kind": "app", "app": id, "node": node.id, "name": node.name, "version": v, "files": files}),
    );
}

fn starter(name: &str) -> String {
    let name = name.replace(['<', '>', '&'], "");
    format!(
        "<!doctype html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<title>{name}</title>\n<style>html,body{{margin:0;height:100%;background:#0a0a0a;color:#8a8a8a;font:13px ui-monospace,Menlo,monospace;display:grid;place-items:center}}</style>\n</head>\n<body>{name}</body>\n</html>\n"
    )
}

/// `create_app`: a new app folder + node (written by `session`).
pub async fn create(
    host: &Arc<Host>,
    session: &str,
    canvas: &str,
    name: &str,
    files: &[FileArg],
    entry: Option<&str>,
    place: Option<PlacementArgs>,
) -> Result<Json> {
    let actor = host.actor(session)?;
    let canvas = host.resolve_canvas(canvas)?;
    let name = name.trim();
    if name.is_empty() {
        bail!("name the app");
    }
    let entry = match entry {
        Some(e) => safe_rel(e)?.to_string_lossy().into_owned(),
        None => "index.html".into(),
    };
    let decoded = decode(files)?;
    let _w = host.apps.write.lock().await;
    let base = slug(name);
    let mut id = base.clone();
    let mut k = 2;
    while app_dir(host, &id).exists() || history_dir(host, &id).exists() {
        id = format!("{base}-{k}");
        k += 1;
    }
    let mut decoded = decoded;
    if !decoded.iter().any(|(p, _)| p.to_string_lossy() == entry) {
        decoded.push((PathBuf::from(&entry), starter(name).into_bytes()));
    }
    let dir = app_dir(host, &id);
    stage_write(&dir, &decoded, &[], true)?;
    let p = place.unwrap_or_default();
    let (w, h) = (p.w.unwrap_or(800.0).clamp(160.0, 4000.0), p.h.unwrap_or(600.0).clamp(120.0, 4000.0));
    let (x, y) = match (p.x, p.y) {
        (Some(x), Some(y)) => (x, y),
        _ => host.find_space(&canvas, p.parent.as_deref(), &p.near.clone().unwrap_or_default(), w, h)?,
    };
    let mut props = Props::new();
    for (k, v) in [("x", json!(x)), ("y", json!(y)), ("w", json!(w)), ("h", json!(h)), ("app", json!(id)), ("entry", json!(entry)), ("version", json!(0))] {
        props.insert(k.into(), v);
    }
    if let Some(chat) = session_chat(host, session) {
        props.insert("chat".into(), json!(chat));
    }
    let res = host.mutate(&canvas, &actor.id, None, |c| {
        c.apply(
            &actor,
            None,
            &[Command::CreateNode { id: None, kind: NodeKind::App, parent: p.parent.clone(), index: None, name: name.chars().take(80).collect(), props, text: None }],
        )
    })?;
    let node_id = res
        .created
        .first()
        .cloned()
        .context("create_app made no node")?;
    let node = host.read(&canvas, |c| c.node(&node_id))?.context("app node vanished")?;
    host.apps.set_owner(&id, session, &actor.id);
    let v = commit(host, &actor, &canvas, &node, &id)?;
    drop(_w);
    host.touch_nodes(session, &canvas, std::slice::from_ref(&node_id));
    card(host, Some(session), &canvas, &node, &id, v);
    let check = check_now(host, &id).await;
    let mut out = summary(host, &node, &id, v);
    out["check"] = check;
    Ok(out)
}

/// `write_app_files`: change files (atomically) and commit a new version.
pub async fn write(
    host: &Arc<Host>,
    session: &str,
    key: &str,
    files: &[FileArg],
    delete: &[String],
    replace: bool,
    check: bool,
) -> Result<Json> {
    let actor = host.actor(session)?;
    let (canvas, node) = find(host, key, None)?;
    let id = app_id(&node).context("app node without an app id")?;
    let decoded = decode(files)?;
    let delete: Vec<PathBuf> = delete.iter().map(|d| safe_rel(d)).collect::<Result<_>>()?;
    if decoded.is_empty() && delete.is_empty() {
        bail!("nothing to write: pass files and/or delete");
    }
    let _w = host.apps.write.lock().await;
    stage_write(&app_dir(host, &id), &decoded, &delete, replace)?;
    host.apps.set_owner(&id, session, &actor.id);
    let node = host.read(&canvas, |c| c.node(&node.id))?.context("app node vanished")?;
    let v = commit(host, &actor, &canvas, &node, &id)?;
    drop(_w);
    host.touch_nodes(session, &canvas, std::slice::from_ref(&node.id));
    card(host, Some(session), &canvas, &node, &id, v);
    let mut out = summary(host, &node, &id, v);
    out["written"] = json!(decoded.iter().map(|(p, b)| json!({"path": p.to_string_lossy(), "bytes": b.len()})).collect::<Vec<_>>());
    if check {
        out["check"] = check_now(host, &id).await;
    } else {
        schedule_check(host, &id);
    }
    Ok(out)
}

fn summary(host: &Host, node: &Node, id: &str, v: u64) -> Json {
    let dir = app_dir(host, id);
    json!({
        "app": id,
        "node": node.id,
        "name": node.name,
        "version": v,
        "dir": dir.display().to_string(),
        "url": app_url(host, id, &entry_of(node)),
        "files": files_of(&dir).into_iter().map(|(p, b, _)| json!({"path": p, "bytes": b})).collect::<Vec<_>>(),
    })
}

/// `read_app`: files (text inline up to a budget), version, node, url, recent errors.
pub fn read(host: &Host, session: Option<&str>, key: &str, paths: &[String]) -> Result<Json> {
    let (_, node) = find(host, key, None)?;
    let id = app_id(&node).context("app node without an app id")?;
    if let Some(s) = session
        && let Ok(a) = host.actor(s)
    {
        host.apps.set_owner(&id, s, &a.id);
    }
    let dir = app_dir(host, &id);
    let v = version_of(&node);
    let mut out = summary(host, &node, &id, v);
    let wanted: Vec<PathBuf> = paths.iter().map(|p| safe_rel(p)).collect::<Result<_>>()?;
    let mut budget: usize = 400_000;
    let mut contents = serde_json::Map::new();
    for (rel, size, _) in files_of(&dir) {
        if !wanted.is_empty() && !wanted.iter().any(|w| Path::new(&rel).starts_with(w)) {
            continue;
        }
        let Ok(bytes) = std::fs::read(dir.join(&rel)) else { continue };
        let value = match String::from_utf8(bytes) {
            Ok(t) if t.len() <= budget => {
                budget -= t.len();
                Json::from(t)
            }
            Ok(_) => Json::from(format!("({size} bytes; over the read budget: read it by path)")),
            Err(_) => Json::from(format!("(binary, {size} bytes)")),
        };
        contents.insert(rel, value);
    }
    out["contents"] = Json::Object(contents);
    let errors: Vec<LogEntry> = host.apps.logs(&id, v).into_iter().filter(LogEntry::is_error).collect();
    out["errors"] = json!(errors);
    Ok(out)
}

/// `app_logs`: console errors / exceptions of the current version (headless check plus open
/// previews). `rerun` (or no check yet for this version) runs the headless check first.
pub async fn logs(host: &Arc<Host>, session: Option<&str>, key: &str, rerun: bool, all: bool) -> Result<Json> {
    let (_, node) = find(host, key, None)?;
    let id = app_id(&node).context("app node without an app id")?;
    if let Some(s) = session
        && let Ok(a) = host.actor(s)
    {
        host.apps.set_owner(&id, s, &a.id);
    }
    let v = host.apps.version(&id).max(version_of(&node));
    let checked = host.apps.with(&id, |s| s.checked);
    let check = if rerun || checked < v { Some(check_now(host, &id).await) } else { None };
    let entries = host.apps.logs(&id, if all { 0 } else { v });
    let errors = entries.iter().filter(|e| e.is_error()).count();
    let note = host.apps.with(&id, |s| s.note.clone());
    Ok(json!({
        "app": id,
        "node": node.id,
        "version": v,
        "errors": errors,
        "ok": errors == 0 && note.is_none(),
        "entries": entries,
        "note": note,
        "check": check,
        "hint": if errors == 0 { "no console errors or uncaught exceptions; interaction-only bugs show up once someone plays it (previews report here too)" } else { "fix the errors with write_app_files; it re-checks" },
    }))
}

/// Logs posted by a running preview (the guard script).
pub fn record_preview_logs(host: &Host, id: &str, entries: &[Json]) {
    let v = host.apps.version(id);
    let at = now_ms();
    let list: Vec<LogEntry> = entries
        .iter()
        .take(50)
        .filter_map(|e| {
            let level = e["level"].as_str().unwrap_or("log");
            let level = match level {
                "error" | "exception" | "warning" | "log" => level,
                "warn" => "warning",
                _ => "log",
            };
            let text: String = e["text"].as_str()?.chars().take(2000).collect();
            Some(LogEntry { level: level.into(), text, source: "preview".into(), version: v, at })
        })
        .collect();
    if list.is_empty() {
        return;
    }
    let errors = list.iter().filter(|e| e.is_error()).count();
    host.apps.push_logs(id, list);
    if errors > 0 {
        let total = host.apps.logs(id, v).iter().filter(|e| e.is_error()).count();
        host.emit(Event::App { app: id.into(), node: None, version: v, what: "logs".into(), errors: Some(total) });
    }
}

/// What a chip on an app node resolves to (chat context): id, folder, url, files.
pub fn chip_info(host: &Host, node: &Node) -> Json {
    let Some(id) = app_id(node) else { return Json::Null };
    let dir = app_dir(host, &id);
    let v = version_of(node);
    let errors: Vec<String> = host.apps.logs(&id, v).into_iter().filter(LogEntry::is_error).map(|e| e.text).take(10).collect();
    json!({
        "app": id,
        "dir": dir.display().to_string(),
        "url": app_url(host, &id, &entry_of(node)),
        "entry": entry_of(node),
        "version": v,
        "files": files_of(&dir).into_iter().map(|(p, b, _)| json!({"path": p, "bytes": b})).collect::<Vec<_>>(),
        "errors": errors,
        "hint": "edit with write_app_files (read_app for the sources); check app_logs after",
    })
}

// ---- checks -----------------------------------------------------------------------------

/// Run the headless check now (waits for a running one to finish first). Returns a compact
/// summary; logs and the snapshot are stored.
pub async fn check_now(host: &Arc<Host>, id: &str) -> Json {
    // Single flight: wait while another check of this app runs.
    loop {
        let busy = {
            let mut c = host.apps.checks.lock().unwrap();
            match c.get(id) {
                Some(_) => true,
                None => {
                    c.insert(id.to_string(), false);
                    false
                }
            }
        };
        if !busy {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let r = run_check(host, id).await;
    let again = host.apps.checks.lock().unwrap().remove(id).unwrap_or(false);
    if again {
        schedule_check(host, id);
    }
    match r {
        Ok(v) => v,
        Err(e) => {
            let note = format!("check failed: {e:#}");
            host.apps.with(id, |s| s.note = Some(note.clone()));
            json!({"ok": false, "note": note})
        }
    }
}

/// Check soon (debounced; queues behind a running check).
pub fn schedule_check(host: &Arc<Host>, id: &str) {
    {
        let mut c = host.apps.checks.lock().unwrap();
        if let Some(again) = c.get_mut(id) {
            *again = true;
            return;
        }
    }
    let host = host.clone();
    let id = id.to_string();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(400)).await;
        check_now(&host, &id).await;
    });
}

async fn run_check(host: &Arc<Host>, id: &str) -> Result<Json> {
    let (canvas, node) = find(host, id, None)?;
    let v = host.apps.version(id).max(version_of(&node));
    let Some(url) = app_url(host, id, &entry_of(&node)) else {
        host.apps.with(id, |s| {
            s.checked = v;
            s.note = Some("the app server isn't running".into());
        });
        return Ok(json!({"ok": false, "note": "the app server isn't running"}));
    };
    let Some(chrome) = cdp::chrome_path() else {
        let note = "no headless Chrome found (set FALOS_CHROME): only open previews report errors";
        host.apps.with(id, |s| {
            s.checked = v;
            s.note = Some(note.into());
        });
        return Ok(json!({"ok": true, "note": note}));
    };
    let (w, h) = ((node.w.round() as u32).clamp(240, 1920), (node.h.round() as u32).clamp(180, 1600));
    let url = format!("{url}{}__falos=check", if url.contains('?') { '&' } else { '?' });
    let out = cdp::check(&chrome, &url, w, h).await?;
    let at = now_ms();
    let entries: Vec<LogEntry> = out
        .entries
        .iter()
        .map(|(level, text)| LogEntry { level: level.clone(), text: text.clone(), source: "check".into(), version: v, at })
        .collect();
    let errors = entries.iter().filter(|e| e.is_error()).count();
    host.apps.with(id, |s| {
        s.logs.retain(|l| !(l.source == "check" && l.version == v));
        s.checked = v;
        s.note = out.note.clone();
    });
    host.apps.push_logs(id, entries.clone());
    if let Some(png) = &out.png {
        let _ = set_snapshot(host, &canvas, &node.id, png);
    }
    host.emit(Event::App { app: id.into(), node: Some(node.id.clone()), version: v, what: "logs".into(), errors: Some(errors) });
    Ok(json!({
        "ok": errors == 0,
        "errors": entries.iter().filter(|e| e.is_error()).map(|e| e.text.clone()).collect::<Vec<_>>(),
        "warnings": entries.iter().filter(|e| e.level == "warning").map(|e| e.text.clone()).take(10).collect::<Vec<_>>(),
        "note": out.note,
    }))
}

fn set_snapshot(host: &Host, canvas: &str, node: &str, png: &[u8]) -> Result<()> {
    let hex: String = Sha256::digest(png).iter().map(|b| format!("{b:02x}")).collect();
    let dir = snapshots_dir(host.dir());
    std::fs::create_dir_all(&dir)?;
    let file = dir.join(format!("{hex}.png"));
    if !file.exists() {
        let tmp = dir.join(format!("{hex}.png.part{}", rand::random::<u32>()));
        std::fs::write(&tmp, png)?;
        std::fs::rename(&tmp, &file)?;
    }
    let asset = format!("sha256:{hex}");
    // None: the node is gone; Some(true): already showing this still.
    let same = host.read(canvas, |c| c.node(node).map(|n| prop_str(&n, "asset") == Some(asset.as_str())))?;
    if same != Some(false) {
        return Ok(());
    }
    let mut props = Props::new();
    props.insert("asset".into(), json!(asset));
    let actor = Actor::agent(SNAPSHOT_ACTOR, "fal-os");
    host.mutate(canvas, &actor.id, None, |c| c.apply(&actor, None, &[Command::UpdateNode { id: node.into(), props }]))?;
    Ok(())
}

// ---- background: reconcile reverts, pick up external edits ---------------------------------

/// Restore files for app nodes whose `version` prop moved (undo, turn revert / redo).
async fn reconcile(host: &Arc<Host>) {
    let mut nodes = vec![];
    for c in host.project().canvases {
        if let Ok(list) = host.read(&c.id, |cv| cv.nodes().into_iter().filter(|n| n.kind == NodeKind::App).collect::<Vec<_>>()) {
            nodes.extend(list);
        }
    }
    for n in nodes {
        let Some(id) = app_id(&n) else { continue };
        let want = version_of(&n);
        if want == 0 || want == host.apps.version(&id) {
            continue;
        }
        let snap = history_dir(host, &id).join(want.to_string());
        if !snap.is_dir() {
            continue;
        }
        let _w = host.apps.write.lock().await;
        if host.apps.version(&id) == want {
            continue;
        }
        let dir = app_dir(host, &id);
        let staged = apps_dir(host).join(format!(".{id}.restore{}", rand::random::<u32>()));
        let r = copy_tree(&snap, &staged).and_then(|_| swap_in(&dir, &staged));
        if let Err(e) = r {
            let _ = std::fs::remove_dir_all(&staged);
            tracing::warn!("apps: restoring {id} v{want}: {e:#}");
            continue;
        }
        let sig = fingerprint(&dir);
        host.apps.with(&id, |s| {
            s.version = want;
            s.sig = sig;
        });
        drop(_w);
        host.emit(Event::App { app: id.clone(), node: Some(n.id.clone()), version: want, what: "files".into(), errors: None });
        schedule_check(host, &id);
    }
}

/// Files changed on disk outside the tools (Codex, an editor): commit them as a version.
async fn poll_external(host: &Arc<Host>) {
    let Ok(rd) = std::fs::read_dir(apps_dir(host)) else { return };
    let ids: Vec<String> = rd
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|id| valid_app_id(id))
        .collect();
    for id in ids {
        let sig = fingerprint(&app_dir(host, &id));
        if host.apps.with(&id, |s| s.sig) == sig {
            continue;
        }
        let Ok(_w) = host.apps.write.try_lock() else { continue };
        // Re-read under the lock (a tool write may have just committed).
        let sig = fingerprint(&app_dir(host, &id));
        if host.apps.with(&id, |s| s.sig) == sig {
            continue;
        }
        let Ok((canvas, node)) = find(host, &id, None) else {
            host.apps.with(&id, |s| s.sig = sig);
            continue;
        };
        let owner = host.apps.with(&id, |s| s.owner.clone()).filter(|o| now_ms() - o.2 < OWNER_TTL_MS);
        let actor_id = owner.as_ref().map(|o| o.1.clone()).unwrap_or_else(|| node.created_by.clone());
        let actor = Actor::agent(&actor_id, &actor_id);
        match commit(host, &actor, &canvas, &node, &id) {
            Ok(v) => {
                drop(_w);
                card(host, owner.as_ref().map(|o| o.0.as_str()), &canvas, &node, &id, v);
                schedule_check(host, &id);
            }
            Err(e) => {
                host.apps.with(&id, |s| s.sig = sig);
                tracing::warn!("apps: committing external change to {id}: {e:#}");
            }
        }
    }
}

/// Seed state from the doc (versions on disk = the nodes' versions) and start the
/// background tasks: revert reconciliation (doc events) and the external-edit poll.
pub fn start(host: &Arc<Host>) {
    host.apps.started.get_or_init(|| {
        for c in host.project().canvases {
            let _ = host.read(&c.id, |cv| {
                for n in cv.nodes().into_iter().filter(|n| n.kind == NodeKind::App) {
                    if let Some(id) = app_id(&n) {
                        let sig = fingerprint(&app_dir(host, &id));
                        host.apps.with(&id, |s| {
                            s.version = version_of(&n);
                            s.sig = sig;
                        });
                    }
                }
            });
        }
        let weak = Arc::downgrade(host);
        let mut rx = host.subscribe();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(Event::Doc { .. }) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        let Some(h) = weak.upgrade() else { return };
                        if h.closed.is_cancelled() {
                            return;
                        }
                        reconcile(&h).await;
                    }
                    Ok(_) => {}
                    Err(_) => return,
                }
            }
        });
        let weak = Arc::downgrade(host);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(POLL).await;
                let Some(h) = weak.upgrade() else { return };
                if h.closed.is_cancelled() {
                    return;
                }
                if apps_dir(&h).is_dir() {
                    poll_external(&h).await;
                }
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_and_ids() {
        assert!(safe_rel("index.html").is_ok());
        assert!(safe_rel("src/game.js").is_ok());
        assert!(safe_rel("./a/b.css").is_ok());
        for bad in ["../x", "/etc/passwd", "a/../../b", ".git/config", "node_modules/x.js", "", "a\\b"] {
            assert!(safe_rel(bad).is_err(), "{bad}");
        }
        assert!(valid_app_id("ping-pong"));
        assert!(!valid_app_id(".history"));
        assert!(!valid_app_id("Ping"));
        assert_eq!(slug("Ping Pong!"), "ping-pong");
    }

    #[test]
    fn staged_write_is_atomic_and_replaces() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().join("apps").join("a");
        stage_write(&dir, &[(PathBuf::from("index.html"), b"1".to_vec()), (PathBuf::from("src/x.js"), b"x".to_vec())], &[], true).unwrap();
        stage_write(&dir, &[(PathBuf::from("index.html"), b"2".to_vec())], &[PathBuf::from("src/x.js")], false).unwrap();
        let files: Vec<String> = files_of(&dir).into_iter().map(|f| f.0).collect();
        assert_eq!(files, vec!["index.html"]);
        assert_eq!(std::fs::read(dir.join("index.html")).unwrap(), b"2");
        // Nothing staged is left behind.
        let left: Vec<_> = std::fs::read_dir(t.path().join("apps")).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(left.len(), 1);
    }
}
