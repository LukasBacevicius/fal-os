//! MCP tools for the op registry: a small surface where the op name is an argument
//! (reference: knowledge `assets`, `export`).

use std::time::Duration;

use anyhow::bail;
use base64::Engine as _;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::{schemars, tool, tool_router};
use serde::Deserialize;
use serde_json::{Value as Json, json};

use super::export::ExportOpts;
use crate::mcp::{CanvasMcp, PlacementArgs};

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ListOpsArgs {
    /// inspect | transform | compose (default: all).
    pub category: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InspectArgs {
    pub session_id: String,
    pub canvas: String,
    /// Node id, asset id (sha256:…), "selection", a frame id, a chip ({"kind":"frame","node","t"} …) or a list.
    pub inputs: Json,
    /// probe | contact_sheet | palette | loudness | scenes | compare (default ["probe"]).
    pub what: Option<Vec<String>>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TransformArgs {
    pub session_id: String,
    pub canvas: String,
    /// Op name from list_ops, e.g. "reframe", "trim", "remove_bg", "ffmpeg".
    pub op: String,
    /// Node id(s), asset id(s), "selection", a frame id (= its media children) or chips:
    /// region → implicit crop, frame → still, range → trim. Several inputs = a batch (one output each).
    pub inputs: Json,
    /// Op args (see list_ops), e.g. {"aspect":"9:16","mode":"pad-blur"}.
    pub args: Option<Json>,
    pub placement: Option<PlacementArgs>,
    /// Output node name.
    pub name: Option<String>,
    /// Wait for the result (default true, up to timeoutSecs); false returns placeholder ids at once.
    pub wait: Option<bool>,
    /// Max seconds to wait (default 120, max 1800).
    pub timeout_secs: Option<u64>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ComposeArgs {
    pub session_id: String,
    pub canvas: String,
    /// "grid" or "contact_sheet".
    pub op: String,
    /// Inputs (node ids, chips, "selection") — or use `frame`.
    pub inputs: Option<Json>,
    /// A frame whose media children are the inputs.
    pub frame: Option<String>,
    pub args: Option<Json>,
    pub placement: Option<PlacementArgs>,
    pub name: Option<String>,
    pub wait: Option<bool>,
    pub timeout_secs: Option<u64>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RerunArgs {
    pub session_id: String,
    pub canvas: String,
    /// Node whose recipe to re-run (a derived or generated node).
    pub node: String,
    /// Args merged over the recipe (op args, or the model input for generations).
    pub overrides: Option<Json>,
    /// Apply the recipe to these nodes instead of the original inputs.
    pub on: Option<Vec<String>>,
    pub wait: Option<bool>,
    pub timeout_secs: Option<u64>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExportArgs {
    pub session_id: String,
    pub canvas: String,
    /// Node ids, a frame id, or "deliverables" (nodes with role final / in a "Deliverables" frame).
    pub nodes: Option<Json>,
    pub frame: Option<String>,
    /// Preset ids or rows, e.g. ["reel", {"preset":"web-image","format":"avif","scale":"2x","suffix":"@2x"}].
    /// Default: each node's saved `export` prop, else "original".
    pub presets: Option<Vec<Json>>,
    /// Destination (absolute, or relative to the project). Default exports/<date>/ (deliverables/ for deliverables).
    pub dir: Option<String>,
    /// Naming pattern; tokens {project} {name} {preset} {w} {h} {n} {index} {date} {node} {scale} {suffix} {ext}.
    pub naming: Option<String>,
    /// Reframe mode for aspect mismatches: pad-blur (default) | crop-center | pad-color.
    pub reframe: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PackageArgs {
    pub session_id: String,
    pub canvas: String,
    /// "project" (canvases + referenced assets) or "deliverables" (exported finals).
    pub kind: String,
}

fn timeout(t: Option<u64>) -> Duration {
    Duration::from_secs(t.unwrap_or(120).clamp(1, 1800))
}

impl CanvasMcp {
    async fn run_op(&self, session: &str, canvas: &str, req: super::Request, wait: Option<bool>, t: Option<u64>) -> anyhow::Result<String> {
        let host = self.host().clone();
        let actor = host.actor(session)?;
        let canvas = host.resolve_canvas(canvas)?;
        let run = super::start(&host, &actor, Some(session), &canvas, req)?;
        host.touch_nodes(session, &canvas, &run.nodes);
        let results = if wait.unwrap_or(true) { run.wait(timeout(t)).await } else { None };
        let mut v = run.summary(results.as_deref());
        if results.is_none() {
            v["hint"] = json!("placeholders convert in place when done; read with root=<node> to follow progress");
        }
        Ok(serde_json::to_string_pretty(&v)?)
    }
}

#[tool_router(router = ops_tools, vis = "pub(crate)")]
impl CanvasMcp {
    #[tool(
        description = "List the asset ops (inspect / transform / compose) with their arg schemas, engines and costs.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn list_ops(&self, Parameters(a): Parameters<ListOpsArgs>) -> CallToolResult {
        let ops: Vec<super::OpSpec> = super::specs().into_iter().filter(|s| a.category.as_deref().is_none_or(|c| c == s.category)).collect();
        self.finish(None, serde_json::to_string_pretty(&json!({"ops": ops})).map_err(Into::into))
    }

    #[tool(
        description = "Read-only media facts, cached by sha: probe, contact_sheet, palette, loudness, scenes, compare. Visual results come back as images.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn inspect(&self, Parameters(a): Parameters<InspectArgs>) -> CallToolResult {
        let host = self.host().clone();
        let mut visuals = vec![];
        let r = async {
            host.actor(&a.session_id)?;
            let (v, vis) = super::inspect::inspect(&host, &a.canvas, &a.inputs, &a.what.unwrap_or_default()).await?;
            visuals = vis;
            Ok(serde_json::to_string_pretty(&v)?)
        }
        .await;
        let mut res = self.finish(Some(&a.session_id), r);
        for v in visuals {
            res.content.push(ContentBlock::text(v.caption));
            res.content.push(ContentBlock::image(base64::engine::general_purpose::STANDARD.encode(&v.jpeg), "image/jpeg"));
        }
        res
    }

    #[tool(
        description = "Run an asset op (crop, resize, reframe, convert, compress, trim, concat, speed, extract_frames, gif, audio_normalize, audio_mix, audio_extract, strip, remove_bg, upscale, inpaint, ffmpeg) on nodes/chips: makes NEW nodes with lineage beside the inputs (inputs are never modified). Batch = several inputs.",
        annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = false, open_world_hint = true)
    )]
    async fn transform(&self, Parameters(a): Parameters<TransformArgs>) -> CallToolResult {
        let r = async {
            if super::spec(&a.op).is_some_and(|s| s.category != "transform") {
                bail!("`{}` isn't a transform op (see list_ops)", a.op);
            }
            let req = super::Request { op: a.op, inputs: a.inputs, args: a.args.unwrap_or(json!({})), placement: a.placement, name: a.name, role: None };
            self.run_op(&a.session_id, &a.canvas, req, a.wait, a.timeout_secs).await
        }
        .await;
        self.finish(Some(&a.session_id), r)
    }

    #[tool(
        description = "Compose several inputs (or a frame's media) into one new image: grid (collage) or contact_sheet (video frames).",
        annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = false, open_world_hint = false)
    )]
    async fn compose(&self, Parameters(a): Parameters<ComposeArgs>) -> CallToolResult {
        let r = async {
            if super::spec(&a.op).is_none_or(|s| s.category != "compose") {
                bail!("compose ops: grid, contact_sheet");
            }
            let inputs = a.inputs.or(a.frame.map(Json::from)).unwrap_or(Json::Null);
            let req = super::Request { op: a.op, inputs, args: a.args.unwrap_or(json!({})), placement: a.placement, name: a.name, role: None };
            self.run_op(&a.session_id, &a.canvas, req, a.wait, a.timeout_secs).await
        }
        .await;
        self.finish(Some(&a.session_id), r)
    }

    #[tool(
        description = "Re-run a node's recipe (derived op or fal generation) with overrides, or apply its recipe to other nodes (`on`). Makes new nodes.",
        annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = false, open_world_hint = true)
    )]
    async fn rerun(&self, Parameters(a): Parameters<RerunArgs>) -> CallToolResult {
        let host = self.host().clone();
        let r = async {
            let actor = host.actor(&a.session_id)?;
            let mut v = super::rerun(&host, &actor, Some(&a.session_id), &a.canvas, &a.node, a.overrides, a.on).await?;
            let rx = v["run"].as_str().and_then(|id| host.ops_runs.0.lock().unwrap().get(id).cloned());
            if a.wait.unwrap_or(true)
                && let Some(mut rx) = rx
            {
                let _ = tokio::time::timeout(timeout(a.timeout_secs), rx.wait_for(|d| d.is_some())).await;
                if let Some(res) = rx.borrow().clone() {
                    v["status"] = json!("done");
                    v["results"] = json!(res);
                }
            }
            Ok(serde_json::to_string_pretty(&v)?)
        }
        .await;
        self.finish(Some(&a.session_id), r)
    }

    #[tool(
        description = "Write files for nodes / a frame / \"deliverables\" with presets (reel, tiktok, short, ig-feed-45, square, youtube-1080, youtube-4k, x, linkedin, master-prores, overlay-alpha, web-image, gif-web, wav, mp3, original). Verifies sha256, logs exports/manifest.jsonl; aspect mismatches reframe into a final node first. Returns {files, dir}.",
        annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = false, open_world_hint = false)
    )]
    async fn export(&self, Parameters(a): Parameters<ExportArgs>) -> CallToolResult {
        let host = self.host().clone();
        let r = async {
            let actor = host.actor(&a.session_id)?;
            let target = match (&a.nodes, &a.frame) {
                (_, Some(f)) => super::export::Target::Frame(f.clone()),
                (Some(n), None) => super::export::Target::from_json(n)?,
                (None, None) => bail!("pass nodes (ids or \"deliverables\") or frame"),
            };
            let opts = ExportOpts { presets: a.presets.unwrap_or_default(), dir: a.dir, naming: a.naming, reframe: a.reframe };
            let v = super::export::export(&host, &actor, Some(&a.session_id), &a.canvas, target, opts).await?;
            Ok(serde_json::to_string_pretty(&v)?)
        }
        .await;
        self.finish(Some(&a.session_id), r)
    }

    #[tool(
        description = "Zip the project (canvases + referenced assets) or its deliverables, with a manifest (nodes, lineage DAG, recipes, models, costs).",
        annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = false, open_world_hint = false)
    )]
    async fn package(&self, Parameters(a): Parameters<PackageArgs>) -> CallToolResult {
        let host = self.host().clone();
        let r = async {
            let actor = host.actor(&a.session_id)?;
            let v = super::export::package(&host, &actor, Some(&a.session_id), &a.canvas, &a.kind).await?;
            Ok(serde_json::to_string_pretty(&v)?)
        }
        .await;
        self.finish(Some(&a.session_id), r)
    }
}
