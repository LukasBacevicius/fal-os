//! App nodes: create / write / read / logs over MCP, the app server (guard injection, MIME,
//! no-store), preview logs, undo restoring files, and (when a headless Chrome is around) the
//! real headless check catching an exception.

use std::sync::Arc;
use std::time::Duration;

use falos_host::context::NoRenderer;
use falos_host::{Host, HostOptions, server};
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::{RoleClient, ServiceExt};
use serde_json::{Value as Json, json};

const TOKEN: &str = "test-token-0123456789abcdef0123456789abcdef";

type Client = RunningService<RoleClient, ()>;

async fn call(c: &Client, tool: &'static str, args: Json) -> Json {
    let r: CallToolResult = c
        .call_tool(CallToolRequestParams::new(tool).with_arguments(args.as_object().unwrap().clone()))
        .await
        .unwrap();
    let t: String = r.content.iter().filter_map(|c| c.as_text().map(|t| t.text.clone())).collect::<Vec<_>>().join("\n");
    assert_ne!(r.is_error, Some(true), "{tool} failed: {t}");
    let end = t.find("\n\nnotices").unwrap_or(t.len());
    serde_json::from_str(&t[..end]).unwrap_or_else(|e| panic!("{e}: {t}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn apps_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let host: Arc<Host> = Host::open(
        &dir.path().join("proj"),
        HostOptions { fal_queue_url: "http://127.0.0.1:9".into(), fal_key: None, fal_session: false, renderer: Box::new(NoRenderer), home: Some(dir.path().join("home")) },
    )
    .unwrap();
    let running = server::serve(host.clone(), 0, TOKEN).await.unwrap();
    let t = StreamableHttpClientTransport::from_config(StreamableHttpClientTransportConfig::with_uri(server::mcp_url(running.addr.port())).auth_header(TOKEN));
    let c: Client = ().serve(t).await.unwrap();
    let sid = call(&c, "session", json!({"agentName": "Coder"})).await["sessionId"].as_str().unwrap().to_string();

    // A broken first version: the exception must show up in the check (when Chrome exists).
    let broken = "<!doctype html><html><head><title>t</title></head><body><canvas id=c></canvas><script type=module src=./main.js></script></body></html>";
    let created = call(&c, "create_app", json!({"sessionId": sid, "canvas": "main", "name": "Ping Pong", "files": [
        {"path": "index.html", "content": broken},
        {"path": "main.js", "content": "console.error('boom'); undefinedThing();"}
    ]}))
    .await;
    assert_eq!(created["app"], "ping-pong");
    assert_eq!(created["version"], 1);
    let node = created["node"].as_str().unwrap().to_string();
    let url = created["url"].as_str().unwrap().to_string();
    let chrome = falos_host::apps::cdp::chrome_path().is_some();
    if chrome {
        let errs = created["check"]["errors"].as_array().unwrap();
        assert!(errs.iter().any(|e| e.as_str().unwrap().contains("undefinedThing")), "{created}");
        assert!(errs.iter().any(|e| e.as_str().unwrap().contains("boom")), "{created}");
    }

    // The app server: guard first, JS MIME, no caching, CORS for opaque-origin iframes.
    let http = reqwest::Client::new();
    let r = http.get(&url).send().await.unwrap();
    assert_eq!(r.headers()["cache-control"], "no-store");
    assert_eq!(r.headers()["access-control-allow-origin"], "*");
    let html = r.text().await.unwrap();
    assert!(html.find("__falos").unwrap() < html.find("main.js").unwrap());
    let js = http.get(url.replace("index.html", "main.js")).send().await.unwrap();
    assert!(js.headers()["content-type"].to_str().unwrap().starts_with("text/javascript"));
    assert_eq!(http.get(url.replace("index.html", "../../etc/passwd")).send().await.unwrap().status(), 404);

    // Fix it: version 2, clean.
    let fixed = call(&c, "write_app_files", json!({"sessionId": sid, "app": "ping-pong", "files": [{"path": "main.js", "content": "document.title = 'ok';"}]})).await;
    assert_eq!(fixed["version"], 2);
    if chrome {
        assert_eq!(fixed["check"]["ok"], true, "{fixed}");
        let snap = host.read("main", |cv| cv.node(&node).unwrap().props.get("asset").cloned()).unwrap();
        assert!(snap.and_then(|s| s.as_str().map(|s| s.starts_with("sha256:"))).unwrap_or(false), "snapshot still");
    }

    // A preview reports an error while someone plays.
    let log_url = format!("{}/proj/ping-pong/__falos/log", falos_host::apps::base_url().unwrap());
    let r = http.post(&log_url).body(r#"[{"level":"exception","text":"TypeError: ball is null"}]"#).send().await.unwrap();
    assert_eq!(r.status(), 204);
    let logs = call(&c, "app_logs", json!({"sessionId": sid, "app": node})).await;
    assert_eq!(logs["errors"], 1, "{logs}");
    assert_eq!(logs["entries"].as_array().unwrap().iter().find(|e| e["source"] == "preview").unwrap()["text"], "TypeError: ball is null");

    // read_app returns contents.
    let read = call(&c, "read_app", json!({"sessionId": sid, "app": "ping-pong"})).await;
    assert_eq!(read["contents"]["main.js"], "document.title = 'ok';");

    // Undo the write: the node goes back to v1 and the files follow.
    call(&c, "undo", json!({"sessionId": sid, "canvas": "main"})).await;
    let main = dir.path().join("proj/apps/ping-pong/main.js");
    let mut restored = false;
    for _ in 0..50 {
        if std::fs::read_to_string(&main).unwrap().contains("undefinedThing") {
            restored = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(restored, "undo restores the v1 files");

    // External edits (Codex writing into the folder) become a new version.
    tokio::time::sleep(Duration::from_millis(300)).await;
    std::fs::write(&main, "document.title = 'by hand';").unwrap();
    let mut v = 0;
    for _ in 0..50 {
        v = host.read("main", |cv| cv.node(&node).unwrap().props["version"].as_u64().unwrap()).unwrap();
        if v >= 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(v, 3, "external edit committed as v3 (v2 stays in history)");
    running.cancel.cancel();
}
