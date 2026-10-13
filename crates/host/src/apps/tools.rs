//! MCP tools for app nodes: `create_app`, `write_app_files`, `read_app`, `app_logs`.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{schemars, tool, tool_router};
use serde::Deserialize;

use super::FileArg;
use crate::mcp::{CanvasMcp, PlacementArgs};

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateAppArgs {
    pub session_id: String,
    pub canvas: String,
    /// Name shown on the node, e.g. "Ping Pong". The app id is a slug of it.
    pub name: String,
    /// Initial files (index.html at least; ES modules, CSS, assets). Omitted: a placeholder index.html.
    pub files: Option<Vec<FileArg>>,
    /// Entry page (default index.html).
    pub entry: Option<String>,
    /// Where and how big (default 800×600, free space next to the selection). w/h = the app's viewport.
    pub placement: Option<PlacementArgs>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WriteAppFilesArgs {
    pub session_id: String,
    /// App id or app node id.
    pub app: String,
    /// Files to create or overwrite, all in one atomic step (the preview reloads once).
    pub files: Option<Vec<FileArg>>,
    /// Relative paths to delete.
    pub delete: Option<Vec<String>>,
    /// Replace the whole app with exactly `files` (default false: merge).
    pub replace: Option<bool>,
    /// Load it headless afterwards and return console errors / exceptions (default true).
    pub check: Option<bool>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReadAppArgs {
    pub session_id: String,
    /// App id or app node id.
    pub app: String,
    /// Only these files or folders (default: all, text inline up to ~400 KB).
    pub paths: Option<Vec<String>>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AppLogsArgs {
    pub session_id: String,
    /// App id or app node id.
    pub app: String,
    /// Run the headless check again first (default: only when the current version wasn't checked).
    pub rerun: Option<bool>,
    /// Include entries of older versions (default: current version only).
    pub all: Option<bool>,
}

#[tool_router(router = app_tools, vis = "pub(crate)")]
impl CanvasMcp {
    #[tool(
        description = "Create a live web app on the canvas (a game, tool, visualization, prototype): a folder of plain web files (index.html + ES modules; CDN imports with pinned versions, no build step) shown as an interactive app node. Returns {app, node, dir, url, version, check:{errors}}. Load get_knowledge [\"apps\"] first.",
        annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = false, open_world_hint = false)
    )]
    async fn create_app(&self, Parameters(a): Parameters<CreateAppArgs>) -> CallToolResult {
        let host = self.host().clone();
        let r = async {
            let v = super::create(&host, &a.session_id, &a.canvas, &a.name, &a.files.unwrap_or_default(), a.entry.as_deref(), a.placement).await?;
            Ok(serde_json::to_string_pretty(&v)?)
        }
        .await;
        self.finish(Some(&a.session_id), r)
    }

    #[tool(
        description = "Write (and/or delete) files of an app in one atomic step: a new version (revertable), the node reloads live, then a headless load returns console errors / exceptions in `check`. Fix errors and write again until check.ok.",
        annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = false, open_world_hint = false)
    )]
    async fn write_app_files(&self, Parameters(a): Parameters<WriteAppFilesArgs>) -> CallToolResult {
        let host = self.host().clone();
        let r = async {
            let v = super::write(
                &host,
                &a.session_id,
                &a.app,
                &a.files.unwrap_or_default(),
                &a.delete.unwrap_or_default(),
                a.replace.unwrap_or(false),
                a.check.unwrap_or(true),
            )
            .await?;
            Ok(serde_json::to_string_pretty(&v)?)
        }
        .await;
        self.finish(Some(&a.session_id), r)
    }

    #[tool(
        description = "Read an app: files with their contents, version, folder, url, and current errors.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn read_app(&self, Parameters(a): Parameters<ReadAppArgs>) -> CallToolResult {
        let host = self.host().clone();
        let r = (|| {
            host.actor(&a.session_id)?;
            let v = super::read(&host, Some(&a.session_id), &a.app, &a.paths.unwrap_or_default())?;
            Ok(serde_json::to_string_pretty(&v)?)
        })();
        self.finish(Some(&a.session_id), r)
    }

    #[tool(
        description = "Console errors, warnings and uncaught exceptions of an app's current version: from a headless load (run on demand) and from previews people are playing on the canvas. Use it after changes and when the user reports a bug.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = false, open_world_hint = false)
    )]
    async fn app_logs(&self, Parameters(a): Parameters<AppLogsArgs>) -> CallToolResult {
        let host = self.host().clone();
        let r = async {
            host.actor(&a.session_id)?;
            let v = super::logs(&host, Some(&a.session_id), &a.app, a.rerun.unwrap_or(false), a.all.unwrap_or(false)).await?;
            Ok(serde_json::to_string_pretty(&v)?)
        }
        .await;
        self.finish(Some(&a.session_id), r)
    }
}
