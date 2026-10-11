//! `inspect`: read-only facts about media, cached by sha in `<project>/.cache/inspect/`.
//! Visual results (contact sheets, palettes) come back as JPEG previews (MCP image blocks).

use std::path::PathBuf;

use anyhow::{Result, bail};
use serde_json::{Value as Json, json};

use super::{Input, ff, img, prepare, probe, resolve_inputs};
use crate::host::Host;

pub const WHAT: &[&str] = &["probe", "contact_sheet", "palette", "loudness", "scenes", "compare"];

/// A visual result: caption + JPEG bytes.
pub struct Visual {
    pub caption: String,
    pub jpeg: Vec<u8>,
}

fn cache_dir(host: &Host) -> PathBuf {
    host.dir().join(".cache").join("inspect")
}

fn cache_key(i: &Input, what: &str) -> Option<String> {
    // Chipped inputs aren't the stored bytes: don't cache them.
    i.chip.is_none().then(|| format!("{}-{what}", i.asset.trim_start_matches("sha256:")))
}

fn read_cache(host: &Host, key: &str) -> Option<(Json, Option<Vec<u8>>)> {
    let d = cache_dir(host);
    let v = serde_json::from_slice(&std::fs::read(d.join(format!("{key}.json"))).ok()?).ok()?;
    Some((v, std::fs::read(d.join(format!("{key}.jpg"))).ok()))
}

fn write_cache(host: &Host, key: &str, v: &Json, jpeg: Option<&[u8]>) {
    let d = cache_dir(host);
    if std::fs::create_dir_all(&d).is_err() {
        return;
    }
    let _ = std::fs::write(d.join(format!("{key}.json")), serde_json::to_vec(v).unwrap_or_default());
    if let Some(j) = jpeg {
        let _ = std::fs::write(d.join(format!("{key}.jpg")), j);
    }
}

/// Inspect every input for every `what`. Returns `[{node, asset, <what>: …}]` and visuals.
pub async fn inspect(host: &Host, canvas: &str, inputs: &Json, what: &[String]) -> Result<(Json, Vec<Visual>)> {
    let what: Vec<String> = if what.is_empty() { vec!["probe".into()] } else { what.to_vec() };
    for w in &what {
        if !WHAT.contains(&w.as_str()) {
            bail!("unknown inspect `{w}` ({}); transcript/describe aren't available in the host yet", WHAT.join(", "));
        }
    }
    let ins = resolve_inputs(host, canvas, inputs)?;
    let tmp = std::env::temp_dir().join(format!("falos-inspect-{}", rand::random::<u64>()));
    std::fs::create_dir_all(&tmp)?;
    let r = run(host, &ins, &what, &tmp).await;
    let _ = std::fs::remove_dir_all(&tmp);
    r
}

async fn run(host: &Host, ins: &[Input], what: &[String], tmp: &std::path::Path) -> Result<(Json, Vec<Visual>)> {
    let mut rows = vec![];
    let mut visuals = vec![];
    let mut prepared = vec![];
    for (k, i) in ins.iter().enumerate() {
        let sub = tmp.join(format!("in{k}"));
        std::fs::create_dir_all(&sub)?;
        prepared.push(prepare(i, &sub).await?);
    }
    for (k, p) in prepared.iter().enumerate() {
        let i = &p.input;
        let mut row = json!({"node": i.node, "asset": i.asset, "name": i.name});
        if i.chip.is_some() {
            row["chip"] = i.chip.clone().unwrap_or_default();
        }
        for w in what {
            if w == "compare" {
                continue;
            }
            let key = cache_key(i, w);
            if let Some((v, jpg)) = key.as_deref().and_then(|k| read_cache(host, k)) {
                if let Some(j) = jpg {
                    visuals.push(Visual { caption: format!("{w} of {}", i.node.as_deref().unwrap_or(&i.asset)), jpeg: j });
                }
                row[w] = v;
                continue;
            }
            let (v, jpg): (Json, Option<Vec<u8>>) = match w.as_str() {
                "probe" => (probe::probe(&p.file).await?.1, None),
                "loudness" => {
                    if !ff::has_audio(p) {
                        (json!({"error": "no audio"}), None)
                    } else {
                        let m = ff::loudness_measure(&p.file, None).await?;
                        let f = |k: &str| m[k].as_str().and_then(|s| s.parse::<f64>().ok());
                        (json!({"integrated_lufs": f("input_i"), "true_peak_db": f("input_tp"), "lra": f("input_lra"), "threshold": f("input_thresh")}), None)
                    }
                }
                "scenes" => {
                    if !ff::has_video(p) || p.kind != falos_doc::NodeKind::Video {
                        (json!({"error": "not a video"}), None)
                    } else {
                        let cuts = ff::scenes(&p.file, 10.0).await?;
                        (json!({"cuts": cuts, "count": cuts.len() + 1, "duration": p.media.duration}), None)
                    }
                }
                "palette" => {
                    let f = if p.kind == falos_doc::NodeKind::Video {
                        ff::still(&p.file, p.media.duration.unwrap_or(0.0) * 0.5, tmp).await?
                    } else {
                        p.file.clone()
                    };
                    let im = img::load(&f).await?;
                    let pal = img::palette(&im, 6);
                    let jpg = img::preview_jpeg(&img::swatches(&pal), 512)?;
                    (json!(pal.iter().map(|(h, s)| json!({"hex": h, "share": s})).collect::<Vec<_>>()), Some(jpg))
                }
                "contact_sheet" => {
                    let im = if p.kind == falos_doc::NodeKind::Video {
                        let sub = tmp.join(format!("cs{k}"));
                        std::fs::create_dir_all(&sub)?;
                        let frames = ff::extract_frames(p, &json!({"count": 12, "format": "jpg"}), &sub).await?;
                        let mut imgs = vec![];
                        for f in &frames {
                            imgs.push(img::load(f).await?);
                        }
                        img::grid(&imgs, 4, 320, 6, img::color(Some("#111111")))?
                    } else if p.kind == falos_doc::NodeKind::Image {
                        img::load(&p.file).await?
                    } else {
                        row[w] = json!({"error": "not visual"});
                        continue;
                    };
                    let dur = p.media.duration.unwrap_or(0.0);
                    let times: Vec<f64> = (0..12).map(|i| ((dur * (i as f64 + 0.5) / 12.0) * 100.0).round() / 100.0).collect();
                    let v = if p.kind == falos_doc::NodeKind::Video { json!({"frames": 12, "cols": 4, "times": times}) } else { json!({"w": p.media.w, "h": p.media.h}) };
                    (v, Some(img::preview_jpeg(&im, 1280)?))
                }
                _ => continue,
            };
            if let Some(j) = &jpg {
                visuals.push(Visual { caption: format!("{w} of {}", i.node.as_deref().unwrap_or(&i.asset)), jpeg: j.clone() });
            }
            if let Some(key) = key {
                write_cache(host, &key, &v, jpg.as_deref());
            }
            row[w] = v;
        }
        rows.push(row);
    }
    if what.iter().any(|w| w == "compare") {
        if prepared.len() < 2 {
            bail!("compare needs 2 inputs");
        }
        let v = compare(&prepared[0], &prepared[1]).await?;
        return Ok((json!({"inputs": rows, "compare": v}), visuals));
    }
    Ok((json!(rows), visuals))
}

/// SSIM / PSNR of b against a (scaled to a's size).
async fn compare(a: &super::Prepared, b: &super::Prepared) -> Result<Json> {
    let (w, h) = (a.media.w.unwrap_or(0.0) as i64, a.media.h.unwrap_or(0.0) as i64);
    if w == 0 || h == 0 {
        bail!("compare needs visual inputs");
    }
    let o = tokio::process::Command::new(probe::ffmpeg_bin())
        .args(["-hide_banner", "-nostdin", "-i"])
        .arg(&a.file)
        .arg("-i")
        .arg(&b.file)
        .args(["-filter_complex", &format!("[1:v]scale={w}:{h},format=yuv420p,split[b1][b2];[0:v]format=yuv420p,split[a1][a2];[a1][b1]ssim;[a2][b2]psnr"), "-f", "null", "-"])
        .output()
        .await?;
    let err = String::from_utf8_lossy(&o.stderr);
    let grab = |key: &str| -> Option<f64> {
        let l = err.lines().rev().find(|l| l.contains(key))?;
        let i = l.find(key)? + key.len();
        l[i..].split_whitespace().next()?.trim_start_matches(':').parse().ok()
    };
    Ok(json!({"ssim": grab("All:"), "psnr_db": grab("average:")}))
}
