//! Media facts via ffprobe (with an in-process image header fallback).

use std::path::Path;

use anyhow::{Context, Result, bail};
use falos_doc::Media;
use serde_json::{Value as Json, json};

pub fn ffprobe_bin() -> String {
    std::env::var("FALOS_FFPROBE").unwrap_or_else(|_| "ffprobe".into())
}

pub fn ffmpeg_bin() -> String {
    std::env::var("FALOS_FFMPEG").unwrap_or_else(|_| "ffmpeg".into())
}

/// Raw `ffprobe -show_streams -show_format` JSON.
pub async fn ffprobe_json(path: &Path) -> Result<Json> {
    let o = tokio::process::Command::new(ffprobe_bin())
        .args(["-v", "error", "-print_format", "json", "-show_streams", "-show_format"])
        .arg(path)
        .output()
        .await
        .context("running ffprobe (install ffmpeg)")?;
    if !o.status.success() {
        bail!("ffprobe: {}", String::from_utf8_lossy(&o.stderr).trim());
    }
    Ok(serde_json::from_slice(&o.stdout)?)
}

fn rate(s: &str) -> Option<f64> {
    let (a, b) = s.split_once('/')?;
    let (a, b): (f64, f64) = (a.parse().ok()?, b.parse().ok()?);
    (b > 0.0 && a > 0.0).then(|| (a / b * 1000.0).round() / 1000.0)
}

/// Full probe: `media` facts plus a compact report (streams, format, tags) for `inspect`.
pub async fn probe(path: &Path) -> Result<(Media, Json)> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    // Still images: decode the header in-process (ffprobe reports odd fps/durations for them).
    if matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp")
        && let Ok(m) = image_media(path)
        && ext != "gif"
    {
        let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let report = json!({"kind": "image", "format": ext, "w": m.w, "h": m.h, "alpha": m.alpha,
                            "color": m.color, "bytes": bytes});
        return Ok((m, report));
    }
    let v = ffprobe_json(path).await?;
    let mut m = Media::default();
    let mut streams = vec![];
    for s in v["streams"].as_array().into_iter().flatten() {
        let ty = s["codec_type"].as_str().unwrap_or("");
        let codec = s["codec_name"].as_str().unwrap_or("").to_string();
        match ty {
            "video" if m.w.is_none() => {
                m.w = s["width"].as_f64();
                m.h = s["height"].as_f64();
                m.fps = s["avg_frame_rate"].as_str().and_then(rate).or_else(|| s["r_frame_rate"].as_str().and_then(rate));
                m.codec = Some(codec.clone());
                let pix = s["pix_fmt"].as_str().unwrap_or("");
                m.alpha = Some(pix.contains('a') && (pix.starts_with("yuva") || pix.starts_with("rgba") || pix.starts_with("argb") || pix.starts_with("bgra") || pix.contains("gbrap") || pix == "pal8"));
                let space = s["color_space"].as_str().or(s["color_primaries"].as_str());
                let trc = s["color_transfer"].as_str();
                m.color = match (space, trc) {
                    (Some(sp), Some(t)) if t != sp && t != "bt709" => Some(format!("{sp}/{t}")),
                    (Some(sp), _) => Some(sp.to_string()),
                    _ => None,
                };
            }
            "audio" if m.codec.is_none() => m.codec = Some(codec.clone()),
            _ => {}
        }
        streams.push(json!({
            "type": ty, "codec": codec, "profile": s["profile"], "w": s["width"], "h": s["height"],
            "pix_fmt": s["pix_fmt"], "fps": s["avg_frame_rate"].as_str().and_then(rate),
            "bitrate": s["bit_rate"].as_str().and_then(|b| b.parse::<u64>().ok()),
            "sample_rate": s["sample_rate"].as_str().and_then(|b| b.parse::<u64>().ok()),
            "channels": s["channels"], "color_space": s["color_space"], "color_transfer": s["color_transfer"],
            "color_primaries": s["color_primaries"], "frames": s["nb_frames"].as_str().and_then(|b| b.parse::<u64>().ok()),
        }));
    }
    m.duration = v["format"]["duration"]
        .as_str()
        .and_then(|d| d.parse::<f64>().ok())
        .filter(|d| *d > 0.0);
    let hdr = m.color.as_deref().is_some_and(|c| c.contains("smpte2084") || c.contains("arib-std-b67"));
    let report = json!({
        "kind": if m.w.is_some() { if m.duration.is_some_and(|d| d > 0.05) && ext != "png" { "video" } else { "image" } } else { "audio" },
        "format": v["format"]["format_name"], "duration": m.duration, "w": m.w, "h": m.h, "fps": m.fps,
        "codec": m.codec, "alpha": m.alpha, "color": m.color, "hdr": hdr,
        "bitrate": v["format"]["bit_rate"].as_str().and_then(|b| b.parse::<u64>().ok()),
        "bytes": v["format"]["size"].as_str().and_then(|b| b.parse::<u64>().ok()),
        "tags": v["format"]["tags"], "streams": streams,
    });
    Ok((m, report))
}

/// Width/height/alpha of a still image without decoding pixels where possible.
pub fn image_media(path: &Path) -> Result<Media> {
    use image::ImageDecoder;
    let dec = image::ImageReader::open(path)?
        .with_guessed_format()?
        .into_decoder()?;
    let (w, h) = dec.dimensions();
    Ok(Media {
        w: Some(w as f64),
        h: Some(h as f64),
        alpha: Some(dec.color_type().has_alpha()),
        color: Some("srgb".into()),
        ..Default::default()
    })
}

/// `probe` that never fails (empty media on error).
pub async fn media_of(path: &Path) -> Media {
    probe(path).await.map(|(m, _)| m).unwrap_or_default()
}
