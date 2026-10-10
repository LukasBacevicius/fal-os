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

const ICON_COLORS = {
  audio: '#f59e0b', model3d: '#22c55e', document: '#ef4444', text: '#60a5fa', vector: '#ec4899',
  embed: '#14b8a6', timeline: '#a855f7', stream: '#f43f5e', generation: '#8b5cf6', note: '#eab308',
};

const NOTE_COLORS = { yellow: '#fde68a', orange: '#fdba74', red: '#fca5a5', pink: '#f9a8d4', purple: '#d8b4fe', blue: '#93c5fd', green: '#86efac', gray: '#d4d4d8', grey: '#d4d4d8', white: '#fafafa' };
function noteColor(c) {
  if (!c) return '#fde68a';
  if (NOTE_COLORS[c]) return NOTE_COLORS[c];
  return /^#([0-9a-f]{3}|[0-9a-f]{6})$/i.test(c) ? c : '#fde68a';
}

function hash(s) { let h = 2166136261; for (let i = 0; i < s.length; i++) { h ^= s.charCodeAt(i); h = Math.imul(h, 16777619); } return h >>> 0; }

function roundRect(g, x, y, w, h, r) { g.beginPath(); g.roundRect ? g.roundRect(x, y, w, h, r) : g.rect(x, y, w, h); }

function glyph(g, kind, x, y, s) {
  g.save(); g.translate(x, y); g.strokeStyle = '#fff'; g.fillStyle = '#fff';
  g.lineWidth = s * 0.07; g.lineCap = 'round'; g.lineJoin = 'round';
  const k = kind.startsWith('style') ? 'style' : kind;
  if (k === 'audio') {
    const bars = [0.35, 0.7, 0.5, 0.95, 0.6, 0.8, 0.4];
    bars.forEach((b, i) => { const bx = s * (0.2 + i * 0.1); g.beginPath(); g.moveTo(bx, s * (0.5 - b * 0.3)); g.lineTo(bx, s * (0.5 + b * 0.3)); g.stroke(); });
  } else if (k === 'model3d') {
    const c = s / 2, r = s * 0.3;
    g.beginPath(); g.moveTo(c, c - r); g.lineTo(c + r, c - r / 2); g.lineTo(c + r, c + r / 2); g.lineTo(c, c + r); g.lineTo(c - r, c + r / 2); g.lineTo(c - r, c - r / 2); g.closePath(); g.stroke();
    g.beginPath(); g.moveTo(c - r, c - r / 2); g.lineTo(c, c); g.lineTo(c + r, c - r / 2); g.moveTo(c, c); g.lineTo(c, c + r); g.stroke();
  } else if (k === 'document') {
    g.beginPath(); g.moveTo(s * 0.3, s * 0.2); g.lineTo(s * 0.58, s * 0.2); g.lineTo(s * 0.72, s * 0.34); g.lineTo(s * 0.72, s * 0.8); g.lineTo(s * 0.3, s * 0.8); g.closePath(); g.stroke();
    [0.45, 0.56, 0.67].forEach(t => { g.beginPath(); g.moveTo(s * 0.38, s * t); g.lineTo(s * 0.64, s * t); g.stroke(); });
  } else if (k === 'text' || k === 'note') {
    g.font = `600 ${s * 0.42}px Inter, system-ui, sans-serif`; g.textAlign = 'center'; g.textBaseline = 'middle'; g.fillText('Aa', s / 2, s / 2 + s * 0.02);
  } else if (k === 'embed') {
    g.font = `600 ${s * 0.36}px ui-monospace, Menlo, monospace`; g.textAlign = 'center'; g.textBaseline = 'middle'; g.fillText('</>', s / 2, s / 2);
  } else if (k === 'generation') {
    const c = s / 2; g.beginPath();
    for (let i = 0; i < 8; i++) { const a = i * Math.PI / 4, r = i % 2 ? s * 0.12 : s * 0.32; g.lineTo(c + Math.cos(a) * r, c + Math.sin(a) * r); }
    g.closePath(); g.fill();
  } else if (k === 'vector') {
    g.beginPath(); g.moveTo(s * 0.22, s * 0.72); g.bezierCurveTo(s * 0.3, s * 0.2, s * 0.7, s * 0.8, s * 0.78, s * 0.28); g.stroke();
    [[0.22, 0.72], [0.78, 0.28]].forEach(([px, py]) => { g.fillRect(s * px - s * 0.05, s * py - s * 0.05, s * 0.1, s * 0.1); });
  } else {
    roundRect(g, s * 0.28, s * 0.28, s * 0.44, s * 0.44, s * 0.08); g.stroke();
  }
  g.restore();
}

function wrap(g, text, maxW, maxLines) {
  const out = [];
  for (const para of String(text).split('\n')) {
    let line = '';
    for (const word of para.split(/\s+/)) {
      const t = line ? line + ' ' + word : word;
      if (g.measureText(t).width > maxW && line) { out.push(line); line = word; } else line = t;
      if (out.length >= maxLines) return out;
    }
    out.push(line);
    if (out.length >= maxLines) return out;
  }
  return out;
}

// Text card: the body is the content; the name is a small caption.
async function textSource(spec) {
  const W = 768, H = Math.max(120, Math.min(2048, Math.round(W / Math.max(0.2, spec.aspect || 1.5))));
  const c = canvas(W, H), g = c.getContext('2d');
  g.fillStyle = '#18181c'; g.fillRect(0, 0, W, H);
  g.strokeStyle = 'rgba(255,255,255,0.10)'; g.lineWidth = 3; g.strokeRect(1.5, 1.5, W - 3, H - 3);
  const pad = 36;
  let top = pad;
  if (spec.title && spec.title !== 'Text') {
    g.fillStyle = '#8b8b94'; g.font = `500 24px Inter, system-ui, -apple-system, sans-serif`; g.textBaseline = 'alphabetic';
    g.fillText(spec.title.slice(0, 60), pad, top + 22);
    top += 44;
  }
  const body = spec.body || '';
  const size = body.length < 80 ? 44 : body.length < 400 ? 34 : 28;
  g.fillStyle = '#f4f4f5'; g.font = `400 ${size}px Inter, system-ui, -apple-system, sans-serif`;
  const lh = Math.round(size * 1.35);
  const lines = wrap(g, body, W - pad * 2, Math.max(1, Math.floor((H - top - pad) / lh)));
  lines.forEach((l, i) => g.fillText(l, pad, top + size + i * lh));
  return { bitmap: await createImageBitmap(c), width: W, height: H, duration: 0 };
}

// Generation placeholder: model, prompt snippet, status and a progress bar. The page adds an
// animated shimmer on top while it runs; this bitmap is re-rendered when status/progress change.
async function generationSource(spec) {
  const W = 768, H = Math.max(200, Math.min(2048, Math.round(W / Math.max(0.2, spec.aspect || 1))));
  const c = canvas(W, H), g = c.getContext('2d');
  const status = spec.status || 'queued';
  const failed = status === 'failed' || status === 'canceled';
  const accent = failed ? '#ef4444' : status === 'done' ? '#22c55e' : '#8b5cf6';
  const grad = g.createLinearGradient(0, 0, W, H);
  grad.addColorStop(0, '#1d1830'); grad.addColorStop(1, '#141418');
  g.fillStyle = grad; g.fillRect(0, 0, W, H);
  g.strokeStyle = accent + '66'; g.lineWidth = 4; g.strokeRect(2, 2, W - 4, H - 4);
  const pad = 40;
  // Header: sparkle glyph + model.
  g.fillStyle = accent; roundRect(g, pad, pad, 64, 64, 16); g.fill();
  glyph(g, 'generation', pad, pad, 64);
  g.textBaseline = 'alphabetic';
  g.fillStyle = '#f4f4f5'; g.font = `600 30px Inter, system-ui, -apple-system, sans-serif`;
  let model = spec.model || 'generation';
  while (g.measureText(model).width > W - pad * 2 - 88 && model.length > 6) model = model.slice(1);
  g.fillText(model === (spec.model || 'generation') ? model : '…' + model, pad + 88, pad + 30);
  g.fillStyle = failed ? '#fca5a5' : '#a1a1aa'; g.font = `500 24px Inter, system-ui, -apple-system, sans-serif`;
  const pct = Math.round(Math.max(0, Math.min(1, Number(spec.progress) || 0)) * 100);
  const label = failed ? (status === 'failed' ? 'Failed' : 'Canceled') : status === 'running' ? `Generating… ${pct ? pct + '%' : ''}` : status === 'done' ? 'Done' : 'Queued';
  g.fillText(label, pad + 88, pad + 62);
  // Prompt snippet.
  const body = failed && spec.error ? spec.error : spec.prompt || '';
  if (body) {
    g.fillStyle = failed ? '#fecaca' : '#d4d4d8'; g.font = `400 30px Inter, system-ui, -apple-system, sans-serif`;
    const top = pad + 120;
    const lines = wrap(g, (failed ? '' : '“') + body + (failed ? '' : '”'), W - pad * 2, Math.max(1, Math.floor((H - top - pad - 40) / 42)));
    lines.forEach((l, i) => g.fillText(l, pad, top + i * 42));
  }
  // Progress bar.
  const by = H - pad - 12;
  g.fillStyle = 'rgba(255,255,255,0.10)'; roundRect(g, pad, by, W - pad * 2, 12, 6); g.fill();
  const frac = status === 'done' ? 1 : status === 'running' ? Math.max(0.06, pct / 100) : status === 'queued' ? 0.03 : 0;
  if (frac > 0) { g.fillStyle = accent; roundRect(g, pad, by, (W - pad * 2) * frac, 12, 6); g.fill(); }
  return { bitmap: await createImageBitmap(c), width: W, height: H, duration: 0 };
}

async function cardSource(spec) {
  const W = 768, H = Math.max(160, Math.min(2048, Math.round(W / Math.max(0.2, spec.aspect || 1.5))));
  const c = canvas(W, H), g = c.getContext('2d');
  const kind = spec.kind;
  const note = kind === 'note';
  const accent = ICON_COLORS[kind] || (kind.startsWith('style') ? '#f97316' : '#71717a');
  g.fillStyle = note ? noteColor(spec.color) : '#1c1c21';
  g.fillRect(0, 0, W, H);
  if (!note) {
    const grad = g.createLinearGradient(0, 0, W, H);
    grad.addColorStop(0, accent + '30'); grad.addColorStop(1, accent + '00');
    g.fillStyle = grad; g.fillRect(0, 0, W, H);
    g.strokeStyle = 'rgba(255,255,255,0.08)'; g.lineWidth = 3; g.strokeRect(1.5, 1.5, W - 3, H - 3);
  }
  const pad = 40, icon = 88;
  const ink = note ? '#3f3a1d' : '#f4f4f5';
  const sub = note ? 'rgba(63,58,29,0.7)' : '#a1a1aa';
  if (!note) {
    g.fillStyle = accent; roundRect(g, pad, pad, icon, icon, 22); g.fill();
    glyph(g, kind, pad, pad, icon);
  }
  const tx = note ? pad : pad + icon + 28;
  g.fillStyle = ink; g.textBaseline = 'alphabetic';
  g.font = `600 38px Inter, system-ui, -apple-system, sans-serif`;
  const title = spec.title || kind;
  let t = title; while (g.measureText(t).width > W - tx - pad && t.length > 4) t = t.slice(0, -2);
  if (t !== title) t = t.slice(0, -1) + '…';
  g.fillText(t, tx, note ? pad + 38 : pad + 44);
  g.fillStyle = sub; g.font = `400 28px Inter, system-ui, -apple-system, sans-serif`;
  if (!note) g.fillText(spec.subtitle || kind, tx, pad + 84);
  const body = spec.body || '';
  const top = note ? pad + 80 : pad + icon + 44;
  if (body) {
    g.fillStyle = note ? ink : '#d4d4d8'; g.font = `400 30px Inter, system-ui, -apple-system, sans-serif`;
    const lines = wrap(g, body, W - pad * 2, Math.max(1, Math.floor((H - top - pad) / 42)));
    lines.forEach((l, i) => g.fillText(l, pad, top + 30 + i * 42));
  } else if (kind === 'audio') {
    const n = 96, bw = (W - pad * 2) / n, mid = top + (H - top - pad) / 2, amp = (H - top - pad) / 2;
    let h = hash(spec.title || 'a');
    g.fillStyle = accent;
    for (let i = 0; i < n; i++) {
      h = Math.imul(h ^ (h >>> 15), 2246822507) >>> 0;
      const env = Math.sin(Math.PI * i / n) * 0.7 + 0.3;
      const a = ((h % 1000) / 1000) * env * amp;
      g.fillRect(pad + i * bw, mid - a, Math.max(2, bw * 0.6), a * 2);
    }
  }
  const bitmap = await createImageBitmap(c);
  return { bitmap, width: W, height: H, duration: 0 };
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
  if (spec.kind === 'text') return textSource(spec);
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
    Card,
    None,
}

pub fn content_of(n: &SceneNode) -> Content {
    match n.kind {
        NodeKind::Image if n.asset.is_some() => Content::Image,
        NodeKind::Video if n.asset.is_some() => Content::Video,
        NodeKind::Frame | NodeKind::Group | NodeKind::Connector => Content::None,
        _ => Content::Card,
    }
}

pub fn kind_label(kind: NodeKind) -> String {
    match kind {
        NodeKind::Model3d => "3D model".into(),
        NodeKind::Document => "Document".into(),
        NodeKind::Audio => "Audio".into(),
        NodeKind::Text => "Text".into(),
        NodeKind::Note => "Note".into(),
        NodeKind::Embed => "Embed".into(),
        NodeKind::Generation => "Generation".into(),
        other => {
            let s = other.to_string();
            let mut c = s.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        }
    }
}

fn set(o: &Object, k: &str, v: impl Into<JsValue>) {
    let _ = Reflect::set(o, &k.into(), &v.into());
}

/// Source key + JS spec for a node's pixels, or `None` if its asset is not available yet.
pub fn source_spec(n: &SceneNode, media: &MediaStore) -> Option<(String, JsValue)> {
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
            let mut subtitle = kind_label(n.kind);
            if let Some(a) = asset {
                let kb = a.size / 1024.0;
                subtitle = if kb > 1024.0 { format!("{subtitle} · {:.1} MB", kb / 1024.0) } else { format!("{subtitle} · {kb:.0} KB") };
            } else if n.asset.is_some() {
                subtitle.push_str(" · missing file");
            }
            let body = n.text.clone().unwrap_or_default();
            let title = if n.name.is_empty() { kind_label(n.kind) } else { n.name.clone() };
            set(&o, "type", "card");
            set(&o, "kind", n.kind.to_string());
            set(&o, "title", title.clone());
            set(&o, "subtitle", subtitle.clone());
            set(&o, "body", body.clone());
            set(&o, "aspect", aspect);
            let mut card = n.card.split('\u{1}');
            match n.kind {
                NodeKind::Note => set(&o, "color", n.card.clone()),
                NodeKind::Generation => {
                    for k in ["status", "progress", "model", "error", "prompt"] {
                        set(&o, k, card.next().unwrap_or("").to_string());
                    }
                }
                _ => {}
            }
            let h = fnv(&format!("{title}\u{1}{subtitle}\u{1}{body}\u{1}{:.2}\u{1}{}", aspect, n.card));
            format!("c:{}:{h:x}", n.id)
        }
        Content::None => return None,
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
