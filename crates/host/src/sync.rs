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
        Err(_) => false,
    }
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
    // Subscribe before the snapshot so nothing falls between them (imports are idempotent).
    let mut rx = host.subscribe();
    let version = host
        .read(&canvas, |c| c.version().to_string())
        .unwrap_or_default();
    let hello = json!({
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
    loop {
        tokio::select! {
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
            msg = socket.recv() => match msg {
                Some(Ok(Message::Binary(b))) => {
                    if let Err(e) = host.mutate(&canvas, &actor, Some(conn), |c| c.import(&b)) {
                        let _ = send_json(&mut socket, json!({"type": "error", "message": format!("{e:#}")})).await;
                    }
                }
                Some(Ok(Message::Text(t))) if t.contains("agent.register") && serde_json::from_str::<serde_json::Value>(&t).is_ok_and(|m| m["type"] == "agent.register") => {
                    let m: serde_json::Value = serde_json::from_str(&t).unwrap_or_default();
                    let reply = agent_register(&host, &m);
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
                    Ok(ClientMsg::Presence { cursor, viewport, status }) => {
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
                    Ok(ClientMsg::Selection { nodes }) => host.set_selection(&info.id, nodes),
                    Err(e) => {
                        let _ = send_json(&mut socket, json!({"type": "error", "message": e.to_string()})).await;
                    }
                },
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
        }
    }
    host.close_session(&info.id);
}
