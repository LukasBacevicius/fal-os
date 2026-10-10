//! "Sign in with fal": OAuth 2.1 (PKCE, public client) against fal's WorkOS AuthKit, the same
//! authorization server the fal MCP relay uses. No API keys: the host holds a refreshable
//! user token in `~/.fal-os/fal-auth.json` (0600) and sends it as `Authorization: Bearer`.
//!
//! Client registration uses dynamic client registration for now; switch to a Client ID
//! Metadata Document once we host one (AuthKit supports both).

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::server::home_dir;

pub const ISSUER: &str = "https://authkit.fal.ai";
/// Fixed loopback port for the OAuth redirect (registered with the client).
pub const CALLBACK_PORT: u16 = 7319;
const SCOPES: &str = "openid profile email offline_access";

#[derive(Debug, Deserialize)]
struct Metadata {
    authorization_endpoint: String,
    token_endpoint: String,
    registration_endpoint: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct ClientReg {
    issuer: String,
    client_id: String,
    redirect_uri: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Unix seconds.
    pub expires_at: u64,
    pub client_id: String,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn client_path() -> PathBuf {
    home_dir().join("oauth-client.json")
}

fn tokens_path() -> PathBuf {
    home_dir().join("fal-auth.json")
}

fn write_private(path: &PathBuf, json: &impl Serialize) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::create_dir_all(home_dir())?;
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
    f.write_all(&serde_json::to_vec_pretty(json)?)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

async fn metadata(http: &reqwest::Client) -> Result<Metadata> {
    Ok(http
        .get(format!("{ISSUER}/.well-known/oauth-authorization-server"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}

async fn client(http: &reqwest::Client, md: &Metadata) -> Result<ClientReg> {
    let redirect_uri = format!("http://127.0.0.1:{CALLBACK_PORT}/callback");
    if let Ok(bytes) = std::fs::read(client_path()) {
        if let Ok(c) = serde_json::from_slice::<ClientReg>(&bytes) {
            if c.issuer == ISSUER && c.redirect_uri == redirect_uri {
                return Ok(c);
            }
        }
    }
    let res: Json = http
        .post(&md.registration_endpoint)
        .json(&serde_json::json!({
            "client_name": "fal-os",
            "redirect_uris": [redirect_uri],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
            "application_type": "native",
        }))
        .send()
        .await?
        .error_for_status()
        .context("client registration failed")?
        .json()
        .await?;
    let reg = ClientReg {
        issuer: ISSUER.into(),
        client_id: res["client_id"].as_str().context("no client_id in registration response")?.into(),
        redirect_uri,
    };
    write_private(&client_path(), &reg)?;
    Ok(reg)
}

fn random_b64(n: usize) -> String {
    let mut b = vec![0u8; n];
    rand::rng().fill_bytes(&mut b);
    URL_SAFE_NO_PAD.encode(b)
}

/// Waits for one GET /callback on the loopback port and returns its query parameters.
async fn wait_for_callback(listener: tokio::net::TcpListener) -> Result<Vec<(String, String)>> {
    loop {
        let (mut sock, _) = listener.accept().await?;
        let mut buf = vec![0u8; 8192];
        let n = sock.read(&mut buf).await?;
        let req = String::from_utf8_lossy(&buf[..n]);
        let path = req.split_whitespace().nth(1).unwrap_or("");
        if !path.starts_with("/callback") {
            let _ = sock.write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\n\r\n").await;
            continue;
        }
        let url = reqwest::Url::parse(&format!("http://127.0.0.1{path}"))?;
        let params: Vec<(String, String)> = url.query_pairs().map(|(k, v)| (k.into(), v.into())).collect();
        let ok = params.iter().any(|(k, _)| k == "code");
        let body = if ok {
            "<html><body style=\"font-family:system-ui;background:#111;color:#eee;padding:48px\"><h2>Signed in to fal-os</h2><p>You can close this tab.</p></body></html>"
        } else {
            "<html><body style=\"font-family:system-ui;padding:48px\"><h2>Sign-in failed</h2><p>Return to fal-os and try again.</p></body></html>"
        };
        let resp = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = sock.write_all(resp.as_bytes()).await;
        return Ok(params);
    }
}

/// Interactive browser sign-in. `open_url` is called with the authorization URL.
pub async fn login(open_url: impl FnOnce(&str)) -> Result<Tokens> {
    let http = reqwest::Client::new();
    let md = metadata(&http).await?;
    let reg = client(&http, &md).await?;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", CALLBACK_PORT))
        .await
        .with_context(|| format!("port {CALLBACK_PORT} is busy; close the other sign-in and retry"))?;

    let verifier = random_b64(48);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = random_b64(16);
    let mut url = reqwest::Url::parse(&md.authorization_endpoint)?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &reg.client_id)
        .append_pair("redirect_uri", &reg.redirect_uri)
        .append_pair("scope", SCOPES)
        .append_pair("state", &state)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256");
    open_url(url.as_str());

    let params = tokio::time::timeout(std::time::Duration::from_secs(600), wait_for_callback(listener))
        .await
        .map_err(|_| anyhow!("timed out waiting for the browser sign-in"))??;
    let get = |k: &str| params.iter().find(|(pk, _)| pk == k).map(|(_, v)| v.clone());
    if let Some(err) = get("error") {
        bail!("sign-in failed: {err} {}", get("error_description").unwrap_or_default());
    }
    if get("state").as_deref() != Some(state.as_str()) {
        bail!("sign-in failed: state mismatch");
    }
    let code = get("code").context("sign-in failed: no authorization code")?;
    let tokens = token_request(
        &http,
        &md.token_endpoint,
        &reg.client_id,
        &[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("redirect_uri", &reg.redirect_uri),
            ("client_id", &reg.client_id),
            ("code_verifier", &verifier),
        ],
        None,
    )
    .await?;
    write_private(&tokens_path(), &tokens)?;
    Ok(tokens)
}

async fn token_request(
    http: &reqwest::Client,
    endpoint: &str,
    client_id: &str,
    form: &[(&str, &str)],
    old_refresh: Option<&str>,
) -> Result<Tokens> {
    let body = form
        .iter()
        .map(|(k, v)| format!("{}={}", k, urlencode(v)))
        .collect::<Vec<_>>()
        .join("&");
    let res = http
        .post(endpoint)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await?;
    let status = res.status();
    let json: Json = res.json().await.unwrap_or(Json::Null);
    if !status.is_success() {
        bail!("token request failed ({status}): {}", json["error_description"].as_str().or(json["error"].as_str()).unwrap_or("unknown error"));
    }
    Ok(Tokens {
        access_token: json["access_token"].as_str().context("no access_token")?.into(),
        refresh_token: json["refresh_token"].as_str().map(String::from).or(old_refresh.map(String::from)),
        expires_at: now() + json["expires_in"].as_u64().unwrap_or(300),
        client_id: client_id.into(),
    })
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn load() -> Option<Tokens> {
    serde_json::from_slice(&std::fs::read(tokens_path()).ok()?).ok()
}

pub fn logout() -> Result<()> {
    match std::fs::remove_file(tokens_path()) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// A valid access token, refreshing it when it expires within a minute.
pub async fn access_token(http: &reqwest::Client) -> Result<String> {
    let t = load().context("not signed in to fal: run `falos-host login`")?;
    if t.expires_at > now() + 60 {
        return Ok(t.access_token);
    }
    let refresh = t.refresh_token.clone().context("fal session expired: run `falos-host login`")?;
    let md = metadata(http).await?;
    let fresh = token_request(
        http,
        &md.token_endpoint,
        &t.client_id,
        &[("grant_type", "refresh_token"), ("refresh_token", &refresh), ("client_id", &t.client_id)],
        Some(&refresh),
    )
    .await
    .context("refreshing the fal session failed: run `falos-host login`")?;
    write_private(&tokens_path(), &fresh)?;
    Ok(fresh.access_token)
}

/// Decoded (unverified) JWT claims, for display.
pub fn claims(token: &str) -> Option<Json> {
    let payload = token.split('.').nth(1)?;
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?).ok()
}
