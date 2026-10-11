//! fal-backed ops (remove_bg, upscale, outpaint, inpaint) through the host's fal queue path,
//! plus fal storage uploads for local media (`mediaInputs`). Models verified against the
//! fal OpenAPI schema endpoint on 2026-10-11:
//!
//! | op | model | input |
//! |---|---|---|
//! | remove_bg | `fal-ai/birefnet/v2` | `image_url`, `model`, `output_format: png` |
//! | upscale (image) | `fal-ai/seedvr/upscale/image` | `image_url`, `upscale_factor`, `output_format: png` |
//! | upscale (video) | `fal-ai/seedvr/upscale/video` | `video_url`, `upscale_factor` |
//! | reframe mode outpaint | `fal-ai/bria/expand` | `image_url`, `canvas_size`, `original_image_size`, `original_image_location`, `prompt` |
//! | inpaint | `fal-ai/flux-pro/v1/fill` | `image_url`, `mask_url`, `prompt` |

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use falos_doc::Actor;
use serde_json::{Value as Json, json};

use crate::fal::{self, Backend, Job};
use crate::host::Host;

pub const REMOVE_BG: &str = "fal-ai/birefnet/v2";
pub const UPSCALE_IMAGE: &str = "fal-ai/seedvr/upscale/image";
pub const UPSCALE_VIDEO: &str = "fal-ai/seedvr/upscale/video";
pub const EXPAND: &str = "fal-ai/bria/expand";
pub const FILL: &str = "fal-ai/flux-pro/v1/fill";

/// USD per unit (fal pricing API, 2026-10-11) and the unit.
pub fn price(model: &str) -> Option<(f64, &'static str)> {
    Some(match model {
        REMOVE_BG => (0.0008, "compute seconds"),
        UPSCALE_IMAGE | UPSCALE_VIDEO => (0.001, "megapixels"),
        EXPAND => (0.04, "generations"),
        FILL => (0.05, "megapixels"),
        _ => return None,
    })
}

/// Estimated cost of one run: `units` megapixels / seconds / generations.
pub fn estimate(model: &str, megapixels: f64, seconds: f64) -> Option<f64> {
    let (p, unit) = price(model)?;
    let units = match unit {
        "megapixels" => megapixels.max(0.01),
        "compute seconds" => seconds.max(1.0),
        _ => 1.0,
    };
    Some((p * units * 10000.0).round() / 10000.0)
}

/// Public URL for an asset's bytes: its recorded `uri`, else a one-time fal storage upload
/// (recorded on the asset so it's uploaded once).
pub async fn public_url(host: &Arc<Host>, actor: &Actor, canvas: &str, asset: &str, file: &Path) -> Result<String> {
    if let Some(u) = host
        .read(canvas, |c| c.asset(asset).and_then(|a| a.uri))?
        .filter(|u| u.starts_with("https://"))
    {
        return Ok(u);
    }
    let url = upload(host, file).await?;
    crate::chat::set_asset_uri(host, actor, canvas, asset, &url)?;
    Ok(url)
}

/// Upload a local file to fal storage; returns its CDN URL.
pub async fn upload(host: &Host, file: &Path) -> Result<String> {
    let bytes = tokio::fs::read(file).await?;
    let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file.bin".into());
    let ext = file.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let mime = crate::assets::mime_of_ext(&ext);
    let (upload_url, file_url) = match fal::backend(host)? {
        Backend::Key(key) => {
            let base = std::env::var("FALOS_FAL_REST_URL").unwrap_or_else(|_| "https://rest.fal.ai".into());
            let r: Json = host
                .http
                .post(format!("{base}/storage/upload/initiate?storage_type=fal-cdn-v3"))
                .header("Authorization", format!("Key {key}"))
                .json(&json!({"content_type": mime, "file_name": name}))
                .send()
                .await?
                .error_for_status()
                .context("fal storage initiate")?
                .json()
                .await?;
            (r["upload_url"].as_str().map(String::from), r["file_url"].as_str().map(String::from))
        }
        Backend::Relay(r) => {
            let v = r
                .call("upload_file", json!({"prepare_upload": true, "file_name": name, "file_size": bytes.len()}))
                .await?;
            (v["upload_url"].as_str().map(String::from), v["file_url"].as_str().or(v["url"].as_str()).map(String::from))
        }
    };
    let (Some(up), Some(url)) = (upload_url, file_url) else {
        bail!("fal storage returned no upload url");
    };
    host.http
        .put(&up)
        .header("content-type", mime)
        .body(bytes)
        .send()
        .await?
        .error_for_status()
        .context("fal storage upload")?;
    Ok(url)
}

pub struct FalRun {
    pub result: Json,
    pub request_id: String,
    pub seconds: f64,
}

/// Run a fal model to completion (queue submit → poll → result).
pub async fn run_model(host: &Arc<Host>, canvas: &str, node: &str, requester: &str, model: &str, input: Json, progress: &(dyn Fn(f64) + Send + Sync)) -> Result<FalRun> {
    let backend = fal::backend(host)?;
    fal::check_model(model)?;
    let job = Job {
        host: host.clone(),
        canvas: canvas.into(),
        node: node.into(),
        model: model.into(),
        input,
        backend,
        requester: requester.into(),
        parents: vec![],
    };
    let started = tokio::time::Instant::now();
    let (id, submit) = job.submit().await?;
    loop {
        if started.elapsed() > Duration::from_secs(30 * 60) {
            bail!("fal job timed out");
        }
        let (status, _, err) = job.status(&id, &submit).await?;
        match status.as_str() {
            "COMPLETED" | "OK" | "SUCCEEDED" => {
                if let Some(e) = err {
                    bail!("{e}");
                }
                break;
            }
            "FAILED" | "ERROR" | "CANCELLED" => bail!("{}", err.unwrap_or(status)),
            "IN_PROGRESS" | "RUNNING" => progress(0.5),
            _ => {}
        }
        tokio::time::sleep(Duration::from_millis(800)).await;
    }
    let result = job.result(&id, &submit).await?;
    Ok(FalRun { result, request_id: id, seconds: started.elapsed().as_secs_f64() })
}

/// Download the first media output of a fal result into `dir`.
pub async fn download_first(host: &Host, result: &Json, dir: &Path) -> Result<PathBuf> {
    let m = fal::find_media(result).ok_or_else(|| anyhow!("no media in fal result"))?;
    let bytes = host.http.get(&m.url).send().await?.error_for_status()?.bytes().await?;
    let out = dir.join(format!("fal.{}", m.ext));
    tokio::fs::write(&out, &bytes).await?;
    Ok(out)
}
