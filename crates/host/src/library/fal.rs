//! fal.ai library client (Platform API `/v1/assets…`, storage upload, Agent projects).
//! Base URLs are overridable for tests: `FALOS_FAL_API`, `FALOS_FAL_REST`, `FALOS_FAL_AGENT`.

use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value as Json, json};

#[derive(Clone)]
pub struct FalApi {
    http: reqwest::Client,
    key: String,
    pub api: String,
    pub rest: String,
    pub agent: String,
}

/// A failed fal call: `retry` = transient (network, 429, 5xx); else the write is dropped.
#[derive(Debug)]
pub struct FalError {
    pub status: Option<u16>,
    pub message: String,
}

impl std::fmt::Display for FalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.status {
            Some(s) => write!(f, "fal {s}: {}", self.message),
            None => write!(f, "fal: {}", self.message),
        }
    }
}
impl std::error::Error for FalError {}

impl FalError {
    pub fn transient(&self) -> bool {
        self.status.is_none_or(|s| s == 429 || s >= 500)
    }
}

/// Whether an error from a fal call is worth retrying.
pub fn transient(e: &anyhow::Error) -> bool {
    e.downcast_ref::<FalError>().is_none_or(FalError::transient)
}

impl FalApi {
    /// A client against explicit base URLs (tests).
    pub fn with_base(key: String, api: &str, rest: &str, agent: &str) -> Self {
        Self { http: reqwest::Client::new(), key, api: api.into(), rest: rest.into(), agent: agent.into() }
    }

    pub fn new(http: reqwest::Client, key: String) -> Self {
        let env = |k: &str, d: &str| std::env::var(k).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| d.to_string());
        Self {
            http,
            key,
            api: env("FALOS_FAL_API", "https://api.fal.ai/v1"),
            rest: env("FALOS_FAL_REST", "https://rest.alpha.fal.ai"),
            agent: env("FALOS_FAL_AGENT", "https://fal.ai/api/agent-v2/sdk"),
        }
    }

    async fn call(&self, method: reqwest::Method, url: &str, body: Option<Json>, idem: Option<&str>) -> Result<Json> {
        let mut req = self.http.request(method, url).header("Authorization", format!("Key {}", self.key)).timeout(Duration::from_secs(60));
        if let Some(k) = idem {
            req = req.header("Idempotency-Key", k);
        }
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await.map_err(|e| FalError { status: None, message: e.to_string() })?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            let msg = serde_json::from_str::<Json>(&text).ok().and_then(|v| v["error"]["message"].as_str().or(v["detail"].as_str()).map(str::to_string)).unwrap_or(text);
            return Err(FalError { status: Some(status.as_u16()), message: msg.chars().take(300).collect() }.into());
        }
        Ok(if text.trim().is_empty() { Json::Null } else { serde_json::from_str(&text).unwrap_or(Json::Null) })
    }

    /// `GET /assets` newest first → (assets, next cursor).
    pub async fn list(&self, params: &[(&str, String)]) -> Result<(Vec<Json>, Option<String>)> {
        let url = reqwest::Url::parse_with_params(&format!("{}/assets", self.api), params)?;
        let v = self.call(reqwest::Method::GET, url.as_str(), None, None).await?;
        let items = v["assets"].as_array().cloned().unwrap_or_default();
        let next = v["next_cursor"].as_str().filter(|_| v["has_more"].as_bool() != Some(false)).map(str::to_string);
        Ok((items, next))
    }

    pub async fn collections(&self) -> Result<Vec<Json>> {
        let mut out = vec![];
        let mut cursor: Option<String> = None;
        for _ in 0..20 {
            let mut p = vec![("limit", "100".to_string())];
            if let Some(c) = &cursor {
                p.push(("cursor", c.clone()));
            }
            let url = reqwest::Url::parse_with_params(&format!("{}/assets/collections", self.api), &p)?;
            let v = self.call(reqwest::Method::GET, url.as_str(), None, None).await?;
            out.extend(v["collections"].as_array().cloned().unwrap_or_default());
            match v["next_cursor"].as_str() {
                Some(c) if v["has_more"].as_bool() != Some(false) => cursor = Some(c.into()),
                _ => break,
            }
        }
        Ok(out)
    }

    pub async fn create_collection(&self, name: &str, parent: Option<&str>) -> Result<String> {
        let v = self
            .call(reqwest::Method::POST, &format!("{}/assets/collections", self.api), Some(json!({"name": name, "parent_collection_id": parent})), Some(&format!("falos-coll-{}-{}", parent.unwrap_or("root"), name)))
            .await?;
        v["collection"]["id"].as_str().or(v["id"].as_str()).map(str::to_string).ok_or_else(|| anyhow!("fal: no collection id in {v}"))
    }

    pub async fn add_to_collection(&self, collection: &str, asset: &str) -> Result<()> {
        self.call(reqwest::Method::POST, &format!("{}/assets/collections/{collection}/assets", self.api), Some(json!({"asset_id": asset})), None).await?;
        Ok(())
    }

    pub async fn favorite(&self, asset: &str, value: bool) -> Result<()> {
        let path = if value { "favorite" } else { "unfavorite" };
        self.call(reqwest::Method::POST, &format!("{}/assets/{path}", self.api), Some(json!({"asset_id": asset})), None).await?;
        Ok(())
    }

    pub async fn tags(&self) -> Result<Vec<Json>> {
        let v = self.call(reqwest::Method::GET, &format!("{}/assets/tags", self.api), None, None).await?;
        Ok(v["tags"].as_array().cloned().unwrap_or_default())
    }

    pub async fn create_tag(&self, name: &str) -> Result<String> {
        let v = self.call(reqwest::Method::POST, &format!("{}/assets/tags", self.api), Some(json!({"name": name})), None).await?;
        v["tag"]["id"].as_str().or(v["id"].as_str()).map(str::to_string).ok_or_else(|| anyhow!("fal: no tag id in {v}"))
    }

    pub async fn set_tags(&self, asset: &str, tag_ids: &[String]) -> Result<()> {
        self.call(reqwest::Method::PUT, &format!("{}/assets/tags", self.api), Some(json!({"asset_id": asset, "tag_ids": tag_ids})), None).await?;
        Ok(())
    }

    /// fal storage (CDN v3, no expiry) → the file URL. Files above 90 MB need multipart
    /// uploads, which aren't implemented.
    pub async fn upload_file(&self, bytes: Vec<u8>, mime: &str, name: &str) -> Result<String> {
        if bytes.len() > 90 * 1024 * 1024 {
            bail!("files over 90 MB can't be uploaded to fal yet");
        }
        let v = self
            .call(reqwest::Method::POST, &format!("{}/storage/upload/initiate?storage_type=fal-cdn-v3", self.rest), Some(json!({"content_type": mime, "file_name": name})), None)
            .await?;
        let (up, file) = (v["upload_url"].as_str().context("no upload_url")?, v["file_url"].as_str().context("no file_url")?);
        let r = self
            .http
            .put(up)
            .header("Content-Type", mime)
            .body(bytes)
            .timeout(Duration::from_secs(600))
            .send()
            .await
            .map_err(|e| FalError { status: None, message: e.to_string() })?;
        if !r.status().is_success() {
            return Err(FalError { status: Some(r.status().as_u16()), message: "storage upload failed".into() }.into());
        }
        Ok(file.to_string())
    }

    /// `POST /assets/uploads` (Idempotency-Key = sha) → the asset.
    pub async fn register_upload(&self, url: &str, kind: &str, prompt: Option<&str>, collection: Option<&str>, sha: &str) -> Result<Json> {
        let mut body = json!({"url": url, "type": kind, "collection_id": collection});
        if let Some(p) = prompt.map(|p| p.chars().take(2000).collect::<String>()).filter(|p| !p.trim().is_empty()) {
            body["prompt"] = json!(p);
        }
        let v = self.call(reqwest::Method::POST, &format!("{}/assets/uploads", self.api), Some(body), Some(sha)).await?;
        Ok(v.get("asset").cloned().unwrap_or(v))
    }

    pub async fn attach_collection(&self, agent_project: &str, collection: &str) -> Result<()> {
        self.call(reqwest::Method::PUT, &format!("{}/agent/projects/{agent_project}/collections/{collection}", self.agent), None, None).await?;
        Ok(())
    }

    /// Library record delete (Agent SDK; the CDN file stays). Used by tests to clean up.
    pub async fn delete_asset(&self, asset: &str) -> Result<()> {
        self.call(reqwest::Method::DELETE, &format!("{}/agent/library/assets/{asset}", self.agent), None, None).await?;
        Ok(())
    }

    pub async fn delete_collection(&self, id: &str) -> Result<()> {
        self.call(reqwest::Method::DELETE, &format!("{}/assets/collections/{id}", self.api), None, None).await?;
        Ok(())
    }

    pub async fn download(&self, url: &str, max: u64) -> Result<Vec<u8>> {
        let mut r = self.http.get(url).timeout(Duration::from_secs(300)).send().await?;
        if !r.status().is_success() {
            bail!("download {url}: {}", r.status());
        }
        let mut out = vec![];
        while let Some(c) = r.chunk().await? {
            out.extend_from_slice(&c);
            if out.len() as u64 > max {
                bail!("download too large");
            }
        }
        Ok(out)
    }
}

/// fal `type` for a mime.
pub fn fal_type(kind: &str) -> &'static str {
    match kind {
        "video" => "video",
        "audio" => "audio",
        "model3d" => "3d",
        _ => "image",
    }
}
