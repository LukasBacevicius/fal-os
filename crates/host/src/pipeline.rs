//! Per-item pipelines (`then` on `generate_batch` / `transform`): each output flows into the
//! next step the moment it lands (e.g. image → image-to-video → upscale), with no agent
//! round-trip in between. Every step is a new node next to the previous one with lineage
//! (parent = the previous step's output); a failure stops only that item's chain.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use falos_doc::{Actor, NodeKind};
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::host::Host;
use crate::mcp::PlacementArgs;

/// One step of a per-item pipeline: a fal model or an asset op.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
pub struct Step {
    /// fal endpoint for this step, e.g. "fal-ai/kling-video/v2.1/standard/image-to-video".
    pub model: Option<String>,
    /// Or an asset op from list_ops (e.g. "upscale", "reframe", "remove_bg").
    pub op: Option<String>,
    /// Model input (prompt, duration, …) or op args, merged with the previous output.
    pub args: Option<Json>,
    /// Model param that receives the previous step's output (default by its kind:
    /// image_url / video_url / audio_url). Ignored for ops (the output is the op's input).
    #[serde(alias = "inputFrom")]
    pub input_from: Option<String>,
    /// Node name for this step's output (default: "<previous name> · <model/op>").
    pub name: Option<String>,
}

pub const MAX_STEPS: usize = 6;
const STEP_TIMEOUT: Duration = Duration::from_secs(30 * 60);

pub fn validate(steps: &[Step]) -> Result<()> {
    if steps.len() > MAX_STEPS {
        bail!("at most {MAX_STEPS} `then` steps");
    }
    for (i, s) in steps.iter().enumerate() {
        match (&s.model, &s.op) {
            (Some(m), None) => crate::fal::check_model(m)?,
            (None, Some(op)) => {
                let spec = crate::ops::spec(op).ok_or_else(|| anyhow!("then[{i}]: unknown op `{op}` (see list_ops)"))?;
                if spec.category == "inspect" {
                    bail!("then[{i}]: `{op}` is read-only");
                }
            }
            _ => bail!("then[{i}]: give exactly one of `model` or `op`"),
        }
    }
    Ok(())
}

/// Default model param for an input of this kind.
fn default_param(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Video => "video_url",
        NodeKind::Audio => "audio_url",
        _ => "image_url",
    }
}

fn short(model_or_op: &str) -> String {
    let parts: Vec<&str> = model_or_op.split('/').filter(|p| !p.is_empty()).collect();
    let tail = parts.iter().rev().take(2).rev().copied().collect::<Vec<_>>().join("/");
    if tail.is_empty() { model_or_op.to_string() } else { tail }
}

/// Wait until `node` is a media node (a batch converts results in coalesced flushes).
pub async fn wait_media(host: &Host, canvas: &str, node: &str, timeout: Duration) -> Result<()> {
    let start = tokio::time::Instant::now();
    loop {
        match host.read(canvas, |c| c.node(node).map(|n| n.kind))? {
            None => bail!("`{node}` was removed"),
            Some(NodeKind::Generation) => {}
            Some(_) => return Ok(()),
        }
        if start.elapsed() > timeout {
            bail!("`{node}` did not finish");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Run `steps` on `start` (a media node), one after another. Returns
/// `{source, nodes, status, error?}` — `nodes` are the step outputs so far.
pub async fn run_chain(host: &Arc<Host>, actor: &Actor, session: Option<&str>, canvas: &str, start: &str, steps: &[Step]) -> Json {
    let mut prev = start.to_string();
    let mut nodes: Vec<String> = vec![];
    for (i, step) in steps.iter().enumerate() {
        match run_step(host, actor, session, canvas, &prev, step).await {
            Ok(n) => {
                nodes.push(n.clone());
                prev = n;
            }
            Err(e) => {
                let msg = format!("{e:#}");
                tracing::warn!("pipeline step {i} after {prev} failed: {msg}");
                return json!({"source": start, "nodes": nodes, "status": "failed", "step": i, "error": msg.chars().take(300).collect::<String>()});
            }
        }
    }
    json!({"source": start, "nodes": nodes, "status": "done"})
}

async fn run_step(host: &Arc<Host>, actor: &Actor, session: Option<&str>, canvas: &str, prev: &str, step: &Step) -> Result<String> {
    let (kind, name, parent, w, h) = host
        .read(canvas, |c| c.node(prev).map(|n| (n.kind, n.name.clone(), n.parent.clone(), n.w, n.h)))?
        .ok_or_else(|| anyhow!("`{prev}` was removed"))?;
    let args = step.args.clone().unwrap_or_else(|| json!({}));
    if let Some(op) = &step.op {
        let run = crate::ops::start(
            host,
            actor,
            session,
            canvas,
            crate::ops::Request {
                op: op.clone(),
                inputs: json!(prev),
                args,
                placement: Some(PlacementArgs { near: Some(vec![prev.to_string()]), ..Default::default() }),
                name: Some(step.name.clone().unwrap_or_else(|| format!("{name} · {op}"))),
                role: None,
                then: vec![],
            },
        )?;
        let results = run.wait(STEP_TIMEOUT).await.ok_or_else(|| anyhow!("{op} timed out"))?;
        let r = results.first().ok_or_else(|| anyhow!("{op} returned nothing"))?;
        if r["status"] != "done" {
            bail!("{op}: {}", r["error"].as_str().unwrap_or(r["status"].as_str().unwrap_or("failed")));
        }
        return Ok(r["node"].as_str().unwrap_or_default().to_string());
    }
    let model = step.model.clone().ok_or_else(|| anyhow!("step needs `model` or `op`"))?;
    let param = step.input_from.clone().unwrap_or_else(|| default_param(kind).to_string());
    let mut refs = serde_json::Map::new();
    refs.insert(param.clone(), if param.ends_with("_urls") { json!([prev]) } else { json!(prev) });
    let (fields, parents) = crate::ops::media_inputs(host, actor, canvas, &refs).await?;
    let mut input = match args {
        Json::Object(o) => o,
        _ => bail!("`args` of a model step must be an object"),
    };
    input.extend(fields);
    let (x, y) = host.find_space(canvas, parent.as_deref(), &[prev.to_string()], w, h)?;
    let label = step.name.clone().unwrap_or_else(|| format!("{name} · {}", short(&model)));
    let place = crate::fal::Placement { x: Some(x), y: Some(y), w: Some(w), h: Some(h), parent };
    crate::fal::run_to_end(host, actor, canvas, &model, Json::Object(input), Some(&label), place, parents, |node| {
        if let Some(s) = session {
            host.activity(s, canvas, "write", &[node.to_string()]);
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_steps() {
        let ok: Vec<Step> = serde_json::from_value(json!([{"model": "fal-ai/x", "input_from": "image_url"}, {"op": "upscale"}])).unwrap();
        assert!(validate(&ok).is_ok());
        let camel: Vec<Step> = serde_json::from_value(json!([{"model": "fal-ai/x", "inputFrom": "start_image_url"}])).unwrap();
        assert_eq!(camel[0].input_from.as_deref(), Some("start_image_url"));
        let both: Vec<Step> = serde_json::from_value(json!([{"model": "a/b", "op": "upscale"}])).unwrap();
        assert!(validate(&both).is_err());
        assert_eq!(default_param(NodeKind::Video), "video_url");
        assert_eq!(short("fal-ai/kling-video/v2.1/standard/image-to-video"), "standard/image-to-video");
    }
}
