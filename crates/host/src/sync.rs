//! Client sync endpoint: `GET /sync/{canvas}` upgraded to a WebSocket (see `PROTOCOL.md`).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Path, Query, State, WebSocketUpgrade};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use falos_doc::ActorKind;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::broadcast::error::RecvError;

use crate::host::{Event, Host};

static NEXT_CONN: AtomicU64 = AtomicU64::new(1);

/// Sync protocol version, sent in `hello`. A web UI built for another version shows
/// "ui out of date · [reload]" instead of failing in odd ways. Bump on breaking changes.
pub const PROTOCOL: u32 = 2;

/// The host's version, sent in `hello`.
pub const HOST_VERSION: &str = env!("CARGO_PKG_VERSION");

/// WebSocket keepalive: a ping this often; a client silent for [`IDLE_DROP`] is gone (a
/// half-open socket, a frozen tab) and its session closes.
const KEEPALIVE: std::time::Duration = std::time::Duration::from_secs(15);
const IDLE_DROP: std::time::Duration = std::time::Duration::from_secs(45);

#[derive(Deserialize)]
pub struct SyncQuery {
    /// Actor id of the human at this client (default `human`).
    actor: Option<String>,
    /// Display name.
    name: Option<String>,
    /// Canvas of the project (default: its first canvas).
    canvas: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMsg {
    Presence {
        cursor: Option<[f64; 2]>,
        viewport: Option<[f64; 4]>,
        status: Option<String>,
    },
    RevokeClaim {
        node: String,
    },
    /// The user's selection (for `get_selection` and placement next to it).
    Selection {
        nodes: Vec<String>,
    },
    /// The window showing this project got focus: it's the project "open in fal OS" that
    /// external agents bind to.
    Focus {},
    /// Heartbeat: answered with `pong` right away (the client reconnects when none comes).
    Ping {
        #[serde(default)]
        t: serde_json::Value,
    },
    /// The user's camera, remembered for a reloaded webview (`hello.view`).
    View { camera: serde_json::Value },
    /// A button on a held notice (`restore`, `fresh`, `save-anyway`, `dismiss`).
    #[serde(rename = "notice.action")]
    NoticeAction { key: String, action: String },
}

/// `{"type":"agent.register","client":"claude"|"codex"|"cursor"|"gemini"|"opencode"}`:
/// write the fal-os MCP server into that client's config. Reply: `agent.registered`.
pub fn agent_register(host: &Host, msg: &serde_json::Value) -> serde_json::Value {
    use clap::ValueEnum;
    let name = msg["client"].as_str().unwrap_or_default().to_string();
    let r = (|| -> anyhow::Result<(String, bool)> {
        let client = crate::register::Client::from_str(&name, true)
            .map_err(|_| anyhow::anyhow!("unknown client `{name}` (claude, codex, cursor, gemini, opencode)"))?;
        let home = host.opts.home.clone().ok_or_else(|| anyhow::anyhow!("HOME is not set"))?;
        let (url, token) = host
            .endpoint
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("the host is not serving yet"))?;
        let o = crate::register::register(client, &home, &url, &token, false)?;
        Ok((o.path.display().to_string(), o.changed()))
    })();
    match r {
        Ok((path, changed)) => {
            json!({"type": "agent.registered", "client": name, "ok": true, "path": path, "changed": changed})
        }
        Err(e) => json!({"type": "agent.registered", "client": name, "ok": false, "path": null, "error": format!("{e:#}")}),
    }
}

fn director_frame(t: &str) -> Option<serde_json::Value> {
    let m: serde_json::Value = serde_json::from_str(t).ok()?;
    matches!(m["type"].as_str(), Some("director.status" | "director.connect" | "director.disconnect")).then_some(m)
}

/// `director.status` / `director.connect` / `director.disconnect` (see `connect.rs`): use
/// fal-os from the user's own Claude Code, Claude desktop app and Codex.
pub fn director(host: &Host, msg: &serde_json::Value) -> serde_json::Value {
    let Some(home) = host.opts.home.clone() else {
        return json!({"type": "director.status", "targets": [], "error": "HOME is not set"});
    };
    let ctx = crate::connect::Ctx::for_host(home, host.endpoint.lock().unwrap().clone());
    crate::connect::handle(&ctx, msg)
}

/// `/sync/<project>` (workspace) or `/sync/<canvas>` (single-project mode and older clients:
/// a canvas of the default project).
pub async fn handler(
    ws: WebSocketUpgrade,
    Path(key): Path<String>,
    Query(q): Query<SyncQuery>,
    State(space): State<Arc<crate::workspace::Workspace>>,
) -> Response {
    let (host, canvas) = match space.resolve_sync(&key, q.canvas.as_deref()) {
        Ok(r) => r,
        Err(e) => return (StatusCode::NOT_FOUND, format!("{e:#}")).into_response(),
    };
    ws.on_upgrade(move |socket| run(host, canvas, q, socket))
}

async fn send_json(socket: &mut WebSocket, v: impl serde::Serialize) -> bool {
    match serde_json::to_string(&v) {
        Ok(s) => socket.send(Message::Text(s.into())).await.is_ok(),
        Err(_) => true,
    }
}

async fn send_snapshot(host: &Host, canvas: &str, socket: &mut WebSocket) -> bool {
    match host.snapshot(canvas) {
        Ok((_, bytes, _)) => socket.send(Message::Binary(bytes.into())).await.is_ok(),
        // A canvas that failed to load: an empty stand-in, so the UI comes up and shows the
        // notice (with [restore backup]); writes to it are refused.
        Err(_) if host.load_failed(canvas) => {
            let bytes = falos_doc::Canvas::new(canvas, canvas).export_snapshot().unwrap_or_default();
            socket.send(Message::Binary(bytes.into())).await.is_ok()
        }
        Err(_) => false,
    }
}

/// Every notice a client should see on connect: the project's held ones and app-wide ones.
fn all_notices(host: &Host) -> Vec<serde_json::Value> {
    let mut v = crate::diag::global_notices();
    v.extend(host.notices());
    v
}

async fn run(host: Arc<Host>, canvas: String, q: SyncQuery, mut socket: WebSocket) {
    let conn = NEXT_CONN.fetch_add(1, Ordering::Relaxed);
    let name = q.name.unwrap_or_else(|| "Human".into());
    let info = host.create_session(
        &name,
        ActorKind::Human,
        Some(q.actor.as_deref().unwrap_or("human")),
    );
    let actor = info.actor.clone();
    let _ = host.set_presence(&info.id, |p| p.canvas = Some(canvas.clone()));
    let seen = |h: &Host| h.human_seen.store(crate::host::now_ms(), std::sync::atomic::Ordering::Relaxed);
    seen(&host);
    // Subscribe before the snapshot so nothing falls between them (imports are idempotent).
    let mut rx = host.subscribe();
    let version = host
        .read(&canvas, |c| c.version().to_string())
        .unwrap_or_default();
    let hello = json!({
        "hostVersion": HOST_VERSION,
        "protocol": PROTOCOL,
        "notices": all_notices(&host),
        "view": host.view(&canvas, &actor),
        "type": "hello",
        "canvas": canvas,
        "version": version,
        "session": info,
        "sessions": host.sessions(),
        "claims": host.claims(&canvas),
        // App nodes load from here: `<apps>/<project>/<app>/` (see apps.rs).
        "apps": crate::apps::base_url(),
        "project": host.id,
    });
    if !send_json(&mut socket, hello).await
        || !send_snapshot(&host, &canvas, &mut socket).await
        || !send_json(&mut socket, crate::chat::history(&host, &canvas)).await
        || !send_json(&mut socket, crate::chat::list_frame(&host, &canvas)).await
    {
        host.close_session(&info.id);
        return;
    }
    let mut keepalive = tokio::time::interval(KEEPALIVE);
    keepalive.tick().await;
    let mut last_rx = std::time::Instant::now();
    loop {
        tokio::select! {
            _ = keepalive.tick() => {
                if last_rx.elapsed() > IDLE_DROP {
                    tracing::info!("sync client {} on {} went silent; closing", info.id, host.id);
                    break;
                }
                if socket.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
            // The project was deleted (moved to the trash).
            _ = host.closed.cancelled() => {
                let _ = send_json(&mut socket, json!({"type": "error", "message": "this project was deleted"})).await;
                let _ = socket.send(Message::Close(None)).await;
                break;
            }
            ev = rx.recv() => match ev {
                Ok(e) => {
                    if e.canvas().is_some_and(|c| c != canvas) {
                        continue;
                    }
                    // A restored canvas: tell the client to load it from scratch, then hang up.
                    if let Event::CanvasReset { .. } = &e {
                        let _ = send_json(&mut socket, &e).await;
                        let _ = socket.send(Message::Close(None)).await;
                        break;
                    }
                    if let Event::Doc { bytes, origin, .. } = &e
                        && *origin != Some(conn)
                        && socket.send(Message::Binary(bytes.to_vec().into())).await.is_err()
                    {
                        break;
                    }
                    if !send_json(&mut socket, &e).await {
                        break;
                    }
                }
                // Fell behind: a full snapshot catches the client up.
                Err(RecvError::Lagged(_)) => {
                    if !send_snapshot(&host, &canvas, &mut socket).await {
                        break;
                    }
                }
                Err(RecvError::Closed) => break,
            },
            msg = socket.recv() => { if msg.is_some() { last_rx = std::time::Instant::now(); } match msg {
                Some(Ok(Message::Binary(b))) => {
                    let blocked = host.write_block(&canvas);
                    if let Err(e) = host.mutate(&canvas, &actor, Some(conn), |c| c.import(&b)) {
                        // Blocked writes (canvas not loaded, read-only) are explained by a held
                        // notice already: no error line per edit.
                        let scope = if blocked.is_some() { "blocked" } else { "sync" };
                        let _ = send_json(&mut socket, json!({"type": "error", "scope": scope, "message": format!("{e:#}")})).await;
                    }
                }
                Some(Ok(Message::Text(t))) if t.contains("agent.register") && serde_json::from_str::<serde_json::Value>(&t).is_ok_and(|m| m["type"] == "agent.register") => {
                    let m: serde_json::Value = serde_json::from_str(&t).unwrap_or_default();
                    let reply = agent_register(&host, &m);
                    let _ = send_json(&mut socket, reply).await;
                }
                Some(Ok(Message::Text(t))) if t.contains("agent.continue") && serde_json::from_str::<serde_json::Value>(&t).is_ok_and(|m| m["type"] == "agent.continue") => {
                    let m: serde_json::Value = serde_json::from_str(&t).unwrap_or_default();
                    let reply = tokio::task::block_in_place(|| crate::director::continue_frame(&host, &m));
                    let _ = send_json(&mut socket, reply).await;
                }
                Some(Ok(Message::Text(t))) if t.contains("director.") && let Some(m) = director_frame(&t) => {
                    let reply = tokio::task::block_in_place(|| director(&host, &m));
                    let _ = send_json(&mut socket, reply).await;
                }
                Some(Ok(Message::Text(t))) if let Some(m) = crate::chat::chat_frame(&t) => {
                    // chat.send / chat.stop: see chat.rs.
                    match crate::chat::handle_client(&host, &canvas, &name, &m).await {
                        Ok(Some(reply)) => { let _ = send_json(&mut socket, reply).await; }
                        Ok(None) => {}
                        Err(e) => { let _ = send_json(&mut socket, json!({"type": "error", "message": format!("{e:#}")})).await; }
                    }
                }
                Some(Ok(Message::Text(t))) => match serde_json::from_str::<ClientMsg>(&t) {
                    Ok(ClientMsg::Focus {}) => seen(&host),
                    Ok(ClientMsg::Ping { t }) => {
                        if !send_json(&mut socket, json!({"type": "pong", "t": t})).await {
                            break;
                        }
                    }
                    Ok(ClientMsg::View { camera }) => host.remember_view(&canvas, &actor, Some(camera), None),
                    Ok(ClientMsg::NoticeAction { key, action }) => {
                        let r = if action == "dismiss" && crate::diag::dismiss(&key) {
                            let _ = send_json(&mut socket, json!({"type": "notice", "notice": {"key": key, "gone": true}})).await;
                            Ok(())
                        } else {
                            let h = host.clone();
                            let (k, a) = (key.clone(), action.clone());
                            tokio::task::spawn_blocking(move || h.notice_action(&k, &a)).await.unwrap_or_else(|e| Err(anyhow::anyhow!("{e}")))
                        };
                        if let Err(e) = r {
                            let _ = send_json(&mut socket, json!({"type": "error", "scope": "notice", "key": key, "message": format!("{e:#}")})).await;
                        }
                    }
                    Ok(ClientMsg::Presence { cursor, viewport, status }) => {
                        if cursor.is_some() {
                            seen(&host);
                        }
                        let _ = host.set_presence(&info.id, |p| {
                            p.cursor = cursor.or(p.cursor);
                            p.viewport = viewport.or(p.viewport);
                            if let Some(s) = status {
                                p.status = s;
                            }
                        });
                    }
                    Ok(ClientMsg::RevokeClaim { node }) => {
                        let _ = host.revoke_claim(&canvas, &node, &actor);
                    }
                    Ok(ClientMsg::Selection { nodes }) => {
                        host.remember_view(&canvas, &actor, None, Some(nodes.clone()));
                        host.set_selection(&info.id, nodes)
                    }
                    Err(e) => {
                        let _ = send_json(&mut socket, json!({"type": "error", "message": e.to_string()})).await;
                    }
                },
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            }},
        }
    }
    host.close_session(&info.id);
}
