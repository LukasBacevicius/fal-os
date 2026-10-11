//! Live fal library writes with a real key (ignored; run explicitly):
//! `FAL_KEY=… cargo test -p falos-host --test library_live_fal -- --ignored`
//! Everything it creates is named "fal-os test (delete me)" and deleted afterwards (the CDN
//! file of the uploaded 64×64 test PNG stays: fal has no API to delete storage files).

use falos_host::library::fal::FalApi;

/// `FAL_KEY`, else the one in `~/.fal-os/fal.env` (read only).
fn fal_key() -> String {
    std::env::var("FAL_KEY").ok().filter(|k| !k.is_empty()).unwrap_or_else(|| {
        let p = std::path::PathBuf::from(std::env::var("HOME").unwrap()).join(".fal-os/fal.env");
        let t = std::fs::read_to_string(p).expect("FAL_KEY or ~/.fal-os/fal.env");
        t.lines()
            .find_map(|l| l.trim().trim_start_matches("export ").strip_prefix("FAL_KEY=").map(|v| v.trim().trim_matches(|c| c == '"' || c == '\'').to_string()))
            .expect("FAL_KEY in fal.env")
    })
}

#[tokio::test]
#[ignore]
async fn live_fal_writes() {
    let key = fal_key();
    let api = FalApi::new(reqwest::Client::new(), key);
    // 64×64 orange PNG.
    let png = {
        let img = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-f", "lavfi", "-i", "color=c=0xff8800:s=64x64", "-frames:v", "1", "-f", "image2pipe", "-c:v", "png", "-"])
            .output()
            .unwrap();
        img.stdout
    };
    let sha: String = {
        use sha2::Digest;
        sha2::Sha256::digest(&png).iter().map(|b| format!("{b:02x}")).collect()
    };
    let root = api.create_collection("fal-os test (delete me)", None).await.expect("create collection");
    let child = api.create_collection("fal-os test (delete me) project", Some(&root)).await.expect("create nested collection");
    let url = api.upload_file(png, "image/png", "falos-test.png").await.expect("storage upload");
    let asset = api.register_upload(&url, "image", Some("fal-os test (delete me)"), Some(&child), &sha).await.expect("register upload");
    let id = asset["asset_id"].as_str().unwrap().to_string();
    let again = api.register_upload(&url, "image", Some("fal-os test (delete me)"), Some(&child), &sha).await.expect("idempotent register");
    assert_eq!(again["asset_id"], asset["asset_id"], "Idempotency-Key = sha");
    api.favorite(&id, true).await.expect("favorite");
    let tag = api.create_tag("falos-test-delete-me").await.expect("create tag");
    api.set_tags(&id, std::slice::from_ref(&tag)).await.expect("set tags");
    api.add_to_collection(&root, &id).await.expect("add to collection");
    let (found, _) = api.list(&[("collection_id", child.clone()), ("limit", "10".into())]).await.expect("list");
    assert!(found.iter().any(|a| a["asset_id"] == id.as_str()));
    // Clean up.
    api.delete_asset(&id).await.expect("delete asset");
    let tags_url = format!("{}/assets/tags/{tag}", api.api);
    let key = fal_key();
    let r = reqwest::Client::new().delete(tags_url).header("Authorization", format!("Key {key}")).send().await.unwrap();
    assert!(r.status().is_success(), "delete tag: {}", r.status());
    api.delete_collection(&child).await.expect("delete nested collection");
    api.delete_collection(&root).await.expect("delete collection");
}
