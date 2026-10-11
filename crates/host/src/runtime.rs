//! The agent runtime link (see `docs/CHAT-PROTOCOL.md`).
//!
//! - `GET /runtime` → WebSocket for the agent runtime (Node, Claude Agent SDK), one for the
//!   whole workspace; frames carry `project` (the project id) both ways. Besides the
//!   per-install bearer token (checked by the server guard) it needs the runtime token in
//!   `X-Falos-Runtime`, which only the supervised runtime gets (env `FALOS_RUNTIME_TOKEN`).
//! - [`supervise`] spawns the runtime process and restarts it when it exits.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde_json::{Value as Json, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::workspace::Workspace;

pub const RUNTIME_HEADER: &str = "x-falos-runtime";

/// The one runtime connection, shared by every project of a workspace.
pub struct RuntimeLink {
    /// Second secret (besides the per-install token) that only the supervised runtime
    /// knows: guards `/runtime`.
    pub token: String,
    /// Outgoing frames to the connected runtime (one at a time; a new one replaces it).
    tx: Mutex<Option<(u64, mpsc::UnboundedSender<String>)>>,
    /// Frames sent while no runtime was connected; flushed on connect.
    pending: Mutex<Vec<String>>,
    /// The host spawned a runtime (so a missing connection is transient).
    pub supervised: AtomicBool,
}

impl Default for RuntimeLink {
    fn default() -> Self {
        let token = std::env::var("FALOS_RUNTIME_TOKEN")
            .ok()
            .filter(|t| t.len() >= 16)
            .unwrap_or_else(|| (0..24).map(|_| format!("{:02x}", rand::random::<u8>())).collect());
        Self {
            token,
            tx: Mutex::default(),
            pending: Mutex::default(),
            supervised: AtomicBool::new(false),
        }
    }
}

impl RuntimeLink {
    /// Send a frame to the runtime, or queue it until one connects.
    pub fn send(&self, frame: String) -> bool {
        if let Some((_, tx)) = &*self.tx.lock().unwrap()
            && tx.send(frame.clone()).is_ok()
        {
            return true;
        }
        self.pending.lock().unwrap().push(frame);
        false
    }

    /// Register a runtime connection; returns its id and receiver (queued frames first).
    pub fn connect(&self) -> (u64, mpsc::UnboundedReceiver<String>) {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::unbounded_channel();
        for f in self.pending.lock().unwrap().drain(..) {
            let _ = tx.send(f);
        }
        *self.tx.lock().unwrap() = Some((id, tx));
        (id, rx)
    }

    /// Connection `id` went away; true when it was the current one.
    pub fn disconnect(&self, id: u64) -> bool {
        let mut r = self.tx.lock().unwrap();
        if r.as_ref().is_some_and(|(i, _)| *i == id) {
            *r = None;
            true
        } else {
            false
        }
    }

    pub fn connected(&self) -> bool {
        self.tx.lock().unwrap().is_some()
    }

    fn authorized(&self, headers: &HeaderMap) -> bool {
        let want = self.token.as_bytes();
        headers
            .get(RUNTIME_HEADER)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|t| {
                let t = t.trim().as_bytes();
                t.len() == want.len() && t.iter().zip(want).fold(0u8, |a, (x, y)| a | (x ^ y)) == 0
            })
    }
}

/// Routes to merge into the server router (behind its token/Host/Origin guard).
pub fn routes() -> Router<Arc<Workspace>> {
    Router::new().route("/runtime", get(ws_handler))
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(space): State<Arc<Workspace>>,
    headers: HeaderMap,
) -> Response {
    if !space.link.authorized(&headers) {
        return (StatusCode::UNAUTHORIZED, "runtime token required").into_response();
    }
    ws.on_upgrade(move |socket| run(space, socket))
}

async fn run(space: Arc<Workspace>, mut socket: WebSocket) {
    let (id, mut rx) = space.link.connect();
    tracing::info!("agent runtime connected");
    let hello = json!({
        "type": "hello",
        "workspace": space.root().map(|p| p.display().to_string()),
        "projects": space.ids(),
    });
    if socket
        .send(Message::Text(hello.to_string().into()))
        .await
        .is_err()
    {
        gone(&space, id);
        return;
    }
    loop {
        tokio::select! {
            out = rx.recv() => match out {
                Some(frame) => {
                    if socket.send(Message::Text(frame.into())).await.is_err() {
                        break;
                    }
                }
                // Replaced by a newer runtime connection.
                None => break,
            },
            msg = socket.recv() => match msg {
                Some(Ok(Message::Text(t))) => handle(&space, &t),
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
        }
    }
    tracing::warn!("agent runtime disconnected");
    gone(&space, id);
}

/// The runtime went away: running chats of every loaded project end with an error.
fn gone(space: &Workspace, id: u64) {
    if space.link.disconnect(id) {
        for host in space.loaded() {
            host.chat.runtime_gone(&host);
        }
    }
}

fn handle(space: &Arc<Workspace>, text: &str) {
    let Ok(msg) = serde_json::from_str::<Json>(text) else {
        tracing::warn!("runtime sent bad JSON");
        return;
    };
    // Frames name their project; older runtimes don't (→ the default project).
    let host = match space.host_for_frame(msg["project"].as_str()) {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!("runtime frame for an unknown project: {e:#}");
            if msg["type"] == "request" {
                space.link.send(
                    json!({"type": "reply", "id": msg["id"], "ok": false, "error": format!("{e:#}")})
                        .to_string(),
                );
            }
            return;
        }
    };
    let host = &host;
    match msg["type"].as_str() {
        Some("event") => {
            let (Some(chat), Some(event)) = (msg["chat"].as_str(), msg.get("event")) else {
                return;
            };
            if !event.is_object() || event["kind"].as_str().is_none() {
                tracing::warn!("runtime event without kind");
                return;
            }
            let mut event = event.clone();
            // Every runtime event is attributed (the director unless it says otherwise).
            if event.get("agent").is_none_or(|a| !a.is_string()) {
                event["agent"] = Json::from("director");
            }
            let ends = event["kind"] == "status" && event["state"] != "running";
            if let Err(e) = host.chat.append(host, chat, msg["canvas"].as_str(), event) {
                tracing::warn!("chat event dropped: {e:#}");
            }
            if ends {
                let (host, chat) = (host.clone(), chat.to_string());
                tokio::spawn(async move {
                    if let Err(e) = crate::chat::ChatHub::flush_queue(&host, &chat).await {
                        tracing::warn!("queued messages of {chat}: {e:#}");
                    }
                });
            }
        }
        // The runtime mapped an agent's fal-os session to its subagent label.
        Some("session.agent") => {
            if let (Some(s), Some(a)) = (msg["session"].as_str(), msg["agent"].as_str()) {
                host.set_session_agent(s, a, msg["parent"].as_str());
            }
        }
        // A queued message went into the running turn.
        Some("injected") => {
            if let (Some(chat), Some(id)) = (msg["chat"].as_str(), msg["id"].as_str()) {
                host.chat.injected(host, chat, id, msg["text"].as_str());
            }
        }
        // A steer arrived when nothing was running: deliver it as a normal message.
        Some("steer.failed") => {
            if let Some(chat) = msg["chat"].as_str() {
                let (host, chat, msg) = (host.clone(), chat.to_string(), msg.clone());
                tokio::spawn(async move {
                    if let Err(e) = crate::chat::steer_failed(&host, &chat, &msg).await {
                        tracing::warn!("steer of {chat}: {e:#}");
                    }
                });
            }
        }
        // The runtime created (or found) the fal Agent project linked to this project.
        Some("project.fal") => {
            if let Some(id) = msg["fal_project"].as_str() {
                if let Err(e) = host.set_fal_project(Some(id)) {
                    tracing::warn!("saving fal_project: {e:#}");
                }
                space.changed();
            }
        }
        // Cross-chat requests (chat.list / chat.read / chat.send): answered with a `reply`.
        Some("request") => {
            let host = host.clone();
            let space = space.clone();
            tokio::spawn(async move {
                let reply = crate::chat::handle_runtime_request(&host, &msg).await;
                space.link.send(reply.to_string());
            });
        }
        Some("log") => tracing::info!("runtime: {}", msg["message"].as_str().unwrap_or_default()),
        _ => {}
    }
}

// ---- supervisor ------------------------------------------------------------------------------

/// Find `apps/agent-runtime` next to the working directory or the executable.
pub fn detect() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("FALOS_RUNTIME") {
        return Some(p.into());
    }
    let mut starts = vec![];
    if let Ok(d) = std::env::current_dir() {
        starts.push(d);
    }
    if let Ok(e) = std::env::current_exe() {
        starts.push(e);
    }
    for s in starts {
        for dir in s.ancestors() {
            let p = dir.join("apps/agent-runtime");
            if p.join("package.json").exists() {
                return Some(p);
            }
        }
    }
    None
}

/// The command that starts the runtime in `dir`: the bundled `dist/main.mjs` with `$FALOS_NODE`
/// (the packaged app, see scripts/package-macos.sh), else its local `tsx`, else `pnpm start`.
fn command(dir: &Path) -> tokio::process::Command {
    let tsx = dir.join("node_modules/.bin/tsx");
    let bundled = dir.join("dist/main.mjs");
    let mut c = if let (Some(node), true) = (std::env::var_os("FALOS_NODE"), bundled.exists()) {
        let mut c = tokio::process::Command::new(node);
        c.arg(bundled);
        c
    } else if tsx.exists() {
        let mut c = tokio::process::Command::new(tsx);
        c.arg(dir.join("src/main.ts"));
        c
    } else {
        let mut c = tokio::process::Command::new("pnpm");
        c.args(["--dir"]).arg(dir).arg("start");
        c
    };
    c.current_dir(dir);
    c
}

/// SIGTERM the runtime's whole process group.
fn kill_group(pid: u32) {
    let _ = std::process::Command::new("kill")
        .args(["-TERM", &format!("-{pid}")])
        .status();
}

/// Spawn the runtime at `dir` and keep it running (restart with backoff) until `cancel`.
pub fn supervise(
    space: Arc<Workspace>,
    dir: PathBuf,
    port: u16,
    token: String,
    cancel: CancellationToken,
) {
    space.link.supervised.store(true, Ordering::Relaxed);
    tokio::spawn(async move {
        let mut backoff = Duration::from_secs(1);
        loop {
            let started = tokio::time::Instant::now();
            let mut cmd = command(&dir);
            cmd.env("FALOS_HOST_URL", format!("http://127.0.0.1:{port}"))
                .env("FALOS_TOKEN", &token)
                .env("FALOS_RUNTIME_TOKEN", &space.link.token)
                .env(
                    "FALOS_WORKSPACE",
                    space.root().map(|p| p.display().to_string()).unwrap_or_default(),
                )
                .env("FALOS_HOST_PID", std::process::id().to_string())
                // Own process group, so stopping it also stops tsx's node child.
                .process_group(0)
                .kill_on_drop(true);
            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!(
                        "could not start the agent runtime in {}: {e}",
                        dir.display()
                    );
                    return;
                }
            };
            tracing::info!(
                "agent runtime started from {} (pid {:?})",
                dir.display(),
                child.id()
            );
            tokio::select! {
                s = child.wait() => tracing::warn!("agent runtime exited: {s:?}"),
                _ = cancel.cancelled() => {
                    if let Some(pid) = child.id() {
                        kill_group(pid);
                    }
                    let _ = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
                    let _ = child.start_kill();
                    return;
                }
            }
            if started.elapsed() > Duration::from_secs(60) {
                backoff = Duration::from_secs(1);
            }
            tokio::select! {
                _ = tokio::time::sleep(backoff) => {}
                _ = cancel.cancelled() => return,
            }
            backoff = (backoff * 2).min(Duration::from_secs(30));
        }
    });
}
