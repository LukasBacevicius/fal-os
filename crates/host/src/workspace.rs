//! Workspace: a folder of projects (`falos-host serve --workspace <dir>`, default
//! `~/fal-os-projects`). A project is one folder (`project.json`, `canvases/main.loro`,
//! `assets/`, `chats/`, `work/`) = one canvas = one fal Agent project, with many chats.
//!
//! Each loaded project is its own [`Host`] (canvases, sessions, claims, event bus, chats), so
//! agents in different projects never mix. They share one agent runtime connection
//! ([`RuntimeLink`]); frames carry `project`. Projects load lazily (first sync client, MCP
//! session, runtime frame or workspace op).
//!
//! `--project <dir>` is single-project mode: a workspace of exactly that project (no create,
//! duplicate or delete).
//!
//! `GET /workspace` → WebSocket: `projects` on connect and on change; `project.*` ops.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::response::Response;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{schemars, tool, tool_router};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use tokio::sync::broadcast;

use crate::host::{Host, HostOptions, now_ms};
use crate::mcp::CanvasMcp;
use crate::runtime::RuntimeLink;

/// Deleted projects move here (inside the workspace), never `rm`.
pub const TRASH: &str = ".trash";

type OptsFn = Box<dyn Fn() -> HostOptions + Send + Sync>;

pub struct Workspace {
    /// `None` in single-project mode.
    root: Option<PathBuf>,
    hosts: Mutex<HashMap<String, Arc<Host>>>,
    opts: OptsFn,
    /// The agent runtime connection, shared by all projects.
    pub link: Arc<RuntimeLink>,
    /// Serialized `projects` frames for `/workspace` clients.
    events: broadcast::Sender<String>,
    last: Mutex<String>,
    /// `(mcp url, token)` of the running server (for `agent.register` in every project).
    endpoint: Mutex<Option<(String, String)>>,
    /// Newest image per project, cached by the assets folder's mtime.
    thumbs: Mutex<HashMap<String, (i64, Option<String>)>>,
    /// Serializes create / duplicate / delete.
    ops: Mutex<()>,
    poller: std::sync::Once,
    /// The asset library, when enabled (`library::Library::enable`).
    pub library: std::sync::OnceLock<Arc<crate::library::Library>>,
}

fn mtime_ms(p: &Path) -> i64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as i64)
}

/// A folder name usable as a project id (no path tricks, not hidden).
pub fn valid_project_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 100
        && !id.starts_with('.')
        && !id.chars().any(|c| matches!(c, '/' | '\\' | '\0') || c.is_control())
}

/// `My Launch Film!` → `my-launch-film`.
pub fn project_slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out: String = out.trim_end_matches('-').chars().take(40).collect();
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() { "project".into() } else { out }
}

fn clean_name(name: &str) -> Result<String> {
    let n: String = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if n.is_empty() {
        bail!("a project needs a name");
    }
    Ok(n.chars().take(80).collect())
}

/// Read-modify-write `project.json` of a project that isn't loaded.
fn edit_project_json(dir: &Path, f: impl FnOnce(&mut serde_json::Map<String, Json>)) -> Result<()> {
    let path = dir.join("project.json");
    let mut v: Json = serde_json::from_slice(&std::fs::read(&path)?).context("reading project.json")?;
    let obj = v.as_object_mut().context("project.json is not an object")?;
    f(obj);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&v)?)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

/// Copy a folder: APFS clonefile (copy-on-write) where possible, else a byte copy. Never
/// hardlinks: an app editing one copy would corrupt the other project's content-addressed store.
fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from).into_iter().flatten().flatten() {
        let (src, dst) = (e.path(), to.join(e.file_name()));
        let ty = e.file_type()?;
        if ty.is_dir() {
            copy_dir(&src, &dst)?;
        } else if ty.is_file() {
            crate::ops::export::clone_or_copy(&src, &dst)?;
        }
    }
    Ok(())
}

impl Workspace {
    /// A workspace folder (created if missing). `opts` makes the options of each project's host.
    pub fn open(root: &Path, opts: impl Fn() -> HostOptions + Send + Sync + 'static) -> Result<Arc<Self>> {
        std::fs::create_dir_all(root)?;
        let root = root.canonicalize()?;
        Ok(Arc::new(Self::new(Some(root), Box::new(opts), Arc::default())))
    }

    /// Single-project mode around an open host.
    pub fn single(host: Arc<Host>) -> Arc<Self> {
        let ws = Self::new(None, Box::new(HostOptions::default), host.chat.link.clone());
        crate::director::write_pointers(&host);
        ws.hosts.lock().unwrap().insert(host.id.clone(), host);
        Arc::new(ws)
    }

    fn new(root: Option<PathBuf>, opts: OptsFn, link: Arc<RuntimeLink>) -> Self {
        let (events, _) = broadcast::channel(64);
        Self {
            root,
            hosts: Mutex::default(),
            opts,
            link,
            events,
            last: Mutex::default(),
            endpoint: Mutex::default(),
            thumbs: Mutex::default(),
            ops: Mutex::new(()),
            poller: std::sync::Once::new(),
            library: std::sync::OnceLock::new(),
        }
    }

    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    pub fn is_single(&self) -> bool {
        self.root.is_none()
    }

    fn single_host(&self) -> Option<Arc<Host>> {
        if self.root.is_some() {
            return None;
        }
        self.hosts.lock().unwrap().values().next().cloned()
    }

    /// Project ids (folder names), unsorted.
    pub fn ids(&self) -> Vec<String> {
        let Some(root) = &self.root else {
            return self.hosts.lock().unwrap().keys().cloned().collect();
        };
        std::fs::read_dir(root)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().join("project.json").is_file())
            .filter_map(|e| e.file_name().to_str().map(str::to_string))
            .filter(|id| valid_project_id(id))
            .collect()
    }

    fn dir_of(&self, id: &str) -> Result<PathBuf> {
        if !valid_project_id(id) {
            bail!("bad project id `{id}`");
        }
        match &self.root {
            Some(root) => Ok(root.join(id)),
            None => self
                .single_host()
                .filter(|h| h.id == id)
                .map(|h| h.dir().to_path_buf())
                .ok_or_else(|| anyhow!("unknown project `{id}`")),
        }
    }

    /// Projects loaded right now.
    pub fn loaded(&self) -> Vec<Arc<Host>> {
        self.hosts.lock().unwrap().values().cloned().collect()
    }

    /// The host of project `id`, loading it on first use.
    pub fn get(&self, id: &str) -> Result<Arc<Host>> {
        if let Some(h) = self.hosts.lock().unwrap().get(id) {
            return Ok(h.clone());
        }
        let dir = self.dir_of(id)?;
        if !dir.join("project.json").is_file() {
            bail!("unknown project `{id}` (see list_projects)");
        }
        let mut hosts = self.hosts.lock().unwrap();
        if let Some(h) = hosts.get(id) {
            return Ok(h.clone());
        }
        let host = Host::open_with(&dir, (self.opts)(), self.link.clone())
            .with_context(|| format!("opening project `{id}`"))?;
        *host.endpoint.lock().unwrap() = self.endpoint.lock().unwrap().clone();
        if tokio::runtime::Handle::try_current().is_ok() {
            host.start_tasks();
        }
        hosts.insert(id.to_string(), host.clone());
        drop(hosts);
        crate::director::write_pointers(&host);
        Ok(host)
    }

    /// The project whose folder contains `path` (a fal OS project folder or anything inside).
    pub fn project_for_path(&self, path: &Path) -> Option<String> {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        match &self.root {
            Some(root) => {
                let rel = path.strip_prefix(root).ok()?;
                let id = rel.components().next()?.as_os_str().to_str()?.to_string();
                (valid_project_id(&id) && root.join(&id).join("project.json").is_file()).then_some(id)
            }
            None => self.single_host().filter(|h| path.starts_with(h.dir())).map(|h| h.id.clone()),
        }
    }

    /// The project the user has open in fal OS right now: of the loaded projects with a
    /// connected human client, the one with the most recent human activity.
    pub fn active_id(&self) -> Option<String> {
        self.loaded()
            .into_iter()
            .filter(|h| h.with_sessions(|ss| ss.values().any(|s| s.actor.kind == falos_doc::ActorKind::Human)))
            .max_by_key(|h| h.human_seen.load(std::sync::atomic::Ordering::Relaxed))
            .map(|h| h.id.clone())
    }

    /// The `project.json` name of a project (loaded or not).
    fn name_of(&self, id: &str) -> Option<String> {
        if let Some(h) = self.hosts.lock().unwrap().get(id) {
            return Some(h.project().name);
        }
        let v: Json = serde_json::from_slice(&std::fs::read(self.dir_of(id).ok()?.join("project.json")).ok()?).ok()?;
        v["name"].as_str().map(str::to_string)
    }

    /// A project id or name (case-insensitive) → its id.
    pub fn resolve(&self, key: &str) -> Result<String> {
        let ids = self.ids();
        if ids.iter().any(|i| i == key) {
            return Ok(key.to_string());
        }
        ids.iter()
            .find(|i| self.name_of(i).is_some_and(|n| n.eq_ignore_ascii_case(key.trim())))
            .cloned()
            .ok_or_else(|| {
                let names: Vec<String> = self.projects().iter().filter_map(|p| p["name"].as_str().map(str::to_string)).collect();
                anyhow!("unknown project `{key}` (projects: {})", names.join(", "))
            })
    }

    /// The most recently touched project (a new "Untitled" one when the workspace is empty).
    pub fn default_id(&self) -> Result<String> {
        if let Some(h) = self.single_host() {
            return Ok(h.id.clone());
        }
        match self.projects().first().and_then(|p| p["id"].as_str().map(str::to_string)) {
            Some(id) => Ok(id),
            None => self.create("Untitled"),
        }
    }

    pub fn default_host(&self) -> Result<Arc<Host>> {
        self.get(&self.default_id()?)
    }

    /// The project a runtime frame names (by id or name), else the default one.
    pub fn host_for_frame(&self, project: Option<&str>) -> Result<Arc<Host>> {
        match project.filter(|p| !p.is_empty()) {
            Some(p) => {
                if let Some(h) = self.hosts.lock().unwrap().get(p) {
                    return Ok(h.clone());
                }
                self.get(&self.resolve(p)?)
            }
            None => self.default_host(),
        }
    }

    /// The loaded project that has MCP session `sid`.
    pub fn host_for_session(&self, sid: &str) -> Option<Arc<Host>> {
        self.loaded()
            .into_iter()
            .find(|h| h.with_sessions(|s| s.contains_key(sid)))
    }

    /// `/sync/<key>`: a project id (its first canvas, or `canvas`), else — for older clients
    /// and single-project mode — a canvas of the default project.
    pub fn resolve_sync(&self, key: &str, canvas: Option<&str>) -> Result<(Arc<Host>, String)> {
        let first = |h: &Host| h.project().canvases.first().map(|c| c.id.clone()).unwrap_or_else(|| "main".into());
        if let Some(h) = self.single_host() {
            let c = match canvas {
                Some(c) => h.resolve_canvas(c)?,
                None if key == h.id => h.resolve_canvas(key).unwrap_or_else(|_| first(&h)),
                None => h.resolve_canvas(key)?,
            };
            return Ok((h, c));
        }
        if valid_project_id(key) && self.dir_of(key)?.join("project.json").is_file() {
            let h = self.get(key)?;
            let c = match canvas {
                Some(c) => h.resolve_canvas(c)?,
                None => first(&h),
            };
            return Ok((h, c));
        }
        let h = self.default_host()?;
        let c = h
            .resolve_canvas(key)
            .map_err(|_| anyhow!("unknown project `{key}`"))?;
        Ok((h, c))
    }

    // ---- list -----------------------------------------------------------------------------

    fn thumb(&self, id: &str, dir: &Path) -> Option<String> {
        let assets = dir.join("assets");
        let m = mtime_ms(&assets);
        if let Some((cm, t)) = self.thumbs.lock().unwrap().get(id)
            && *cm == m
        {
            return t.clone();
        }
        let newest = std::fs::read_dir(&assets)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_str()?.to_string();
                let (hex, ext) = name.split_once('.')?;
                let image = matches!(ext.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg" | "webp" | "gif" | "avif");
                (image && hex.len() == 64).then(|| (mtime_ms(&e.path()), hex.to_string()))
            })
            .max();
        let t = newest.map(|(_, hex)| format!("/assets/{hex}?project={id}"));
        self.thumbs.lock().unwrap().insert(id.to_string(), (m, t.clone()));
        t
    }

    /// One `projects` row.
    fn row(&self, id: &str) -> Option<Json> {
        let dir = self.dir_of(id).ok()?;
        let loaded = self.hosts.lock().unwrap().get(id).cloned();
        let (name, extra) = match &loaded {
            Some(h) => {
                let p = h.project();
                (p.name, p.extra)
            }
            None => {
                let v: Json = serde_json::from_slice(&std::fs::read(dir.join("project.json")).ok()?).ok()?;
                (
                    v["name"].as_str().unwrap_or(id).to_string(),
                    v.as_object().cloned().unwrap_or_default(),
                )
            }
        };
        let updated = [
            extra.get("touched").and_then(Json::as_i64).unwrap_or(0),
            mtime_ms(&dir.join("project.json")),
            mtime_ms(&dir.join("chats").join("index.json")),
        ]
        .into_iter()
        .max()
        .unwrap_or(0);
        let running = loaded.as_ref().map_or(0, |h| h.chat.running_count(h));
        Some(json!({
            "id": id,
            "name": name,
            "updated": updated,
            "running": running,
            "fal_project": extra.get("fal_project").and_then(Json::as_str),
            "thumb": self.thumb(id, &dir),
        }))
    }

    /// All projects, most recently updated first.
    pub fn projects(&self) -> Vec<Json> {
        let mut rows: Vec<Json> = self.ids().iter().filter_map(|id| self.row(id)).collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r["updated"].as_i64().unwrap_or(0)));
        rows
    }

    fn frame(&self) -> String {
        json!({"type": "projects", "projects": self.projects()}).to_string()
    }

    /// Recompute the project list and broadcast it when it changed.
    pub fn changed(&self) {
        let f = self.frame();
        let mut last = self.last.lock().unwrap();
        if *last != f {
            *last = f.clone();
            let _ = self.events.send(f);
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.events.subscribe()
    }

    /// Poll for changes (running counts, recency, folders added by hand) once a second.
    pub fn start(self: &Arc<Self>) {
        self.poller.call_once(|| {
            for h in self.loaded() {
                h.start_tasks();
            }
            let weak = Arc::downgrade(self);
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    let Some(ws) = weak.upgrade() else { return };
                    ws.changed();
                }
            });
        });
    }

    pub fn set_endpoint(&self, url: String, token: String) {
        *self.endpoint.lock().unwrap() = Some((url.clone(), token.clone()));
        for h in self.loaded() {
            *h.endpoint.lock().unwrap() = Some((url.clone(), token.clone()));
        }
    }

    /// Save every loaded project.
    pub fn flush_all(&self) {
        for h in self.loaded() {
            if let Err(e) = h.flush() {
                tracing::error!("saving {} failed: {e:#}", h.id);
            }
        }
    }

    // ---- ops ------------------------------------------------------------------------------

    fn workspace_root(&self, op: &str) -> Result<&Path> {
        self.root
            .as_deref()
            .ok_or_else(|| anyhow!("can't {op} projects in single-project mode (start the host with --workspace)"))
    }

    fn free_id(root: &Path, name: &str) -> String {
        let base = project_slug(name);
        (1..)
            .map(|n| if n == 1 { base.clone() } else { format!("{base}-{n}") })
            .find(|id| !root.join(id).exists())
            .unwrap()
    }

    /// Create a project; returns its id.
    pub fn create(&self, name: &str) -> Result<String> {
        let root = self.workspace_root("create")?;
        let name = clean_name(name)?;
        let _g = self.ops.lock().unwrap();
        let id = Self::free_id(root, &name);
        let dir = root.join(&id);
        std::fs::create_dir_all(&dir)?;
        let mut p = falos_doc::Project::new(&id, &name);
        p.extra.insert("touched".into(), json!(now_ms()));
        p.save(&dir)?;
        drop(_g);
        self.get(&id)?;
        self.changed();
        Ok(id)
    }

    pub fn rename(&self, id: &str, name: &str) -> Result<()> {
        let name = clean_name(name)?;
        let loaded = self.hosts.lock().unwrap().get(id).cloned();
        match loaded {
            Some(h) => {
                h.update_project(|p| p.name = name)?;
                crate::director::write_pointers(&h);
            }
            None => {
                let dir = self.dir_of(id)?;
                if !dir.join("project.json").is_file() {
                    bail!("unknown project `{id}`");
                }
                edit_project_json(&dir, |o| {
                    o.insert("name".into(), json!(name));
                })?
            }
        }
        self.changed();
        Ok(())
    }

    /// Mark a project as just used (recency: the default project for agents).
    pub fn touch(&self, id: &str) -> Result<()> {
        let now = now_ms();
        let loaded = self.hosts.lock().unwrap().get(id).cloned();
        match loaded {
            Some(h) => h.update_project(|p| {
                p.extra.insert("touched".into(), json!(now));
            })?,
            None => {
                let dir = self.dir_of(id)?;
                if !dir.join("project.json").is_file() {
                    bail!("unknown project `{id}`");
                }
                edit_project_json(&dir, |o| {
                    o.insert("touched".into(), json!(now));
                })?
            }
        }
        self.changed();
        Ok(())
    }

    /// Copy a project's canvas and assets (not its chats, work files or fal Agent project)
    /// into a new project "<name> copy"; returns the new id.
    pub fn duplicate(&self, id: &str) -> Result<String> {
        let root = self.workspace_root("duplicate")?.to_path_buf();
        let src = self.dir_of(id)?;
        if !src.join("project.json").is_file() {
            bail!("unknown project `{id}`");
        }
        if let Some(h) = self.hosts.lock().unwrap().get(id) {
            h.flush()?;
        }
        let name = clean_name(&format!("{} copy", self.name_of(id).unwrap_or_else(|| id.to_string())))?;
        let _g = self.ops.lock().unwrap();
        let new = Self::free_id(&root, &name);
        let dst = root.join(&new);
        copy_dir(&src.join("canvases"), &dst.join("canvases"))?;
        if src.join("assets").is_dir() {
            copy_dir(&src.join("assets"), &dst.join("assets"))?;
        }
        let mut p = falos_doc::Project::load(&src)?;
        p.id = new.clone();
        p.name = name;
        p.extra.remove("fal_project");
        p.extra.insert("touched".into(), json!(now_ms()));
        p.save(&dst)?;
        drop(_g);
        self.changed();
        Ok(new)
    }

    /// Move a project to `<workspace>/.trash/` (never deleted); its chats stop and its
    /// clients disconnect.
    pub fn delete(&self, id: &str) -> Result<()> {
        let root = self.workspace_root("delete")?.to_path_buf();
        let src = self.dir_of(id)?;
        if !src.join("project.json").is_file() {
            bail!("unknown project `{id}`");
        }
        let _g = self.ops.lock().unwrap();
        if let Some(h) = self.hosts.lock().unwrap().remove(id) {
            h.close();
        }
        let trash = root.join(TRASH);
        std::fs::create_dir_all(&trash)?;
        std::fs::rename(&src, trash.join(format!("{id}-{}", now_ms())))
            .with_context(|| format!("moving {} to the trash", src.display()))?;
        self.thumbs.lock().unwrap().remove(id);
        drop(_g);
        self.changed();
        Ok(())
    }

    /// Handle a `/workspace` client frame; returns the reply for that client, if any.
    pub fn handle_client(&self, msg: &Json) -> Result<Option<Json>> {
        let id = || -> Result<String> {
            let id = msg["id"].as_str().context("needs id")?;
            if !self.ids().iter().any(|i| i == id) {
                bail!("unknown project `{id}`");
            }
            Ok(id.to_string())
        };
        match msg["type"].as_str() {
            Some("project.create") => {
                let id = self.create(msg["name"].as_str().unwrap_or("Untitled"))?;
                Ok(Some(json!({"type": "project.created", "id": id})))
            }
            Some("project.rename") => {
                self.rename(&id()?, msg["name"].as_str().unwrap_or_default())?;
                Ok(None)
            }
            Some("project.duplicate") => {
                let new = self.duplicate(&id()?)?;
                Ok(Some(json!({"type": "project.created", "id": new, "from": msg["id"]})))
            }
            Some("project.delete") => {
                self.delete(&id()?)?;
                Ok(None)
            }
            Some("project.touch") => {
                self.touch(&id()?)?;
                Ok(None)
            }
            other => bail!("unknown workspace message {other:?}"),
        }
    }
}

// ---- /workspace WebSocket ----------------------------------------------------------------------

pub async fn ws_handler(ws: WebSocketUpgrade, State(space): State<Arc<Workspace>>) -> Response {
    ws.on_upgrade(move |socket| run(space, socket))
}

async fn run(space: Arc<Workspace>, mut socket: WebSocket) {
    let mut rx = space.subscribe();
    if socket.send(Message::Text(space.frame().into())).await.is_err() {
        return;
    }
    // Library: per-client replies + broadcast `library.changed` / `library.show`.
    let library = space.library.get().cloned();
    let (ltx, mut lrx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let mut lib_events = library.as_ref().map(|l| l.subscribe());
    if let Some(l) = &library {
        let _ = ltx.send(l.status().to_string());
    }
    loop {
        tokio::select! {
            Some(f) = lrx.recv() => {
                if socket.send(Message::Text(f.into())).await.is_err() {
                    break;
                }
            }
            f = async { match lib_events.as_mut() { Some(r) => r.recv().await, None => std::future::pending().await } } => match f {
                Ok(f) => {
                    if socket.send(Message::Text(f.into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    if socket.send(Message::Text(serde_json::json!({"type": "library.changed", "shas": [], "all": true}).to_string().into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => lib_events = None,
            },
            f = rx.recv() => match f {
                Ok(f) => {
                    if socket.send(Message::Text(f.into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    if socket.send(Message::Text(space.frame().into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            msg = socket.recv() => match msg {
                Some(Ok(Message::Text(t))) => {
                    let parsed = serde_json::from_str::<Json>(&t).ok();
                    if let Some(m) = parsed.filter(|m| m["type"].as_str().is_some_and(|t| t.starts_with("library.")))
                    {
                        match &library {
                            Some(l) => {
                                tokio::spawn(crate::library::ws::handle(l.clone(), m, ltx.clone()));
                            }
                            None => {
                                let _ = ltx.send(json!({"type": "error", "id": m["id"], "scope": "library", "message": "the asset library isn't enabled on this host"}).to_string());
                            }
                        }
                        continue;
                    }
                    let reply = match serde_json::from_str::<Json>(&t) {
                        Ok(m) => {
                            let space = space.clone();
                            match tokio::task::spawn_blocking(move || space.handle_client(&m)).await {
                                Ok(Ok(r)) => r,
                                Ok(Err(e)) => Some(json!({"type": "error", "message": format!("{e:#}")})),
                                Err(e) => Some(json!({"type": "error", "message": e.to_string()})),
                            }
                        }
                        Err(e) => Some(json!({"type": "error", "message": e.to_string()})),
                    };
                    if let Some(r) = reply
                        && socket.send(Message::Text(r.to_string().into())).await.is_err()
                    {
                        break;
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
        }
    }
}

// ---- MCP tools ------------------------------------------------------------------------------------

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ListProjectsArgs {}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateProjectArgs {
    /// Project name as humans should see it, e.g. "Headphones launch".
    pub name: String,
}

#[tool_router(router = workspace_tools, vis = "pub(crate)")]
impl CanvasMcp {
    #[tool(
        description = "List the projects in this fal-os workspace (each project is one canvas with its own chats): id, name, updated (ms), running chats. Pass a project id or name to `session` to work in it; without one, `session` uses the most recently used project.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn list_projects(&self, Parameters(_a): Parameters<ListProjectsArgs>) -> CallToolResult {
        let r = (|| {
            let rows: Vec<Json> = self
                .workspace()
                .projects()
                .into_iter()
                .map(|p| json!({"id": p["id"], "name": p["name"], "updated": p["updated"], "running": p["running"]}))
                .collect();
            Ok(serde_json::to_string_pretty(&rows)?)
        })();
        self.finish(None, r)
    }

    #[tool(
        description = "Create a new project (an empty canvas) in this fal-os workspace. Returns its id; then call `session` with project=<id>.",
        annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = false, open_world_hint = false)
    )]
    async fn create_project(&self, Parameters(a): Parameters<CreateProjectArgs>) -> CallToolResult {
        let r = (|| {
            let ws = self.workspace();
            let id = ws.create(&a.name)?;
            let name = ws.name_of(&id).unwrap_or_default();
            *self.conn.created.lock().unwrap() = Some(id.clone());
            Ok(serde_json::to_string_pretty(&json!({"id": id, "name": name, "hint": "session(agentName, project=<id>)"}))?)
        })();
        self.finish(None, r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_and_ids() {
        assert_eq!(project_slug("My Launch Film!"), "my-launch-film");
        assert_eq!(project_slug("  "), "project");
        assert!(valid_project_id("demo"));
        assert!(valid_project_id("My Project"));
        assert!(!valid_project_id(".trash"));
        assert!(!valid_project_id("a/b"));
        assert!(!valid_project_id(".."));
        assert!(!valid_project_id(""));
    }
}
