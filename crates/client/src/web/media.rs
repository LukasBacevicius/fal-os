//! Browser media plumbing: content hashing, image/video probing, and tile production.
//!
//! Every drawn node gets its pixels from a *source* bitmap kept in a small JS-side LRU
//! (decoded once, capped at 2048 px): the image itself, a video's poster frame, or a
//! placeholder card drawn on a canvas (audio / 3D / text / pdf …). Tiles for the engine's tiers
//! are produced from the source with the browser's high-quality resize
//! (`createImageBitmap(src, {resizeWidth, resizeHeight})`, canvas fallback) and copied to the
//! GPU with `copyExternalImageToTexture` by the engine.

use std::collections::HashMap;

use falos_doc::NodeKind;
use js_sys::{Object, Reflect};
use wasm_bindgen::prelude::*;

use crate::scene::SceneNode;

#[wasm_bindgen(inline_js = r#"
const sources = new Map(); // key -> Promise<{bitmap, width, height, duration}>
const MAX_SOURCES = 32;
const MAX_SOURCE_PX = 2048;

function hasOffscreen() { return typeof OffscreenCanvas !== 'undefined'; }
function canvas(w, h) {
  if (hasOffscreen()) return new OffscreenCanvas(w, h);
  const c = document.createElement('canvas'); c.width = w; c.height = h; return c;
}

async function resize(src, w, h) {
  w = Math.max(1, Math.round(w)); h = Math.max(1, Math.round(h));
  try {
    const b = await createImageBitmap(src, { resizeWidth: w, resizeHeight: h, resizeQuality: 'high' });
    if (b.width === w && b.height === h) return b;
    b.close();
  } catch (e) { /* fall through to canvas */ }
  const c = canvas(w, h);
  const g = c.getContext('2d');
  g.imageSmoothingEnabled = true;
  g.imageSmoothingQuality = 'high';
  g.drawImage(src, 0, 0, w, h);
  return await createImageBitmap(c);
}

async function capped(src, w, h) {
  const m = Math.max(w, h);
  if (m <= MAX_SOURCE_PX) return null;
  const s = MAX_SOURCE_PX / m;
  return await resize(src, w * s, h * s);
}

async function imageSource(blob) {
  const full = await createImageBitmap(blob);
  const width = full.width, height = full.height;
  const small = await capped(full, width, height);
  if (small) full.close();
  return { bitmap: small || full, width, height, duration: 0 };
}

export async function probe_video(url) {
  const v = document.createElement('video');
  v.muted = true; v.playsInline = true; v.preload = 'auto'; v.crossOrigin = 'anonymous';
  v.src = url;
  await new Promise((res, rej) => {
    v.addEventListener('loadeddata', res, { once: true });
    v.addEventListener('error', () => rej(new Error('video failed to load')), { once: true });
  });
  const duration = isFinite(v.duration) ? v.duration : 0;
  const t = Math.min(0.5, duration * 0.1);
  if (t > 0) {
    await new Promise((res) => { v.addEventListener('seeked', res, { once: true }); v.currentTime = t; });
  }
  const width = v.videoWidth, height = v.videoHeight;
  const m = Math.max(width, height), s = Math.min(1, 1024 / m);
  const bitmap = await resize(v, width * s, height * s);
  v.removeAttribute('src'); v.load();
  return { bitmap, width, height, duration };
}

// Cards follow UI-SPEC.md: monospace, grays only, no ornament. Sizes are in canvas px; `k`
// converts CSS px at 100% zoom (the node's world width maps to the canvas width).
const MONO = 'ui-monospace, "SF Mono", Menlo, monospace';
const THEMES = {
  dark: { panel: '#0f0f0f', tile: '#1a1a1c', text: '#e6e6e6', dim: '#8a8a8a', faint: '#4a4a4a' },
  light: { panel: '#f7f7f7', tile: '#ececee', text: '#0a0a0a', dim: '#6e6e6e', faint: '#b5b5b5' },
};
const theme = (spec) => THEMES[spec.theme] || THEMES.dark;

function cardCanvas(spec, minH) {
  const W = 768;
  const H = Math.max(minH, Math.min(2048, Math.round(W / Math.max(0.2, spec.aspect || 1.5))));
  const c = canvas(W, H), g = c.getContext('2d');
  const k = W / Math.max(40, Number(spec.w) || 320);
  return { c, g, W, H, k };
}

function wrap(g, text, maxW, maxLines) {
  const out = [];
  for (const para of String(text).split('\n')) {
    let line = '';
    for (const word of para.split(/(\s+)/)) {
      const t = line + word;
      if (g.measureText(t).width > maxW && line.trim()) { out.push(line.trimEnd()); line = word.trimStart(); } else line = t;
      if (out.length >= maxLines) return out;
    }
    out.push(line.trimEnd());
    if (out.length >= maxLines) return out;
  }
  return out;
}

function clip(g, text, maxW) {
  let t = String(text);
  if (g.measureText(t).width <= maxW) return t;
  while (t.length > 1 && g.measureText(t + '…').width > maxW) t = t.slice(0, -1);
  return t + '…';
}

// note / text: a plain mono text block on the panel color (the overlay draws the 1 px border).
async function textSource(spec) {
  const th = theme(spec);
  const { c, g, W, H, k } = cardCanvas(spec, 64);
  g.fillStyle = th.panel; g.fillRect(0, 0, W, H);
  const pad = 10 * k, fs = 12.5 * k, lh = Math.round(fs * 1.45);
  g.textBaseline = 'alphabetic';
  let top = pad;
  const body = spec.body || '';
  if (spec.kind === 'text' && spec.title && spec.title !== body.split('\n')[0]) {
    g.fillStyle = th.dim; g.font = `400 ${fs * 0.9}px ${MONO}`;
    g.fillText(clip(g, spec.title, W - pad * 2), pad, top + fs);
    top += lh;
  }
  g.fillStyle = th.text; g.font = `400 ${fs}px ${MONO}`;
  const lines = wrap(g, body || spec.title || '', W - pad * 2, Math.max(1, Math.floor((H - top - pad * 0.5) / lh)));
  lines.forEach((l, i) => g.fillText(l, pad, top + fs + i * lh));
  return { bitmap: await createImageBitmap(c), width: W, height: H, duration: 0 };
}

// Failed generation: the tile with `failed` in dim mono (the error goes to the chat).
async function generationSource(spec) {
  const th = theme(spec);
  const { c, g, W, H, k } = cardCanvas(spec, 64);
  g.fillStyle = th.tile; g.fillRect(0, 0, W, H);
  const pad = 10 * k, fs = 12 * k;
  g.fillStyle = th.dim; g.font = `400 ${fs}px ${MONO}`; g.textBaseline = 'alphabetic';
  g.fillText(spec.status === 'failed' ? 'failed' : String(spec.status || 'failed'), pad, pad + fs);
  return { bitmap: await createImageBitmap(c), width: W, height: H, duration: 0 };
}

// Real waveform: decode the audio and draw min/max peaks per column.
async function audioPeaks(spec, n) {
  if (!spec.url) return null;
  try {
    const buf = await (await fetch(spec.url)).arrayBuffer();
    if (buf.byteLength > 64 * 1024 * 1024) return null;
    const AC = globalThis.OfflineAudioContext || globalThis.webkitOfflineAudioContext;
    const ctx = new AC(1, 1, 8000);
    const audio = await ctx.decodeAudioData(buf);
    const data = audio.getChannelData(0);
    const step = Math.max(1, Math.floor(data.length / n));
    const peaks = new Float32Array(n);
    let max = 1e-6;
    for (let i = 0; i < n; i++) {
      let m = 0;
      const end = Math.min(data.length, (i + 1) * step);
      for (let j = i * step; j < end; j += 4) m = Math.max(m, Math.abs(data[j]));
      peaks[i] = m; max = Math.max(max, m);
    }
    for (let i = 0; i < n; i++) peaks[i] /= max;
    return peaks;
  } catch (e) {
    return null;
  }
}

const TAGS = { model3d: '3d', document: 'pdf', vector: 'svg', audio: 'audio', embed: 'embed', timeline: 'timeline', stream: 'stream', image: 'img', video: 'video' };
function tag(spec) {
  if (spec.kind === 'document' && spec.mime && spec.mime !== 'application/pdf') return 'doc';
  if (String(spec.kind).startsWith('style:')) return 'style';
  return TAGS[spec.kind] || spec.kind;
}

// Everything else: `[3d] name` on the panel color; audio adds its waveform in dim.
async function cardSource(spec) {
  const th = theme(spec);
  const { c, g, W, H, k } = cardCanvas(spec, 64);
  g.fillStyle = th.panel; g.fillRect(0, 0, W, H);
  const pad = 10 * k, fs = 12 * k;
  g.textBaseline = 'alphabetic'; g.font = `400 ${fs}px ${MONO}`;
  const label = `[${tag(spec)}] `;
  g.fillStyle = th.dim; g.fillText(label, pad, pad + fs);
  const lw = g.measureText(label).width;
  g.fillStyle = th.text; g.fillText(clip(g, spec.title || '', W - pad * 2 - lw), pad + lw, pad + fs);
  if (spec.missing) {
    g.fillStyle = th.faint; g.fillText('missing file', pad, pad + fs * 2.6);
  }
  if (spec.kind === 'audio') {
    const top = pad + fs * 2.2, bottom = H - pad, mid = (top + bottom) / 2, amp = (bottom - top) / 2;
    const n = Math.max(32, Math.floor((W - pad * 2) / Math.max(2, 2 * k)));
    const peaks = await audioPeaks(spec, n);
    g.fillStyle = th.dim;
    const bw = (W - pad * 2) / n;
    if (peaks) {
      for (let i = 0; i < n; i++) {
        const a = Math.max(k * 0.5, peaks[i] * amp);
        g.fillRect(pad + i * bw, mid - a, Math.max(1, bw - k * 0.6), a * 2);
      }
    } else {
      g.fillRect(pad, mid - k * 0.5, W - pad * 2, Math.max(1, k));
    }
  }
  return { bitmap: await createImageBitmap(c), width: W, height: H, duration: 0 };
}

async function urlSource(url) {
  const r = await fetch(url);
  if (!r.ok) throw new Error(`fetch ${r.status}`);
  return imageSource(await r.blob());
}

function makeSource(spec) {
  if (spec.type === 'blob') return imageSource(spec.blob);
  if (spec.type === 'url') return urlSource(spec.url);
  if (spec.kind === 'generation') return generationSource(spec);
  if (spec.kind === 'text' || spec.kind === 'note') return textSource(spec);
  if (spec.type === 'video') return probe_video(spec.url);
  return cardSource(spec);
}

function source(key, spec) {
  let p = sources.get(key);
  if (p) { sources.delete(key); sources.set(key, p); return p; }
  p = makeSource(spec);
  sources.set(key, p);
  p.catch(() => sources.delete(key));
  while (sources.size > MAX_SOURCES) {
    const [k, old] = sources.entries().next().value;
    sources.delete(k);
    old.then(s => s.bitmap.close()).catch(() => {});
  }
  return p;
}

// A square `size` tile of the source (content squeezed; the engine restores the aspect).
export async function make_tile(key, spec, size) {
  const s = await source(key, spec);
  const tile = await resize(s.bitmap, size, size);
  return { tile, width: s.width, height: s.height, duration: s.duration };
}

export function forget_source(key) {
  const p = sources.get(key);
  if (p) { sources.delete(key); p.then(s => s.bitmap.close()).catch(() => {}); }
}

export async function probe_image(blob) {
  const b = await createImageBitmap(blob);
  const r = { width: b.width, height: b.height };
  b.close();
  return r;
}

export async function sha256_hex(blob) {
  const buf = await blob.arrayBuffer();
  if (globalThis.crypto && crypto.subtle) {
    const h = await crypto.subtle.digest('SHA-256', buf);
    return 'sha256:' + Array.from(new Uint8Array(h), b => b.toString(16).padStart(2, '0')).join('');
  }
  // Insecure contexts have no SubtleCrypto: fall back to a 64-bit FNV-1a content id.
  const a = new Uint8Array(buf); let h1 = 2166136261, h2 = 16777619;
  for (let i = 0; i < a.length; i++) { h1 = Math.imul(h1 ^ a[i], 16777619); h2 = Math.imul(h2 ^ a[i], 2246822519); }
  return 'fnv:' + (h1 >>> 0).toString(16).padStart(8, '0') + (h2 >>> 0).toString(16).padStart(8, '0');
}

export async function blob_text(blob) { return await blob.text(); }

export async function fetch_blob(url) {
  const r = await fetch(url);
  if (!r.ok) throw new Error(`fetch ${url}: ${r.status}`);
  return await r.blob();
}
"#)]
extern "C" {
    #[wasm_bindgen(catch)]
    pub fn make_tile(key: &str, spec: &JsValue, size: u32) -> Result<js_sys::Promise, JsValue>;
    pub fn forget_source(key: &str);
    #[wasm_bindgen(catch)]
    pub fn probe_image(blob: &web_sys::Blob) -> Result<js_sys::Promise, JsValue>;
    #[wasm_bindgen(catch)]
    pub fn probe_video(url: &str) -> Result<js_sys::Promise, JsValue>;
    #[wasm_bindgen(catch)]
    pub fn sha256_hex(blob: &web_sys::Blob) -> Result<js_sys::Promise, JsValue>;
    #[wasm_bindgen(catch)]
    pub fn blob_text(blob: &web_sys::Blob) -> Result<js_sys::Promise, JsValue>;
    #[wasm_bindgen(catch)]
    pub fn fetch_blob(url: &str) -> Result<js_sys::Promise, JsValue>;
}

/// A locally available asset (content-addressed blob).
pub struct AssetEntry {
    /// Local bytes; `None` for assets streamed from the host by URL.
    pub blob: Option<web_sys::Blob>,
    pub url: String,
    pub size: f64,
    pub mime: String,
    pub name: String,
    /// Natural media size, once known.
    pub dims: Option<(u32, u32)>,
    pub duration: f64,
}

#[derive(Default)]
pub struct MediaStore {
    pub assets: HashMap<String, AssetEntry>,
}

impl MediaStore {
    pub fn insert(&mut self, id: &str, blob: web_sys::Blob, mime: &str, name: &str) -> &mut AssetEntry {
        if !self.assets.contains_key(id) {
            let url = web_sys::Url::create_object_url_with_blob(&blob).unwrap_or_default();
            self.assets.insert(
                id.to_string(),
                AssetEntry { size: blob.size(), blob: Some(blob), url, mime: mime.to_string(), name: name.to_string(), dims: None, duration: 0.0 },
            );
        }
        self.assets.get_mut(id).expect("inserted")
    }

    pub fn insert_remote(&mut self, id: &str, url: &str, mime: &str, name: &str, size: f64) {
        self.assets.entry(id.to_string()).or_insert_with(|| AssetEntry {
            blob: None,
            url: url.to_string(),
            size,
            mime: mime.to_string(),
            name: name.to_string(),
            dims: None,
            duration: 0.0,
        });
    }

    pub fn get(&self, id: &str) -> Option<&AssetEntry> {
        self.assets.get(id)
    }

    /// Content aspect (w/h) of an asset, if known.
    pub fn aspect(&self, id: &str) -> Option<f32> {
        let (w, h) = self.assets.get(id)?.dims?;
        (w > 0 && h > 0).then(|| w as f32 / h as f32)
    }
}

/// How a node gets its pixels.
pub enum Content {
    Image,
    Video,
    /// Pixels drawn on a canvas (text, notes, audio, other types, failed generations).
    Card,
    /// A generation in flight: the engine's generating tile (no pixels).
    Generating,
    None,
}

/// Generation status (first field of [`SceneNode::card`]).
pub fn gen_status(n: &SceneNode) -> &str {
    n.card.split('\u{1}').next().unwrap_or("")
}

pub fn content_of(n: &SceneNode) -> Content {
    match n.kind {
        NodeKind::Image if n.asset.is_some() => Content::Image,
        NodeKind::Video if n.asset.is_some() => Content::Video,
        NodeKind::Frame | NodeKind::Group | NodeKind::Connector => Content::None,
        NodeKind::Generation if !matches!(gen_status(n), "failed" | "canceled" | "cancelled") => Content::Generating,
        _ => Content::Card,
    }
}

fn set(o: &Object, k: &str, v: impl Into<JsValue>) {
    let _ = Reflect::set(o, &k.into(), &v.into());
}

/// Source key + JS spec for a node's pixels, or `None` if its asset is not available yet.
/// `light` selects the light theme's card colors.
pub fn source_spec(n: &SceneNode, media: &MediaStore, light: bool) -> Option<(String, JsValue)> {
    let o = Object::new();
    let key = match content_of(n) {
        Content::Image => {
            let a = media.get(n.asset.as_deref()?)?;
            match &a.blob {
                Some(b) => {
                    set(&o, "type", "blob");
                    set(&o, "blob", b.clone());
                }
                None => {
                    set(&o, "type", "url");
                    set(&o, "url", a.url.clone());
                }
            }
            format!("a:{}", n.asset.as_deref()?)
        }
        Content::Video => {
            let a = media.get(n.asset.as_deref()?)?;
            set(&o, "type", "video");
            set(&o, "url", a.url.clone());
            format!("a:{}", n.asset.as_deref()?)
        }
        Content::Card => {
            let aspect = n.rect[2] / n.rect[3].max(1.0);
            let asset = n.asset.as_deref().and_then(|a| media.get(a));
            let body = n.text.clone().unwrap_or_default();
            let title = n.name.clone();
            let theme = if light { "light" } else { "dark" };
            set(&o, "type", "card");
            set(&o, "kind", n.kind.to_string());
            set(&o, "title", title.clone());
            set(&o, "body", body.clone());
            set(&o, "aspect", aspect);
            set(&o, "w", n.rect[2]);
            set(&o, "theme", theme);
            if let Some(a) = asset {
                set(&o, "mime", a.mime.clone());
                if n.kind == NodeKind::Audio {
                    set(&o, "url", a.url.clone());
                }
            } else if n.asset.is_some() && n.kind != NodeKind::Generation {
                set(&o, "missing", true);
            }
            if n.kind == NodeKind::Generation {
                set(&o, "status", gen_status(n).to_string());
            }
            // Width is part of the key: text is laid out at 100% zoom of the node's width.
            let h = fnv(&format!("{title}\u{1}{body}\u{1}{:.2}\u{1}{:.0}\u{1}{theme}\u{1}{}\u{1}{}", aspect, n.rect[2], n.card, asset.is_some()));
            format!("c:{}:{h:x}", n.id)
        }
        Content::Generating | Content::None => return None,
    };
    Some((key, o.into()))
}

fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
}

/// Node kind for an imported file.
pub fn classify(mime: &str, name: &str) -> NodeKind {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match () {
        _ if mime == "image/svg+xml" || ext == "svg" => NodeKind::Vector,
        _ if mime.starts_with("image/") => NodeKind::Image,
        _ if mime.starts_with("video/") || ["mp4", "mov", "webm", "m4v"].contains(&ext.as_str()) => NodeKind::Video,
        _ if mime.starts_with("audio/") || ["mp3", "wav", "aac", "flac", "ogg", "m4a"].contains(&ext.as_str()) => NodeKind::Audio,
        _ if mime.starts_with("model/") || ["glb", "gltf", "usdz", "obj", "ply", "splat", "fbx", "stl"].contains(&ext.as_str()) => {
            NodeKind::Model3d
        }
        _ if mime == "application/pdf" || ext == "pdf" => NodeKind::Document,
        _ if mime.starts_with("text/") || ["md", "txt", "json", "csv"].contains(&ext.as_str()) => NodeKind::Text,
        _ => NodeKind::Document,
    }
}
