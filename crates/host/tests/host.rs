//! End-to-end: real server on an ephemeral port, rmcp client over Streamable HTTP, and
//! the WebSocket sync channel.

use std::sync::Arc;
use std::time::Duration;

use falos_doc::{Actor, Canvas, Command, NodeKind, Props};
use falos_host::context::NoRenderer;
use falos_host::{Host, HostOptions, server};
use futures::{SinkExt, StreamExt};
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::{RoleClient, ServiceExt};
use serde_json::{Value as Json, json};
use tokio_tungstenite::tungstenite::Message;

const TOKEN: &str = "test-token-0123456789abcdef0123456789abcdef";

struct Env {
    _dir: tempfile::TempDir,
    host: Arc<Host>,
    port: u16,
    _running: server::Running,
}

async fn start(fal_queue_url: &str, fal_key: Option<&str>) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let host = Host::open(
        &dir.path().join("proj"),
        HostOptions {
            fal_queue_url: fal_queue_url.into(),
            fal_key: fal_key.map(String::from),
            fal_session: false,
            renderer: Box::new(NoRenderer),
            home: Some(dir.path().join("home")),
        },
    )
    .unwrap();
    let running = server::serve(host.clone(), 0, TOKEN).await.unwrap();
    Env {
        _dir: dir,
        host,
        port: running.addr.port(),
        _running: running,
    }
}

type Client = RunningService<RoleClient, ()>;

async fn client(port: u16) -> Client {
    let t = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(server::mcp_url(port)).auth_header(TOKEN),
    );
    ().serve(t).await.unwrap()
}

async fn call_raw(c: &Client, tool: &'static str, args: Json) -> CallToolResult {
    c.call_tool(CallToolRequestParams::new(tool).with_arguments(args.as_object().unwrap().clone()))
        .await
        .unwrap()
}

fn text(r: &CallToolResult) -> String {
    r.content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Call a tool that must succeed; returns its text.
async fn call(c: &Client, tool: &'static str, args: Json) -> String {
    let r = call_raw(c, tool, args).await;
    let t = text(&r);
    assert_ne!(r.is_error, Some(true), "{tool} failed: {t}");
    t
}

/// Parse the JSON part of a tool result (before any appended notices).
fn js(t: &str) -> Json {
    let end = t.find("\n\nnotices").unwrap_or(t.len());
    serde_json::from_str(&t[..end]).unwrap_or_else(|e| panic!("{e}: {t}"))
}

fn version_of(read: &str) -> String {
    read.split(" · ")
        .nth(1)
        .unwrap()
        .trim_start_matches("version ")
        .to_string()
}

#[tokio::test]
async fn mcp_end_to_end() {
    let env = start("http://127.0.0.1:1", None).await;
    let claude = client(env.port).await;

    let tools = claude.list_all_tools().await.unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    for n in [
        "session",
        "read",
        "edit",
        "undo",
        "revert_since",
        "list_annotations",
        "get_annotation",
        "reply",
        "annotate",
        "claim",
        "release",
        "set_status",
        "generate",
        "get_knowledge",
    ] {
        assert!(names.contains(&n), "missing tool {n}");
    }
    let read_tool = tools.iter().find(|t| t.name == "read").unwrap();
    assert_eq!(
        read_tool.annotations.as_ref().unwrap().read_only_hint,
        Some(true)
    );

    // No silent fallback: a missing session is an error.
    let r = call_raw(
        &claude,
        "read",
        json!({"sessionId": "nope", "canvas": "main"}),
    )
    .await;
    assert_eq!(r.is_error, Some(true));
    assert!(text(&r).contains("call the `session` tool"));

    let s = js(&call(
        &claude,
        "session",
        json!({"agentName": "Claude Code", "canvas": "main"}),
    )
    .await);
    let sid = s["sessionId"].as_str().unwrap().to_string();
    assert_eq!(s["actor"], "@claude-code");
    assert!(
        s["knowledge"]
            .as_array()
            .unwrap()
            .iter()
            .any(|k| k == "format")
    );

    // Create nodes.
    let e = js(&call(&claude, "edit", json!({
        "sessionId": sid, "canvas": "main", "requestId": "r1",
        "text": "frame \"Board\" s(1200,800)\n  image \"red car\" p(40,80) s(512,512) asset(sha:9f)",
    })).await);
    let created: Vec<String> = serde_json::from_value(e["created"].clone()).unwrap();
    assert_eq!(created.len(), 2);
    let (frame, img) = (created[0].clone(), created[1].clone());
    assert!(e["preview"].as_str().unwrap().contains("red car"));

    // Idempotent retry returns the first result and doesn't create again.
    let again = js(&call(
        &claude,
        "edit",
        json!({
            "sessionId": sid, "canvas": "main", "requestId": "r1",
            "text": "frame \"Board\" s(1200,800)",
        }),
    )
    .await);
    assert_eq!(again["created"], e["created"]);

    let read = call(&claude, "read", json!({"sessionId": sid, "canvas": "main"})).await;
    assert!(read.contains("<untrusted-canvas-data>") && read.contains("</untrusted-canvas-data>"));
    assert!(
        read.contains(&format!(
            "{img} image \"red car\" p(40,80) s(512,512) asset(sha:9f) by(@claude-code)"
        )),
        "{read}"
    );
    assert_eq!(read.matches(" frame ").count(), 1);
    let blurry = call(
        &claude,
        "read",
        json!({"sessionId": sid, "canvas": "main", "detail": "blurry", "depth": 0}),
    )
    .await;
    assert!(blurry.contains("more(1)"), "{blurry}");

    // Second agent; annotate assigned to it.
    let codex = client(env.port).await;
    let cs = js(&call(&codex, "session", json!({"agentName": "Codex"})).await);
    let csid = cs["sessionId"].as_str().unwrap().to_string();
    let a = js(&call(
        &claude,
        "annotate",
        json!({
            "sessionId": sid, "canvas": "main", "nodeId": img, "anchor": "box(0.1,0.2,0.4,0.3)",
            "text": "make the sky stormier", "assignee": "codex",
        }),
    )
    .await);
    let ann = a["annotationId"].as_str().unwrap().to_string();

    let list = call(
        &codex,
        "list_annotations",
        json!({
            "sessionId": csid, "canvas": "main", "status": "open", "assignee": "me",
        }),
    )
    .await;
    assert!(
        list.contains(&format!("! {ann} open on({img}) to(@codex)")),
        "{list}"
    );
    assert!(list.contains("<untrusted-canvas-data>"));
    assert!(
        list.contains("assigned to you"),
        "assignment notice missing: {list}"
    );

    let g = call(
        &codex,
        "get_annotation",
        json!({"sessionId": csid, "canvas": "main", "id": ann}),
    )
    .await;
    assert!(
        g.contains("anchor: box(0.1,0.2,0.4,0.3)")
            && g.contains("anchor in node pixels: box(")
            && g.contains("> @claude-code \"make the sky stormier\""),
        "{g}"
    );
    assert!(g.contains("red car"));

    call(
        &codex,
        "reply",
        json!({
            "sessionId": csid, "canvas": "main", "annotationId": ann,
            "text": "done, see v2", "status": "resolved", "outcome": [img],
        }),
    )
    .await;
    let list = call(
        &codex,
        "list_annotations",
        json!({"sessionId": csid, "canvas": "main", "status": "resolved"}),
    )
    .await;
    assert!(
        list.contains(&format!("! {ann} resolved")) && list.contains("done, see v2"),
        "{list}"
    );

    // Staleness: claude reads, codex moves the image, claude edits it.
    let v = version_of(&call(&claude, "read", json!({"sessionId": sid, "canvas": "main"})).await);
    call(
        &codex,
        "edit",
        json!({"sessionId": csid, "canvas": "main", "text": format!("{img} p(60,80)")}),
    )
    .await;
    let e = js(&call(&claude, "edit", json!({
        "sessionId": sid, "canvas": "main", "expectedVersion": v, "text": format!("{img} s(600,600)"),
    })).await);
    let stale = &e["stale"];
    assert!(
        stale["summary"]
            .as_str()
            .unwrap()
            .contains(&format!("{img} by @codex")),
        "{e}"
    );
    assert_eq!(stale["conflicts"], json!([img]));

    // Claims: codex claims the frame; claude is warned when it edits it; human revokes.
    let c = js(&call(
        &codex,
        "claim",
        json!({"sessionId": csid, "canvas": "main", "ids": [frame]}),
    )
    .await);
    assert_eq!(c["granted"], json!([frame]));
    let c2 = js(&call(
        &claude,
        "claim",
        json!({"sessionId": sid, "canvas": "main", "ids": [frame]}),
    )
    .await);
    assert_eq!(c2["refused"].as_array().unwrap().len(), 1);
    let e = js(&call(
        &claude,
        "edit",
        json!({"sessionId": sid, "canvas": "main", "text": format!("{frame} \"Board v2\"")}),
    )
    .await);
    assert!(
        e["warnings"].to_string().contains("claimed by @codex"),
        "{e}"
    );
    assert!(env.host.revoke_claim("main", &frame, "lukas").unwrap());
    let t = call(
        &codex,
        "set_status",
        json!({"sessionId": csid, "text": "painting"}),
    )
    .await;
    assert!(t.contains("revoked by @lukas"), "{t}");

    // Undo: only claude's last transaction.
    let n = js(&call(
        &claude,
        "edit",
        json!({"sessionId": sid, "canvas": "main", "text": "note \"scratch\" text(\"tmp\")"}),
    )
    .await);
    let note = n["created"][0].as_str().unwrap().to_string();
    let u = js(&call(&claude, "undo", json!({"sessionId": sid, "canvas": "main"})).await);
    assert_eq!(u["undone"], 1);
    let read = call(&claude, "read", json!({"sessionId": sid, "canvas": "main"})).await;
    assert!(
        !read.contains(&note) && read.contains(&img) && read.contains("p(60,80)"),
        "{read}"
    );

    // Knowledge + generate without a fal session or key.
    let k = call(
        &claude,
        "get_knowledge",
        json!({"keys": ["format", "collaboration"]}),
    )
    .await;
    assert!(k.contains("# Canvas text format") && k.contains("# Collaborating"));
    let r = call_raw(&claude, "generate", json!({
        "sessionId": sid, "canvas": "main", "model": "fal-ai/flux/dev", "input": {"prompt": "x"},
    })).await;
    assert_eq!(r.is_error, Some(true));
    assert!(text(&r).contains("not signed in to fal"));

    // Autosave reaches disk.
    env.host.flush().unwrap();
    let p = falos_doc::Project::load(env.host.dir()).unwrap();
    let saved = p.open_canvas(env.host.dir(), "main").unwrap();
    assert!(saved.node(&img).is_some());
}

#[tokio::test]
async fn rejects_missing_or_bad_token_and_foreign_hosts() {
    let env = start("http://127.0.0.1:1", None).await;
    let http = reqwest::Client::new();
    let url = server::mcp_url(env.port);
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#;
    let post = |auth: Option<&str>| {
        let mut r = http
            .post(&url)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .body(body);
        if let Some(a) = auth {
            r = r.header("authorization", a);
        }
        r
    };
    assert_eq!(post(None).send().await.unwrap().status(), 401);
    assert_eq!(
        post(Some("Bearer wrong")).send().await.unwrap().status(),
        401
    );
    assert!(
        post(Some(&format!("Bearer {TOKEN}")))
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    let evil = post(Some(&format!("Bearer {TOKEN}")))
        .header("origin", "https://evil.example")
        .send()
        .await
        .unwrap();
    assert_eq!(evil.status(), 403);
    let rebind = post(Some(&format!("Bearer {TOKEN}")))
        .header("host", "evil.example")
        .send()
        .await
        .unwrap();
    assert_eq!(rebind.status(), 403);
    // WS without token.
    let ws =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{}/sync/main", env.port)).await;
    assert!(ws.is_err());
}

async fn next_msg(
    ws: &mut (impl StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin),
) -> Message {
    tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("ws timeout")
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn ws_sync_both_directions() {
    let env = start("http://127.0.0.1:1", None).await;
    let (mut ws, _) = tokio_tungstenite::connect_async(format!(
        "ws://127.0.0.1:{}/sync/main?token={TOKEN}&actor=lukas&name=Lukas",
        env.port
    ))
    .await
    .unwrap();
    let Message::Text(hello) = next_msg(&mut ws).await else {
        panic!("expected hello")
    };
    let hello: Json = serde_json::from_str(&hello).unwrap();
    assert_eq!(hello["type"], "hello");
    assert_eq!(hello["session"]["actor"], "lukas");
    let Message::Binary(snap) = next_msg(&mut ws).await else {
        panic!("expected snapshot")
    };
    let mut local = Canvas::from_snapshot(&snap).unwrap();

    // Agent edit over MCP arrives as a binary Loro update + a JSON summary.
    let claude = client(env.port).await;
    let s = js(&call(&claude, "session", json!({"agentName": "Claude"})).await);
    let sid = s["sessionId"].as_str().unwrap();
    let e = js(&call(
        &claude,
        "edit",
        json!({"sessionId": sid, "canvas": "main", "text": "note \"hi\" text(\"from claude\")"}),
    )
    .await);
    let note = e["created"][0].as_str().unwrap().to_string();
    let mut got_update = false;
    let mut got_summary = false;
    while !(got_update && got_summary) {
        match next_msg(&mut ws).await {
            Message::Binary(b) => {
                local.import(&b).unwrap();
                got_update = local.node(&note).is_some();
            }
            Message::Text(t) => {
                let v: Json = serde_json::from_str(&t).unwrap();
                if v["type"] == "doc" {
                    assert_eq!(v["actor"], "claude");
                    got_summary = v["created"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|c| c == &json!(note));
                }
            }
            _ => {}
        }
    }
    assert_eq!(
        local.node(&note).unwrap().text.as_deref(),
        Some("from claude")
    );

    // Human edit in the UI replica goes up as a binary frame.
    let before = local.vv();
    let props: Props = serde_json::from_value(json!({"x": 5, "y": 5})).unwrap();
    let r = local
        .apply(
            &Actor::human("lukas", "Lukas"),
            None,
            &[Command::CreateNode {
                id: None,
                kind: NodeKind::Frame,
                parent: None,
                index: None,
                name: "human frame".into(),
                props,
                text: None,
            }],
        )
        .unwrap();
    let frame = r.created[0].clone();
    ws.send(Message::Binary(
        local.export_updates(&before).unwrap().into(),
    ))
    .await
    .unwrap();
    ws.send(Message::Text(
        r#"{"type":"presence","cursor":[1,2],"status":"looking"}"#.into(),
    ))
    .await
    .unwrap();
    let mut seen = false;
    for _ in 0..50 {
        let read = call(&claude, "read", json!({"sessionId": sid, "canvas": "main"})).await;
        if read.contains(&format!("{frame} frame \"human frame\" p(5,5) by(@lukas)")) {
            seen = read.contains("@lukas (Lukas) status \"looking\"");
            if seen {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(seen, "human edit/presence not visible to the agent");
}

#[tokio::test]
async fn generate_runs_fal_queue_job() {
    use axum::{
        Json as AJson, Router,
        routing::{get, post},
    };
    // Mock fal queue + CDN.
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let b = base.clone();
    let app = Router::new()
        .route("/fal-ai/mock", post(move |AJson(input): AJson<Json>| {
            let b = b.clone();
            async move {
                assert_eq!(input["prompt"], "red car");
                AJson(json!({"request_id": "req-1", "status_url": format!("{b}/status"), "response_url": format!("{b}/result")}))
            }
        }))
        .route("/status", get(|| async { AJson(json!({"status": "COMPLETED"})) }))
        .route("/result", get({
            let b = base.clone();
            move || async move { AJson(json!({"seed": 7, "images": [{"url": format!("{b}/out.png"), "width": 100, "height": 50, "content_type": "image/png"}]})) }
        }))
        .route("/out.png", get(|| async { vec![1u8, 2, 3, 4] }));
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });

    let env = start(&base, Some("k")).await;
    let c = client(env.port).await;
    let s = js(&call(&c, "session", json!({"agentName": "Claude"})).await);
    let sid = s["sessionId"].as_str().unwrap();
    let g = js(&call(&c, "generate", json!({
        "sessionId": sid, "canvas": "main", "model": "fal-ai/mock", "input": {"prompt": "red car"},
        "placement": {"x": 0, "y": 0, "w": 400, "h": 400},
    })).await);
    let node = g["node"].as_str().unwrap().to_string();
    let mut done = String::new();
    for _ in 0..100 {
        let r = call(
            &c,
            "read",
            json!({"sessionId": sid, "canvas": "main", "root": node}),
        )
        .await;
        if r.contains(&format!("{node} image")) {
            done = r;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        done.contains("s(400,200)") && done.contains("asset(sha256:"),
        "{done}"
    );
    let n = env.host.read("main", |c| c.node(&node)).unwrap().unwrap();
    let asset = env
        .host
        .read("main", |c| c.asset(n.props["asset"].as_str().unwrap()))
        .unwrap()
        .unwrap();
    let l = asset.lineage.unwrap();
    assert_eq!(
        (l.model.as_str(), l.actor.as_str(), l.seed),
        ("fal-ai/mock", "claude", Some(7))
    );
    assert_eq!(l.fal_request_id.as_deref(), Some("req-1"));
}

#[tokio::test]
async fn asset_routes_put_get_range_and_cors() {
    use sha2::{Digest, Sha256};
    let env = start("http://127.0.0.1:1", None).await;
    let http = reqwest::Client::new();
    let body: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
    let hex: String = Sha256::digest(&body)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let url = format!("http://127.0.0.1:{}/assets/{hex}", env.port);
    let auth = format!("Bearer {TOKEN}");

    // No token → 401; HEAD before upload → 404.
    assert_eq!(http.get(&url).send().await.unwrap().status(), 401);
    let head = http.head(&url).header("authorization", &auth).send();
    assert_eq!(head.await.unwrap().status(), 404);
    // A wrong hash is rejected and leaves nothing behind.
    let bad = http
        .put(&url)
        .header("authorization", &auth)
        .header("content-type", "video/mp4")
        .body(b"nope".to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 400);
    assert!(falos_host::assets::find(&env.host.dir().join("assets"), &hex).is_none());
    // Upload: the file lands in <project>/assets with the extension of its content type.
    let put = || {
        http.put(&url)
            .header("authorization", &auth)
            .header("content-type", "video/mp4")
            .body(body.clone())
            .send()
    };
    assert_eq!(put().await.unwrap().status(), 201);
    assert!(env.host.dir().join(format!("assets/{hex}.mp4")).exists());
    // Re-upload dedupes.
    assert_eq!(put().await.unwrap().status(), 200);

    // Full GET with ?token= (what <video src> uses); type from the stored extension.
    let full = http
        .get(format!("{url}?token={TOKEN}"))
        .send()
        .await
        .unwrap();
    assert_eq!(full.status(), 200);
    assert_eq!(full.headers()["content-type"], "video/mp4");
    assert_eq!(full.headers()["accept-ranges"], "bytes");
    assert_eq!(full.bytes().await.unwrap().as_ref(), body.as_slice());
    // Byte ranges (and the `sha256:` id form).
    let range = |u: String, r: &'static str| {
        http.get(u)
            .header("authorization", &auth)
            .header("range", r)
            .send()
    };
    let part = range(
        format!("http://127.0.0.1:{}/assets/sha256:{hex}", env.port),
        "bytes=10-19",
    )
    .await
    .unwrap();
    assert_eq!(part.status(), 206);
    assert_eq!(part.headers()["content-range"], "bytes 10-19/1000");
    assert_eq!(part.bytes().await.unwrap().as_ref(), &body[10..20]);
    let tail = range(url.clone(), "bytes=-5").await.unwrap();
    assert_eq!(tail.status(), 206);
    assert_eq!(tail.bytes().await.unwrap().as_ref(), &body[995..]);
    let unsat = range(url.clone(), "bytes=5000-").await.unwrap();
    assert_eq!(unsat.status(), 416);

    // CORS: preflight without credentials; allowed origins echoed; foreign ones refused.
    let pre = http
        .request(reqwest::Method::OPTIONS, &url)
        .header("origin", "http://127.0.0.1:5173")
        .header("access-control-request-method", "PUT")
        .send()
        .await
        .unwrap();
    assert_eq!(pre.status(), 204);
    assert_eq!(
        pre.headers()["access-control-allow-origin"],
        "http://127.0.0.1:5173"
    );
    let get = http
        .get(&url)
        .header("authorization", &auth)
        .header("origin", "http://localhost:5173")
        .send()
        .await
        .unwrap();
    assert_eq!(
        get.headers()["access-control-allow-origin"],
        "http://localhost:5173"
    );
    let evil = http
        .get(&url)
        .header("authorization", &auth)
        .header("origin", "https://evil.example")
        .send()
        .await
        .unwrap();
    assert_eq!(evil.status(), 403);
    // PUT never accepts ?token=.
    let qput = http
        .put(format!("{url}?token={TOKEN}"))
        .body(body.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(qput.status(), 401);
}

#[tokio::test]
async fn import_url_downloads_media_into_assets() {
    use sha2::{Digest, Sha256};
    // A stand-in CDN: a PNG served as octet-stream (sniffed), an HTML page, and a 404.
    let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    png.extend_from_slice(&320u32.to_be_bytes());
    png.extend_from_slice(&200u32.to_be_bytes());
    png.extend_from_slice(&[8, 6, 0, 0, 0, 1, 2, 3, 4]);
    let served = png.clone();
    let cdn = axum::Router::new()
        .route("/img", axum::routing::get(move || async move { ([("content-type", "application/octet-stream")], served) }))
        .route("/page", axum::routing::get(|| async { ([("content-type", "text/html")], "<html></html>") }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cdn_port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, cdn).await.unwrap() });

    let env = start("http://127.0.0.1:1", None).await;
    let http = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{}/import/url", env.port);
    let auth = format!("Bearer {TOKEN}");
    let post = |body: Json| http.post(&url).header("authorization", &auth).json(&body).send();
    let hex: String = Sha256::digest(&png).iter().map(|b| format!("{b:02x}")).collect();

    assert_eq!(http.post(&url).json(&json!({"url": "x"})).send().await.unwrap().status(), 401);
    let r = post(json!({"url": format!("http://127.0.0.1:{cdn_port}/img")})).await.unwrap();
    assert_eq!(r.status(), 201);
    let v: Json = r.json().await.unwrap();
    assert_eq!(v["id"], format!("sha256:{hex}"));
    assert_eq!((v["mime"].as_str(), v["width"].as_u64(), v["height"].as_u64()), (Some("image/png"), Some(320), Some(200)));
    assert_eq!(std::fs::read(env.host.dir().join(format!("assets/{hex}.png"))).unwrap(), png);
    // Known digest: answered from the assets without downloading (the URL is dead).
    let r = post(json!({"url": "http://127.0.0.1:1/gone", "sha256": format!("sha256:{hex}")})).await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.json::<Json>().await.unwrap()["existing"], true);
    // A digest that doesn't match the download is refused.
    let wrong = "0".repeat(64);
    let r = post(json!({"url": format!("http://127.0.0.1:{cdn_port}/img"), "sha256": wrong})).await.unwrap();
    assert_eq!(r.status(), 400);
    // Not media, upstream errors, bad schemes.
    let r = post(json!({"url": format!("http://127.0.0.1:{cdn_port}/page")})).await.unwrap();
    assert_eq!(r.status(), 415);
    let r = post(json!({"url": format!("http://127.0.0.1:{cdn_port}/missing")})).await.unwrap();
    assert_eq!(r.status(), 502);
    assert_eq!(post(json!({"url": "file:///etc/passwd"})).await.unwrap().status(), 400);
    // Browser preflight from the canvas UI.
    let pre = http
        .request(reqwest::Method::OPTIONS, &url)
        .header("origin", "http://127.0.0.1:5197")
        .header("access-control-request-method", "POST")
        .send()
        .await
        .unwrap();
    assert_eq!(pre.status(), 204);
    assert!(pre.headers()["access-control-allow-methods"].to_str().unwrap().contains("POST"));
}

#[tokio::test]
async fn annotate_defaults_normalized_and_agent_presence_follows_work() {
    let env = start("http://127.0.0.1:1", None).await;
    let c = client(env.port).await;
    let s = js(&call(
        &c,
        "session",
        json!({"agentName": "Claude Code", "canvas": "main"}),
    )
    .await);
    let sid = s["sessionId"].as_str().unwrap().to_string();
    let e = js(&call(&c, "edit", json!({
        "sessionId": sid, "canvas": "main",
        "text": "frame \"Board\" p(100,100) s(1200,800)\n  image \"car\" p(40,80) s(512,256) asset(sha:9f)",
    })).await);
    let img = e["created"][1].as_str().unwrap().to_string();
    // The cursor sits on the center of the last created node, in world coordinates
    // (child positions are relative to their parent frame).
    let me = || {
        env.host
            .sessions()
            .into_iter()
            .find(|x| x.id == sid)
            .unwrap()
    };
    assert_eq!(me().presence.cursor, Some([396.0, 308.0]));
    assert_eq!(me().presence.canvas.as_deref(), Some("main"));
    assert!(me().presence.viewport.is_some());

    // Default anchor = the whole node, normalized; get_annotation also gives node pixels.
    let a = js(&call(
        &c,
        "annotate",
        json!({"sessionId": sid, "canvas": "main", "nodeId": img, "text": "whole thing"}),
    )
    .await);
    let ann = a["annotationId"].as_str().unwrap().to_string();
    let g = call(
        &c,
        "get_annotation",
        json!({"sessionId": sid, "canvas": "main", "id": ann}),
    )
    .await;
    assert!(g.contains("anchor: box(0,0,1,1)"), "{g}");
    assert!(g.contains("anchor in node pixels: box(0,0,512,256)"), "{g}");
    // Pixel coordinates are refused with a hint.
    let r = call_raw(&c, "annotate", json!({
        "sessionId": sid, "canvas": "main", "nodeId": img, "text": "px", "anchor": "box(10,20,200,100)",
    })).await;
    assert_eq!(r.is_error, Some(true));
    assert!(text(&r).contains("normalized"), "{}", text(&r));
    // A point anchor moves the cursor onto that point.
    call(&c, "annotate", json!({
        "sessionId": sid, "canvas": "main", "nodeId": img, "text": "here", "anchor": "pt(0.5,0.25)",
    }))
    .await;
    assert_eq!(me().presence.cursor, Some([396.0, 244.0]));
}
