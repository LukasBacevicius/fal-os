//! Asset library end to end: ingest from a workspace, `/workspace` library protocol,
//! thumbnails (image / video poster + strip / audio waveform), color + text search, fal sync
//! against a mock fal API (pull + match, upload, favorite), placing a remote asset, and the
//! library MCP tools. Needs ffmpeg on PATH (skips otherwise).

use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::routing::{get, post, put};
use falos_host::chat::{ImportOpts, LineageArgs, import_file};
use falos_host::context::NoRenderer;
use falos_host::library::{Library, fal::FalApi};
use falos_host::workspace::Workspace;
use falos_host::{Host, HostOptions, server};
use futures::{SinkExt, StreamExt};
use rmcp::model::{CallToolRequestParams, ClientCapabilities, ClientConfig, Implementation};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::ServiceExt;
use serde_json::{Value as Json, json};
use tokio_tungstenite::tungstenite::Message;

const TOKEN: &str = "test-token-0123456789abcdef0123456789abcdef";

fn opts(home: PathBuf) -> impl Fn() -> HostOptions + Send + Sync + 'static {
    move || HostOptions { fal_queue_url: "http://127.0.0.1:1".into(), fal_key: None, fal_session: false, renderer: Box::new(NoRenderer), home: Some(home.clone()) }
}

fn ffmpeg(args: &[&str]) -> bool {
    Command::new("ffmpeg").args(["-v", "error", "-y"]).args(args).status().is_ok_and(|s| s.success())
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn next_where(ws: &mut Ws, pred: impl Fn(&Json) -> bool) -> Json {
    loop {
        let m = tokio::time::timeout(Duration::from_secs(30), ws.next()).await.expect("ws timeout").unwrap().unwrap();
        if let Message::Text(t) = m {
            let v: Json = serde_json::from_str(&t).unwrap();
            assert!(!(v["type"] == "error" && v["scope"] == "library"), "library error: {v}");
            if pred(&v) {
                return v;
            }
        }
    }
}

async fn send(ws: &mut Ws, v: Json) {
    ws.send(Message::Text(v.to_string().into())).await.unwrap();
}

async fn query(ws: &mut Ws, id: &str, q: Json) -> Json {
    send(ws, json!({"type": "library.query", "id": id, "query": q})).await;
    next_where(ws, |v| v["type"] == "library.results" && v["id"] == id && v["partial"] != true).await
}

// ---- mock fal -------------------------------------------------------------------------------

#[derive(Default)]
struct Mock {
    base: String,
    calls: Vec<String>,
    png: Vec<u8>,
}
type M = Arc<Mutex<Mock>>;

async fn mock_list(State(m): State<M>, q: axum::extract::Query<std::collections::HashMap<String, String>>) -> axum::Json<Json> {
    let mut m = m.lock().unwrap();
    m.calls.push(format!("list {}", q.get("q").cloned().unwrap_or_default()));
    let base = m.base.clone();
    axum::Json(json!({"assets": [
        {"asset_id": "gen1", "request_id": "req-1", "url": format!("{base}/files/fox.png"), "type": "image", "source": "response", "endpoint": "fal-ai/flux/dev",
         "prompt": "a red fox", "is_favorited": true, "tags": [{"id": "t1", "name": "Hero"}], "collection_ids": [], "created_at": "2026-10-11T10:00:00Z"},
        {"asset_id": "remote1", "request_id": "upload:9", "url": format!("{base}/files/remote.png"), "type": "image", "source": "upload", "content_type": "image/png",
         "prompt": "a blue square moodboard", "is_favorited": false, "tags": [], "collection_ids": [], "created_at": "2026-10-10T10:00:00Z"}
    ], "next_cursor": null, "has_more": false}))
}

async fn mock_file(State(m): State<M>) -> Vec<u8> {
    m.lock().unwrap().png.clone()
}

async fn mock_record(State(m): State<M>, uri: axum::http::Uri, body: axum::body::Bytes) -> axum::Json<Json> {
    let body = String::from_utf8_lossy(&body).into_owned();
    let mut m = m.lock().unwrap();
    m.calls.push(format!("{} {body}", uri.path()));
    let base = m.base.clone();
    let p = uri.path();
    axum::Json(if p.ends_with("/initiate") {
        json!({"upload_url": format!("{base}/put"), "file_url": format!("{base}/files/up.png")})
    } else if p.ends_with("/assets/uploads") {
        json!({"asset": {"asset_id": "up1", "url": format!("{base}/files/up.png"), "type": "image", "source": "upload"}})
    } else if p.ends_with("/collections") {
        json!({"collection": {"id": format!("col{}", m.calls.len())}})
    } else if p.ends_with("/tags") && body.contains("\"name\"") {
        json!({"tag": {"id": "t9"}})
    } else {
        json!({"success": true})
    })
}

async fn mock_empty(State(m): State<M>, uri: axum::http::Uri) -> axum::Json<Json> {
    m.lock().unwrap().calls.push(format!("GET {}", uri.path()));
    axum::Json(json!({"collections": [], "tags": [], "has_more": false}))
}

async fn start_mock(png: Vec<u8>) -> (M, String) {
    let m: M = Arc::new(Mutex::new(Mock { png, ..Default::default() }));
    let app = Router::new()
        .route("/v1/assets", get(mock_list))
        .route("/v1/assets/collections", get(mock_empty).post(mock_record))
        .route("/v1/assets/tags", get(mock_empty).post(mock_record).put(mock_record))
        .route("/v1/assets/uploads", post(mock_record))
        .route("/v1/assets/favorite", post(mock_record))
        .route("/v1/assets/unfavorite", post(mock_record))
        .route("/storage/upload/initiate", post(mock_record))
        .route("/put", put(mock_record))
        .route("/files/{f}", get(mock_file))
        .with_state(m.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
    m.lock().unwrap().base = base.clone();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (m, base)
}

async fn http_get(port: u16, path: &str) -> (u16, Vec<u8>) {
    let r = reqwest::get(format!("http://127.0.0.1:{port}{path}{}token={TOKEN}", if path.contains('?') { '&' } else { '?' })).await.unwrap();
    (r.status().as_u16(), r.bytes().await.unwrap().to_vec())
}

fn sha_of(items: &Json, kind: &str) -> String {
    items["items"].as_array().unwrap().iter().find(|i| i["kind"] == kind).unwrap_or_else(|| panic!("no {kind} in {items}"))["sha"].as_str().unwrap().to_string()
}

async fn wait_for(mut f: impl FnMut() -> bool) {
    for _ in 0..200 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("condition not met in time");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn library_end_to_end() {
    if !ffmpeg(&["-version"]) {
        eprintln!("ffmpeg missing: skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let media = dir.path().join("media");
    std::fs::create_dir_all(&media).unwrap();
    let m = |n: &str| media.join(n).display().to_string();
    assert!(ffmpeg(&["-f", "lavfi", "-i", "color=c=0xe02020:s=320x200", "-frames:v", "1", &m("red.png")]));
    assert!(ffmpeg(&["-f", "lavfi", "-i", "color=c=0x2040e0:s=64x64", "-frames:v", "1", &m("blue.png")]));
    assert!(ffmpeg(&["-f", "lavfi", "-i", "testsrc=s=320x240:d=3:r=10", "-pix_fmt", "yuv420p", &m("clip.mp4")]));
    assert!(ffmpeg(&["-f", "lavfi", "-i", "sine=f=440:d=1", &m("tone.wav")]));
    assert!(ffmpeg(&["-f", "lavfi", "-i", "color=c=0x20c040:s=128x96", "-frames:v", "1", &m("fox.png")]));

    let root = dir.path().join("projects");
    let home = dir.path().join("falos-home");
    {
        let h = Host::open(&root.join("demo"), opts(dir.path().join("h"))()).unwrap();
        let who = falos_doc::Actor::human("human", "Human");
        for f in ["red.png", "clip.mp4", "tone.wav"] {
            import_file(&h, &who, "main", &media.join(f), ImportOpts::default()).await.unwrap();
        }
        let lineage = LineageArgs { model: Some("fal-ai/flux/dev".into()), params: Some(json!({"prompt": "a red fox in the snow"})), fal_request_id: Some("req-1".into()), ..Default::default() };
        import_file(&h, &who, "main", &media.join("fox.png"), ImportOpts { lineage: Some(lineage), ..Default::default() }).await.unwrap();
        h.flush().unwrap();
    }
    let blue = std::fs::read(media.join("blue.png")).unwrap();
    let (mock, base) = start_mock(blue).await;
    let ws = Workspace::open(&root, opts(dir.path().join("h"))).unwrap();
    let lib = Library::open_with(&home, Some(FalApi::with_base("k".into(), &format!("{base}/v1"), &base, &format!("{base}/agent")))).unwrap();
    let running = server::serve_workspace(ws.clone(), 0, TOKEN).await.unwrap();
    let lib = Library::attach(&ws, lib);
    let port = running.addr.port();
    lib.ingest_all().unwrap();

    let (mut w, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/workspace?token={TOKEN}")).await.unwrap();
    let st = next_where(&mut w, |v| v["type"] == "library.status").await;
    assert_eq!(st["fal"], "connected");

    // This project: 4 assets; inputs vs generated.
    let all = query(&mut w, "q1", json!({"project": "demo", "semantic": false})).await;
    assert_eq!(all["items"].as_array().unwrap().len(), 4, "{all}");
    let inputs = query(&mut w, "q2", json!({"project": "demo", "role": ["input"]})).await;
    assert_eq!(inputs["items"].as_array().unwrap().len(), 3);
    let gen_q = query(&mut w, "q3", json!({"project": "demo", "source": ["generate"]})).await;
    assert_eq!(gen_q["items"][0]["model"], "fal-ai/flux/dev");
    assert_eq!(gen_q["items"][0]["role"], "candidate");
    assert!(gen_q["items"][0]["lineage"].as_str().unwrap_or("").contains("gen:fal-ai/flux/dev"), "{gen_q}");
    let fox = gen_q["items"][0]["sha"].as_str().unwrap().to_string();
    let txt = query(&mut w, "q4", json!({"text": "fox snow", "semantic": false})).await;
    assert_eq!(txt["items"][0]["sha"], fox);

    // Thumbnails on demand: image, video poster + strip, audio waveform.
    let png = sha_of(&inputs, "image");
    let mp4 = sha_of(&all, "video");
    let wav = sha_of(&all, "audio");
    for (k, size) in [(&png, "256"), (&png, "1024"), (&mp4, "256"), (&mp4, "strip"), (&wav, "256")] {
        // A slow generation answers 503 (still generating): retry like the grid does.
        let mut got = http_get(port, &format!("/thumbs/{k}-{size}.webp")).await;
        for _ in 0..20 {
            if got.0 != 503 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
            got = http_get(port, &format!("/thumbs/{k}-{size}.webp")).await;
        }
        let (status, body) = got;
        assert_eq!(status, 200, "{k}-{size}: {}", String::from_utf8_lossy(&body));
        assert_eq!(&body[..4], b"RIFF");
    }
    let (status, _) = reqwest::get(format!("http://127.0.0.1:{port}/thumbs/{png}-256.webp")).await.map(|r| (r.status().as_u16(), ())).unwrap();
    assert_eq!(status, 401, "thumbs need the token");
    // Background thumbnailer fills palettes; color search finds the red image.
    let lib2 = lib.clone();
    let p0 = png.clone();
    wait_for(move || lib2.detail(&p0, None).map(|d| d["palette"].is_array()).unwrap_or(false)).await;
    let red = query(&mut w, "q5", json!({"color": "#e02020"})).await;
    assert_eq!(red["items"][0]["kind"], "image", "{red}");
    assert!(red["items"][0]["palette"][0].as_str().unwrap().starts_with("#e"), "{red}");
    let vid = lib.detail(&mp4, None).unwrap();
    assert_eq!((vid["w"].as_i64(), vid["h"].as_i64()), (Some(320), Some(240)), "{vid}");

    // fal pull: the generation links to its fal asset; a remote-only upload appears.
    lib.pull_tick().await.unwrap();
    let d = lib.detail(&fox, None).unwrap();
    assert_eq!(d["fal"]["assetRecordId"], "gen1");
    assert_eq!(d["favorite"], true, "fal is master for metadata");
    assert_eq!(d["tags"], json!(["hero"]));
    let remote = query(&mut w, "q6", json!({"synced": true, "source": ["import"], "semantic": false})).await;
    let rkey = remote["items"][0]["sha"].as_str().unwrap().to_string();
    // (The background thumbnailer may already have fetched it and re-keyed it by sha.)
    assert!(rkey == "fal_remote1" || rkey.len() == 64, "{rkey}");
    assert_eq!(remote["items"][0]["caption"], "a blue square moodboard");

    // Semantic search goes to fal and merges.
    let sem = query(&mut w, "q7", json!({"text": "moodboard"})).await;
    assert!(sem["items"].as_array().unwrap().iter().any(|i| i["sha"] == rkey.as_str() || i["fal"]["assetRecordId"] == "remote1"), "{sem}");
    assert!(mock.lock().unwrap().calls.iter().any(|c| c == "list moodboard"));

    // Explicit upload of a local input → storage + register (Idempotency-Key = sha).
    send(&mut w, json!({"type": "library.upload", "id": "u1", "shas": [png], "project": "demo"})).await;
    let up = next_where(&mut w, |v| v["type"] == "library.uploading").await;
    assert_eq!(up["queued"].as_array().unwrap().len(), 1);
    let lib2 = lib.clone();
    let png2 = png.clone();
    wait_for(move || lib2.detail(&png2, None).map(|d| d["fal"]["synced"] == true).unwrap_or(false)).await;
    {
        let calls = mock.lock().unwrap().calls.clone();
        assert!(calls.iter().any(|c| c.starts_with("/storage/upload/initiate")), "{calls:?}");
        assert!(calls.iter().any(|c| c.starts_with("/v1/assets/uploads") && c.contains("col")), "uploads go to the project collection: {calls:?}");
    }

    // Favorite + tags write through the outbox for assets on fal.
    send(&mut w, json!({"type": "library.favorite", "id": "f1", "shas": [png], "value": true})).await;
    send(&mut w, json!({"type": "library.tag", "id": "t1", "shas": [png], "add": ["Mood"]})).await;
    next_where(&mut w, |v| v["type"] == "library.changed").await;
    let m2 = mock.clone();
    wait_for(move || {
        let c = m2.lock().unwrap().calls.clone();
        c.iter().any(|c| c.starts_with("/v1/assets/favorite")) && c.iter().any(|c| c.starts_with("/v1/assets/tags") && c.contains("tag_ids"))
    })
    .await;
    let tagged = query(&mut w, "q8", json!({"tag": ["mood"]})).await;
    assert_eq!(tagged["items"][0]["sha"], png);

    // Place the remote-only asset: downloaded, re-keyed by sha, new node in demo.
    send(&mut w, json!({"type": "library.place", "id": "p1", "sha": rkey, "project": "demo", "x": 2000, "y": 0})).await;
    let placed = next_where(&mut w, |v| v["type"] == "library.placed").await;
    let new_sha = placed["sha"].as_str().unwrap().to_string();
    assert_eq!(new_sha.len(), 64);
    let host = ws.get("demo").unwrap();
    assert!(falos_host::assets::find(&host.dir().join("assets"), &new_sha).is_some());
    let node = host.read("main", |c| c.node(placed["node"].as_str().unwrap())).unwrap().unwrap();
    assert_eq!(node.props["asset"], format!("sha256:{new_sha}"));
    assert!(lib.detail("fal_remote1", None).is_err() || rkey.len() == 64, "re-keyed");
    assert_eq!(lib.detail(&new_sha, None).unwrap()["fal"]["assetRecordId"], "remote1");

    // MCP tools.
    let cfg = StreamableHttpClientTransportConfig::with_uri(server::mcp_url(port)).auth_header(TOKEN);
    let c = ClientConfig::new(ClientCapabilities::default(), Implementation::new("test", "1.0")).serve(StreamableHttpClientTransport::from_config(cfg)).await.unwrap();
    let call = |tool: &'static str, args: Json| {
        let c = &c;
        async move {
            let r = c.call_tool(CallToolRequestParams::new(tool).with_arguments(args.as_object().unwrap().clone())).await.unwrap();
            let t: String = r.content.iter().filter_map(|x| x.as_text().map(|t| t.text.clone())).filter(|t| !t.starts_with("{\"project\"")).collect();
            assert_ne!(r.is_error, Some(true), "{tool}: {t}");
            t
        }
    };
    let s: Json = serde_json::from_str(&call("session", json!({"agentName": "tester", "project": "demo"})).await).unwrap();
    let sid = s["sessionId"].as_str().unwrap();
    let found = call("search_assets", json!({"sessionId": sid, "query": {"text": "fox"}})).await;
    assert!(found.contains(&fox[..12]) && found.contains("fal-ai/flux/dev"), "{found}");
    let detail = call("get_asset", json!({"sessionId": sid, "asset": &fox[..10]})).await;
    assert!(detail.contains("a red fox in the snow"), "{detail}");
    call("show_assets", json!({"sessionId": sid, "shas": [&fox[..10]]})).await;
    let shown = next_where(&mut w, |v| v["type"] == "library.show").await;
    assert_eq!(shown["shas"], json!([fox]));
    let placed = call("place_asset", json!({"sessionId": sid, "sha": &mp4[..10]})).await;
    assert!(placed.contains("\"node\""), "{placed}");
    let n = host.read("main", |c| c.nodes().iter().filter(|n| n.props.get("asset").and_then(Json::as_str) == Some(&format!("sha256:{mp4}"))).count()).unwrap();
    assert_eq!(n, 2, "two placements, one asset");

    drop(c);
    running.cancel.cancel();
}
