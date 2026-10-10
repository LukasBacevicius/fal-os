//! The agent runtime link (see `docs/CHAT-PROTOCOL.md`).
//!
//! - `GET /runtime` → WebSocket for the agent runtime (Node, Claude Agent SDK). Besides the
//!   per-install bearer token (checked by the server guard) it needs the runtime token in
//!   `X-Falos-Runtime`, which only the supervised runtime gets (env `FALOS_RUNTIME_TOKEN`).
//! - [`supervise`] spawns the runtime process and restarts it when it exits.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::Router;
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde_json::{Value as Json, json};
use tokio_util::sync::CancellationToken;

use crate::host::Host;

pub const RUNTIME_HEADER: &str = "x-falos-runtime";

fn authorized(host: &Host, headers: &HeaderMap) -> bool {
    let want = host.chat.runtime_token.as_bytes();
    headers
        .get(RUNTIME_HEADER)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|t| {
            let t = t.trim().as_bytes();
            t.len() == want.len() && t.iter().zip(want).fold(0u8, |a, (x, y)| a | (x ^ y)) == 0
        })
}

/// Routes to merge into the server router (behind its token/Host/Origin guard).
pub fn routes() -> Router<Arc<Host>> {
    Router::new().route("/runtime", get(ws_handler))
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(host): State<Arc<Host>>,
    headers: HeaderMap,
) -> Response {
    if !authorized(&host, &headers) {
        return (StatusCode::UNAUTHORIZED, "runtime token required").into_response();
    }
    ws.on_upgrade(move |socket| run(host, socket))
}

async fn run(host: Arc<Host>, mut socket: WebSocket) {
    host.chat.start_list_task(&host);
    let (id, mut rx) = host.chat.connect_runtime();
    tracing::info!("agent runtime connected");
    let hello = json!({"type": "hello", "project": host.dir().display().to_string()});
    if socket
        .send(Message::Text(hello.to_string().into()))
        .await
        .is_err()
    {
        host.chat.disconnect_runtime(&host, id);
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
                Some(Ok(Message::Text(t))) => handle(&host, &t),
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
        }
    }
    tracing::warn!("agent runtime disconnected");
    host.chat.disconnect_runtime(&host, id);
}

fn handle(host: &Arc<Host>, text: &str) {
    let Ok(msg) = serde_json::from_str::<Json>(text) else {
        tracing::warn!("runtime sent bad JSON");
        return;
    };
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
        // Cross-chat requests (chat.list / chat.read / chat.send): answered with a `reply`.
        Some("request") => {
            let host = host.clone();
            tokio::spawn(async move {
                let reply = crate::chat::handle_runtime_request(&host, &msg).await;
                host.chat.to_runtime(reply.to_string());
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

/// The command that starts the runtime in `dir`: its local `tsx`, else `pnpm start`.
fn command(dir: &Path) -> tokio::process::Command {
    let tsx = dir.join("node_modules/.bin/tsx");
    let mut c = if tsx.exists() {
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
    host: Arc<Host>,
    dir: PathBuf,
    port: u16,
    token: String,
    cancel: CancellationToken,
) {
    host.chat.supervised.store(true, Ordering::Relaxed);
    tokio::spawn(async move {
        let mut backoff = Duration::from_secs(1);
        loop {
            let started = tokio::time::Instant::now();
            let mut cmd = command(&dir);
            cmd.env("FALOS_HOST_URL", format!("http://127.0.0.1:{port}"))
                .env("FALOS_TOKEN", &token)
                .env("FALOS_RUNTIME_TOKEN", &host.chat.runtime_token)
                .env("FALOS_PROJECT_DIR", host.dir())
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
