//! Host core: the canvas authority for one project.
//!
//! Owns the project folder, the open canvases, the session (actor) registry with presence,
//! soft leased claims, the event bus and the debounced autosave.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use falos_doc::{Actor, ActorKind, Annotation, Canvas, Project, Version};
use serde::Serialize;
use tokio::sync::{Notify, broadcast};

use crate::context::{ContextRenderer, NoRenderer};

pub const IDEMPOTENCY_WINDOW: Duration = Duration::from_secs(120);
const AUTOSAVE_DEBOUNCE: Duration = Duration::from_millis(400);
const COLORS: [&str; 8] = [
    "#e5484d", "#f76b15", "#ffc53d", "#46a758", "#12a594", "#0090ff", "#8e4ec6", "#d6409f",
];

/// `FAL_KEY=…` from `$FALOS_HOME/fal.env` (the same file the agent runtime reads).
fn fal_env_key() -> Option<String> {
    let text = std::fs::read_to_string(crate::server::home_dir().join("fal.env")).ok()?;
    text.lines().find_map(|l| {
        let l = l.trim().trim_start_matches("export ").trim();
        let v = l.strip_prefix("FAL_KEY")?.trim_start().strip_prefix('=')?;
        let v = v.trim().trim_matches(|c| c == '"' || c == '\'');
        (!v.is_empty()).then(|| v.to_string())
    })
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Everything observable that happens on the host. Sent on a tokio broadcast channel to
/// sync clients (see `PROTOCOL.md`).
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// A doc change. `bytes` (Loro updates) travel as a separate binary frame.
    Doc {
        canvas: String,
        actor: String,
        version: String,
        created: Vec<String>,
        touched: Vec<String>,
        #[serde(skip)]
        bytes: Arc<Vec<u8>>,
        /// Sync connection the change came from (not echoed back to it).
        #[serde(skip)]
        origin: Option<u64>,
    },
    Presence {
        session: SessionInfo,
    },
    SessionClosed {
        session: String,
    },
    Claims {
        canvas: String,
        claims: Vec<ClaimInfo>,
    },
    ClaimRevoked {
        canvas: String,
        node: String,
        actor: String,
        by: String,
    },
    AnnotationAssigned {
        canvas: String,
        annotation: String,
        node: String,
        assignee: String,
        by: String,
    },
    Job {
        canvas: String,
        node: String,
        model: String,
        status: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        queue_position: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        /// USD spent (fal-backed ops), on `done`.
        #[serde(skip_serializing_if = "Option::is_none")]
        cost: Option<f64>,
    },
    /// A chat event (see `chat.rs`, `docs/CHAT-PROTOCOL.md`).
    #[serde(rename = "chat.event")]
    ChatEvent {
        canvas: String,
        chat: String,
        seq: u64,
        event: serde_json::Value,
    },
    /// The chats of a canvas changed (see `chat.rs`).
    #[serde(rename = "chat.list")]
    ChatList {
        canvas: String,
        chats: serde_json::Value,
    },
    /// Batch generation progress (see `batch.rs`); same payload as the `batch` chat event.
    #[serde(rename = "batch")]
    Batch { canvas: String, batch: serde_json::Value },
    /// A chat was deleted (see `chat.rs`).
    #[serde(rename = "chat.deleted")]
    ChatDeleted { canvas: String, chat: String },
    /// An agent session read or changed nodes (see `activity.rs`).
    #[serde(rename = "activity")]
    Activity {
        canvas: String,
        session: String,
        agent: String,
        chat: Option<String>,
        color: String,
        op: String,
        nodes: Vec<String>,
        at: i64,
    },
    /// Nodes left a session's working set (`finish_working`, expiry, session end). `nodes: null` = all.
    #[serde(rename = "activity.clear")]
    ActivityClear {
        session: String,
        nodes: Option<Vec<String>>,
    },
    /// An MCP session from an external agent client started (connect flow).
    #[serde(rename = "agent.connected")]
    AgentConnected {
        client: String,
        #[serde(rename = "clientName")]
        client_name: String,
        #[serde(rename = "agentName")]
        agent_name: String,
        session: String,
        chat: Option<String>,
        transport: String,
    },
    /// An app's files changed (`what: files`: reload) or it logged errors (`what: logs`); see `apps`.
    #[serde(rename = "app")]
    App {
        app: String,
        node: Option<String>,
        version: u64,
        what: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        errors: Option<usize>,
    },
    /// An external agent session's first mutation (connect flow).
    #[serde(rename = "agent.first_write")]
    AgentFirstWrite {
        client: String,
        session: String,
        canvas: String,
        nodes: Vec<String>,
    },
    /// A held notice changed (see [`Host::set_notice`]): `{key, level, text, actions, copy?}`,
    /// or `{key, gone: true}`.
    #[serde(rename = "notice")]
    Notice { notice: serde_json::Value },
    /// The canvas on disk was replaced (restored from a backup, started fresh): clients load
    /// it again from scratch instead of merging.
    #[serde(rename = "canvas.reset")]
    CanvasReset { canvas: String },
}

impl Event {
    pub fn canvas(&self) -> Option<&str> {
        match self {
            Event::Doc { canvas, .. }
            | Event::Claims { canvas, .. }
            | Event::ClaimRevoked { canvas, .. }
            | Event::AnnotationAssigned { canvas, .. }
            | Event::Job { canvas, .. }
            | Event::ChatEvent { canvas, .. }
            | Event::ChatList { canvas, .. }
            | Event::ChatDeleted { canvas, .. }
            | Event::Batch { canvas, .. }
            | Event::CanvasReset { canvas, .. }
            | Event::Activity { canvas, .. } => Some(canvas),
            Event::Presence { .. }
            | Event::SessionClosed { .. }
            | Event::ActivityClear { .. }
            | Event::AgentConnected { .. }
            | Event::App { .. }
            | Event::Notice { .. }
            | Event::AgentFirstWrite { .. } => None,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Presence {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canvas: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<[f64; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport: Option<[f64; 4]>,
    pub last_activity: i64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub id: String,
    pub agent_name: String,
    pub actor: String,
    pub kind: ActorKind,
    /// The chat's color when the session belongs to a chat, else the session's own.
    pub color: String,
    pub presence: Presence,
    /// Chat this session works for (a runtime chat `c_…` or an external pseudo-chat `ext:…`).
    pub chat: Option<String>,
    /// Agent label: the runtime's subagent label (`generator-2`) or the actor id.
    pub agent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// What the cursor shows: the `set_status` text, else the agent name.
    pub label: String,
    /// `thinking` | `writing` | `idle` (idle after 10 s without activity).
    pub state: String,
    /// Normalized MCP client (`claude`, `codex`, `cursor`, `gemini`, `opencode`, …) of external agents.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
}

pub struct Session {
    pub id: String,
    pub actor: Actor,
    pub color: String,
    pub presence: Presence,
    /// Canvas version this session last read, per canvas: the default staleness baseline.
    pub read_versions: HashMap<String, Version>,
    /// Things that happened to this session (claim revoked, annotation assigned), drained
    /// into its next tool result.
    pub notices: Vec<String>,
    /// See [`SessionInfo`].
    pub chat: Option<String>,
    pub chat_color: Option<String>,
    pub agent: Option<String>,
    pub parent: Option<String>,
    pub client: Option<String>,
    /// `stdio` when connected through `falos-host mcp`.
    pub via: Option<String>,
    pub state: String,
    /// Human sessions: the user's current selection (`selection` frames).
    pub selection: Vec<String>,
    /// (canvas, node) → last activity (ms): the working set behind `activity` events.
    pub working: HashMap<(String, String), i64>,
    /// Has mutated anything yet (for `agent.first_write`).
    pub wrote: bool,
    /// External agents: what the humans sent them (messages, comments, selection, new
    /// references), drained by the `inbox` tool and appended to tool results (see `director`).
    pub inbox: Vec<crate::director::InboxItem>,
    /// External agents: the MCP connection, for push (`notifications/claude/channel`).
    pub peer: Option<rmcp::Peer<rmcp::RoleServer>>,
    /// How `session` chose the project (`folder …`, `open in fal OS`, …).
    pub bound_by: Option<String>,
}

impl Session {
    pub fn agent_label(&self) -> String {
        self.agent.clone().unwrap_or_else(|| self.actor.id.clone())
    }

    pub fn display_color(&self) -> String {
        self.chat_color.clone().unwrap_or_else(|| self.color.clone())
    }

    pub fn info(&self) -> SessionInfo {
        let label = if self.presence.status.trim().is_empty() {
            self.agent.clone().unwrap_or_else(|| self.actor.name.clone())
        } else {
            self.presence.status.clone()
        };
        SessionInfo {
            id: self.id.clone(),
            agent_name: self.actor.name.clone(),
            actor: self.actor.id.clone(),
            kind: self.actor.kind,
            color: self.display_color(),
            presence: self.presence.clone(),
            chat: self.chat.clone(),
            agent: self.agent_label(),
            parent: self.parent.clone(),
            label,
            state: self.state.clone(),
            client: self.client.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ClaimInfo {
    pub node: String,
    pub session: String,
    pub actor: String,
    /// Unix millis.
    pub expires: i64,
}

#[derive(Default)]
pub struct ClaimOutcome {
    pub granted: Vec<String>,
    /// `id: claimed by @actor` for ids another session holds.
    pub refused: Vec<String>,
}

struct Open {
    canvas: Canvas,
    dirty: bool,
    /// Nodes in the file on disk (last load or save): the collapse guard's baseline.
    saved_nodes: usize,
    /// Nodes removed by mutations since the last save (explains a drop in the count).
    deleted: usize,
    /// "save anyway": the next save skips the collapse guard.
    force: bool,
}

/// Saves stopped by the guard or failing, per canvas (for notices).
const SAVE_KEY: &str = "save";

pub struct HostOptions {
    pub fal_queue_url: String,
    pub fal_key: Option<String>,
    /// Use the signed-in fal session (via the fal MCP relay) when there is no `fal_key`.
    pub fal_session: bool,
    pub renderer: Box<dyn ContextRenderer>,
    /// Home directory whose agent configs `agent.register` edits (default `$HOME`).
    pub home: Option<PathBuf>,
}

impl Default for HostOptions {
    fn default() -> Self {
        Self {
            fal_queue_url: std::env::var("FALOS_FAL_QUEUE_URL")
                .unwrap_or_else(|_| "https://queue.fal.run".into()),
            fal_key: std::env::var("FAL_KEY")
                .ok()
                .filter(|k| !k.is_empty())
                .or_else(fal_env_key),
            fal_session: true,
            renderer: Box::new(NoRenderer),
            home: std::env::var_os("HOME").map(PathBuf::from),
        }
    }
}

pub struct Host {
    /// Project id: the project folder's name (stable; `project.json`'s `name` is the label).
    pub id: String,
    dir: PathBuf,
    /// Cancelled when the project is closed (deleted / moved to the trash): sync clients
    /// disconnect and nothing is written to the folder any more.
    pub closed: tokio_util::sync::CancellationToken,
    autosaver: std::sync::Once,
    project: Mutex<Project>,
    canvases: Mutex<HashMap<String, Arc<Mutex<Open>>>>,
    sessions: Mutex<HashMap<String, Session>>,
    claims: Mutex<BTreeMap<(String, String), ClaimInfo>>,
    idempotency: Mutex<HashMap<(String, String), (Instant, String)>>,
    events: broadcast::Sender<Event>,
    save_needed: Notify,
    pub opts: HostOptions,
    pub http: reqwest::Client,
    /// Chat routing between canvas clients and the agent runtime.
    pub chat: crate::chat::ChatHub,
    /// `(mcp url, token)` of the running server, for `agent.register`.
    pub endpoint: Mutex<Option<(String, String)>>,
    /// Running and finished batch generations (`generate_batch`).
    pub batches: crate::batch::Batches,
    /// Single generation / op jobs in flight: node → `queued` | `running` (from `Event::Job`;
    /// the tray's "12 generating · 3 queued").
    pub jobs: Mutex<HashMap<String, String>>,
    /// Running and finished op runs (`transform` / `compose`).
    pub ops_runs: crate::ops::Runs,
    /// App nodes: versions, logs, checks (see `apps`).
    pub apps: crate::apps::Apps,
    sweeper: std::sync::Once,
    /// Wakes `inbox` long-polls when an external agent's inbox gets something (see `director`).
    pub inbox_wake: Notify,
    /// Messages for an external agent client whose session is gone (`codex` → items), handed
    /// to its next session.
    pub inbox_orphans: Mutex<HashMap<String, Vec<crate::director::InboxItem>>>,
    /// Last time a human client showed activity here (ms): connect, selection, focus. The
    /// most recent connected project is the one "open in fal OS" for agent binding.
    pub human_seen: std::sync::atomic::AtomicI64,
    /// Canvases that failed to load: id → why. Their files are never written; writes are refused.
    failed: Mutex<HashMap<String, String>>,
    /// Read-only because a newer fal OS wrote it: `""` = the whole project, else a canvas id.
    read_only: Mutex<HashMap<String, String>>,
    /// Held notices for the UI, by key (see [`Host::set_notice`]).
    notices: Mutex<BTreeMap<String, serde_json::Value>>,
    /// Each human actor's last camera and selection per canvas, so a reloaded webview comes
    /// back where it was: (canvas, actor) → `{camera, selection}`.
    views: Mutex<HashMap<(String, String), serde_json::Value>>,
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
    let out: String = out.trim_end_matches('-').chars().take(24).collect();
    if out.is_empty() { "agent".into() } else { out }
}

fn random_hex(bytes: usize) -> String {
    (0..bytes)
        .map(|_| format!("{:02x}", rand::random::<u8>()))
        .collect()
}

impl Host {
    /// Open (or create) the project at `dir`. Ensures at least one canvas exists.
    pub fn open(dir: &Path, opts: HostOptions) -> Result<Arc<Self>> {
        Self::open_with(dir, opts, Arc::default())
    }

    /// [`Host::open`] sharing a runtime link with the other projects of a workspace.
    pub fn open_with(
        dir: &Path,
        opts: HostOptions,
        link: Arc<crate::runtime::RuntimeLink>,
    ) -> Result<Arc<Self>> {
        std::fs::create_dir_all(dir)?;
        let dir = dir.canonicalize()?;
        let folder = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "project".into());
        let mut notices = BTreeMap::new();
        let mut read_only = HashMap::new();
        let pj = dir.join("project.json");
        let mut project = if pj.exists() {
            match Project::load(&dir) {
                Ok(p) => p,
                // A damaged project.json: keep it aside and rebuild it from the canvas files,
                // so everything else in the project still opens.
                Err(e) => {
                    let name = std::fs::read(&pj)
                        .ok()
                        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                        .and_then(|v| v["name"].as_str().map(str::to_string))
                        .unwrap_or_else(|| folder.clone());
                    let kept = crate::persist::set_aside(&pj).context("loading project.json")?;
                    let p = crate::persist::rebuild_project(&dir, &slug(&folder), &name);
                    p.save(&dir)?;
                    tracing::error!("project.json in {} was damaged ({e}); rebuilt from the canvas files, original kept as {}", dir.display(), kept.display());
                    let kept = kept.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    notices.insert(
                        "project.json".to_string(),
                        notice_json("project.json", "error", &format!("project.json was damaged: rebuilt from the canvas files (the original is kept as {kept})"), &[("dismiss", "dismiss")], None),
                    );
                    p
                }
            }
        } else {
            let p = Project::new(&slug(&folder), &folder);
            p.save(&dir)?;
            p
        };
        if project.format > falos_doc::FORMAT {
            let why = format!("made by a newer fal OS (format {} > {}): read-only here, saving paused · update fal OS to edit it", project.format, falos_doc::FORMAT);
            tracing::warn!("{}: {why}", dir.display());
            notices.insert("readonly".to_string(), notice_json("readonly", "ongoing", &why, &[], None));
            read_only.insert(String::new(), why);
        } else if project.format < falos_doc::FORMAT {
            // Migrate: keep the original first.
            if pj.is_file() {
                crate::persist::archive(&dir, &pj, project.format)?;
            }
            project.format = falos_doc::FORMAT;
            project.save(&dir)?;
        }
        if project.canvases.is_empty() && read_only.is_empty() {
            project.save_canvas(&dir, &Canvas::new("main", "Main"))?;
        }
        let (events, _) = broadcast::channel(1024);
        let id = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| project.id.clone());
        Ok(Arc::new(Self {
            id,
            dir,
            closed: Default::default(),
            autosaver: std::sync::Once::new(),
            project: Mutex::new(project),
            canvases: Mutex::default(),
            sessions: Mutex::default(),
            claims: Mutex::default(),
            idempotency: Mutex::default(),
            events,
            save_needed: Notify::new(),
            opts,
            http: reqwest::Client::new(),
            chat: crate::chat::ChatHub::new(link),
            endpoint: Mutex::default(),
            batches: Default::default(),
            jobs: Mutex::default(),
            ops_runs: Default::default(),
            apps: Default::default(),
            sweeper: std::sync::Once::new(),
            inbox_wake: Notify::new(),
            inbox_orphans: Mutex::default(),
            human_seen: Default::default(),
            failed: Mutex::default(),
            read_only: Mutex::new(read_only),
            notices: Mutex::new(notices),
            views: Mutex::default(),
        }))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn project(&self) -> Project {
        self.project.lock().unwrap().clone()
    }

    /// Change `project.json` (name, extra fields) and save it.
    pub fn update_project(&self, f: impl FnOnce(&mut Project)) -> Result<()> {
        if self.closed.is_cancelled() {
            bail!("project `{}` is closed", self.id);
        }
        if let Some(why) = self.read_only.lock().unwrap().get("") {
            bail!("project `{}` is {why}", self.id);
        }
        let mut p = self.project.lock().unwrap();
        f(&mut p);
        p.save(&self.dir)?;
        Ok(())
    }

    /// The linked fal Agent project (`fal_project` in `project.json`).
    pub fn fal_project(&self) -> Option<String> {
        self.project.lock().unwrap().extra.get("fal_project")?.as_str().map(str::to_string)
    }

    pub fn set_fal_project(&self, id: Option<&str>) -> Result<()> {
        self.update_project(|p| match id {
            Some(id) => {
                p.extra.insert("fal_project".into(), serde_json::Value::from(id));
            }
            None => {
                p.extra.remove("fal_project");
            }
        })
    }

    /// Start the per-host background tasks (sweeper, chat list broadcasts, autosave); idempotent.
    pub fn start_tasks(self: &Arc<Self>) {
        self.start_sweeper();
        self.chat.start_list_task(self);
        crate::apps::start(self);
        self.autosaver.call_once(|| {
            tokio::spawn(self.clone().autosave());
        });
    }

    /// Close the project: flush, stop its running chats, disconnect its clients; nothing is
    /// written to its folder afterwards (it is about to move).
    pub fn close(&self) {
        if let Err(e) = self.flush() {
            tracing::error!("saving {} before closing: {e:#}", self.id);
        }
        for row in self.chat.list(self, None).as_array().into_iter().flatten() {
            if row["state"] == "running"
                && row["external"] != true
                && let Some(chat) = row["chat"].as_str()
            {
                self.chat.to_runtime(self, serde_json::json!({"type": "stop", "chat": chat}));
            }
        }
        self.closed.cancel();
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    pub fn emit(&self, e: Event) {
        if let Event::Job { node, status, .. } = &e {
            let mut jobs = self.jobs.lock().unwrap();
            match status.as_str() {
                "queued" | "running" => {
                    jobs.insert(node.clone(), status.clone());
                }
                _ => {
                    jobs.remove(node);
                }
            }
        }
        let _ = self.events.send(e);
    }

    // ---- canvases ----------------------------------------------------------------------

    /// Resolve a canvas id or name to its id.
    pub fn resolve_canvas(&self, key: &str) -> Result<String> {
        let p = self.project.lock().unwrap();
        p.canvases
            .iter()
            .find(|c| c.id == key)
            .or_else(|| p.canvases.iter().find(|c| c.name.eq_ignore_ascii_case(key)))
            .map(|c| c.id.clone())
            .ok_or_else(|| {
                let ids: Vec<&str> = p.canvases.iter().map(|c| c.id.as_str()).collect();
                anyhow!(
                    "unknown canvas `{key}` (canvases: {}); call `session` with canvas=\"{key}\" to create it",
                    ids.join(", ")
                )
            })
    }

    /// Create a canvas named `name` if no canvas has that id or name. Returns its id.
    pub fn ensure_canvas(&self, name: &str) -> Result<String> {
        if let Ok(id) = self.resolve_canvas(name) {
            return Ok(id);
        }
        if let Some(why) = self.read_only.lock().unwrap().get("") {
            bail!("project `{}` is {why}", self.id);
        }
        let mut id = slug(name);
        let mut p = self.project.lock().unwrap();
        if p.canvases.iter().any(|c| c.id == id) {
            id = format!("{id}-{}", random_hex(2));
        }
        p.save_canvas(&self.dir, &Canvas::new(&id, name))?;
        Ok(id)
    }

    fn open_canvas(&self, id: &str) -> Result<Arc<Mutex<Open>>> {
        let id = self.resolve_canvas(id)?;
        let mut map = self.canvases.lock().unwrap();
        if let Some(o) = map.get(&id) {
            return Ok(o.clone());
        }
        if let Some(why) = self.failed.lock().unwrap().get(&id) {
            bail!("canvas `{id}` couldn't be opened ({why}); its file is untouched");
        }
        let (mut canvas, bytes) = match self.load_canvas_file(&id) {
            Ok(c) => c,
            Err(e) => {
                let why = format!("{e:#}");
                tracing::error!("canvas `{id}` of {} failed to load: {why}", self.id);
                self.failed.lock().unwrap().insert(id.clone(), why.clone());
                let n = crate::persist::backups(&self.dir, &id).len();
                let actions: &[(&str, &str)] = if n > 0 { &[("restore", "restore backup")] } else { &[("fresh", "start fresh")] };
                let name = self.canvas_name(&id);
                self.set_notice(&format!("load:{id}"), "ongoing", &format!("couldn't open {name}, the file is untouched{}", if n > 0 { "" } else { " · no backup yet" }), actions, None);
                bail!("canvas `{id}` couldn't be opened ({why}); its file is untouched");
            }
        };
        let format = canvas.format();
        let mut dirty = false;
        if format > falos_doc::FORMAT {
            let why = format!("made by a newer fal OS (format {format} > {}): read-only here, saving paused · update fal OS to edit it", falos_doc::FORMAT);
            self.read_only.lock().unwrap().insert(id.clone(), why.clone());
            self.set_notice(&format!("readonly:{id}"), "ongoing", &format!("{}: {why}", self.canvas_name(&id)), &[], None);
        } else if !self.is_read_only(&id) {
            // A first good copy for the backup ring as soon as a canvas loads.
            if crate::persist::backups(&self.dir, &id).is_empty() {
                let _ = crate::persist::write_backup(&self.dir, &id, &bytes);
            }
            if format < falos_doc::FORMAT {
                // Migrate in memory; the original is archived before the first save.
                let file = falos_doc::canvas_path(&self.dir, &id)?;
                crate::persist::archive(&self.dir, &file, format)?;
                canvas.set_format(falos_doc::FORMAT)?;
                dirty = true;
            }
        }
        let saved_nodes = canvas.node_count();
        let o = Arc::new(Mutex::new(Open {
            canvas,
            dirty,
            saved_nodes,
            deleted: 0,
            force: false,
        }));
        map.insert(id, o.clone());
        if dirty {
            self.save_needed.notify_one();
        }
        Ok(o)
    }

    /// Tests only: swap an open canvas's in-memory doc behind the host's back (no mutation
    /// bookkeeping), the way a bug that resets a doc would. The collapse guard must catch it.
    #[doc(hidden)]
    pub fn replace_canvas_unchecked(&self, id: &str, canvas: Canvas) -> Result<()> {
        let o = self.open_canvas(id)?;
        let mut o = o.lock().unwrap();
        o.canvas = canvas;
        o.dirty = true;
        Ok(())
    }

    fn load_canvas_file(&self, id: &str) -> Result<(Canvas, Vec<u8>)> {
        let path = falos_doc::canvas_path(&self.dir, id)?;
        let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        let canvas = Canvas::from_snapshot(&bytes).with_context(|| format!("{} is not a readable canvas", path.display()))?;
        Ok((canvas, bytes))
    }

    fn canvas_name(&self, id: &str) -> String {
        self.project
            .lock()
            .unwrap()
            .canvases
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| id.to_string())
    }

    fn is_read_only(&self, canvas: &str) -> bool {
        let ro = self.read_only.lock().unwrap();
        ro.contains_key("") || ro.contains_key(canvas)
    }

    /// Why writes to `canvas` are refused right now (failed to load, read-only), if they are.
    pub fn write_block(&self, canvas: &str) -> Option<String> {
        let id = self.resolve_canvas(canvas).unwrap_or_else(|_| canvas.to_string());
        if let Some(why) = self.failed.lock().unwrap().get(&id) {
            return Some(format!("canvas `{id}` isn't loaded ({why}); the user can restore a backup in fal OS: retry shortly"));
        }
        let ro = self.read_only.lock().unwrap();
        ro.get("").or_else(|| ro.get(&id)).map(|why| format!("canvas `{id}` is {why}"))
    }

    /// Whether `canvas` failed to load (its file is kept untouched).
    pub fn load_failed(&self, canvas: &str) -> bool {
        self.failed.lock().unwrap().contains_key(canvas)
    }

    // ---- notices --------------------------------------------------------------------------

    /// Raise or update a held notice for the UI. `level`: `ongoing` (one keyed line, not
    /// dismissable while active), `error` (stays until dismissed), `ok` / `info` (fade).
    /// `actions`: `(id, label)` pairs the UI sends back as `notice.action`.
    pub fn set_notice(&self, key: &str, level: &str, text: &str, actions: &[(&str, &str)], copy: Option<String>) {
        let n = notice_json(key, level, text, actions, copy);
        let changed = {
            let mut m = self.notices.lock().unwrap();
            let changed = m.get(key) != Some(&n);
            m.insert(key.to_string(), n.clone());
            changed
        };
        if changed {
            self.emit(Event::Notice { notice: n });
        }
    }

    /// Drop a held notice; `resolved` turns it into a short success line ("back in sync").
    pub fn clear_notice(&self, key: &str, resolved: Option<&str>) {
        if self.notices.lock().unwrap().remove(key).is_none() {
            return;
        }
        let n = match resolved {
            Some(t) => notice_json(key, "ok", t, &[], None),
            None => serde_json::json!({"key": key, "gone": true}),
        };
        self.emit(Event::Notice { notice: n });
    }

    /// The held notices, for a client that connects.
    pub fn notices(&self) -> Vec<serde_json::Value> {
        self.notices.lock().unwrap().values().cloned().collect()
    }

    /// A notice button: `restore` / `fresh` (a canvas that failed to load), `save-anyway`
    /// (a refused save), `dismiss`.
    pub fn notice_action(&self, key: &str, action: &str) -> Result<()> {
        match (action, key.split_once(':')) {
            ("restore", Some(("load", id))) => self.restore_backup(id),
            ("fresh", Some(("load", id))) => self.start_fresh(id),
            ("save-anyway", Some(("refused", id))) => {
                let o = self.canvases.lock().unwrap().get(id).cloned().ok_or_else(|| anyhow!("canvas `{id}` isn't open"))?;
                o.lock().unwrap().force = true;
                self.save_needed.notify_one();
                Ok(())
            }
            ("dismiss", _) => {
                let level = self.notices.lock().unwrap().get(key).map(|n| n["level"].as_str().unwrap_or("").to_string());
                if level.as_deref() == Some("ongoing") {
                    bail!("that notice stays until the problem is solved");
                }
                self.clear_notice(key, None);
                Ok(())
            }
            _ => bail!("unknown notice action `{action}` for `{key}`"),
        }
    }

    /// Replace a canvas that failed to load with its newest backup that loads. The broken file
    /// is kept next to it (`<id>.loro.broken-<ms>`), byte for byte.
    pub fn restore_backup(&self, id: &str) -> Result<()> {
        let file = falos_doc::canvas_path(&self.dir, id)?;
        let mut chosen = None;
        for (_, path) in crate::persist::backups(&self.dir, id) {
            let Ok(bytes) = std::fs::read(&path) else { continue };
            if Canvas::from_snapshot(&bytes).is_ok() {
                // The newest slot is rewritten on every save: its age is its mtime.
                let ms = std::fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_millis() as i64);
                chosen = Some((ms, bytes));
                break;
            }
        }
        let (ms, bytes) = chosen.ok_or_else(|| anyhow!("no readable backup of canvas `{id}`"))?;
        if file.exists() {
            crate::persist::set_aside(&file)?;
        }
        falos_doc::write_durable(&file, &bytes)?;
        self.failed.lock().unwrap().remove(id);
        let mins = ((now_ms() - ms) / 60_000).max(0);
        let age = if mins < 1 { "under a minute old".to_string() } else { format!("{mins} min old") };
        tracing::info!("canvas `{id}` of {} restored from the backup saved at {ms}", self.id);
        self.clear_notice(&format!("load:{id}"), Some(&format!("restored {} from the last good copy ({age})", self.canvas_name(id))));
        self.emit(Event::CanvasReset { canvas: id.to_string() });
        Ok(())
    }

    /// Start a canvas that failed to load (and has no backup) empty; the broken file is kept
    /// next to it.
    pub fn start_fresh(&self, id: &str) -> Result<()> {
        if !self.load_failed(id) {
            bail!("canvas `{id}` loaded fine");
        }
        let file = falos_doc::canvas_path(&self.dir, id)?;
        if file.exists() {
            crate::persist::set_aside(&file)?;
        }
        let name = self.canvas_name(id);
        self.project.lock().unwrap().save_canvas(&self.dir, &Canvas::new(id, &name))?;
        self.failed.lock().unwrap().remove(id);
        self.clear_notice(&format!("load:{id}"), Some(&format!("{name} started fresh (the old file is kept)")));
        self.emit(Event::CanvasReset { canvas: id.to_string() });
        Ok(())
    }

    /// Remember a human actor's camera / selection on a canvas (see `views`).
    pub fn remember_view(&self, canvas: &str, actor: &str, camera: Option<serde_json::Value>, selection: Option<Vec<String>>) {
        let mut views = self.views.lock().unwrap();
        let v = views.entry((canvas.to_string(), actor.to_string())).or_insert_with(|| serde_json::json!({}));
        if let Some(c) = camera {
            v["camera"] = c;
        }
        if let Some(s) = selection {
            v["selection"] = serde_json::json!(s);
        }
    }

    pub fn view(&self, canvas: &str, actor: &str) -> Option<serde_json::Value> {
        self.views.lock().unwrap().get(&(canvas.to_string(), actor.to_string())).cloned()
    }

    /// Read-only access to a canvas.
    pub fn read<T>(&self, canvas: &str, f: impl FnOnce(&Canvas) -> T) -> Result<T> {
        let o = self.open_canvas(canvas)?;
        let o = o.lock().unwrap();
        Ok(f(&o.canvas))
    }

    /// Mutate a canvas. Emits the resulting Loro update + change summary, detects new
    /// annotation assignments, and schedules an autosave.
    pub fn mutate<T>(
        &self,
        canvas: &str,
        actor: &str,
        origin: Option<u64>,
        f: impl FnOnce(&mut Canvas) -> falos_doc::Result<T>,
    ) -> Result<T> {
        let id = self.resolve_canvas(canvas)?;
        if let Some(why) = self.write_block(&id) {
            bail!("{why}");
        }
        let o = self.open_canvas(&id)?;
        let (out, event, assigned, created_ids, touched_ids, comments) = {
            let mut o = o.lock().unwrap();
            let before_vv = o.canvas.vv();
            let before_version = o.canvas.version();
            let before_anns = o.canvas.annotations();
            let before_nodes: Vec<String> = o.canvas.nodes().into_iter().map(|n| n.id).collect();
            let out = f(&mut o.canvas)?;
            if o.canvas.vv() == before_vv {
                return Ok(out);
            }
            o.dirty = true;
            let gone = before_nodes.iter().filter(|n| o.canvas.node(n).is_none()).count();
            o.deleted += gone;
            let bytes = o.canvas.export_updates(&before_vv)?;
            let changes = o.canvas.changes_since(&before_version).unwrap_or_default();
            let mut touched: Vec<String> = changes
                .iter()
                .flat_map(|c| c.nodes.iter().chain(&c.annotations).cloned())
                .collect();
            touched.sort();
            touched.dedup();
            let existed: std::collections::HashSet<String> = before_anns
                .iter()
                .map(|a| a.id.clone())
                .chain(before_nodes)
                .collect();
            let created = touched
                .iter()
                .filter(|t| !existed.contains(*t))
                .cloned()
                .collect();
            let after_anns = o.canvas.annotations();
            let assigned = new_assignments(&before_anns, &after_anns);
            let comments = new_messages(&before_anns, &after_anns, actor);
            let (c2, t2): (Vec<String>, Vec<String>) = (created, touched.clone());
            let event = Event::Doc {
                canvas: id.clone(),
                actor: actor.to_string(),
                version: o.canvas.version().to_string(),
                created: c2.clone(),
                touched,
                bytes: Arc::new(bytes),
                origin,
            };
            (out, event, assigned, c2, t2, comments)
        };
        self.emit(event);
        self.after_mutation(&id, actor, &created_ids, &touched_ids);
        crate::director::on_change(self, &id, actor, &created_ids, comments);
        for a in assigned {
            let assignee = a.assignee.clone().unwrap_or_default();
            self.notify_actor(
                &assignee,
                format!(
                    "annotation {} on {} in canvas `{id}` was assigned to you by @{actor}; see get_annotation",
                    a.id, a.node_id
                ),
            );
            self.emit(Event::AnnotationAssigned {
                canvas: id.clone(),
                annotation: a.id,
                node: a.node_id,
                assignee,
                by: actor.to_string(),
            });
        }
        self.save_needed.notify_one();
        Ok(out)
    }

    pub fn snapshot(&self, canvas: &str) -> Result<(String, Vec<u8>, String)> {
        let id = self.resolve_canvas(canvas)?;
        self.read(&id, |c| {
            Ok((id.clone(), c.export_snapshot()?, c.version().to_string()))
        })?
    }

    /// Write every dirty canvas to disk (durable: temp → fsync → rename → fsync dir). Before
    /// a canvas file is replaced, it goes into the backup ring when the newest backup is older
    /// than [`crate::persist::backup_every`]. Canvases that failed to load or are read-only are
    /// never written. A save that would drop most of the nodes without matching deletes is
    /// refused (the file on disk is kept) with a notice. Tries every canvas; returns the first
    /// error.
    pub fn flush(&self) -> Result<()> {
        if self.closed.is_cancelled() || self.read_only.lock().unwrap().contains_key("") {
            return Ok(());
        }
        let open: Vec<(String, Arc<Mutex<Open>>)> = self.canvases.lock().unwrap().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        let mut first_err = None;
        for (id, o) in open {
            let mut o = o.lock().unwrap();
            if !o.dirty || self.is_read_only(&id) || self.load_failed(&id) {
                continue;
            }
            let now = o.canvas.node_count();
            let refused_key = format!("refused:{id}");
            if !o.force && crate::persist::collapse_refused(o.saved_nodes, now, o.deleted) {
                let msg = format!(
                    "didn't save {}: {} of {} nodes vanished at once without deletes; the last good file is kept",
                    self.canvas_name(&id),
                    o.saved_nodes - now,
                    o.saved_nodes
                );
                tracing::error!("{}: {msg}", self.id);
                self.set_notice(&refused_key, "ongoing", &msg, &[("save-anyway", "save anyway")], None);
                continue;
            }
            let r = (|| -> Result<()> {
                let bytes = o.canvas.export_snapshot()?;
                let file = falos_doc::canvas_path(&self.dir, &id)?;
                falos_doc::write_durable(&file, &bytes)?;
                self.project.lock().unwrap().record_canvas(&self.dir, &o.canvas)?;
                // A failed backup never fails the save itself.
                if let Err(e) = crate::persist::track_save(&self.dir, &id, &bytes, crate::persist::backup_every()) {
                    tracing::warn!("backup of canvas `{id}` failed: {e:#}");
                }
                Ok(())
            })();
            match r {
                Ok(()) => {
                    o.dirty = false;
                    o.saved_nodes = now;
                    o.deleted = 0;
                    o.force = false;
                    self.clear_notice(&refused_key, Some("saved"));
                }
                Err(e) => {
                    let e = e.context(format!("saving canvas `{id}`"));
                    first_err.get_or_insert(e);
                }
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Debounced autosave loop; run it on the runtime for the host's lifetime. A failed save
    /// keeps the canvas dirty and retries with backoff (1 → 2 → 4 … 30 s) under a held
    /// "not saved to disk yet · retrying" notice, which turns into "saved to disk" once a
    /// retry succeeds.
    pub async fn autosave(self: Arc<Self>) {
        loop {
            tokio::select! {
                _ = self.save_needed.notified() => {}
                _ = self.closed.cancelled() => return,
            }
            tokio::time::sleep(AUTOSAVE_DEBOUNCE).await;
            let mut backoff = Duration::from_secs(1);
            loop {
                let h = self.clone();
                let r = tokio::task::spawn_blocking(move || h.flush()).await.unwrap_or_else(|e| Err(anyhow!("save task: {e}")));
                match r {
                    Ok(()) => {
                        self.clear_notice(SAVE_KEY, Some("saved to disk"));
                        break;
                    }
                    Err(e) => {
                        tracing::error!("autosave failed (retrying in {}s): {e:#}", backoff.as_secs());
                        self.set_notice(SAVE_KEY, "ongoing", &format!("not saved to disk yet · retrying ({})", short_error(&e)), &[], None);
                        tokio::select! {
                            _ = tokio::time::sleep(backoff) => {}
                            _ = self.closed.cancelled() => return,
                        }
                        backoff = (backoff * 2).min(Duration::from_secs(30));
                    }
                }
            }
        }
    }

    // ---- sessions ----------------------------------------------------------------------

    /// Register a session. The actor id is a slug of `agent_name`, suffixed when another
    /// session already uses it so every session has its own undo stack.
    pub fn create_session(
        &self,
        agent_name: &str,
        kind: ActorKind,
        actor_id: Option<&str>,
    ) -> SessionInfo {
        let mut sessions = self.sessions.lock().unwrap();
        let base = slug(actor_id.unwrap_or(agent_name));
        let taken = |id: &str| sessions.values().any(|s| s.actor.id == id);
        let actor_id = (1..)
            .map(|n| {
                if n == 1 {
                    base.clone()
                } else {
                    format!("{base}-{n}")
                }
            })
            .find(|id| !taken(id))
            .unwrap();
        let color = COLORS[actor_id.bytes().map(|b| b as usize).sum::<usize>() % COLORS.len()];
        let s = Session {
            id: format!("s_{}", random_hex(8)),
            actor: Actor {
                id: actor_id,
                name: agent_name.to_string(),
                kind,
            },
            color: color.into(),
            presence: Presence {
                last_activity: now_ms(),
                ..Default::default()
            },
            read_versions: HashMap::new(),
            notices: vec![],
            chat: None,
            chat_color: None,
            agent: None,
            parent: None,
            client: None,
            via: None,
            state: "idle".into(),
            selection: vec![],
            working: HashMap::new(),
            wrote: false,
            inbox: vec![],
            peer: None,
            bound_by: None,
        };
        let info = s.info();
        sessions.insert(s.id.clone(), s);
        drop(sessions);
        self.emit(Event::Presence {
            session: info.clone(),
        });
        info
    }

    pub fn close_session(&self, id: &str) {
        let removed = self.sessions.lock().unwrap().remove(id);
        if let Some(s) = removed {
            self.claims.lock().unwrap().retain(|_, c| c.session != id);
            if !s.working.is_empty() {
                self.emit(Event::ActivityClear {
                    session: id.into(),
                    nodes: None,
                });
            }
            self.emit(Event::SessionClosed { session: id.into() });
            if let Some(chat) = s.chat.as_deref().filter(|c| c.starts_with("ext:")) {
                self.chat.ext_session_closed(self, chat);
            }
        }
    }

    /// Run `f` on the session table.
    pub fn with_sessions<T>(&self, f: impl FnOnce(&mut HashMap<String, Session>) -> T) -> T {
        f(&mut self.sessions.lock().unwrap())
    }

    /// Start the once-per-host background sweeper (activity expiry, idle presence, ext chats).
    pub fn start_sweeper(self: &Arc<Self>) {
        self.sweeper.call_once(|| {
            let weak = Arc::downgrade(self);
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    let Some(host) = weak.upgrade() else { return };
                    host.sweep();
                }
            });
        });
    }

    /// Look up a session, mark activity, and run `f` on it.
    pub fn with_session<T>(&self, id: &str, f: impl FnOnce(&mut Session) -> T) -> Result<T> {
        let mut sessions = self.sessions.lock().unwrap();
        let s = sessions.get_mut(id).ok_or_else(|| {
            anyhow!(
                "unknown sessionId `{id}`: call the `session` tool first and pass its sessionId"
            )
        })?;
        s.presence.last_activity = now_ms();
        Ok(f(s))
    }

    pub fn actor(&self, session: &str) -> Result<Actor> {
        self.with_session(session, |s| s.actor.clone())
    }

    pub fn sessions(&self) -> Vec<SessionInfo> {
        let mut v: Vec<_> = self
            .sessions
            .lock()
            .unwrap()
            .values()
            .map(Session::info)
            .collect();
        v.sort_by(|a, b| a.actor.cmp(&b.actor));
        v
    }

    pub fn take_notices(&self, session: &str) -> Vec<String> {
        self.with_session(session, |s| std::mem::take(&mut s.notices))
            .unwrap_or_default()
    }

    fn notify_actor(&self, actor: &str, msg: String) {
        for s in self.sessions.lock().unwrap().values_mut() {
            if s.actor.id == actor {
                s.notices.push(msg.clone());
            }
        }
    }

    pub fn set_presence(
        &self,
        session: &str,
        f: impl FnOnce(&mut Presence),
    ) -> Result<SessionInfo> {
        let info = self.with_session(session, |s| {
            f(&mut s.presence);
            s.info()
        })?;
        self.emit(Event::Presence {
            session: info.clone(),
        });
        Ok(info)
    }

    // ---- claims ------------------------------------------------------------------------

    fn prune_claims(claims: &mut BTreeMap<(String, String), ClaimInfo>) {
        let now = now_ms();
        claims.retain(|_, c| c.expires > now);
    }

    pub fn claims(&self, canvas: &str) -> Vec<ClaimInfo> {
        let mut claims = self.claims.lock().unwrap();
        Self::prune_claims(&mut claims);
        claims
            .iter()
            .filter(|((c, _), _)| c == canvas)
            .map(|(_, v)| v.clone())
            .collect()
    }

    fn emit_claims(&self, canvas: &str) {
        self.emit(Event::Claims {
            canvas: canvas.into(),
            claims: self.claims(canvas),
        });
    }

    pub fn claim(
        &self,
        session: &str,
        canvas: &str,
        ids: &[String],
        ttl: Duration,
    ) -> Result<ClaimOutcome> {
        let actor = self.actor(session)?;
        let canvas = self.resolve_canvas(canvas)?;
        let missing: Vec<&String> = self.read(&canvas, |c| {
            ids.iter().filter(|id| c.node(id).is_none()).collect()
        })?;
        if !missing.is_empty() {
            bail!("unknown node ids: {missing:?}");
        }
        let mut out = ClaimOutcome::default();
        {
            let mut claims = self.claims.lock().unwrap();
            Self::prune_claims(&mut claims);
            let expires = now_ms() + ttl.as_millis() as i64;
            for id in ids {
                let key = (canvas.clone(), id.clone());
                match claims.get(&key) {
                    Some(c) if c.session != session => {
                        out.refused.push(format!("{id}: claimed by @{}", c.actor))
                    }
                    _ => {
                        claims.insert(
                            key,
                            ClaimInfo {
                                node: id.clone(),
                                session: session.into(),
                                actor: actor.id.clone(),
                                expires,
                            },
                        );
                        out.granted.push(id.clone());
                    }
                }
            }
        }
        self.emit_claims(&canvas);
        Ok(out)
    }

    /// Release this session's claims on `ids` (all of them when empty).
    pub fn release(&self, session: &str, canvas: &str, ids: &[String]) -> Result<usize> {
        self.actor(session)?;
        let canvas = self.resolve_canvas(canvas)?;
        let n = {
            let mut claims = self.claims.lock().unwrap();
            let before = claims.len();
            claims.retain(|(c, node), v| {
                !(c == &canvas && v.session == session && (ids.is_empty() || ids.contains(node)))
            });
            before - claims.len()
        };
        self.emit_claims(&canvas);
        Ok(n)
    }

    /// Human override: drop whoever's claim on `node`; the holder gets a notice.
    pub fn revoke_claim(&self, canvas: &str, node: &str, by: &str) -> Result<bool> {
        let canvas = self.resolve_canvas(canvas)?;
        let removed = self
            .claims
            .lock()
            .unwrap()
            .remove(&(canvas.clone(), node.to_string()));
        let Some(c) = removed else { return Ok(false) };
        if let Some(s) = self.sessions.lock().unwrap().get_mut(&c.session) {
            s.notices.push(format!(
                "your claim on {node} in canvas `{canvas}` was revoked by @{by}; stop editing it"
            ));
        }
        self.emit(Event::ClaimRevoked {
            canvas: canvas.clone(),
            node: node.into(),
            actor: c.actor,
            by: by.into(),
        });
        self.emit_claims(&canvas);
        Ok(true)
    }

    /// Warnings for `ids` claimed by sessions other than `session`.
    pub fn claim_warnings(&self, session: &str, canvas: &str, ids: &[String]) -> Vec<String> {
        self.claims(canvas)
            .into_iter()
            .filter(|c| c.session != session && ids.contains(&c.node))
            .map(|c| {
                format!(
                    "{} is claimed by @{} (claims are soft; coordinate before changing it further)",
                    c.node, c.actor
                )
            })
            .collect()
    }

    // ---- idempotency -------------------------------------------------------------------

    pub fn idempotent_get(&self, session: &str, request: &str) -> Option<String> {
        let mut m = self.idempotency.lock().unwrap();
        m.retain(|_, (t, _)| t.elapsed() < IDEMPOTENCY_WINDOW);
        m.get(&(session.into(), request.into()))
            .map(|(_, r)| r.clone())
    }

    pub fn idempotent_put(&self, session: &str, request: &str, result: String) {
        self.idempotency
            .lock()
            .unwrap()
            .insert((session.into(), request.into()), (Instant::now(), result));
    }
}

/// One notice as the UI gets it.
fn notice_json(key: &str, level: &str, text: &str, actions: &[(&str, &str)], copy: Option<String>) -> serde_json::Value {
    let actions: Vec<serde_json::Value> = actions.iter().map(|(id, label)| serde_json::json!({"id": id, "label": label})).collect();
    let mut n = serde_json::json!({"key": key, "level": level, "text": text, "actions": actions});
    if let Some(c) = copy {
        n["copy"] = c.into();
    }
    n
}

/// The innermost cause of an error, short (e.g. "Permission denied (os error 13)").
fn short_error(e: &anyhow::Error) -> String {
    let s = e.root_cause().to_string();
    s.chars().take(80).collect()
}

/// Thread messages `actor` added between two snapshots: `(annotation, node, text, assignee)`.
fn new_messages(before: &[Annotation], after: &[Annotation], actor: &str) -> Vec<(String, String, String, Option<String>)> {
    let prev: HashMap<&str, usize> = before.iter().map(|a| (a.id.as_str(), a.thread.len())).collect();
    let mut out = vec![];
    for a in after {
        let seen = prev.get(a.id.as_str()).copied().unwrap_or(0);
        for m in a.thread.iter().skip(seen).filter(|m| m.author == actor) {
            out.push((a.id.clone(), a.node_id.clone(), m.text.clone(), a.assignee.clone()));
        }
    }
    out
}

/// Annotations whose assignee is new or changed between two snapshots.
fn new_assignments(before: &[Annotation], after: &[Annotation]) -> Vec<Annotation> {
    let prev: HashMap<&str, Option<&str>> = before
        .iter()
        .map(|a| (a.id.as_str(), a.assignee.as_deref()))
        .collect();
    after
        .iter()
        .filter(|a| {
            a.assignee.is_some()
                && prev.get(a.id.as_str()).copied().flatten() != a.assignee.as_deref()
        })
        .cloned()
        .collect()
}
