//! A synthetic library for perf work:
//! `FALOS_SYNTH_HOME=<dir> FALOS_SYNTH_N=5000 cargo test -p falos-host --lib synth_library -- --ignored`
//! writes rows + 256/1024 thumbnails (half of them in project `FALOS_SYNTH_PROJECT`, default demo).

#[test]
#[ignore]
fn synth_library() {
    use image::{Rgba, RgbaImage};
    let home = std::path::PathBuf::from(std::env::var("FALOS_SYNTH_HOME").expect("FALOS_SYNTH_HOME"));
    let n: usize = std::env::var("FALOS_SYNTH_N").ok().and_then(|v| v.parse().ok()).unwrap_or(5000);
    let project = std::env::var("FALOS_SYNTH_PROJECT").unwrap_or_else(|_| "demo".into());
    let c = super::db::open(&home.join("library.db")).unwrap();
    let thumbs = home.join("thumbs");
    std::fs::create_dir_all(&thumbs).unwrap();
    let words = ["fox", "neon", "city", "portrait", "forest", "ocean", "desert", "robot", "flower", "mountain", "night", "studio", "glass", "bee", "car", "sunset"];
    let models = ["fal-ai/flux/dev", "fal-ai/kling-video/v2.5-turbo/pro/image-to-video", "fal-ai/nano-banana", "fal-ai/veo3"];
    let aspects = [(16, 9), (9, 16), (1, 1), (4, 3), (3, 4), (21, 9), (2, 3)];
    let mut rng = 0x2545F4914F6CDD1Du64;
    let mut r = || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    c.execute_batch("BEGIN").unwrap();
    for i in 0..n {
        let sha = format!("{:064x}", (i as u128 + 1) * 0x9E3779B97F4A7C15u128);
        let (aw, ah) = aspects[(r() % aspects.len() as u64) as usize];
        let kind = match r() % 20 {
            0 => "audio",
            1..=3 => "video",
            _ => "image",
        };
        let source = ["import", "generate", "generate", "derive"][(r() % 4) as usize];
        let (w, h) = (aw * 120, ah * 120);
        let prompt = format!("{} {} {}", words[(r() % 16) as usize], words[(r() % 16) as usize], words[(r() % 16) as usize]);
        let base = [(r() % 256) as u8, (r() % 256) as u8, (r() % 256) as u8];
        let th = 256u32;
        let tw = if kind == "audio" { 512 } else { (256 * aw / ah) as u32 };
        let img = RgbaImage::from_fn(tw, th, |x, y| {
            let t = (x + y) as f32 / (tw + th) as f32;
            Rgba([(base[0] as f32 * (0.4 + 0.6 * t)) as u8, (base[1] as f32 * (1.0 - 0.5 * t)) as u8, base[2], 255])
        });
        let webp = super::thumbs::encode_webp(&img, 70.0).unwrap();
        std::fs::write(thumbs.join(format!("{sha}-256.webp")), &webp).unwrap();
        std::fs::write(thumbs.join(format!("{sha}-1024.webp")), &webp).unwrap();
        let pal = serde_json::json!(super::thumbs::palette(&img, 6).iter().map(|(h, w)| serde_json::json!({"hex": h, "w": w})).collect::<Vec<_>>()).to_string();
        let mime = match kind {
            "audio" => "audio/mpeg",
            "video" => "video/mp4",
            _ => "image/png",
        };
        let dims = (kind != "audio").then_some((w, h));
        c.execute(
            "INSERT OR REPLACE INTO assets(sha,kind,mime,w,h,duration,source,prompt,model,palette,tiny,created,updated,thumb) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,1)",
            rusqlite::params![
                sha,
                kind,
                mime,
                dims.map(|d| d.0),
                dims.map(|d| d.1),
                (kind != "image").then_some(5.0 + (i % 30) as f64),
                source,
                prompt,
                (source == "generate").then(|| models[i % 4]),
                pal,
                super::thumbs::tiny(&img),
                1_790_000_000_000i64 + i as i64 * 1000,
                0
            ],
        )
        .unwrap();
        if i % 2 == 0 {
            c.execute("INSERT OR IGNORE INTO asset_projects VALUES(?,?)", rusqlite::params![sha, project]).unwrap();
        }
        super::db::reindex(&c, &sha).unwrap();
    }
    c.execute_batch("COMMIT").unwrap();
}
