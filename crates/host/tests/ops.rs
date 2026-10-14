//! Assets v1: provenance on import, protected inputs, the op registry (ffmpeg / image / fal
//! with a mock queue), inspect, rerun, export + verify + manifest, packages, the REST mirror
//! and mediaInputs. Media is generated with ffmpeg into temp dirs.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use falos_host::context::NoRenderer;
use falos_host::{Host, HostOptions, server};
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::{RoleClient, ServiceExt};
use serde_json::{Value as Json, json};

const TOKEN: &str = "test-token-0123456789abcdef0123456789abcdef";

struct Env {
    dir: tempfile::TempDir,
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
    Env { dir, host, port: running.addr.port(), _running: running }
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
    r.content.iter().filter_map(|c| c.as_text().map(|t| t.text.clone())).filter(|t| !t.starts_with("{\"project\"")).collect::<Vec<_>>().join("\n")
}

async fn call(c: &Client, tool: &'static str, args: Json) -> Json {
    let r = call_raw(c, tool, args).await;
    let t = text(&r);
    assert_ne!(r.is_error, Some(true), "{tool} failed: {t}");
    let end = t.find("\n\nnotices").unwrap_or(t.len());
    // inspect appends captions after the JSON.
    serde_json::Deserializer::from_str(&t[..end]).into_iter::<Json>().next().unwrap().unwrap_or_else(|e| panic!("{e}: {t}"))
}

fn ffmpeg(args: &[&str]) {
    let o = std::process::Command::new("ffmpeg").args(["-y", "-v", "error"]).args(args).output().unwrap();
    assert!(o.status.success(), "ffmpeg {args:?}: {}", String::from_utf8_lossy(&o.stderr));
}

/// A 2 s 320×180 test video with a tone, and a 160×120 PNG.
fn media(dir: &Path) -> (PathBuf, PathBuf) {
    let v = dir.join("clip.mp4");
    let p = dir.join("photo.png");
    ffmpeg(&[
        "-f", "lavfi", "-i", "testsrc=size=320x180:rate=25:duration=2", "-f", "lavfi", "-i", "sine=frequency=440:duration=2",
        "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest", v.to_str().unwrap(),
    ]);
    ffmpeg(&["-f", "lavfi", "-i", "testsrc=size=160x120", "-frames:v", "1", p.to_str().unwrap()]);
    (v, p)
}

fn probe(path: &str) -> Json {
    let o = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-print_format", "json", "-show_streams", "-show_format", path])
        .output()
        .unwrap();
    serde_json::from_slice(&o.stdout).unwrap()
}

fn sha(path: &str) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(std::fs::read(path).unwrap()).iter().map(|b| format!("{b:02x}")).collect()
}

async fn session(c: &Client) -> String {
    call(c, "session", json!({"agentName": "Motion", "canvas": "main"})).await["sessionId"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn import_inspect_transform_rerun_export_package() {
    let env = start("http://127.0.0.1:1", None).await;
    let (video, photo) = media(env.dir.path());
    let c = client(env.port).await;
    let names: Vec<String> = c.list_all_tools().await.unwrap().iter().map(|t| t.name.to_string()).collect();
    for n in ["list_ops", "inspect", "transform", "compose", "rerun", "export", "package"] {
        assert!(names.contains(&n.to_string()), "missing {n}");
    }
    let sid = session(&c).await;
    let ops = call(&c, "list_ops", json!({})).await;
    let op_names: Vec<&str> = ops["ops"].as_array().unwrap().iter().map(|o| o["name"].as_str().unwrap()).collect();
    for n in ["crop", "resize", "reframe", "convert", "compress", "trim", "concat", "speed", "extract_frames", "gif",
              "audio_normalize", "audio_mix", "audio_extract", "strip", "remove_bg", "upscale", "inpaint", "ffmpeg", "grid", "contact_sheet"] {
        assert!(op_names.contains(&n), "missing op {n}");
    }

    // Imports are protected inputs with origin + media.
    let v = call(&c, "import_file", json!({"sessionId": sid, "canvas": "main", "path": video})).await;
    let p = call(&c, "import_file", json!({"sessionId": sid, "canvas": "main", "path": photo})).await;
    let (vn, pn) = (v["node"].as_str().unwrap().to_string(), p["node"].as_str().unwrap().to_string());
    let (node, asset) = env.host.read("main", |cv| {
        let n = cv.node(&vn).unwrap();
        (n.clone(), cv.asset(n.asset_id().unwrap()).unwrap())
    }).unwrap();
    assert!(node.is_protected());
    assert_eq!(node.role(), Some("input"));
    assert_eq!(asset.source, falos_doc::Source::Import);
    assert_eq!(asset.origin.as_ref().unwrap().filename, "clip.mp4");
    assert_eq!(asset.media.as_ref().unwrap().w, Some(320.0));
    let r = call_raw(&c, "edit", json!({"sessionId": sid, "canvas": "main", "text": format!("delete({vn})")})).await;
    assert_eq!(r.is_error, Some(true));
    assert!(text(&r).contains("derive instead"), "{}", text(&r));

    // inspect: probe + contact sheet (image block) + loudness; cached by sha.
    let r = call_raw(&c, "inspect", json!({"sessionId": sid, "canvas": "main", "inputs": vn, "what": ["probe", "contact_sheet", "loudness", "palette"]})).await;
    assert_ne!(r.is_error, Some(true), "{}", text(&r));
    assert!(r.content.iter().any(|b| b.as_image().is_some()), "no image block");
    let t = text(&r);
    let rows: Json = serde_json::Deserializer::from_str(&t).into_iter::<Json>().next().unwrap().unwrap();
    assert_eq!(rows[0]["probe"]["w"], 320.0);
    assert!(rows[0]["loudness"]["integrated_lufs"].is_number(), "{rows}");
    assert!(env.host.dir().join(".cache/inspect").read_dir().unwrap().count() >= 3);

    // crop (image, in-process) → a new derived node with lineage; the input is untouched.
    let r = call(&c, "transform", json!({"sessionId": sid, "canvas": "main", "op": "crop", "inputs": pn, "args": {"aspect": "1:1"}})).await;
    assert_eq!(r["results"][0]["status"], "done", "{r}");
    let crop = r["nodes"][0].as_str().unwrap().to_string();
    let (cn, ca) = env.host.read("main", |cv| {
        let n = cv.node(&crop).unwrap();
        (n.clone(), cv.asset(n.asset_id().unwrap()).unwrap())
    }).unwrap();
    assert_eq!(cn.kind, falos_doc::NodeKind::Image);
    assert_eq!(ca.source, falos_doc::Source::Derive);
    assert_eq!(ca.media.as_ref().unwrap().w, Some(120.0));
    let l = ca.lineage.unwrap();
    assert_eq!((l.op.as_str(), l.deterministic), ("crop", true));
    assert_eq!(l.parents[0].node.as_deref(), Some(pn.as_str()));
    assert_eq!(l.recipe["args"]["aspect"], "1:1");
    let read = call_raw(&c, "read", json!({"sessionId": sid, "canvas": "main"})).await;
    assert!(text(&read).contains(&format!("src(derive) by(@ops) ← op:crop{{1:1,center}} of {pn} by @motion")), "{}", text(&read));

    // reframe video 9:16 pad-blur → even dims, yuv420p, faststart, comment.
    let r = call(&c, "transform", json!({"sessionId": sid, "canvas": "main", "op": "reframe", "inputs": vn, "args": {"aspect": "9:16"}})).await;
    let _rf = r["nodes"][0].as_str().unwrap().to_string();
    let file = falos_host::chat::asset_file(&env.host, r["results"][0]["asset"].as_str().unwrap()).unwrap();
    let pr = probe(file.to_str().unwrap());
    let vs = pr["streams"].as_array().unwrap().iter().find(|s| s["codec_type"] == "video").unwrap().clone();
    assert_eq!((vs["width"].as_i64(), vs["height"].as_i64(), vs["pix_fmt"].as_str()), (Some(320), Some(568), Some("yuv420p")));
    assert!(pr["format"]["tags"]["comment"].as_str().unwrap().starts_with("falos:"));

    // trim via a range chip, extract_frames (multi-output), gif, audio_normalize, ffmpeg hatch.
    let r = call(&c, "transform", json!({"sessionId": sid, "canvas": "main", "op": "trim", "inputs": [{"kind": "range", "node": vn, "t0": 0.5, "t1": 1.5}]})).await;
    let tf = falos_host::chat::asset_file(&env.host, r["results"][0]["asset"].as_str().unwrap()).unwrap();
    let d: f64 = probe(tf.to_str().unwrap())["format"]["duration"].as_str().unwrap().parse().unwrap();
    assert!((0.8..1.3).contains(&d), "{d}");
    let r = call(&c, "transform", json!({"sessionId": sid, "canvas": "main", "op": "extract_frames", "inputs": vn, "args": {"count": 3}})).await;
    assert_eq!(r["results"][0]["nodes"].as_array().unwrap().len(), 3, "{r}");
    let r = call(&c, "transform", json!({"sessionId": sid, "canvas": "main", "op": "gif", "inputs": vn, "args": {"width": 160, "fps": 8}})).await;
    assert_eq!(r["results"][0]["status"], "done", "{r}");
    let r = call(&c, "transform", json!({"sessionId": sid, "canvas": "main", "op": "audio_normalize", "inputs": vn, "args": {"lufs": -16}})).await;
    assert_eq!(r["results"][0]["status"], "done", "{r}");
    let r = call(&c, "transform", json!({"sessionId": sid, "canvas": "main", "op": "ffmpeg", "inputs": [vn],
        "args": {"args": ["-i", "{in0}", "-vf", "hflip", "-an", "{out}"], "ext": "mp4"}})).await;
    assert_eq!(r["results"][0]["status"], "done", "{r}");
    let r = call(&c, "transform", json!({"sessionId": sid, "canvas": "main", "op": "concat", "inputs": [vn, vn], "args": {"xfade": 0.5}})).await;
    let cf = falos_host::chat::asset_file(&env.host, r["results"][0]["asset"].as_str().unwrap()).unwrap();
    let d: f64 = probe(cf.to_str().unwrap())["format"]["duration"].as_str().unwrap().parse().unwrap();
    assert!((3.2..3.9).contains(&d), "concat duration {d}");
    // Upscaling by resize is refused (never upscale silently).
    let r = call(&c, "transform", json!({"sessionId": sid, "canvas": "main", "op": "resize", "inputs": pn, "args": {"w": 640}})).await;
    assert_eq!(r["results"][0]["status"], "failed");
    assert!(r["results"][0]["error"].as_str().unwrap().contains("upscale"));

    // Batch: one output per input, placeholders at once with wait:false.
    let r = call(&c, "transform", json!({"sessionId": sid, "canvas": "main", "op": "convert", "inputs": [pn, crop], "args": {"format": "webp"}, "wait": false})).await;
    assert_eq!(r["nodes"].as_array().unwrap().len(), 2);
    assert_eq!(r["status"], "running");

    // rerun with overrides → a new crop with the other aspect.
    let r = call(&c, "rerun", json!({"sessionId": sid, "canvas": "main", "node": crop, "overrides": {"aspect": "4:3"}})).await;
    let re = r["results"][0]["asset"].as_str().unwrap().to_string();
    let w = env.host.read("main", |cv| cv.asset(&re).unwrap().media.unwrap().w).unwrap();
    assert_eq!(w, Some(160.0));

    // compose grid.
    let r = call(&c, "compose", json!({"sessionId": sid, "canvas": "main", "op": "grid", "inputs": [pn, crop, vn], "args": {"cell": 128}})).await;
    assert_eq!(r["results"][0]["status"], "done", "{r}");

    // export: reel (aspect mismatch → reframed final node), web-image @2x avif-less webp, original.
    let r = call(&c, "export", json!({"sessionId": sid, "canvas": "main", "nodes": [vn], "presets": ["reel", "original"]})).await;
    let files = r["files"].as_array().unwrap();
    assert_eq!(files.len(), 2, "{r}");
    assert!(r["warnings"].to_string().contains("reframed"), "{r}");
    let reel = &files[0];
    let path = reel["path"].as_str().unwrap();
    assert!(path.contains("/exports/"), "{path}");
    assert_eq!(reel["sha256"].as_str().unwrap(), sha(path));
    // Source is 320 wide: kept at source size, never upscaled.
    assert_eq!((reel["w"].as_u64(), reel["h"].as_u64()), (Some(320), Some(568)));
    let final_node = reel["node"].as_str().unwrap();
    assert_ne!(final_node, vn);
    assert_eq!(env.host.read("main", |cv| cv.node(final_node).unwrap().role().map(String::from)).unwrap().as_deref(), Some("final"));
    assert_eq!(files[1]["sha256"].as_str().unwrap(), sha(video.to_str().unwrap()));
    let manifest = std::fs::read_to_string(env.host.dir().join("exports/manifest.jsonl")).unwrap();
    assert_eq!(manifest.lines().count(), 2);
    assert!(manifest.contains("\"preset\":\"reel\""));
    // Same export again: Downloads-style names, v2.
    let r2 = call(&c, "export", json!({"sessionId": sid, "canvas": "main", "nodes": [pn], "presets": [{"preset": "web-image", "scale": "0.5x", "suffix": "@half"}, "original"]})).await;
    let f0 = r2["files"][0]["name"].as_str().unwrap();
    assert!(f0.ends_with("@half.webp"), "{f0}");
    let r3 = call(&c, "export", json!({"sessionId": sid, "canvas": "main", "nodes": [pn], "presets": [{"preset": "web-image", "scale": "0.5x", "suffix": "@half"}]})).await;
    assert!(r3["files"][0]["name"].as_str().unwrap().contains("_v2"), "{r3}");

    // The external agent's pseudo-chat got `files` events.
    let chat = env.host.with_sessions(|ss| ss.get(&sid).and_then(|s| s.chat.clone())).unwrap();
    let log = std::fs::read_to_string(env.host.dir().join("chats").join(format!("{}.jsonl", chat.replace(':', "_")))).unwrap_or_default();
    assert!(log.contains("\"kind\":\"files\""), "no files event in {chat}");

    // Deliverables: the reframed final node.
    let r = call(&c, "export", json!({"sessionId": sid, "canvas": "main", "nodes": "deliverables"})).await;
    assert!(r["dir"].as_str().unwrap().ends_with("/deliverables"));
    let r = call(&c, "package", json!({"sessionId": sid, "canvas": "main", "kind": "project"})).await;
    let zip_path = r["path"].as_str().unwrap();
    assert!(zip_path.ends_with(".zip") && r["bytes"].as_u64().unwrap() > 1000);
    let z = zip::ZipArchive::new(std::fs::File::open(zip_path).unwrap()).unwrap();
    let names: Vec<&str> = z.file_names().collect();
    assert!(names.contains(&"manifest.json") && names.contains(&"project.json"), "{names:?}");
    assert!(names.iter().any(|n| n.starts_with("assets/")));
}

#[tokio::test]
async fn rest_mirror_runs_as_the_human() {
    let env = start("http://127.0.0.1:1", None).await;
    let (_, photo) = media(env.dir.path());
    let c = client(env.port).await;
    let sid = session(&c).await;
    let p = call(&c, "import_file", json!({"sessionId": sid, "canvas": "main", "path": photo})).await;
    let pn = p["node"].as_str().unwrap().to_string();
    let http = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{}", env.port);
    let get = |p: &str| http.get(format!("{base}{p}")).bearer_auth(TOKEN).send();
    let ops: Json = get("/api/ops").await.unwrap().json().await.unwrap();
    assert!(ops["ops"].as_array().unwrap().len() > 15);
    let presets: Json = get("/api/presets").await.unwrap().json().await.unwrap();
    assert!(presets["version"].is_string());
    assert!(presets["presets"].as_array().unwrap().iter().any(|p| p["id"] == "reel"));
    assert_eq!(http.get(format!("{base}/api/ops")).send().await.unwrap().status(), 401);
    let post = |p: &str, b: Json| http.post(format!("{base}{p}?canvas=main&actor=lukas&name=Lukas")).bearer_auth(TOKEN).json(&b).send();
    let r: Json = post("/api/transform", json!({"op": "resize", "inputs": [pn], "args": {"w": 80}})).await.unwrap().json().await.unwrap();
    let node = r["nodes"][0].as_str().unwrap().to_string();
    assert_eq!(r["status"], "running");
    // Humans may touch protected inputs (e.g. delete); the op lands as the ops actor with lineage actor = lukas.
    let mut tries = 0;
    let asset = loop {
        let a = env.host.read("main", |cv| cv.node(&node).and_then(|n| n.asset_id().and_then(|a| cv.asset(a)))).unwrap();
        if let Some(a) = a {
            break a;
        }
        tries += 1;
        assert!(tries < 100, "op never finished");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    };
    assert_eq!(asset.lineage.unwrap().actor, "lukas");
    let r: Json = post("/api/inspect", json!({"inputs": pn, "what": ["palette"]})).await.unwrap().json().await.unwrap();
    assert!(r["images"][0]["data"].as_str().unwrap().starts_with("data:image/jpeg;base64,"));
    let r: Json = post("/api/export", json!({"nodes": [node], "presets": ["original"], "dir": "out"})).await.unwrap().json().await.unwrap();
    assert!(r["files"][0]["path"].as_str().unwrap().contains("/proj/out/"), "{r}");
    let r = post("/api/transform", json!({"op": "nope", "inputs": [pn]})).await.unwrap();
    assert_eq!(r.status(), 400);

    // Wider import types: md → text node, srt → document, heic → PNG proxy + original kept,
    // and lineage on import (a Bash result) → derived, unprotected.
    let md = env.dir.path().join("brief.md");
    std::fs::write(&md, "# Brief\nmake it pop").unwrap();
    let t = call(&c, "import_file", json!({"sessionId": sid, "canvas": "main", "path": md})).await;
    assert_eq!(t["type"], "text");
    let srt = env.dir.path().join("subs.srt");
    std::fs::write(&srt, "1\n00:00:00,000 --> 00:00:01,000\nhi\n").unwrap();
    assert_eq!(call(&c, "import_file", json!({"sessionId": sid, "canvas": "main", "path": srt})).await["type"], "document");
    let heic = env.dir.path().join("phone.heic");
    if std::process::Command::new("sips").args(["-s", "format", "heic"]).arg(&photo).arg("--out").arg(&heic).output().is_ok_and(|o| o.status.success()) {
        let h = call(&c, "import_file", json!({"sessionId": sid, "canvas": "main", "path": heic})).await;
        assert!(h["file"].as_str().unwrap().ends_with(".png"), "{h}");
        let n = env.host.read("main", |cv| cv.node(h["node"].as_str().unwrap()).unwrap()).unwrap();
        assert!(n.props["original"].as_str().unwrap().starts_with("sha256:"));
    }
    let flipped = env.dir.path().join("flipped.png");
    ffmpeg(&["-i", photo.to_str().unwrap(), "-vf", "hflip", flipped.to_str().unwrap()]);
    let d = call(&c, "import_file", json!({"sessionId": sid, "canvas": "main", "path": flipped, "name": "flipped",
        "lineage": {"kind": "derive", "op": "ffmpeg", "recipe": {"args": ["-vf", "hflip"]}, "parents": [pn]}})).await;
    let (dn, da) = env.host.read("main", |cv| {
        let n = cv.node(d["node"].as_str().unwrap()).unwrap();
        (n.clone(), cv.asset(n.asset_id().unwrap()).unwrap())
    }).unwrap();
    assert!(!dn.is_protected());
    assert_eq!(da.source, falos_doc::Source::Derive);
    let l = da.lineage.unwrap();
    assert_eq!((l.op.as_str(), l.parents[0].node.as_deref()), ("ffmpeg", Some(pn.as_str())));
}

/// A mock fal queue + storage for fal-backed ops and mediaInputs.
async fn mock_fal() -> String {
    use axum::extract::Path as P;
    use axum::{Json as AJson, Router, routing::{get, post, put}};
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let png = {
        let mut img = image::RgbaImage::new(32, 32);
        for (x, _, p) in img.enumerate_pixels_mut() {
            *p = image::Rgba([200, 10, 10, if x < 16 { 255 } else { 0 }]);
        }
        let mut buf = std::io::Cursor::new(vec![]);
        img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
        buf.into_inner()
    };
    let b = base.clone();
    let submit = move |P(model): P<String>, AJson(input): AJson<Json>| {
        let b = b.clone();
        async move {
            assert!(input.to_string().contains("https://"), "{input}");
            let id = format!("r-{}", model.replace('/', "_"));
            AJson(json!({"request_id": id, "status_url": format!("{b}/status/{id}"), "response_url": format!("{b}/result/{id}")}))
        }
    };
    let b2 = base.clone();
    let app = Router::new()
        .route("/fal-ai/{*model}", post(submit))
        .route("/status/{id}", get(|| async { AJson(json!({"status": "COMPLETED"})) }))
        .route("/result/{id}", get(move |P(id): P<String>| {
            let b = b2.clone();
            async move { AJson(json!({"seed": 5, "image": {"url": format!("{b}/out/{id}.png"), "content_type": "image/png", "width": 32, "height": 32}})) }
        }))
        .route("/out/{f}", get(move |P(f): P<String>| {
            let mut png = png.clone();
            png.extend_from_slice(f.as_bytes());
            async move { png }
        }))
        .route("/storage/upload/initiate", post({
            let b = base.clone();
            move || {
                let b = b.clone();
                async move { AJson(json!({"upload_url": format!("{b}/put/x"), "file_url": "https://cdn.example/x.png"})) }
            }
        }))
        .route("/put/{f}", put(|| async { "ok" }));
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    base
}

#[tokio::test]
async fn fal_ops_and_media_inputs() {
    let base = mock_fal().await;
    // SAFETY: set once, before any request uses it; only this test reads it.
    unsafe { std::env::set_var("FALOS_FAL_REST_URL", &base) };
    let env = start(&base, Some("test-key")).await;
    let (_, photo) = media(env.dir.path());
    let c = client(env.port).await;
    let sid = session(&c).await;
    let p = call(&c, "import_file", json!({"sessionId": sid, "canvas": "main", "path": photo})).await;
    let pn = p["node"].as_str().unwrap().to_string();
    // remove_bg: uploads the input once (uri cached), runs the model, records lineage + cost.
    let r = call(&c, "transform", json!({"sessionId": sid, "canvas": "main", "op": "remove_bg", "inputs": pn})).await;
    assert_eq!(r["results"][0]["status"], "done", "{r}");
    let a = env.host.read("main", |cv| cv.asset(r["results"][0]["asset"].as_str().unwrap()).unwrap()).unwrap();
    let l = a.lineage.unwrap();
    assert_eq!(l.engine_version.as_deref(), Some("fal-ai/birefnet/v2"));
    assert!(l.fal_request_id.is_some() && l.cost.is_some() && !l.deterministic);
    let uri = env.host.read("main", |cv| cv.asset(p["asset"].as_str().unwrap()).unwrap().uri).unwrap();
    assert_eq!(uri.as_deref(), Some("https://cdn.example/x.png"));
    // generate with mediaInputs (node + region chip) → image_url filled, parents recorded.
    let g = call(&c, "generate", json!({"sessionId": sid, "canvas": "main", "model": "fal-ai/mock-edit", "input": {"prompt": "x"},
        "mediaInputs": {"image_url": pn, "mask_url": {"kind": "region", "node": pn, "box": [0, 0, 0.5, 0.5]}}})).await;
    let gn = g["node"].as_str().unwrap().to_string();
    let mut asset = None;
    for _ in 0..100 {
        asset = env.host.read("main", |cv| cv.node(&gn).and_then(|n| n.asset_id().and_then(|a| cv.asset(a)))).unwrap();
        if asset.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let l = asset.expect("generation finished").lineage.unwrap();
    assert_eq!(l.recipe["image_url"], "https://cdn.example/x.png");
    assert_eq!(l.parents.len(), 2);
    assert_eq!(l.parents[0].node.as_deref(), Some(pn.as_str()));
    assert_eq!(l.parents[1].role, falos_doc::ParentRole::Mask);
}
