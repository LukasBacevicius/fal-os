//! In-process image engine: decode (`image`), Lanczos resize (`fast_image_resize`),
//! encode PNG/JPEG/GIF/WebP (lossy via libwebp) and AVIF (via `sips`/ffmpeg), grids,
//! palettes.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use image::{DynamicImage, GenericImageView, Rgba, RgbaImage};

use super::probe::ffmpeg_bin;

pub const ENGINE: &str = concat!("falos-image ", env!("CARGO_PKG_VERSION"));

/// Decode any still the host can read (formats `image` lacks go through ffmpeg/sips).
pub async fn load(path: &Path) -> Result<DynamicImage> {
    let p = path.to_path_buf();
    let direct = tokio::task::spawn_blocking(move || image::ImageReader::open(&p)?.with_guessed_format()?.decode().map_err(anyhow::Error::from)).await?;
    if let Ok(i) = direct {
        return Ok(i);
    }
    let tmp = std::env::temp_dir().join(format!("falos-dec-{}.png", rand::random::<u64>()));
    let o = tokio::process::Command::new(ffmpeg_bin())
        .args(["-y", "-v", "error", "-i"])
        .arg(path)
        .args(["-frames:v", "1"])
        .arg(&tmp)
        .output()
        .await?;
    if !o.status.success() {
        let s = tokio::process::Command::new("sips").args(["-s", "format", "png"]).arg(path).arg("--out").arg(&tmp).output().await;
        if !s.is_ok_and(|s| s.status.success()) {
            bail!("can't decode {} as an image", path.display());
        }
    }
    let img = image::open(&tmp).with_context(|| format!("decoding {}", path.display()));
    let _ = std::fs::remove_file(&tmp);
    img
}

/// Lanczos3 resize to exactly `w`×`h`.
pub fn resize(img: &DynamicImage, w: u32, h: u32) -> Result<DynamicImage> {
    use fast_image_resize::{FilterType, ResizeAlg, ResizeOptions, Resizer};
    let (w, h) = (w.max(1), h.max(1));
    if img.dimensions() == (w, h) {
        return Ok(img.clone());
    }
    let src = DynamicImage::ImageRgba8(img.to_rgba8());
    let mut dst = DynamicImage::ImageRgba8(RgbaImage::new(w, h));
    Resizer::new()
        .resize(&src, &mut dst, &ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Lanczos3)))
        .map_err(|e| anyhow!("resize: {e}"))?;
    Ok(dst)
}

pub fn crop(img: &DynamicImage, x: u32, y: u32, w: u32, h: u32) -> DynamicImage {
    let (iw, ih) = img.dimensions();
    let x = x.min(iw.saturating_sub(1));
    let y = y.min(ih.saturating_sub(1));
    img.crop_imm(x, y, w.min(iw - x).max(1), h.min(ih - y).max(1))
}

/// Parse "#rrggbb[aa]" (default black).
pub fn color(s: Option<&str>) -> Rgba<u8> {
    let h = s.unwrap_or("#000000").trim_start_matches('#');
    let b = |i: usize| u8::from_str_radix(h.get(i..i + 2).unwrap_or("00"), 16).unwrap_or(0);
    Rgba([b(0), b(2), b(4), if h.len() >= 8 { b(6) } else { 255 }])
}

/// Place `img` centered on a `w`×`h` canvas: solid `bg` or a blurred, cover-scaled copy.
pub fn pad(img: &DynamicImage, w: u32, h: u32, blur_bg: bool, bg: Rgba<u8>) -> Result<DynamicImage> {
    let (iw, ih) = img.dimensions();
    let s = (w as f64 / iw as f64).min(h as f64 / ih as f64);
    let (fw, fh) = (((iw as f64 * s).round() as u32).max(1), ((ih as f64 * s).round() as u32).max(1));
    let fg = resize(img, fw, fh)?;
    let mut canvas = if blur_bg {
        let c = (w as f64 / iw as f64).max(h as f64 / ih as f64);
        let (cw, ch) = (((iw as f64 * c) as u32).max(w), ((ih as f64 * c) as u32).max(h));
        // Blur small (cheap, strong), then scale up.
        let small = resize(img, (cw / 16).max(2), (ch / 16).max(2))?.blur(3.0);
        let big = resize(&small, cw, ch)?;
        let mut b = crop(&big, (cw - w) / 2, (ch - h) / 2, w, h).to_rgba8();
        for p in b.pixels_mut() {
            for c in 0..3 {
                p.0[c] = (p.0[c] as f32 * 0.8) as u8;
            }
            p.0[3] = 255;
        }
        b
    } else {
        RgbaImage::from_pixel(w, h, bg)
    };
    image::imageops::overlay(&mut canvas, &fg.to_rgba8(), ((w - fw) / 2) as i64, ((h - fh) / 2) as i64);
    Ok(DynamicImage::ImageRgba8(canvas))
}

/// Encode to `path` by its extension. `quality` 1..100 for jpg/webp/avif.
pub async fn save(img: &DynamicImage, path: &Path, quality: Option<f32>) -> Result<()> {
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let q = quality.unwrap_or(match ext.as_str() {
        "jpg" | "jpeg" => 90.0,
        "webp" => 85.0,
        _ => 80.0,
    });
    let img = img.clone();
    let p = path.to_path_buf();
    match ext.as_str() {
        "avif" => {
            let tmp = path.with_extension("tmp.png");
            img.save_with_format(&tmp, image::ImageFormat::Png)?;
            let r = encode_avif(&tmp, path, q).await;
            let _ = std::fs::remove_file(&tmp);
            r
        }
        _ => tokio::task::spawn_blocking(move || -> Result<()> {
            match ext.as_str() {
                "jpg" | "jpeg" => {
                    let f = std::io::BufWriter::new(std::fs::File::create(&p)?);
                    let rgb = DynamicImage::ImageRgb8(flatten(&img).to_rgb8());
                    rgb.write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(f, q.clamp(1.0, 100.0) as u8))?;
                }
                "webp" => {
                    let rgba = img.to_rgba8();
                    let enc = webp::Encoder::from_rgba(rgba.as_raw(), rgba.width(), rgba.height());
                    let mem = if q >= 100.0 { enc.encode_lossless() } else { enc.encode(q.clamp(1.0, 99.0)) };
                    std::fs::write(&p, &*mem)?;
                }
                "png" => img.save_with_format(&p, image::ImageFormat::Png)?,
                "gif" => img.save_with_format(&p, image::ImageFormat::Gif)?,
                "bmp" => img.save_with_format(&p, image::ImageFormat::Bmp)?,
                e => bail!("can't write images as .{e} (png jpg webp avif gif)"),
            }
            Ok(())
        })
        .await?,
    }
}

/// Composite over white (JPEG has no alpha).
fn flatten(img: &DynamicImage) -> DynamicImage {
    if !img.color().has_alpha() {
        return img.clone();
    }
    let mut bg = RgbaImage::from_pixel(img.width(), img.height(), Rgba([255, 255, 255, 255]));
    image::imageops::overlay(&mut bg, &img.to_rgba8(), 0, 0);
    DynamicImage::ImageRgba8(bg)
}

async fn encode_avif(src: &Path, dst: &Path, q: f32) -> Result<()> {
    let sips = tokio::process::Command::new("sips")
        .args(["-s", "format", "avif", "-s", "formatOptions", &format!("{}", q.clamp(1.0, 100.0) as u32)])
        .arg(src)
        .arg("--out")
        .arg(dst)
        .output()
        .await;
    if sips.is_ok_and(|o| o.status.success()) && dst.exists() {
        return Ok(());
    }
    let crf = (63.0 - q.clamp(1.0, 100.0) * 0.5) as u32;
    let o = tokio::process::Command::new(ffmpeg_bin())
        .args(["-y", "-v", "error", "-i"])
        .arg(src)
        .args(["-c:v", "libsvtav1", "-crf", &crf.to_string(), "-pix_fmt", "yuv420p", "-f", "avif"])
        .arg(dst)
        .output()
        .await?;
    if !o.status.success() {
        bail!("avif encode failed: {}", String::from_utf8_lossy(&o.stderr).trim());
    }
    Ok(())
}

/// Grid of images (each fit into a `cell`×`cell` box), `cols` wide, on `bg`.
pub fn grid(imgs: &[DynamicImage], cols: usize, cell: u32, gap: u32, bg: Rgba<u8>) -> Result<DynamicImage> {
    if imgs.is_empty() {
        bail!("grid needs images");
    }
    let cols = cols.clamp(1, imgs.len());
    let rows = imgs.len().div_ceil(cols);
    // Cell aspect from the first image.
    let (fw, fh) = imgs[0].dimensions();
    let (cw, ch) = if fw >= fh { (cell, (cell as f64 * fh as f64 / fw as f64) as u32) } else { ((cell as f64 * fw as f64 / fh as f64) as u32, cell) };
    let (cw, ch) = (cw.max(1), ch.max(1));
    let w = cols as u32 * cw + (cols as u32 + 1) * gap;
    let h = rows as u32 * ch + (rows as u32 + 1) * gap;
    let mut out = RgbaImage::from_pixel(w, h, bg);
    for (i, img) in imgs.iter().enumerate() {
        let (c, r) = ((i % cols) as u32, (i / cols) as u32);
        let (iw, ih) = img.dimensions();
        let s = (cw as f64 / iw as f64).min(ch as f64 / ih as f64);
        let (tw, th) = (((iw as f64 * s) as u32).max(1), ((ih as f64 * s) as u32).max(1));
        let t = resize(img, tw, th)?;
        let x = gap + c * (cw + gap) + (cw - tw) / 2;
        let y = gap + r * (ch + gap) + (ch - th) / 2;
        image::imageops::overlay(&mut out, &t.to_rgba8(), x as i64, y as i64);
    }
    Ok(DynamicImage::ImageRgba8(out))
}

/// Dominant colors by k-means on a 64×64 thumbnail: `[(hex, share)]`, most common first.
pub fn palette(img: &DynamicImage, k: usize) -> Vec<(String, f64)> {
    let small = img.thumbnail(64, 64).to_rgba8();
    let px: Vec<[f32; 3]> = small
        .pixels()
        .filter(|p| p.0[3] > 32)
        .map(|p| [p.0[0] as f32, p.0[1] as f32, p.0[2] as f32])
        .collect();
    if px.is_empty() {
        return vec![];
    }
    let k = k.clamp(1, 12).min(px.len());
    // Deterministic init: evenly spaced samples by luminance.
    let mut sorted = px.clone();
    sorted.sort_by(|a, b| (a[0] + a[1] + a[2]).total_cmp(&(b[0] + b[1] + b[2])));
    let mut centers: Vec<[f32; 3]> = (0..k).map(|i| sorted[(i * sorted.len() + sorted.len() / 2) / k]).collect();
    let mut assign = vec![0usize; px.len()];
    for _ in 0..12 {
        for (i, p) in px.iter().enumerate() {
            assign[i] = (0..k)
                .min_by(|&a, &b| dist(p, &centers[a]).total_cmp(&dist(p, &centers[b])))
                .unwrap_or(0);
        }
        let mut sums = vec![[0f32; 4]; k];
        for (i, p) in px.iter().enumerate() {
            let s = &mut sums[assign[i]];
            s[0] += p[0];
            s[1] += p[1];
            s[2] += p[2];
            s[3] += 1.0;
        }
        for (c, s) in centers.iter_mut().zip(&sums) {
            if s[3] > 0.0 {
                *c = [s[0] / s[3], s[1] / s[3], s[2] / s[3]];
            }
        }
    }
    let mut counts = vec![0usize; k];
    for a in &assign {
        counts[*a] += 1;
    }
    let mut out: Vec<(String, f64)> = centers
        .iter()
        .zip(&counts)
        .filter(|(_, n)| **n > 0)
        .map(|(c, n)| {
            (
                format!("#{:02x}{:02x}{:02x}", c[0].round() as u8, c[1].round() as u8, c[2].round() as u8),
                (*n as f64 / px.len() as f64 * 1000.0).round() / 1000.0,
            )
        })
        .collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1));
    out
}

fn dist(a: &[f32; 3], b: &[f32; 3]) -> f32 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}

/// Swatch strip PNG for a palette.
pub fn swatches(colors: &[(String, f64)]) -> DynamicImage {
    let n = colors.len().max(1) as u32;
    let mut img = RgbaImage::new(n * 64, 64);
    for (i, (hex, _)) in colors.iter().enumerate() {
        let c = color(Some(hex));
        for x in 0..64 {
            for y in 0..64 {
                img.put_pixel(i as u32 * 64 + x, y, c);
            }
        }
    }
    DynamicImage::ImageRgba8(img)
}

/// True when every pixel is fully transparent (a blank render).
pub fn is_blank(img: &DynamicImage) -> bool {
    img.color().has_alpha() && img.to_rgba8().pixels().all(|p| p.0[3] == 0)
}

/// Encode an image as a JPEG (long side ≤ `max`) for MCP image blocks / previews.
pub fn preview_jpeg(img: &DynamicImage, max: u32) -> Result<Vec<u8>> {
    let t = if img.width().max(img.height()) > max { img.thumbnail(max, max) } else { img.clone() };
    let mut buf = std::io::Cursor::new(vec![]);
    DynamicImage::ImageRgb8(flatten(&t).to_rgb8()).write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 82))?;
    Ok(buf.into_inner())
}

/// A temp output path in `dir`.
pub fn out(dir: &Path, name: &str) -> PathBuf {
    dir.join(name)
}
