//! Library messages on the `/workspace` socket (docs/ASSETS-V1.md §7).
//!
//! client → server: `library.query {id, query}`, `library.tag {shas, add?, remove?}`,
//! `library.favorite {shas, value}`, `library.collect {shas, collection}`,
//! `library.place {sha, project, x?, y?, canvas?}`, `library.upload {shas, project?}`,
//! `library.local_only {shas, value}`, `library.get {sha}`.
//! server → client: `library.results {id, items, next, total, partial}` (a query with text
//! first answers from the local index with `partial: true`, then again with fal semantic
//! matches merged), `library.changed {shas}`, `library.show {shas}`, `library.status`,
//! `library.placed`, `library.uploading`, `library.asset`, `library.ok`.

use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::{Value as Json, json};
use tokio::sync::mpsc::UnboundedSender;

use super::Library;
use super::query::{self, Query};

fn strs(v: &Json) -> Vec<String> {
    match v {
        Json::Array(a) => a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect(),
        Json::String(s) => vec![s.clone()],
        _ => vec![],
    }
}

/// Local results for a query (sync; SQLite).
pub fn results(lib: &Library, id: &Json, q: &Query, partial: bool) -> Result<Json> {
    let project = query::scope(q).map(str::to_string);
    let (page, items) = lib.with_db(|c| {
        let page = query::run(c, q)?;
        let items = query::items(c, &page.keys, project.as_deref())?;
        Ok((page, items))
    })?;
    Ok(json!({"type": "library.results", "id": id, "items": items, "next": page.next, "total": page.total, "partial": partial}))
}

/// Whether a query also goes to fal semantic search, and with which params.
fn semantic_params(lib: &Library, q: &Query) -> Option<Vec<(&'static str, String)>> {
    lib.fal.as_ref()?;
    if q.semantic == Some(false) || q.cursor.as_deref().is_some_and(|c| c != "0") || q.color.is_some() {
        return None;
    }
    let mut p = vec![];
    if let Some(sim) = &q.similar_to {
        let (kind, url): (String, Option<String>) = lib
            .with_db(|c| {
                let k = query::resolve_key(c, sim).or_else(|| query::key_of_node(c, sim, query::scope(q))).context("no asset")?;
                Ok(c.query_row("SELECT kind, fal_url FROM assets WHERE sha=?", [&k], |r| Ok((r.get(0)?, r.get(1)?)))?)
            })
            .ok()?;
        let url = url?;
        p.push((if kind == "video" { "search_video_url" } else { "search_image_url" }, url));
    } else {
        let t = q.text.as_deref().map(str::trim).filter(|t| t.len() >= 3)?;
        p.push(("q", t.to_string()));
    }
    if let Some(k) = q.kind.as_ref().and_then(|k| (k.len() == 1).then(|| k[0].clone())) {
        p.push(("media_type", if k == "model3d" { "3d".into() } else { k }));
    }
    Some(p)
}

/// Handle one `library.*` frame; replies go to `tx` (this client only).
pub async fn handle(lib: Arc<Library>, msg: Json, tx: UnboundedSender<String>) {
    let id = msg["id"].clone();
    let send = |v: Json| {
        let _ = tx.send(v.to_string());
    };
    let r: Result<()> = async {
        match msg["type"].as_str().unwrap_or("") {
            "library.query" => {
                let q: Query = serde_json::from_value(msg["query"].clone()).context("bad query")?;
                let sem = semantic_params(&lib, &q);
                let l = lib.clone();
                let (q2, id2) = (q.clone(), id.clone());
                let partial = sem.is_some();
                let first = tokio::task::spawn_blocking(move || results(&l, &id2, &q2, partial)).await??;
                send(first);
                if let Some(params) = sem {
                    match lib.fal_search(params).await {
                        Ok(extra) => {
                            let l = lib.clone();
                            let id2 = id.clone();
                            let merged = tokio::task::spawn_blocking(move || -> Result<Json> {
                                // Local matches first, then fal's (same filters, no text).
                                let local = l.with_db(|c| Ok(query::run(c, &q)?.keys))?;
                                let mut keys = local.clone();
                                if !extra.is_empty() {
                                    let fq = Query { text: None, similar_to: None, shas: Some(extra.clone()), cursor: None, limit: Some(10_000), sort: None, ..q.clone() };
                                    let allowed: std::collections::HashSet<String> = l.with_db(|c| Ok(query::run(c, &fq)?.keys))?.into_iter().collect();
                                    keys.extend(extra.into_iter().filter(|k| allowed.contains(k) && !local.contains(k)));
                                }
                                let total = keys.len();
                                let limit = q.limit.unwrap_or(200).clamp(1, 10_000);
                                keys.truncate(limit);
                                let items = l.with_db(|c| query::items(c, &keys, query::scope(&q)))?;
                                let next = (total > limit).then(|| limit.to_string());
                                Ok(json!({"type": "library.results", "id": id2, "items": items, "next": next, "total": total, "partial": false}))
                            })
                            .await??;
                            send(merged);
                        }
                        Err(e) => {
                            tracing::debug!("library: fal search: {e:#}");
                            let mut done = results(&lib, &id, &q, false)?;
                            done["semantic_error"] = json!(format!("{e:#}"));
                            send(done);
                        }
                    }
                }
            }
            "library.tag" => {
                let keys = lib.tag(&strs(&msg["shas"]), &strs(&msg["add"]), &strs(&msg["remove"]))?;
                send(json!({"type": "library.ok", "id": id, "op": "tag", "shas": keys}));
            }
            "library.favorite" => {
                let keys = lib.favorite(&strs(&msg["shas"]), msg["value"].as_bool().unwrap_or(true))?;
                send(json!({"type": "library.ok", "id": id, "op": "favorite", "shas": keys}));
            }
            "library.local_only" => {
                let keys = lib.local_only(&strs(&msg["shas"]), msg["value"].as_bool().unwrap_or(true))?;
                send(json!({"type": "library.ok", "id": id, "op": "local_only", "shas": keys}));
            }
            "library.collect" => {
                let c = lib.collect(&strs(&msg["shas"]), msg["collection"].as_str().unwrap_or(""))?;
                send(json!({"type": "library.ok", "id": id, "op": "collect", "collection": c}));
            }
            "library.upload" => {
                let (queued, skipped) = lib.upload(&strs(&msg["shas"]), msg["project"].as_str())?;
                send(json!({"type": "library.uploading", "id": id, "queued": queued, "skipped": skipped}));
            }
            "library.place" => {
                let sha = msg["sha"].as_str().context("library.place needs sha")?;
                let ws = lib.workspace().context("no workspace")?;
                let project = match msg["project"].as_str() {
                    Some(p) => p.to_string(),
                    None => ws.default_id()?,
                };
                let human = falos_doc::Actor::human("human", "you");
                let r = lib.place(sha, &project, msg["canvas"].as_str(), (msg["x"].as_f64(), msg["y"].as_f64()), msg["w"].as_f64(), &human).await?;
                send(json!({"type": "library.placed", "id": id, "node": r["node"], "project": r["project"], "canvas": r["canvas"], "sha": r["sha"]}));
            }
            "library.get" => {
                let d = lib.detail(msg["sha"].as_str().unwrap_or(""), msg["project"].as_str())?;
                send(json!({"type": "library.asset", "id": id, "asset": d}));
            }
            "library.status" => send(lib.status()),
            other => anyhow::bail!("unknown library message `{other}`"),
        }
        Ok(())
    }
    .await;
    if let Err(e) = r {
        send(json!({"type": "error", "id": id, "scope": "library", "message": format!("{e:#}")}));
    }
}
