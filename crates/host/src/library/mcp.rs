//! Library MCP tools: `search_assets`, `get_asset`, `place_asset`, `tag_assets`,
//! `collect_assets`, `show_assets` (docs/ASSETS-V1.md §2, §7).

use std::fmt::Write as _;
use std::sync::Arc;

use anyhow::{Context, Result};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{schemars, tool, tool_router};
use serde::Deserialize;
use serde_json::{Value as Json, json};

use super::Library;
use super::query::{self, Query};
use crate::mcp::CanvasMcp;

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SearchAssetsArgs {
    pub session_id: String,
    /// Filters: {text?, similar_to?: sha|node, color?: "#hex", source?: [import|generate|derive|render], role?: [input|candidate|wip|final|rejected], kind?: [image|video|audio], project?: id|"*" (default: this project), favorite?, collection?, tag?: [], model?, after?/before? (ms), rule? (smart rule JSON {all|any:[{field,op,value}]}), sort?, limit? (default 30)}
    #[serde(default)]
    pub query: Query,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GetAssetArgs {
    pub session_id: String,
    /// sha256 (or a unique prefix ≥ 6 chars), `fal_<id>`, or a node id on this project's canvases.
    pub asset: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlaceAssetArgs {
    pub session_id: String,
    /// Canvas id or name (default: the project's first canvas).
    #[serde(default)]
    pub canvas: Option<String>,
    /// sha (or prefix) / `fal_<id>` from search_assets.
    pub sha: String,
    /// {x?, y?, w?}; free space is found when x/y are omitted.
    #[serde(default)]
    pub placement: Option<Json>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TagAssetsArgs {
    pub session_id: String,
    pub shas: Vec<String>,
    #[serde(default)]
    pub add: Vec<String>,
    #[serde(default)]
    pub remove: Vec<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CollectAssetsArgs {
    pub session_id: String,
    pub shas: Vec<String>,
    /// Collection id or name (a new name creates it; shared with fal for assets on fal).
    pub collection: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ShowAssetsArgs {
    pub session_id: String,
    /// Assets to show the user in their library view.
    pub shas: Vec<String>,
}

fn short(s: &str) -> &str {
    if s.len() == 64 { &s[..12] } else { s }
}

/// One line per item, for agents.
pub fn lines(items: &[Json]) -> String {
    let mut out = String::new();
    for it in items {
        let s = |k: &str| it[k].as_str().unwrap_or("");
        let dims = match (it["w"].as_i64(), it["h"].as_i64()) {
            (Some(w), Some(h)) => format!(" {w}x{h}"),
            _ => String::new(),
        };
        let dur = it["duration"].as_f64().map(|d| format!(" {d:.1}s")).unwrap_or_default();
        let _ = write!(out, "{} {}{}{} src:{}", short(s("sha")), s("kind"), dims, dur, s("source"));
        if let Some(r) = it["role"].as_str() {
            let _ = write!(out, " role:{r}");
        }
        if !s("model").is_empty() {
            let _ = write!(out, " model:{}", s("model"));
        }
        if it["favorite"] == true {
            out.push_str(" ★");
        }
        if it["fal"]["synced"] == true {
            out.push_str(" ↑fal");
        }
        let tags: Vec<&str> = it["tags"].as_array().into_iter().flatten().filter_map(Json::as_str).collect();
        if !tags.is_empty() {
            let _ = write!(out, " #{}", tags.join(" #"));
        }
        let p = it["prompt"].as_str().or(it["caption"].as_str()).or(it["name"].as_str()).unwrap_or("");
        if !p.is_empty() {
            let p: String = p.chars().take(90).collect();
            let _ = write!(out, " \"{}\"", crate::context::untrusted(&p));
        }
        out.push('\n');
    }
    out
}

impl CanvasMcp {
    fn library(&self) -> Result<Arc<Library>> {
        self.workspace().library.get().cloned().context("the asset library isn't enabled on this host")
    }

    fn check_session(&self, sid: &str) -> Result<falos_doc::Actor> {
        self.host().actor(sid)
    }

    /// `get_asset` from this project's canvases (a node id or `sha256:…`): the asset record,
    /// the node(s) showing it and the original's absolute `file`.
    fn asset_from_doc(&self, key: &str) -> Option<Json> {
        let host = self.host();
        let canvases: Vec<String> = host.project().canvases.iter().map(|c| c.id.clone()).collect();
        for canvas in canvases {
            let found = host
                .read(&canvas, |c| {
                    let (node, id) = match c.node(key) {
                        Some(n) => (Some(n.id.clone()), n.asset_id()?.to_string()),
                        None => (None, c.asset(key).map(|a| a.id)?),
                    };
                    Some((node, c.asset(&id).unwrap_or_else(|| falos_doc::Asset { id: id.clone(), ..Default::default() })))
                })
                .ok()
                .flatten();
            if let Some((node, asset)) = found {
                let file = crate::chat::asset_file(host, &asset.id).and_then(|p| std::path::absolute(p).ok());
                return Some(json!({
                    "sha": asset.id, "mime": asset.mime, "source": asset.source, "uri": asset.uri,
                    "origin": asset.origin, "lineage": asset.lineage, "media": asset.media,
                    "nodes": node.map(|n| vec![json!({"project": host.id, "canvas": canvas, "node": n})]).unwrap_or_default(),
                    "file": file.map(|f| f.display().to_string()),
                    "indexed": false,
                }));
            }
        }
        None
    }
}

#[tool_router(router = library_tools, vis = "pub(crate)")]
impl CanvasMcp {
    #[tool(
        description = "Search the asset library (every project's inputs, generations, derivatives and renders, plus the user's fal.ai library). Search before generating something that may already exist. Defaults to this project; project:\"*\" for everything. Returns one line per asset: sha prefix, kind, size, source, role, model, prompt. Use show_assets to put results in front of the user, place_asset to put one on the canvas.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = true)
    )]
    async fn search_assets(&self, Parameters(a): Parameters<SearchAssetsArgs>) -> CallToolResult {
        let r = async {
            self.check_session(&a.session_id)?;
            let lib = self.library()?;
            let mut q = a.query;
            if q.project.is_none() {
                q.project = Some(self.host().id.clone());
            }
            q.limit = Some(q.limit.unwrap_or(30).min(200));
            let mut keys = lib.with_db(|c| Ok(query::run(c, &q)?.keys))?;
            if (q.text.is_some() || q.similar_to.is_some()) && lib.fal.is_some() && q.semantic != Some(false) && q.project.as_deref() == Some("*") {
                let mut p = vec![];
                if let Some(t) = &q.text {
                    p.push(("q", t.clone()));
                }
                if !p.is_empty()
                    && let Ok(extra) = lib.fal_search(p).await
                {
                    for k in extra {
                        if !keys.contains(&k) && keys.len() < q.limit.unwrap_or(30) {
                            keys.push(k);
                        }
                    }
                }
            }
            let items = lib.with_db(|c| query::items(c, &keys, query::scope(&q)))?;
            if items.is_empty() {
                return Ok("no assets match".to_string());
            }
            Ok(format!("{} assets:\n{}", items.len(), lines(&items)))
        }
        .await;
        self.finish(Some(&a.session_id), r)
    }

    #[tool(
        description = "Everything about one asset: kind, size, source, prompt, model, lineage, palette, tags, collections, placements (which canvases/nodes show it), fal status and `file`: the absolute path of the original on disk (copy or `export` it before editing it in another app). Accepts a sha (or prefix) or a node id.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn get_asset(&self, Parameters(a): Parameters<GetAssetArgs>) -> CallToolResult {
        let r = (|| {
            self.check_session(&a.session_id)?;
            let lib = self.library();
            let d = lib.as_ref().map_err(|e| anyhow::anyhow!("{e:#}")).and_then(|lib| lib.detail(&a.asset, Some(&self.host().id)));
            match d {
                Ok(d) => Ok(serde_json::to_string_pretty(&d)?),
                // Not indexed yet (the library catches up in the background) or no library:
                // answer from the project's canvases.
                Err(e) => match self.asset_from_doc(&a.asset) {
                    Some(d) => Ok(serde_json::to_string_pretty(&d)?),
                    None => Err(e),
                },
            }
        })();
        self.finish(Some(&a.session_id), r)
    }

    #[tool(
        description = "Place a library asset on a canvas of this project as a new node (a new placement of the same asset; remote fal assets are downloaded first). Returns the node id.",
        annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = false, open_world_hint = true)
    )]
    async fn place_asset(&self, Parameters(a): Parameters<PlaceAssetArgs>) -> CallToolResult {
        let r = async {
            let actor = self.check_session(&a.session_id)?;
            let lib = self.library()?;
            let p = a.placement.unwrap_or(Json::Null);
            let res = lib.place(&a.sha, &self.host().id, a.canvas.as_deref(), (p["x"].as_f64(), p["y"].as_f64()), p["w"].as_f64(), &actor).await?;
            Ok(serde_json::to_string_pretty(&res)?)
        }
        .await;
        self.finish(Some(&a.session_id), r)
    }

    #[tool(
        description = "Add / remove tags on library assets (lowercase; shared with fal for assets on fal).",
        annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = true, open_world_hint = true)
    )]
    async fn tag_assets(&self, Parameters(a): Parameters<TagAssetsArgs>) -> CallToolResult {
        let r = (|| {
            self.check_session(&a.session_id)?;
            let keys = self.library()?.tag(&a.shas, &a.add, &a.remove)?;
            Ok(json!({"tagged": keys.len()}).to_string())
        })();
        self.finish(Some(&a.session_id), r)
    }

    #[tool(
        description = "Add library assets to a collection (by id or name; a new name creates it). Collections are shared with fal.",
        annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = true, open_world_hint = true)
    )]
    async fn collect_assets(&self, Parameters(a): Parameters<CollectAssetsArgs>) -> CallToolResult {
        let r = (|| {
            self.check_session(&a.session_id)?;
            let id = self.library()?.collect(&a.shas, &a.collection)?;
            Ok(json!({"collection": id, "added": a.shas.len()}).to_string())
        })();
        self.finish(Some(&a.session_id), r)
    }

    #[tool(
        description = "Show assets to the user: opens their library view with exactly these assets (e.g. search results to pick from).",
        annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn show_assets(&self, Parameters(a): Parameters<ShowAssetsArgs>) -> CallToolResult {
        let r = (|| {
            self.check_session(&a.session_id)?;
            let lib = self.library()?;
            let keys: Vec<String> = lib.with_db(|c| Ok(a.shas.iter().filter_map(|s| query::resolve_key(c, s)).collect()))?;
            if keys.is_empty() {
                anyhow::bail!("none of those assets are in the library");
            }
            lib.show(&keys);
            Ok(json!({"shown": keys.len()}).to_string())
        })();
        self.finish(Some(&a.session_id), r)
    }
}
