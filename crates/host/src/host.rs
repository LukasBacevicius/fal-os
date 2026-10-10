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
    },
}

impl Event {
    pub fn canvas(&self) -> Option<&str> {
        match self {
            Event::Doc { canvas, .. }
            | Event::Claims { canvas, .. }
            | Event::ClaimRevoked { canvas, .. }
            | Event::AnnotationAssigned { canvas, .. }
            | Event::Job { canvas, .. } => Some(canvas),
            Event::Presence { .. } | Event::SessionClosed { .. } => None,
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
    pub color: String,
    pub presence: Presence,
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
}

impl Session {
    fn info(&self) -> SessionInfo {
        SessionInfo {
            id: self.id.clone(),
            agent_name: self.actor.name.clone(),
            actor: self.actor.id.clone(),
            kind: self.actor.kind,
            color: self.color.clone(),
            presence: self.presence.clone(),
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
}

pub struct HostOptions {
    pub fal_queue_url: String,
    pub fal_key: Option<String>,
    pub renderer: Box<dyn ContextRenderer>,
}

impl Default for HostOptions {
    fn default() -> Self {
        Self {
            fal_queue_url: std::env::var("FALOS_FAL_QUEUE_URL")
                .unwrap_or_else(|_| "https://queue.fal.run".into()),
            fal_key: std::env::var("FAL_KEY").ok().filter(|k| !k.is_empty()),
            renderer: Box::new(NoRenderer),
        }
    }
}

pub struct Host {
    dir: PathBuf,
    project: Mutex<Project>,
    canvases: Mutex<HashMap<String, Arc<Mutex<Open>>>>,
    sessions: Mutex<HashMap<String, Session>>,
    claims: Mutex<BTreeMap<(String, String), ClaimInfo>>,
    idempotency: Mutex<HashMap<(String, String), (Instant, String)>>,
    events: broadcast::Sender<Event>,
    save_needed: Notify,
    pub opts: HostOptions,
    pub http: reqwest::Client,
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
        std::fs::create_dir_all(dir)?;
        let dir = dir.canonicalize()?;
        let mut project = if dir.join("project.json").exists() {
            Project::load(&dir).context("loading project.json")?
        } else {
            let name = dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "project".into());
            let p = Project::new(&slug(&name), &name);
            p.save(&dir)?;
            p
        };
        if project.canvases.is_empty() {
            project.save_canvas(&dir, &Canvas::new("main", "Main"))?;
        }
        let (events, _) = broadcast::channel(1024);
        Ok(Arc::new(Self {
            dir,
            project: Mutex::new(project),
            canvases: Mutex::default(),
            sessions: Mutex::default(),
            claims: Mutex::default(),
            idempotency: Mutex::default(),
            events,
            save_needed: Notify::new(),
            opts,
            http: reqwest::Client::new(),
        }))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn project(&self) -> Project {
        self.project.lock().unwrap().clone()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    pub fn emit(&self, e: Event) {
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
        let canvas = self.project.lock().unwrap().open_canvas(&self.dir, &id)?;
        let o = Arc::new(Mutex::new(Open {
            canvas,
            dirty: false,
        }));
        map.insert(id, o.clone());
        Ok(o)
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
        let o = self.open_canvas(&id)?;
        let (out, event, assigned) = {
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
            let assigned = new_assignments(&before_anns, &o.canvas.annotations());
            let event = Event::Doc {
                canvas: id.clone(),
                actor: actor.to_string(),
                version: o.canvas.version().to_string(),
                created,
                touched,
                bytes: Arc::new(bytes),
                origin,
            };
            (out, event, assigned)
        };
        self.emit(event);
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

    /// Write every dirty canvas to disk (atomic, via `Project::save_canvas`).
    pub fn flush(&self) -> Result<()> {
        let open: Vec<_> = self.canvases.lock().unwrap().values().cloned().collect();
        for o in open {
            let mut o = o.lock().unwrap();
            if o.dirty {
                self.project
                    .lock()
                    .unwrap()
                    .save_canvas(&self.dir, &o.canvas)?;
                o.dirty = false;
            }
        }
        Ok(())
    }

    /// Debounced autosave loop; run it on the runtime for the host's lifetime.
    pub async fn autosave(self: Arc<Self>) {
        loop {
            self.save_needed.notified().await;
            tokio::time::sleep(AUTOSAVE_DEBOUNCE).await;
            if let Err(e) = self.flush() {
                tracing::error!("autosave failed: {e:#}");
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
        if self.sessions.lock().unwrap().remove(id).is_some() {
            self.claims.lock().unwrap().retain(|_, c| c.session != id);
            self.emit(Event::SessionClosed { session: id.into() });
        }
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
