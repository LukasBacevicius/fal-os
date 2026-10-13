//! Thumbnails, palettes and tiny similarity vectors. Originals are never decoded in the grid:
//! the UI only loads `/thumbs/<sha>-256.webp` (and `-1024` for quick look / info, `-strip` for
//! video hover scrub).
//!
//! - images: decoded and resized in-process (`image`), ffmpeg as a fallback decoder
//!   (avif/heic/tiff/…); lossy WebP via libwebp.
//! - video: 12 frames sampled across the clip (input seeking, so remote URLs only fetch what
//!   they need); the poster is the frame with the best contrast and mid exposure (never a black
//!   or flat opening frame); the 12 frames also make the 4×3 hover strip.
//! - audio: a mono waveform.
//!
//! `palette`: k-means (k=6) in Lab over the 256 thumbnail; `tiny`: an 8×8 RGB vector for local
//! "find similar".

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use image::{DynamicImage, GenericImageView, RgbaImage, imageops::FilterType};

use crate::ops::probe::{ffmpeg_bin, ffprobe_bin};

pub const SMALL: u32 = 256;
pub const LARGE: u32 = 1024;

/// What a thumbnail job learned about the asset.
#[derive(Default, Debug, Clone)]
pub struct Made {
    pub w: Option<i64>,
    pub h: Option<i64>,
    pub duration: Option<f64>,
    /// `[(hex, weight)]`, heaviest first.
    pub palette: Vec<(String, f32)>,
    pub tiny: Option<Vec<u8>>,
}

pub fn file_name(key: &str, size: &str) -> String {
    format!("{key}-{size}.webp")
}

/// `<key>-<256|1024|strip>.webp` → (key, size).
pub fn parse_name(name: &str) -> Option<(String, &'static str)> {
    let stem = name.strip_suffix(".webp")?;
    let (key, size) = stem.rsplit_once('-')?;
    let size = match size {
        "256" => "256",
        "1024" => "1024",
        "strip" => "strip",
        _ => return None,
    };
    valid_key(key).then(|| (key.to_string(), size))
}

/// A sha256 hex digest or `fal_<assetRecordId>`.
pub fn valid_key(key: &str) -> bool {
    (key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()))
        || key.strip_prefix("fal_").is_some_and(|r| !r.is_empty() && r.len() <= 64 && r.bytes().all(|b| b.is_ascii_alphanumeric()))
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension(format!("tmp{}", rand::random::<u32>()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn encode_webp(img: &RgbaImage, quality: f32) -> Result<Vec<u8>> {
    let enc = webp::Encoder::from_rgba(img.as_raw(), img.width(), img.height());
    let mem = enc.encode_simple(false, quality).map_err(|e| anyhow!("webp encode: {e:?}"))?;
    Ok(mem.to_vec())
}

/// Scale so the height is at most `h` and the width at most `3h` (keeps very wide images
/// readable in a justified row); never upscales.
fn fit_height(img: &DynamicImage, h: u32) -> DynamicImage {
    let (w0, h0) = img.dimensions();
    let s = (h as f64 / h0 as f64).min(3.0 * h as f64 / w0 as f64).min(1.0);
    if s >= 1.0 {
        return img.clone();
    }
    let (w, h) = (((w0 as f64 * s).round() as u32).max(1), ((h0 as f64 * s).round() as u32).max(1));
    img.resize_exact(w, h, FilterType::Triangle)
}

fn fit_box(img: &DynamicImage, side: u32) -> DynamicImage {
    let (w0, h0) = img.dimensions();
    if w0 <= side && h0 <= side {
        return img.clone();
    }
    img.resize(side, side, FilterType::Triangle)
}

/// Decode image bytes; ffmpeg handles what the `image` crate can't.
pub fn decode_image(bytes: &[u8], path: Option<&Path>) -> Result<DynamicImage> {
    if let Ok(img) = image::load_from_memory(bytes) {
        return Ok(img);
    }
    let src = match path {
        Some(p) => p.to_path_buf(),
        None => {
            let tmp = std::env::temp_dir().join(format!("falos-dec-{}", rand::random::<u64>()));
            std::fs::write(&tmp, bytes)?;
            tmp
        }
    };
    let out = Command::new(ffmpeg_bin())
        .args(["-v", "error", "-i"])
        .arg(&src)
        .args(["-frames:v", "1", "-vf", "scale='min(2048,iw)':-2", "-f", "image2pipe", "-c:v", "png", "-"])
        .output();
    if path.is_none() {
        let _ = std::fs::remove_file(&src);
    }
    let out = out.context("running ffmpeg (is it installed?)")?;
    if !out.status.success() || out.stdout.is_empty() {
        bail!("can't decode image: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(image::load_from_memory(&out.stdout)?)
}

/// 256 + 1024 thumbnails of a decoded image, plus palette and tiny vector.
pub fn image_thumbs(img: &DynamicImage, dir: &Path, key: &str) -> Result<Made> {
    let (w, h) = img.dimensions();
    let small = fit_height(img, SMALL).to_rgba8();
    let large = fit_box(img, LARGE).to_rgba8();
    write_atomic(&dir.join(file_name(key, "1024")), &encode_webp(&large, 82.0)?)?;
    write_atomic(&dir.join(file_name(key, "256")), &encode_webp(&small, 78.0)?)?;
    Ok(Made { w: Some(w as i64), h: Some(h as i64), duration: None, palette: palette(&small, 6), tiny: Some(tiny(&small)) })
}

// ---- video -----------------------------------------------------------------------------------

#[derive(Default, Debug)]
pub struct Probe {
    pub w: Option<i64>,
    pub h: Option<i64>,
    pub duration: Option<f64>,
    pub video: bool,
    pub audio: bool,
}

/// ffprobe a local path or URL.
pub fn probe(src: &str) -> Result<Probe> {
    let out = Command::new(ffprobe_bin())
        .args(["-v", "error", "-show_entries", "stream=codec_type,width,height,duration:format=duration", "-of", "json", src])
        .output()
        .context("running ffprobe (is ffmpeg installed?)")?;
    if !out.status.success() {
        bail!("ffprobe: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let mut p = Probe::default();
    for s in v["streams"].as_array().into_iter().flatten() {
        match s["codec_type"].as_str() {
            Some("video") if !p.video => {
                p.video = true;
                p.w = s["width"].as_i64();
                p.h = s["height"].as_i64();
                p.duration = s["duration"].as_str().and_then(|d| d.parse().ok());
            }
            Some("audio") => p.audio = true,
            _ => {}
        }
    }
    if let Some(d) = v["format"]["duration"].as_str().and_then(|d| d.parse::<f64>().ok()) {
        p.duration = Some(d);
    }
    Ok(p)
}

fn frame_at(src: &str, t: f64, vf: &str) -> Result<DynamicImage> {
    let out = Command::new(ffmpeg_bin())
        .args(["-v", "error", "-ss", &format!("{t:.3}"), "-i", src, "-frames:v", "1", "-vf", vf, "-f", "image2pipe", "-c:v", "png", "-"])
        .output()
        .context("running ffmpeg")?;
    if !out.status.success() || out.stdout.is_empty() {
        bail!("ffmpeg frame @{t:.2}s: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(image::load_from_memory(&out.stdout)?)
}

/// How good a poster a frame makes: contrast (luma std-dev) at a mid exposure, with some
/// colorfulness; black, blown-out and flat frames score ~0.
pub fn poster_score(img: &RgbaImage) -> f32 {
    let (mut n, mut sum, mut sq, mut chroma) = (0f32, 0f32, 0f32, 0f32);
    for p in img.pixels().step_by(3) {
        let [r, g, b, _] = p.0.map(|c| c as f32 / 255.0);
        let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        sum += y;
        sq += y * y;
        chroma += r.max(g).max(b) - r.min(g).min(b);
        n += 1.0;
    }
    if n == 0.0 {
        return 0.0;
    }
    let mean = sum / n;
    let std = (sq / n - mean * mean).max(0.0).sqrt();
    let exposure = (1.0 - ((mean - 0.45).abs() / 0.45)).clamp(0.0, 1.0);
    std * (0.35 + 0.65 * exposure) * (1.0 + 0.5 * chroma / n)
}

pub const STRIP_COLS: u32 = 4;
pub const STRIP_ROWS: u32 = 3;

/// Poster (256/1024) + 4×3 hover strip of a video at a path or URL.
pub fn video_thumbs(src: &str, dir: &Path, key: &str) -> Result<Made> {
    let p = probe(src)?;
    if !p.video {
        bail!("no video stream");
    }
    let dur = p.duration.unwrap_or(0.0).max(0.0);
    let n = STRIP_COLS * STRIP_ROWS;
    let times: Vec<f64> = if dur > 0.2 {
        (0..n).map(|i| dur * (i as f64 + 0.5) / n as f64).collect()
    } else {
        vec![0.0]
    };
    let vf = format!("scale=-2:{SMALL}");
    let frames: Vec<Option<DynamicImage>> = std::thread::scope(|s| {
        let mut out = vec![];
        for chunk in times.chunks(4) {
            let hs: Vec<_> = chunk.iter().map(|&t| s.spawn({ let vf = vf.clone(); move || frame_at(src, t, &vf).ok() })).collect();
            out.extend(hs.into_iter().map(|h| h.join().ok().flatten()));
        }
        out
    });
    let ok: Vec<(usize, &DynamicImage)> = frames.iter().enumerate().filter_map(|(i, f)| f.as_ref().map(|f| (i, f))).collect();
    let Some(&(best, best_img)) = ok.iter().max_by(|a, b| poster_score(&a.1.to_rgba8()).total_cmp(&poster_score(&b.1.to_rgba8()))) else {
        bail!("couldn't decode any frame");
    };
    let small = fit_height(best_img, SMALL).to_rgba8();
    write_atomic(&dir.join(file_name(key, "256")), &encode_webp(&small, 78.0)?)?;
    let large = frame_at(src, times[best], &format!("scale='min({LARGE},iw)':-2"))
        .map(|f| fit_box(&f, LARGE).to_rgba8())
        .unwrap_or_else(|_| small.clone());
    write_atomic(&dir.join(file_name(key, "1024")), &encode_webp(&large, 82.0)?)?;
    if frames.len() == n as usize {
        let (fw, fh) = best_img.dimensions();
        let ch = 128u32;
        let cw = (((fw as f64 / fh as f64) * ch as f64).round() as u32).max(2);
        let mut strip = RgbaImage::from_pixel(cw * STRIP_COLS, ch * STRIP_ROWS, image::Rgba([10, 10, 10, 255]));
        let mut last: Option<&DynamicImage> = None;
        for (i, f) in frames.iter().enumerate() {
            let f = f.as_ref().or(last).unwrap_or(best_img);
            last = Some(f);
            let cell = f.resize_exact(cw, ch, FilterType::Triangle).to_rgba8();
            image::imageops::overlay(&mut strip, &cell, ((i as u32 % STRIP_COLS) * cw) as i64, ((i as u32 / STRIP_COLS) * ch) as i64);
        }
        write_atomic(&dir.join(file_name(key, "strip")), &encode_webp(&strip, 70.0)?)?;
    }
    Ok(Made { w: p.w, h: p.h, duration: p.duration, palette: palette(&small, 6), tiny: Some(tiny(&small)) })
}

// ---- audio -----------------------------------------------------------------------------------

/// Peak amplitudes (0..1) of a mono 8 kHz decode, `cols` buckets.
fn peaks(src: &str, cols: usize) -> Result<(Vec<f32>, f64)> {
    let out = Command::new(ffmpeg_bin())
        .args(["-v", "error", "-i", src, "-t", "1800", "-ac", "1", "-ar", "8000", "-f", "s16le", "-"])
        .output()
        .context("running ffmpeg")?;
    if !out.status.success() {
        bail!("ffmpeg audio decode: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let samples: Vec<i16> = out.stdout.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
    if samples.is_empty() {
        bail!("no audio samples");
    }
    let per = samples.len().div_ceil(cols).max(1);
    let mut v: Vec<f32> = samples.chunks(per).map(|c| c.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0) as f32 / 32768.0).collect();
    let max = v.iter().cloned().fold(0.01f32, f32::max);
    for x in &mut v {
        *x /= max;
    }
    Ok((v, samples.len() as f64 / 8000.0))
}

fn waveform(peaks: &[f32], w: u32, h: u32) -> RgbaImage {
    let mut img = RgbaImage::from_pixel(w, h, image::Rgba([15, 15, 15, 255]));
    let bar = 2u32;
    let gap = 1u32;
    let n = (w / (bar + gap)).max(1) as usize;
    for i in 0..n {
        let a = peaks.get(i * peaks.len() / n).cloned().unwrap_or(0.0).max(0.02);
        let bh = ((a * (h as f32 * 0.8)) as u32).max(1);
        let y0 = (h - bh) / 2;
        let x0 = i as u32 * (bar + gap);
        for x in x0..(x0 + bar).min(w) {
            for y in y0..y0 + bh {
                img.put_pixel(x, y, image::Rgba([138, 138, 138, 255]));
            }
        }
    }
    img
}

pub fn audio_thumbs(src: &str, dir: &Path, key: &str) -> Result<Made> {
    let (p, dur) = peaks(src, 1024)?;
    write_atomic(&dir.join(file_name(key, "256")), &encode_webp(&waveform(&p, 512, SMALL), 80.0)?)?;
    write_atomic(&dir.join(file_name(key, "1024")), &encode_webp(&waveform(&p, LARGE, 512), 85.0)?)?;
    Ok(Made { w: None, h: None, duration: Some(dur), palette: vec![], tiny: None })
}

// ---- documents, fonts, svg -------------------------------------------------------------------

/// Runs `cmd`, killing it after `secs` (Quick Look can hang on some files).
fn run_bounded(mut cmd: Command, secs: u64) -> Result<std::process::Output> {
    use std::process::Stdio;
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        if child.try_wait()?.is_some() {
            return Ok(child.wait_with_output()?);
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("timed out after {secs}s");
        }
        std::thread::sleep(std::time::Duration::from_millis(40));
    }
}

fn scratch_dir() -> Result<PathBuf> {
    let d = std::env::temp_dir().join(format!("falos-render-{}", rand::random::<u64>()));
    std::fs::create_dir_all(&d)?;
    Ok(d)
}

/// One PDF page as an image `w` px wide (poppler's `pdftoppm`).
pub fn pdf_page(path: &Path, page: u32, w: u32) -> Result<DynamicImage> {
    let tmp = scratch_dir()?;
    let prefix = tmp.join("p");
    let mut cmd = Command::new("pdftoppm");
    cmd.args(["-png", "-singlefile", "-f", &page.to_string(), "-l", &page.to_string(), "-scale-to-x", &w.to_string(), "-scale-to-y", "-1"])
        .arg(path)
        .arg(&prefix);
    let out = run_bounded(cmd, 30);
    let png = tmp.join("p.png");
    let res = match out {
        Ok(o) if o.status.success() && png.is_file() => image::open(&png).map_err(Into::into),
        Ok(o) => Err(anyhow!("pdftoppm: {}", String::from_utf8_lossy(&o.stderr).trim())),
        Err(e) => Err(e.context("running pdftoppm (poppler)")),
    };
    let _ = std::fs::remove_dir_all(&tmp);
    res
}

/// Page count of a PDF: `pdfinfo`, else counting `/Type /Page` objects.
pub fn pdf_pages(path: &Path) -> Result<u32> {
    if let Ok(o) = run_bounded({ let mut c = Command::new("pdfinfo"); c.arg(path); c }, 15)
        && o.status.success()
        && let Some(n) = String::from_utf8_lossy(&o.stdout).lines().find_map(|l| l.strip_prefix("Pages:").and_then(|v| v.trim().parse().ok()))
    {
        return Ok(n);
    }
    let bytes = std::fs::read(path)?;
    let mut n = 0u32;
    let pat = b"/Type";
    let mut i = 0;
    while let Some(off) = bytes[i..].windows(pat.len()).position(|w| w == pat) {
        let mut j = i + off + pat.len();
        while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\n' || bytes[j] == b'\r') {
            j += 1;
        }
        if bytes[j..].starts_with(b"/Page") && !bytes[j..].starts_with(b"/Pages") {
            n += 1;
        }
        i = j;
    }
    if n == 0 {
        bail!("not a readable pdf");
    }
    Ok(n)
}

/// macOS Quick Look's thumbnail of a file (svg, fonts, pdf, …), bounded in time.
pub fn quicklook(path: &Path, size: u32) -> Result<DynamicImage> {
    let tmp = scratch_dir()?;
    let mut cmd = Command::new("qlmanage");
    cmd.args(["-t", "-s", &size.to_string(), "-o"]).arg(&tmp).arg(path);
    let out = run_bounded(cmd, 20);
    let name = format!("{}.png", path.file_name().and_then(|n| n.to_str()).unwrap_or("x"));
    let png = tmp.join(name);
    let res = match out {
        Ok(_) if png.is_file() => image::open(&png).map_err(Into::into),
        Ok(o) => Err(anyhow!("quick look made no thumbnail: {}", String::from_utf8_lossy(&o.stderr).trim())),
        Err(e) => Err(e.context("running qlmanage")),
    };
    let _ = std::fs::remove_dir_all(&tmp);
    res
}

/// Can [`doc_thumbs`] draw this file?
pub fn doc_thumbable(path: &Path, mime: &str) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    mime == "application/pdf" || mime == "image/svg+xml" || mime.starts_with("font/") || matches!(ext.as_str(), "pdf" | "svg" | "ttf" | "otf" | "woff" | "woff2")
}

/// First page of a PDF (pdftoppm, else Quick Look); Quick Look for svg and fonts.
pub fn doc_thumbs(path: &Path, mime: &str, dir: &Path, key: &str) -> Result<Made> {
    let pdf = mime == "application/pdf" || path.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf"));
    let img = if pdf { pdf_page(path, 1, LARGE).or_else(|_| quicklook(path, LARGE))? } else { quicklook(path, LARGE)? };
    image_thumbs(&img, dir, key)
}

// ---- color -----------------------------------------------------------------------------------

pub fn srgb_to_lab([r, g, b]: [u8; 3]) -> [f32; 3] {
    let lin = |c: u8| {
        let c = c as f32 / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    let (r, g, b) = (lin(r), lin(g), lin(b));
    let x = (0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047;
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let z = (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883;
    let f = |t: f32| if t > 0.008856 { t.cbrt() } else { 7.787 * t + 16.0 / 116.0 };
    let (fx, fy, fz) = (f(x), f(y), f(z));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

pub fn lab_to_srgb([l, a, b]: [f32; 3]) -> [u8; 3] {
    let fy = (l + 16.0) / 116.0;
    let fx = fy + a / 500.0;
    let fz = fy - b / 200.0;
    let inv = |t: f32| if t.powi(3) > 0.008856 { t.powi(3) } else { (t - 16.0 / 116.0) / 7.787 };
    let (x, y, z) = (inv(fx) * 0.95047, inv(fy), inv(fz) * 1.08883);
    let r = 3.2406 * x - 1.5372 * y - 0.4986 * z;
    let g = -0.9689 * x + 1.8758 * y + 0.0415 * z;
    let bb = 0.0557 * x - 0.2040 * y + 1.0570 * z;
    let gam = |c: f32| {
        let c = if c <= 0.0031308 { 12.92 * c } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
        (c.clamp(0.0, 1.0) * 255.0).round() as u8
    };
    [gam(r), gam(g), gam(bb)]
}

pub fn hex_of([r, g, b]: [u8; 3]) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

pub fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let h = s.trim().trim_start_matches('#');
    let h = match h.len() {
        3 => h.chars().flat_map(|c| [c, c]).collect::<String>(),
        6 => h.to_string(),
        _ => return None,
    };
    let v = u32::from_str_radix(&h, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

pub fn delta_e(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// Dominant colors: k-means in Lab (deterministic k-means++-ish seeding by farthest point).
pub fn palette(img: &RgbaImage, k: usize) -> Vec<(String, f32)> {
    let small = image::imageops::resize(img, 48, 48, FilterType::Triangle);
    let px: Vec<[f32; 3]> = small.pixels().filter(|p| p.0[3] > 128).map(|p| srgb_to_lab([p.0[0], p.0[1], p.0[2]])).collect();
    if px.is_empty() {
        return vec![];
    }
    let mut centers: Vec<[f32; 3]> = vec![px[px.len() / 2]];
    while centers.len() < k.min(px.len()) {
        let far = px
            .iter()
            .max_by(|a, b| {
                let da = centers.iter().map(|c| delta_e(**a, *c)).fold(f32::MAX, f32::min);
                let db = centers.iter().map(|c| delta_e(**b, *c)).fold(f32::MAX, f32::min);
                da.total_cmp(&db)
            })
            .copied()
            .unwrap();
        if centers.iter().any(|c| delta_e(*c, far) < 1.0) {
            break;
        }
        centers.push(far);
    }
    let mut counts = vec![0usize; centers.len()];
    for _ in 0..10 {
        let mut sums = vec![[0f32; 3]; centers.len()];
        counts = vec![0; centers.len()];
        for p in &px {
            let i = (0..centers.len()).min_by(|&a, &b| delta_e(*p, centers[a]).total_cmp(&delta_e(*p, centers[b]))).unwrap();
            counts[i] += 1;
            for d in 0..3 {
                sums[i][d] += p[d];
            }
        }
        for (i, c) in centers.iter_mut().enumerate() {
            if counts[i] > 0 {
                *c = [sums[i][0] / counts[i] as f32, sums[i][1] / counts[i] as f32, sums[i][2] / counts[i] as f32];
            }
        }
    }
    let total = px.len() as f32;
    let mut out: Vec<(String, f32)> = centers
        .iter()
        .zip(&counts)
        .filter(|(_, n)| **n > 0)
        .map(|(c, n)| (hex_of(lab_to_srgb(*c)), (*n as f32 / total * 1000.0).round() / 1000.0))
        .collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1));
    out
}

/// 8×8 RGB (192 bytes) for local "find similar".
pub fn tiny(img: &RgbaImage) -> Vec<u8> {
    image::imageops::resize(img, 8, 8, FilterType::Triangle).pixels().flat_map(|p| [p.0[0], p.0[1], p.0[2]]).collect()
}

pub fn tiny_distance(a: &[u8], b: &[u8]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return f32::MAX;
    }
    // Compare structure (mean-removed) and color separately.
    let mean = |v: &[u8]| v.iter().map(|&x| x as f32).sum::<f32>() / v.len() as f32;
    let (ma, mb) = (mean(a), mean(b));
    let s: f32 = a.iter().zip(b).map(|(&x, &y)| ((x as f32 - ma) - (y as f32 - mb)).powi(2)).sum::<f32>() / a.len() as f32;
    let c: f32 = a.iter().zip(b).map(|(&x, &y)| (x as f32 - y as f32).powi(2)).sum::<f32>() / a.len() as f32;
    s.sqrt() * 0.6 + c.sqrt() * 0.4
}

pub fn thumbs_exist(dir: &Path, key: &str) -> bool {
    dir.join(file_name(key, "256")).is_file()
}

pub fn path_of(dir: &Path, key: &str, size: &str) -> PathBuf {
    dir.join(file_name(key, size))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_hex_lab() {
        let k = "a".repeat(64);
        assert_eq!(parse_name(&format!("{k}-256.webp")), Some((k.clone(), "256")));
        assert_eq!(parse_name("fal_db5p2gc-strip.webp"), Some(("fal_db5p2gc".into(), "strip")));
        assert_eq!(parse_name("../x-256.webp"), None);
        assert_eq!(parse_name(&format!("{k}-512.webp")), None);
        assert_eq!(parse_hex("#f40"), Some([255, 68, 0]));
        let back = lab_to_srgb(srgb_to_lab([200, 30, 90]));
        assert!(back.iter().zip([200u8, 30, 90]).all(|(a, b)| (*a as i32 - b as i32).abs() <= 2), "{back:?}");
    }

    #[test]
    fn palette_and_poster() {
        let mut img = RgbaImage::from_pixel(64, 64, image::Rgba([255, 0, 0, 255]));
        for y in 0..64 {
            for x in 0..16 {
                img.put_pixel(x, y, image::Rgba([0, 0, 255, 255]));
            }
        }
        let p = palette(&img, 6);
        assert_eq!(p[0].0, "#ff0000");
        assert!(p.iter().any(|(h, _)| h == "#0000ff"));
        let black = RgbaImage::from_pixel(32, 32, image::Rgba([0, 0, 0, 255]));
        assert!(poster_score(&img) > poster_score(&black) + 0.01);
        let webp = encode_webp(&img, 80.0).unwrap();
        assert_eq!(&webp[0..4], b"RIFF");
    }
}
