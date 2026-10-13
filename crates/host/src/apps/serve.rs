//! The app server: a separate loopback listener for app folders,
//! `http://127.0.0.1:<port>/<project>/<app>/<path>`.
//!
//! No token: the origin holds nothing (no MCP, no assets), and app pages run sandboxed with an
//! opaque origin on the canvas. Every response is `no-store` and CORS `*` (an opaque-origin
//! iframe loads ES modules cross-origin). HTML gets the guard script injected first.

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Path as UrlPath, Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::Value as Json;

use crate::workspace::Workspace;

/// Injected as the first script of every HTML page an app serves.
pub const GUARD: &str = r#"<script>(()=>{if(window.__falos)return;
var F=window.__falos={muted:true,ctx:new Set(),media:new Set()};
var check=/[?&]__falos=check/.test(location.search);
var embedded=false;try{embedded=window.parent!==window}catch(e){embedded=true}
function post(m){if(!embedded)return;try{m.falos=1;parent.postMessage(m,'*')}catch(e){}}
var base=location.pathname.split('/').slice(0,3).join('/')+'/__falos/log',q=[],t=0;
function str(a){try{if(a instanceof Error)return a.stack||String(a);if(typeof a==='object')return JSON.stringify(a);return String(a)}catch(e){return String(a)}}
function log(level,args){if(check)return;var text=Array.prototype.map.call(args,str).join(' ').slice(0,2000);q.push({level:level,text:text});post({type:'log',level:level});
if(!t)t=setTimeout(function(){t=0;var b=q.splice(0,50);if(b.length)try{navigator.sendBeacon(base,JSON.stringify(b))}catch(e){}},250)}
['error','warn'].forEach(function(k){var o=console[k];console[k]=function(){log(k==='warn'?'warning':'error',arguments);return o.apply(console,arguments)}});
addEventListener('error',function(e){if(e.target&&e.target!==window&&(e.target.src||e.target.href)){log('error',['failed to load '+(e.target.src||e.target.href)]);return}
log('exception',[(e.error&&e.error.stack)||e.message+(e.filename?' ('+e.filename.split('/').pop()+':'+e.lineno+')':'')])},true);
addEventListener('unhandledrejection',function(e){log('exception',['unhandled rejection: '+str(e.reason)])});
try{window.localStorage.getItem('x')}catch(e){var mem=function(){var d={};return{getItem:function(k){return k in d?d[k]:null},setItem:function(k,v){d[k]=String(v)},removeItem:function(k){delete d[k]},clear:function(){d={}},key:function(i){return Object.keys(d)[i]||null},get length(){return Object.keys(d).length}}};
try{Object.defineProperty(window,'localStorage',{value:mem(),configurable:true});Object.defineProperty(window,'sessionStorage',{value:mem(),configurable:true})}catch(e2){}}
var AC=window.AudioContext||window.webkitAudioContext;
if(AC){var res=AC.prototype.resume;AC.prototype.resume=function(){F.ctx.add(this);return F.muted?Promise.resolve():res.call(this)};
var W=function(a,b){var c=new AC(a,b);F.ctx.add(c);if(F.muted)try{c.suspend()}catch(e){}return c};W.prototype=AC.prototype;
window.AudioContext=W;if(window.webkitAudioContext)window.webkitAudioContext=W}
var play=HTMLMediaElement.prototype.play;HTMLMediaElement.prototype.play=function(){F.media.add(this);if(F.muted)this.muted=true;return play.apply(this,arguments)};
function apply(){F.ctx.forEach(function(c){try{F.muted?c.suspend():res.call(c)}catch(e){}});F.media.forEach(function(m){m.muted=F.muted})}
addEventListener('message',function(e){var d=e.data;if(!d||e.source!==window.parent)return;if(d.falos==='mute'){F.muted=!!d.muted;apply()}else if(d.falos==='focus'){try{window.focus()}catch(e){}}});
addEventListener('keydown',function(e){if(e.key==='Escape')post({type:'esc'})},true);
addEventListener('pointerdown',function(){post({type:'pointer'})},true);
addEventListener('load',function(){post({type:'ready'})});
})();</script>"#;

/// Puts [`GUARD`] right after `<head…>` (else after `<html…>`, else first).
pub fn inject(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let at = ["<head", "<html"].iter().find_map(|tag| {
        let i = lower.find(tag)?;
        // Not `<header`.
        let next = lower.as_bytes().get(i + tag.len()).copied();
        if !matches!(next, Some(b'>' | b' ' | b'\n' | b'\t' | b'\r')) {
            return None;
        }
        Some(i + lower[i..].find('>')? + 1)
    });
    match at {
        Some(i) => format!("{}{GUARD}{}", &html[..i], &html[i..]),
        None => format!("{GUARD}{html}"),
    }
}

pub fn mime(path: &str) -> &'static str {
    let ext = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" | "cjs" | "jsx" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "wasm" => "application/wasm",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "m4a" => "audio/mp4",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "glb" => "model/gltf-binary",
        "gltf" => "model/gltf+json",
        "txt" | "md" | "csv" => "text/plain; charset=utf-8",
        "xml" => "application/xml",
        _ => "application/octet-stream",
    }
}

fn headers(resp: &mut Response) {
    let h = resp.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    h.insert("cross-origin-resource-policy", HeaderValue::from_static("cross-origin"));
    h.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
}

async fn loopback(req: Request, next: Next) -> Response {
    let ok = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|h| {
            let host = h.rsplit_once(':').map_or(h, |(a, _)| a);
            matches!(host, "127.0.0.1" | "localhost" | "[::1]")
        });
    if !ok {
        return (StatusCode::FORBIDDEN, "loopback only").into_response();
    }
    let mut resp = next.run(req).await;
    headers(&mut resp);
    resp
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "not found").into_response()
}

async fn file(method: Method, space: &Workspace, project: &str, app: &str, path: &str) -> Response {
    if !crate::workspace::valid_project_id(project) || !super::valid_app_id(app) {
        return not_found();
    }
    let rel = if path.is_empty() || path.ends_with('/') { format!("{path}index.html") } else { path.to_string() };
    let Ok(rel) = super::safe_rel(&rel) else { return not_found() };
    let Ok(host) = space.get(project) else { return not_found() };
    let mut p = super::app_dir(&host, app).join(&rel);
    if p.is_dir() {
        p = p.join("index.html");
    }
    let Ok(bytes) = tokio::fs::read(&p).await else { return not_found() };
    let name = p.to_string_lossy().into_owned();
    let ct = mime(&name);
    let body = if ct.starts_with("text/html") {
        Body::from(inject(&String::from_utf8_lossy(&bytes)))
    } else {
        Body::from(bytes)
    };
    let mut resp = Response::new(if method == Method::HEAD { Body::empty() } else { body });
    resp.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(ct));
    resp
}

async fn get_root(UrlPath((project, app)): UrlPath<(String, String)>) -> Response {
    // `/<project>/<app>` → `/<project>/<app>/` so relative imports resolve inside the app.
    (StatusCode::TEMPORARY_REDIRECT, [(header::LOCATION, format!("/{project}/{app}/"))]).into_response()
}

async fn get_index(method: Method, State(space): State<Arc<Workspace>>, UrlPath((project, app)): UrlPath<(String, String)>) -> Response {
    file(method, &space, &project, &app, "").await
}

async fn get_file(
    method: Method,
    State(space): State<Arc<Workspace>>,
    UrlPath((project, app, path)): UrlPath<(String, String, String)>,
) -> Response {
    file(method, &space, &project, &app, &path).await
}

/// Console errors / exceptions from a running preview (`navigator.sendBeacon`).
async fn post_log(State(space): State<Arc<Workspace>>, UrlPath((project, app)): UrlPath<(String, String)>, body: Bytes) -> Response {
    if body.len() > 256 * 1024 || !super::valid_app_id(&app) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Ok(host) = space.get(&project) else { return not_found() };
    let Ok(Json::Array(items)) = serde_json::from_slice::<Json>(&body) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if super::app_dir(&host, &app).is_dir() {
        super::record_preview_logs(&host, &app, &items);
    }
    StatusCode::NO_CONTENT.into_response()
}

/// The file list (the canvas's read-only `[src]` view): `{version, files:[{path, bytes}]}`.
async fn list(State(space): State<Arc<Workspace>>, UrlPath((project, app)): UrlPath<(String, String)>) -> Response {
    if !super::valid_app_id(&app) {
        return not_found();
    }
    let Ok(host) = space.get(&project) else { return not_found() };
    let dir = super::app_dir(&host, &app);
    if !dir.is_dir() {
        return not_found();
    }
    let files: Vec<Json> = super::files_of(&dir).into_iter().map(|(p, b, _)| serde_json::json!({"path": p, "bytes": b})).collect();
    axum::Json(serde_json::json!({"app": app, "version": host.apps.version(&app), "files": files})).into_response()
}

pub fn router(space: Arc<Workspace>) -> Router {
    Router::new()
        .route("/{project}/{app}/__falos/files", get(list))
        .route("/{project}/{app}", get(get_root))
        .route("/{project}/{app}/", get(get_index).head(get_index))
        .route("/{project}/{app}/__falos/log", post(post_log))
        .route("/{project}/{app}/{*path}", get(get_file).head(get_file))
        .with_state(space)
        .layer(middleware::from_fn(loopback))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_goes_first() {
        let h = inject("<!doctype html><html><head><script src=x.js></script></head></html>");
        assert!(h.find("__falos").unwrap() < h.find("x.js").unwrap());
        assert!(h.starts_with("<!doctype html><html><head><script>"));
        let h = inject("<header>hi</header>");
        assert!(h.starts_with("<script>"));
        assert_eq!(mime("a/b.mjs"), "text/javascript; charset=utf-8");
    }
}
