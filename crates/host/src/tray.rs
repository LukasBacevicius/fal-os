//! `GET /api/status`: what the desktop shell's menu bar item shows (apps/desktop `tray.rs`):
//! running chat runs across the loaded projects (with their current step and an unanswered
//! question, if any), in-flight generations, and runs that ended since the host started.
//! The shell polls it about once a second, so it only reads in-memory state.

use std::sync::Arc;

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::workspace::Workspace;

/// Ended runs kept in the answer, newest first.
const RECENT: usize = 5;

pub fn routes() -> Router<Arc<Workspace>> {
    Router::new().route("/api/status", get(status))
}

async fn status(State(space): State<Arc<Workspace>>) -> Json<Value> {
    Json(tokio::task::spawn_blocking(move || summary(&space)).await.unwrap_or_else(|_| json!({})))
}

pub fn summary(space: &Workspace) -> Value {
    let (mut runs, mut recent) = (Vec::new(), Vec::new());
    let (mut generating, mut queued) = (0, 0);
    let mut hosts = space.loaded();
    hosts.sort_by(|a, b| a.id.cmp(&b.id));
    for h in hosts {
        let name = h.project().name;
        let (r, done) = h.chat.tray(&h);
        let tag = |mut v: Value| {
            v["project"] = json!(h.id);
            v["project_name"] = json!(name);
            v
        };
        runs.extend(r.into_iter().map(tag));
        recent.extend(done.into_iter().map(tag));
        let (g, q) = h.batches.in_flight();
        generating += g;
        queued += q;
        for s in h.jobs.lock().unwrap().values() {
            if s == "running" {
                generating += 1
            } else {
                queued += 1
            }
        }
    }
    recent.sort_by_key(|v| std::cmp::Reverse(v["ended"].as_i64().unwrap_or(0)));
    recent.truncate(RECENT);
    json!({
        "runs": runs,
        "generating": generating,
        "queued": queued,
        "recent": recent,
        "runtime": space.link.connected(),
    })
}
