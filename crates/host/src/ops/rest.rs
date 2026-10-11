//! Host REST mirror of the op tools for the UI (`docs/ASSETS-V1.md` §7): token auth (server
//! guard), `?project=<id>&canvas=<id>`, actor = the human (`?actor=<id>&name=…`, default the
//! project's connected human). Long ops return placeholder node ids at once; progress
//! arrives as `job` events on `/sync`.

#![allow(clippy::result_large_err)]

use std::sync::Arc;

use axum::Json as AxJson;
use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine as _;
use falos_doc::{Actor, ActorKind};
use serde::Deserialize;
use serde_json::{Value as Json, json};

use super::export::{ExportOpts, Target};
use crate::host::Host;
use crate::mcp::PlacementArgs;
use crate::workspace::Workspace;

#[derive(Deserialize, Default)]
pub struct ApiQuery {
    project: Option<String>,
    canvas: Option<String>,
    actor: Option<String>,
    name: Option<String>,
}

pub fn routes() -> Router<Arc<Workspace>> {
    Router::new()
        .route("/api/ops", get(ops))
        .route("/api/presets", get(presets))
        .route("/api/inspect", post(inspect))
        .route("/api/transform", post(transform))
        .route("/api/compose", post(compose))
        .route("/api/rerun", post(rerun))
        .route("/api/export", post(export))
        .route("/api/package", post(package))
}

fn err(status: StatusCode, e: impl std::fmt::Display) -> Response {
    (status, AxJson(json!({"error": e.to_string()}))).into_response()
}

fn ok(v: Json) -> Response {
    AxJson(v).into_response()
}

struct Ctx {
    host: Arc<Host>,
    canvas: String,
    actor: Actor,
}

fn ctx(space: &Workspace, q: &ApiQuery, body: &Json) -> Result<Ctx, Response> {
    let host = space.host_for_frame(q.project.as_deref()).map_err(|e| err(StatusCode::NOT_FOUND, format!("{e:#}")))?;
    let key = q
        .canvas
        .clone()
        .or_else(|| body["canvas"].as_str().map(String::from))
        .or_else(|| host.project().canvases.first().map(|c| c.id.clone()))
        .unwrap_or_else(|| "main".into());
    let canvas = host.resolve_canvas(&key).map_err(|e| err(StatusCode::NOT_FOUND, format!("{e:#}")))?;
    let actor = match &q.actor {
        Some(a) if !a.is_empty() && a.chars().all(|c| c.is_ascii_alphanumeric() || "_-.".contains(c)) => {
            Actor::human(a, q.name.as_deref().unwrap_or(a))
        }
        Some(a) => return Err(err(StatusCode::BAD_REQUEST, format!("bad actor `{a}`"))),
        // The connected human (sync client identity), else `human`.
        None => host
            .sessions()
            .into_iter()
            .find(|s| s.kind == ActorKind::Human)
            .map(|s| Actor::human(&s.actor, &s.agent_name))
            .unwrap_or_else(|| Actor::human("human", "Human")),
    };
    Ok(Ctx { host, canvas, actor })
}

async fn ops() -> Response {
    ok(json!({"ops": super::specs()}))
}

async fn presets() -> Response {
    ok(super::export::presets())
}

async fn inspect(State(space): State<Arc<Workspace>>, Query(q): Query<ApiQuery>, AxJson(b): AxJson<Json>) -> Response {
    let c = match ctx(&space, &q, &b) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let what: Vec<String> = b["what"].as_array().map(|a| a.iter().filter_map(|w| w.as_str().map(String::from)).collect()).unwrap_or_default();
    match super::inspect::inspect(&c.host, &c.canvas, &b["inputs"], &what).await {
        Ok((v, visuals)) => {
            let images: Vec<Json> = visuals
                .iter()
                .map(|v| json!({"caption": v.caption, "data": format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(&v.jpeg))}))
                .collect();
            ok(json!({"results": v, "images": images}))
        }
        Err(e) => err(StatusCode::BAD_REQUEST, format!("{e:#}")),
    }
}

fn placement(b: &Json) -> Option<PlacementArgs> {
    b.get("placement").and_then(|p| serde_json::from_value(p.clone()).ok())
}

fn start_op(c: &Ctx, b: &Json, category: &str) -> Result<Json, Response> {
    let op = b["op"].as_str().ok_or_else(|| err(StatusCode::BAD_REQUEST, "op required"))?;
    let spec = super::spec(op).ok_or_else(|| err(StatusCode::BAD_REQUEST, format!("unknown op `{op}`")))?;
    if spec.category != category {
        return Err(err(StatusCode::BAD_REQUEST, format!("`{op}` is a {} op", spec.category)));
    }
    let inputs = if b["inputs"].is_null() { b["frame"].clone() } else { b["inputs"].clone() };
    let run = super::start(&c.host, &c.actor, None, &c.canvas, super::Request {
        op: op.into(),
        inputs,
        args: b["args"].clone(),
        placement: placement(b),
        name: b["name"].as_str().map(String::from),
        role: None,
    })
    .map_err(|e| err(StatusCode::BAD_REQUEST, format!("{e:#}")))?;
    Ok(run.summary(None))
}

async fn transform(State(space): State<Arc<Workspace>>, Query(q): Query<ApiQuery>, AxJson(b): AxJson<Json>) -> Response {
    match ctx(&space, &q, &b).and_then(|c| start_op(&c, &b, "transform")) {
        Ok(v) => ok(v),
        Err(r) => r,
    }
}

async fn compose(State(space): State<Arc<Workspace>>, Query(q): Query<ApiQuery>, AxJson(b): AxJson<Json>) -> Response {
    match ctx(&space, &q, &b).and_then(|c| start_op(&c, &b, "compose")) {
        Ok(v) => ok(v),
        Err(r) => r,
    }
}

async fn rerun(State(space): State<Arc<Workspace>>, Query(q): Query<ApiQuery>, AxJson(b): AxJson<Json>) -> Response {
    let c = match ctx(&space, &q, &b) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let Some(node) = b["node"].as_str() else {
        return err(StatusCode::BAD_REQUEST, "node required");
    };
    let on = b["on"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect());
    let overrides = b.get("overrides").cloned().filter(|o| o.is_object());
    match super::rerun(&c.host, &c.actor, None, &c.canvas, node, overrides, on).await {
        Ok(v) => ok(v),
        Err(e) => err(StatusCode::BAD_REQUEST, format!("{e:#}")),
    }
}

/// Export target from `{nodes}`, `{frame}` or `{target: "deliverables"}` (or `nodes: "deliverables"`).
pub fn target_of(b: &Json) -> anyhow::Result<Target> {
    if b["target"] == "deliverables" || b["deliverables"] == true {
        return Ok(Target::Deliverables);
    }
    if let Some(f) = b["frame"].as_str() {
        return Ok(Target::Frame(f.into()));
    }
    Target::from_json(&b["nodes"])
}

async fn export(State(space): State<Arc<Workspace>>, Query(q): Query<ApiQuery>, AxJson(b): AxJson<Json>) -> Response {
    let c = match ctx(&space, &q, &b) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let target = match target_of(&b) {
        Ok(t) => t,
        Err(e) => return err(StatusCode::BAD_REQUEST, e),
    };
    let opts = ExportOpts {
        presets: b["presets"].as_array().cloned().unwrap_or_default(),
        dir: b["dir"].as_str().map(String::from),
        naming: b["naming"].as_str().map(String::from),
        reframe: b["reframe"].as_str().map(String::from),
    };
    match super::export::export(&c.host, &c.actor, None, &c.canvas, target, opts).await {
        Ok(v) => ok(v),
        Err(e) => err(StatusCode::BAD_REQUEST, format!("{e:#}")),
    }
}

async fn package(State(space): State<Arc<Workspace>>, Query(q): Query<ApiQuery>, AxJson(b): AxJson<Json>) -> Response {
    let c = match ctx(&space, &q, &b) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let kind = b["kind"].as_str().unwrap_or("project");
    match super::export::package(&c.host, &c.actor, None, &c.canvas, kind).await {
        Ok(v) => ok(v),
        Err(e) => err(StatusCode::BAD_REQUEST, format!("{e:#}")),
    }
}

