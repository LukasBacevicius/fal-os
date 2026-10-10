//! Async fal queue jobs behind `generation` nodes.
//!
//! `start` creates the placeholder node as the requesting actor and returns its id at once;
//! a background task submits to `https://queue.fal.run/{model}`, polls the status, and on
//! completion downloads the output into `<project>/assets/`, records an asset with lineage
//! and converts the node in place (same id) into the media node.
//!
//! Future: expose the job as an MCP Task (tasks extension) so clients can await it.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use falos_doc::{Actor, Asset, Command, Lineage, NodeKind, Props};
use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};

use crate::host::{Event, Host};

const POLL: Duration = Duration::from_millis(1000);
const TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// The system actor that writes job progress (kept off the requester's undo stack).
fn jobs_actor() -> Actor {
    Actor::agent("fal", "fal queue")
}

pub struct Placement {
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub w: Option<f64>,
    pub h: Option<f64>,
    pub parent: Option<String>,
}

/// Create the generation node and spawn the job. Returns the node id.
pub fn start(
    host: &Arc<Host>,
    actor: &Actor,
    canvas: &str,
    model: &str,
    input: Json,
    place: Placement,
) -> Result<String> {
    let key = host
        .opts
        .fal_key
        .clone()
        .ok_or_else(|| anyhow!("FAL_KEY is not set for the fal-os host; set it in the host's environment and restart `falos-host serve`"))?;
    if model.is_empty() || model.contains("..") || model.starts_with('/') {
        bail!("bad model id `{model}`");
    }
    let canvas = host.resolve_canvas(canvas)?;
    let (w, h) = (place.w.unwrap_or(512.0), place.h.unwrap_or(512.0));
    let (x, y) = match (place.x, place.y) {
        (Some(x), Some(y)) => (x, y),
        _ => host.read(&canvas, |c| {
            // Right of the existing top-level content.
            let right = c
                .nodes()
                .iter()
                .filter(|n| n.parent.is_none())
                .map(|n| n.x + n.w)
                .fold(f64::NEG_INFINITY, f64::max);
            (if right.is_finite() { right + 40.0 } else { 0.0 }, 0.0)
        })?,
    };
    let name: String = input
        .get("prompt")
        .and_then(Json::as_str)
        .unwrap_or(model)
        .chars()
        .take(48)
        .collect();
    let props: Props = serde_json::from_value(json!({
        "x": x, "y": y, "w": w, "h": h,
        "status": "queued", "model": model, "params": input,
    }))?;
    let res = host.mutate(&canvas, &actor.id, None, |c| {
        c.apply(
            actor,
            None,
            &[Command::CreateNode {
                id: None,
                kind: NodeKind::Generation,
                parent: place.parent.clone(),
                index: None,
                name,
                props,
                text: None,
            }],
        )
    })?;
    let node = res.created[0].clone();
    let job = Job {
        host: host.clone(),
        canvas,
        node: node.clone(),
        model: model.into(),
        input,
        key,
        requester: actor.id.clone(),
    };
    tokio::spawn(async move {
        if let Err(e) = job.run().await {
            job.fail(&format!("{e:#}"));
        }
    });
    Ok(node)
}

struct Job {
    host: Arc<Host>,
    canvas: String,
    node: String,
    model: String,
    input: Json,
    key: String,
    requester: String,
}

impl Job {
    fn event(&self, status: &str, queue_position: Option<u64>, error: Option<String>) {
        self.host.emit(Event::Job {
            canvas: self.canvas.clone(),
            node: self.node.clone(),
            model: self.model.clone(),
            status: status.into(),
            queue_position,
            error,
        });
    }

    /// Update the generation node's props as the jobs actor. Returns false when the node
    /// is gone (e.g. the requester undid it), which cancels the job.
    fn update(&self, props: Json) -> bool {
        let actor = jobs_actor();
        let props: Props = serde_json::from_value(props).unwrap_or_default();
        self.host
            .mutate(&self.canvas, &actor.id, None, |c| {
                if c.node(&self.node)
                    .is_none_or(|n| n.kind != NodeKind::Generation)
                {
                    return Ok(false);
                }
                c.apply(
                    &actor,
                    None,
                    &[Command::UpdateNode {
                        id: self.node.clone(),
                        props,
                    }],
                )?;
                Ok(true)
            })
            .unwrap_or(false)
    }

    fn fail(&self, err: &str) {
        tracing::warn!("generation {} failed: {err}", self.node);
        self.update(json!({"status": "failed", "error": err}));
        self.event("failed", None, Some(err.into()));
    }

    async fn run(&self) -> Result<()> {
        let http = &self.host.http;
        let auth = format!("Key {}", self.key);
        let submit: Json = http
            .post(format!("{}/{}", self.host.opts.fal_queue_url, self.model))
            .header("Authorization", &auth)
            .json(&self.input)
            .send()
            .await?
            .error_for_status()
            .context("fal queue submit")?
            .json()
            .await?;
        let request_id = submit["request_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let status_url = submit["status_url"]
            .as_str()
            .ok_or_else(|| anyhow!("fal queue: no status_url in {submit}"))?;
        let response_url = submit["response_url"]
            .as_str()
            .ok_or_else(|| anyhow!("fal queue: no response_url in {submit}"))?;
        if !self.update(json!({"job": request_id})) {
            return Ok(());
        }
        self.event("queued", None, None);

        let started = tokio::time::Instant::now();
        let mut last = String::new();
        loop {
            if started.elapsed() > TIMEOUT {
                bail!("timed out after {} min", TIMEOUT.as_secs() / 60);
            }
            let s: Json = http
                .get(status_url)
                .header("Authorization", &auth)
                .send()
                .await?
                .error_for_status()
                .context("fal queue status")?
                .json()
                .await?;
            let status = s["status"].as_str().unwrap_or_default();
            let pos = s["queue_position"].as_u64();
            match status {
                "COMPLETED" => {
                    if let Some(e) = s.get("error").and_then(Json::as_str) {
                        bail!("{e}");
                    }
                    break;
                }
                "IN_PROGRESS" if last != status => {
                    if !self.update(json!({"status": "running", "progress": 0.5})) {
                        return Ok(());
                    }
                    self.event("running", None, None);
                }
                "IN_QUEUE" => self.event("queued", pos, None),
                _ => {}
            }
            last = status.to_string();
            tokio::time::sleep(POLL).await;
        }

        let result: Json = http
            .get(response_url)
            .header("Authorization", &auth)
            .send()
            .await?
            .error_for_status()
            .context("fal queue result")?
            .json()
            .await?;
        let media = find_media(&result).ok_or_else(|| anyhow!("no media url in fal result"))?;
        let bytes = http
            .get(&media.url)
            .send()
            .await?
            .error_for_status()
            .context("downloading output")?
            .bytes()
            .await?;
        let hex: String = Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let dir = self.host.dir().join("assets");
        tokio::fs::create_dir_all(&dir).await?;
        let file = dir.join(format!("{hex}.{}", media.ext));
        tokio::fs::write(&file, &bytes).await?;
        let asset = Asset {
            id: format!("sha256:{hex}"),
            mime: media.mime.clone(),
            uri: Some(media.url.clone()),
            lineage: Some(Lineage {
                model: self.model.clone(),
                params: self.input.clone(),
                seed: result.get("seed").and_then(Json::as_i64),
                parents: vec![],
                fal_request_id: Some(request_id),
                cost: None,
                actor: self.requester.clone(),
            }),
        };
        self.convert(media, asset)?;
        self.event("done", None, None);
        Ok(())
    }

    /// Turn the generation node into the media node, keeping id, parent and z-order.
    fn convert(&self, media: Media, asset: Asset) -> Result<()> {
        let actor = jobs_actor();
        self.host.mutate(&self.canvas, &actor.id, None, |c| {
            let Some(n) = c
                .node(&self.node)
                .filter(|n| n.kind == NodeKind::Generation)
            else {
                return Ok(());
            };
            let siblings: Vec<String> = match &n.parent {
                Some(p) => c.node(p).map(|p| p.children).unwrap_or_default(),
                None => c
                    .nodes()
                    .into_iter()
                    .filter(|x| x.parent.is_none())
                    .map(|x| x.id)
                    .collect(),
            };
            let index = siblings.iter().position(|s| *s == n.id);
            let h = match (media.width, media.height) {
                (Some(mw), Some(mh)) if mw > 0.0 => n.w * mh / mw,
                _ => n.h,
            };
            let props: Props = serde_json::from_value(json!({
                "x": n.x, "y": n.y, "w": n.w, "h": h, "asset": asset.id,
            }))?;
            c.apply(
                &actor,
                None,
                &[
                    Command::PutAsset(asset.clone()),
                    Command::DeleteNode { id: n.id.clone() },
                    Command::CreateNode {
                        id: Some(n.id.clone()),
                        kind: media.kind,
                        parent: n.parent.clone(),
                        index,
                        name: n.name.clone(),
                        props,
                        text: None,
                    },
                ],
            )?;
            Ok(())
        })
    }
}

struct Media {
    url: String,
    mime: Option<String>,
    kind: NodeKind,
    ext: String,
    width: Option<f64>,
    height: Option<f64>,
}

/// First `{url, content_type?}` object in a fal result, depth-first.
fn find_media(v: &Json) -> Option<Media> {
    match v {
        Json::Object(o) => {
            if let Some(url) = o.get("url").and_then(Json::as_str)
                && url.starts_with("http")
            {
                let mime = o
                    .get("content_type")
                    .and_then(Json::as_str)
                    .map(String::from);
                let path = url.split(['?', '#']).next().unwrap_or(url);
                let ext = path
                    .rsplit_once('.')
                    .map(|(_, e)| e.to_lowercase())
                    .filter(|e| e.len() <= 5 && e.chars().all(|c| c.is_ascii_alphanumeric()))
                    .or_else(|| {
                        mime.as_deref()
                            .and_then(|m| m.split('/').nth(1))
                            .map(String::from)
                    })
                    .unwrap_or_else(|| "bin".into());
                let top = mime
                    .as_deref()
                    .and_then(|m| m.split('/').next())
                    .unwrap_or("");
                let kind = match (top, ext.as_str()) {
                    ("video", _) | (_, "mp4" | "mov" | "webm") => NodeKind::Video,
                    ("audio", _) | (_, "mp3" | "wav" | "ogg" | "flac" | "m4a") => NodeKind::Audio,
                    ("model", _) | (_, "glb" | "gltf" | "usdz" | "ply" | "splat") => {
                        NodeKind::Model3d
                    }
                    (_, "svg") => NodeKind::Vector,
                    _ => NodeKind::Image,
                };
                return Some(Media {
                    url: url.into(),
                    mime,
                    kind,
                    ext,
                    width: o.get("width").and_then(Json::as_f64),
                    height: o.get("height").and_then(Json::as_f64),
                });
            }
            o.values().find_map(find_media)
        }
        Json::Array(a) => a.iter().find_map(find_media),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_first_media() {
        let m = find_media(&json!({
            "seed": 3,
            "images": [{"url": "https://x/y/a.png?t=1", "width": 1024, "height": 512, "content_type": "image/png"}]
        }))
        .unwrap();
        assert_eq!(
            (m.kind, m.ext.as_str(), m.width),
            (NodeKind::Image, "png", Some(1024.0))
        );
        let v = find_media(&json!({"video": {"url": "https://x/v.mp4"}})).unwrap();
        assert_eq!(v.kind, NodeKind::Video);
        assert!(find_media(&json!({"text": "no"})).is_none());
    }
}
