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
use falos_doc::{Actor, Asset, Command, Lineage, NodeKind, Parent, Props};
use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};

use crate::fal_relay::Relay;
use crate::host::{Event, Host};

/// How jobs reach fal: the user's OAuth session through the fal MCP relay (default), or a
/// raw `FAL_KEY` against the queue API (CI/tests).
#[derive(Clone)]
pub(crate) enum Backend {
    Relay(&'static Relay),
    Key(String),
}

fn relay(http: &reqwest::Client) -> &'static Relay {
    static RELAY: std::sync::OnceLock<Relay> = std::sync::OnceLock::new();
    RELAY.get_or_init(|| Relay::new(http.clone()))
}

const POLL: Duration = Duration::from_millis(1000);
const TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// The system actor that writes job progress (kept off the requester's undo stack).
pub(crate) fn jobs_actor() -> Actor {
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
/// How the host talks to fal: `FAL_KEY`, else the signed-in fal session.
pub(crate) fn backend(host: &Host) -> Result<Backend> {
    Ok(match host.opts.fal_key.clone() {
        Some(key) => Backend::Key(key),
        None if host.opts.fal_session && Relay::available() => Backend::Relay(relay(&host.http)),
        None => bail!("the fal-os host is not signed in to fal: ask the user to run `falos-host login`"),
    })
}

pub(crate) fn check_model(model: &str) -> Result<()> {
    if model.is_empty() || model.contains("..") || model.starts_with('/') {
        bail!("bad model id `{model}`");
    }
    Ok(())
}

pub fn start(
    host: &Arc<Host>,
    actor: &Actor,
    canvas: &str,
    model: &str,
    input: Json,
    place: Placement,
    parents: Vec<Parent>,
) -> Result<String> {
    let job = create(host, actor, canvas, model, input, None, place, parents)?;
    let node = job.node.clone();
    tokio::spawn(async move {
        if let Err(e) = job.run().await {
            job.fail(&format!("{e:#}"));
        }
    });
    Ok(node)
}

/// Like [`start`], but runs the job to the end (pipeline steps). Returns the node id once it
/// is the media node; errors (the node shows `failed`) when the job fails or was canceled.
/// `created` sees the placeholder id as soon as it exists.
#[allow(clippy::too_many_arguments)]
pub async fn run_to_end(
    host: &Arc<Host>,
    actor: &Actor,
    canvas: &str,
    model: &str,
    input: Json,
    name: Option<&str>,
    place: Placement,
    parents: Vec<Parent>,
    created: impl FnOnce(&str),
) -> Result<String> {
    let job = create(host, actor, canvas, model, input, name, place, parents)?;
    created(&job.node);
    if let Err(e) = job.run().await {
        let msg = format!("{e:#}");
        job.fail(&msg);
        bail!("{msg}");
    }
    let converted = host
        .read(&job.canvas, |c| c.node(&job.node).is_some_and(|n| n.kind != NodeKind::Generation))
        .unwrap_or(false);
    if !converted {
        bail!("`{}` was removed before it finished", job.node);
    }
    Ok(job.node)
}

/// Create the generation node (placeholder) for a job without starting it.
#[allow(clippy::too_many_arguments)]
fn create(
    host: &Arc<Host>,
    actor: &Actor,
    canvas: &str,
    model: &str,
    input: Json,
    name: Option<&str>,
    place: Placement,
    parents: Vec<Parent>,
) -> Result<Job> {
    let backend = backend(host)?;
    check_model(model)?;
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
    let name: String = name
        .or(input.get("prompt").and_then(Json::as_str))
        .unwrap_or(model)
        .chars()
        .take(if name.is_some() { 80 } else { 48 })
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
    Ok(Job {
        host: host.clone(),
        canvas,
        node: res.created[0].clone(),
        model: model.into(),
        input,
        backend,
        requester: actor.id.clone(),
        parents,
    })
}

pub(crate) struct Job {
    pub host: Arc<Host>,
    pub canvas: String,
    pub node: String,
    pub model: String,
    pub input: Json,
    pub backend: Backend,
    pub requester: String,
    /// Media inputs (`mediaInputs`), recorded as lineage parents.
    pub parents: Vec<Parent>,
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
            cost: None,
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

    pub(crate) async fn submit(&self) -> Result<(String, Json)> {
        match &self.backend {
            Backend::Key(key) => {
                let submit: Json = self
                    .host
                    .http
                    .post(format!("{}/{}", self.host.opts.fal_queue_url, self.model))
                    .header("Authorization", format!("Key {key}"))
                    .json(&self.input)
                    .send()
                    .await?
                    .error_for_status()
                    .context("fal queue submit")?
                    .json()
                    .await?;
                let id = submit["request_id"].as_str().unwrap_or_default().to_string();
                Ok((id, submit))
            }
            Backend::Relay(r) => {
                let submit = r
                    .call("submit_job", json!({"endpoint_id": self.model, "input": self.input}))
                    .await?;
                let id = submit["request_id"]
                    .as_str()
                    .ok_or_else(|| anyhow!("fal submit_job returned no request_id: {submit}"))?
                    .to_string();
                Ok((id, submit))
            }
        }
    }

    /// Returns the job status (`IN_QUEUE`, `IN_PROGRESS`, `COMPLETED`, …) and queue position.
    pub(crate) async fn status(&self, id: &str, submit: &Json) -> Result<(String, Option<u64>, Option<String>)> {
        let s: Json = match &self.backend {
            Backend::Key(key) => {
                let url = submit["status_url"]
                    .as_str()
                    .ok_or_else(|| anyhow!("fal queue: no status_url in {submit}"))?;
                self.host
                    .http
                    .get(url)
                    .header("Authorization", format!("Key {key}"))
                    .send()
                    .await?
                    .error_for_status()
                    .context("fal queue status")?
                    .json()
                    .await?
            }
            Backend::Relay(r) => {
                let mut args = json!({"endpoint_id": self.model, "request_id": id});
                if let Some(u) = submit["status_url"].as_str() {
                    args["status_url"] = u.into();
                }
                r.call("check_job", args).await?
            }
        };
        let status = s["status"].as_str().unwrap_or_default().to_uppercase().replace(' ', "_");
        let error = s.get("error").and_then(Json::as_str).map(String::from);
        Ok((status, s["queue_position"].as_u64(), error))
    }

    pub(crate) async fn result(&self, id: &str, submit: &Json) -> Result<Json> {
        match &self.backend {
            Backend::Key(key) => {
                let url = submit["response_url"]
                    .as_str()
                    .ok_or_else(|| anyhow!("fal queue: no response_url in {submit}"))?;
                Ok(self
                    .host
                    .http
                    .get(url)
                    .header("Authorization", format!("Key {key}"))
                    .send()
                    .await?
                    .error_for_status()
                    .context("fal queue result")?
                    .json()
                    .await?)
            }
            Backend::Relay(r) => {
                let mut args = json!({"endpoint_id": self.model, "request_id": id});
                if let Some(u) = submit["response_url"].as_str() {
                    args["response_url"] = u.into();
                }
                r.call("get_job_result", args).await
            }
        }
    }

    async fn run(&self) -> Result<()> {
        let http = &self.host.http;
        let (request_id, submit) = self.submit().await?;
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
            let (status, pos, error) = self.status(&request_id, &submit).await?;
            match status.as_str() {
                "COMPLETED" | "OK" | "SUCCEEDED" => {
                    if let Some(e) = error {
                        bail!("{e}");
                    }
                    break;
                }
                "FAILED" | "ERROR" | "CANCELLED" => bail!("{}", error.unwrap_or(status)),
                "IN_PROGRESS" | "RUNNING" if last != status => {
                    if !self.update(json!({"status": "running", "progress": 0.5})) {
                        return Ok(());
                    }
                    self.event("running", None, None);
                }
                "IN_QUEUE" | "QUEUED" => self.event("queued", pos, None),
                _ => {}
            }
            last = status;
            tokio::time::sleep(POLL).await;
        }

        let result = self.result(&request_id, &submit).await?;
        let media = find_media(&result).ok_or_else(|| anyhow!("no media url in fal result"))?;
        let asset = download(
            &self.host,
            &media,
            Lineage {
                seed: result.get("seed").and_then(Json::as_i64),
                fal_request_id: Some(request_id),
                parents: self.parents.clone(),
                ..Lineage::generate(&self.model, self.input.clone(), &self.requester)
            },
        )
        .await?;
        let actor = jobs_actor();
        self.host.mutate(&self.canvas, &actor.id, None, |c| {
            let cmds = convert_cmds(c, &self.node, &media, &asset)
                .map_err(|e| falos_doc::Error::Invalid(format!("{e:#}")))?;
            if !cmds.is_empty() {
                c.apply(&actor, None, &cmds)?;
            }
            Ok(())
        })?;
        let _ = http;
        self.event("done", None, None);
        Ok(())
    }
}

/// Download a fal output into the project's assets (by sha256); returns its asset record.
pub(crate) async fn download(host: &Host, media: &Media, lineage: Lineage) -> Result<Asset> {
    let bytes = host
        .http
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
    let dir = host.dir().join("assets");
    tokio::fs::create_dir_all(&dir).await?;
    let file = dir.join(format!("{hex}.{}", media.ext));
    if !file.exists() {
        tokio::fs::write(&file, &bytes).await?;
    }
    let source = match lineage.kind {
        falos_doc::LineageKind::Derive => falos_doc::Source::Derive,
        falos_doc::LineageKind::Render => falos_doc::Source::Render,
        _ => falos_doc::Source::Generate,
    };
    Ok(Asset {
        id: format!("sha256:{hex}"),
        mime: media.mime.clone(),
        uri: Some(media.url.clone()),
        source,
        media: (media.width.is_some() || media.height.is_some()).then(|| falos_doc::Media {
            w: media.width,
            h: media.height,
            ..Default::default()
        }),
        lineage: Some(lineage),
        origin: None,
    })
}

/// Commands that turn the generation node `node` into the media node (same id, parent and
/// z-order; height follows the media's aspect). Empty when the node is gone or converted.
pub(crate) fn convert_cmds(
    c: &falos_doc::Canvas,
    node: &str,
    media: &Media,
    asset: &Asset,
) -> Result<Vec<Command>> {
    let Some(n) = c.node(node).filter(|n| n.kind == NodeKind::Generation) else {
        return Ok(vec![]);
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
        "x": n.x, "y": n.y, "w": n.w, "h": h, "asset": asset.id, "role": "candidate",
    }))?;
    Ok(vec![
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
    ])
}

pub(crate) struct Media {
    pub url: String,
    pub mime: Option<String>,
    pub kind: NodeKind,
    pub ext: String,
    pub width: Option<f64>,
    pub height: Option<f64>,
}

/// Every media output of a fal result, in order (e.g. all of `images` for `num_images` > 1).
pub(crate) fn find_all_media(v: &Json) -> Vec<Media> {
    fn walk(v: &Json, out: &mut Vec<Media>) {
        if let Some(m) = find_media_here(v) {
            out.push(m);
            return;
        }
        match v {
            Json::Object(o) => o.values().for_each(|x| walk(x, out)),
            Json::Array(a) => a.iter().for_each(|x| walk(x, out)),
            _ => {}
        }
    }
    let mut out = vec![];
    walk(v, &mut out);
    out
}

fn find_media_here(v: &Json) -> Option<Media> {
    match v {
        Json::Object(o) if o.get("url").and_then(Json::as_str).is_some() => find_media(v),
        _ => None,
    }
}

/// First `{url, content_type?}` object in a fal result, depth-first.
pub(crate) fn find_media(v: &Json) -> Option<Media> {
    match v {
        Json::Object(o) => {
            if let Some(url) = o.get("url").and_then(Json::as_str)
                && url.starts_with("http")
                && !url.contains("queue.fal.run")
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
