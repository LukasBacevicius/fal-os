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
}

pub async fn handler(
    ws: WebSocketUpgrade,
    Path(canvas): Path<String>,
    Query(q): Query<SyncQuery>,
    State(host): State<Arc<Host>>,
) -> Response {
    let canvas = match host.resolve_canvas(&canvas) {
        Ok(c) => c,
        Err(e) => return (StatusCode::NOT_FOUND, e.to_string()).into_response(),
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
    });
    if !send_json(&mut socket, hello).await
        || !send_snapshot(&host, &canvas, &mut socket).await
        || !send_json(&mut socket, crate::chat::history(&host, &canvas)).await
    {
        host.close_session(&info.id);
        return;
    }
    loop {
        tokio::select! {
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
                Some(Ok(Message::Text(t))) if let Some(m) = crate::chat::chat_frame(&t) => {
                    // chat.send / chat.stop: see chat.rs.
                    if let Err(e) = crate::chat::handle_client(&host, &canvas, &name, &m).await {
                        let _ = send_json(&mut socket, json!({"type": "error", "message": format!("{e:#}")})).await;
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
