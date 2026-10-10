//! Web `VideoSource`: one hidden `<video>` per video node (muted, loop, playsinline), decoded by
//! the browser's hardware decoder. Two ways to get a new frame onto the GPU:
//!
//! - [`IngestMode::Import`] (default, zero-copy): `GPUDevice.importExternalTexture(video)` and a
//!   tiny blit drawn straight into the engine's detail slot (all mips). wgpu 30 does not implement
//!   `ExternalTexture` on its webgpu backend (`unimplemented!`), so this goes around wgpu through
//!   the raw handles it exposes (`Device::as_webgpu`, `Texture::as_webgpu`) and a few lines of JS.
//! - [`IngestMode::Copy`]: wgpu's `Queue::copy_external_image_to_texture` into a per-video texture
//!   at native resolution, which the engine then blits. GPU-side, but WebKit spends ~0.8 ms of
//!   main-thread time per call (see docs/spike-results.md).
//!
//! [`WebVideos`] is a cheap clonable handle: give one clone to the renderer
//! ([`crate::Renderer::set_video_source`]) and keep another to register sources, read the
//! playhead and hold a video on a frame (annotations).

use crate::Handle;
use crate::media::{FrameUpdate, SlotTarget, VideoSource};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

const HAVE_CURRENT_DATA: u16 = 2;

thread_local! {
    static PRESENTED: Cell<u64> = const { Cell::new(0) };
}

/// Frames presented by all playing `<video>` elements so far (via `requestVideoFrameCallback`,
/// which only fires for frames a playing video actually presents): proof of real playback.
pub fn presented_total() -> u64 {
    PRESENTED.with(Cell::get)
}

type FrameCallback = Closure<dyn FnMut(JsValue, JsValue)>;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum IngestMode {
    Import,
    Copy,
}

#[wasm_bindgen(inline_js = r#"
const WGSL = `
@group(0) @binding(0) var src: texture_external;
@group(0) @binding(1) var samp: sampler;
struct VsOut { @builtin(position) pos: vec4f, @location(0) uv: vec2f };
@vertex fn vs(@builtin(vertex_index) vi: u32) -> VsOut {
  let c = vec2f(f32(vi & 1u), f32(vi >> 1u));
  return VsOut(vec4f(c.x * 2.0 - 1.0, 1.0 - c.y * 2.0, 0.0, 1.0), c);
}
// External textures have no mips: box-filter each output texel's footprint with 4x4 taps.
@fragment fn fs(in: VsOut) -> @location(0) vec4f {
  let step = vec2f(dpdx(in.uv).x, dpdy(in.uv).y);
  var acc = vec3f(0.0);
  for (var y = 0; y < 4; y++) {
    for (var x = 0; x < 4; x++) {
      let o = (vec2f(f32(x), f32(y)) + 0.5) * 0.25 - 0.5;
      acc += textureSampleBaseClampToEdge(src, samp, in.uv + o * step).rgb;
    }
  }
  return vec4f(acc / 16.0, 1.0);
}`;
const states = new WeakMap();
// Draws the current frame of `video` into one detail slot of `texture` (layer, mip-0 origin
// x/y, mip-0 size, mips levels). The external texture is consumed within this task, as required.
export function import_blit(device, texture, video, layer, x, y, size, mips) {
  let s = states.get(device);
  if (!s) {
    const module = device.createShaderModule({ code: WGSL });
    s = {
      pipeline: device.createRenderPipeline({
        layout: 'auto',
        vertex: { module, entryPoint: 'vs' },
        fragment: { module, entryPoint: 'fs', targets: [{ format: 'rgba8unorm' }] },
        primitive: { topology: 'triangle-strip' },
      }),
      sampler: device.createSampler({ magFilter: 'linear', minFilter: 'linear' }),
      texture: null,
      views: new Map(),
    };
    states.set(device, s);
  }
  if (s.texture !== texture) { s.texture = texture; s.views.clear(); }
  const bindGroup = device.createBindGroup({
    layout: s.pipeline.getBindGroupLayout(0),
    entries: [
      { binding: 0, resource: device.importExternalTexture({ source: video }) },
      { binding: 1, resource: s.sampler },
    ],
  });
  const encoder = device.createCommandEncoder();
  for (let m = 0; m < mips; m++) {
    const key = layer * 32 + m;
    let view = s.views.get(key);
    if (!view) {
      view = texture.createView({ dimension: '2d', baseMipLevel: m, mipLevelCount: 1, baseArrayLayer: layer, arrayLayerCount: 1 });
      s.views.set(key, view);
    }
    const pass = encoder.beginRenderPass({ colorAttachments: [{ view, loadOp: 'load', storeOp: 'store' }] });
    pass.setPipeline(s.pipeline);
    pass.setBindGroup(0, bindGroup);
    pass.setViewport(x >> m, y >> m, size >> m, size >> m, 0, 1);
    pass.draw(4);
    pass.end();
  }
  device.queue.submit([encoder.finish()]);
}
"#)]
extern "C" {
    #[wasm_bindgen(catch)]
    #[allow(clippy::too_many_arguments)]
    fn import_blit(
        device: &JsValue,
        texture: &JsValue,
        video: &web_sys::HtmlVideoElement,
        layer: u32,
        x: u32,
        y: u32,
        size: u32,
        mips: u32,
    ) -> Result<(), JsValue>;
}

struct Video {
    el: web_sys::HtmlVideoElement,
    /// Frames presented, counted by `requestVideoFrameCallback` (0 if unsupported / not firing).
    presented: Rc<Cell<u32>>,
    seen: u32,
    last_time: f64,
    /// Held on a frame (annotation): stays paused at this time whatever the engine wants.
    held: Option<f64>,
    /// A seek happened: deliver the next ready frame even if no new frame was presented.
    refresh: bool,
    texture: Option<(wgpu::Texture, wgpu::TextureView)>,
    _callback: Rc<RefCell<Option<FrameCallback>>>,
}

#[derive(Default)]
struct Inner {
    srcs: HashMap<Handle, String>,
    /// Created lazily the first time the engine asks for a video, so only videos that have
    /// ever been among the active set own an element (and a decoder / buffered media).
    videos: HashMap<Handle, Video>,
}

#[derive(Clone)]
pub struct WebVideos {
    inner: Rc<RefCell<Inner>>,
    mode: IngestMode,
    on_play_error: Rc<Closure<dyn FnMut(JsValue)>>,
    /// Copy mode: the frame texture handed to the engine this call (views are refcounted).
    last_view: Option<wgpu::TextureView>,
}

impl WebVideos {
    pub fn new(mode: IngestMode) -> Self {
        Self {
            inner: Rc::default(),
            mode,
            on_play_error: Rc::new(Closure::new(|e: JsValue| log::debug!("video play() rejected: {e:?}"))),
            last_view: None,
        }
    }

    /// Sets the media URL of video `id` (an object URL or a served path).
    pub fn set_src(&self, id: Handle, url: &str) {
        let mut inner = self.inner.borrow_mut();
        if inner.srcs.get(&id).map(String::as_str) == Some(url) {
            return;
        }
        inner.srcs.insert(id, url.to_string());
        if let Some(v) = inner.videos.remove(&id) {
            drop_video(v);
        }
    }

    /// Forgets video `id` and releases its element.
    pub fn remove(&self, id: Handle) {
        let mut inner = self.inner.borrow_mut();
        inner.srcs.remove(&id);
        if let Some(v) = inner.videos.remove(&id) {
            drop_video(v);
        }
    }

    /// Current playhead in seconds (0 if the element does not exist yet).
    pub fn current_time(&self, id: Handle) -> f64 {
        self.inner.borrow().videos.get(&id).map_or(0.0, |v| v.held.unwrap_or_else(|| v.el.current_time()))
    }

    pub fn duration(&self, id: Handle) -> f64 {
        self.inner.borrow().videos.get(&id).map_or(0.0, |v| v.el.duration()).max(0.0)
    }

    /// Pauses video `id` on time `t` (seconds) until [`WebVideos::release`].
    pub fn hold(&self, id: Handle, t: f64) {
        let mut inner = self.inner.borrow_mut();
        let Some(src) = inner.srcs.get(&id).cloned() else { return };
        let v = inner.videos.entry(id).or_insert_with(|| create_video(&src));
        v.held = Some(t);
        let _ = v.el.pause();
        if (v.el.current_time() - t).abs() > 1e-3 {
            v.el.set_current_time(t);
        }
        v.refresh = true;
    }

    /// Lets a held video play again when the engine wants it.
    pub fn release(&self, id: Handle) {
        if let Some(v) = self.inner.borrow_mut().videos.get_mut(&id) {
            v.held = None;
        }
    }

    pub fn is_held(&self, id: Handle) -> bool {
        self.inner.borrow().videos.get(&id).is_some_and(|v| v.held.is_some())
    }
}

fn drop_video(v: Video) {
    let _ = v.el.pause();
    v.el.remove_attribute("src").ok();
    v.el.load();
    v.el.remove();
}

fn create_video(src: &str) -> Video {
    let document = web_sys::window().and_then(|w| w.document()).expect("document");
    let el: web_sys::HtmlVideoElement = document.create_element("video").expect("video").unchecked_into();
    el.set_muted(true);
    el.set_default_muted(true);
    el.set_loop(true);
    el.set_preload("auto");
    el.set_cross_origin(Some("anonymous"));
    for attr in ["muted", "playsinline"] {
        let _ = el.set_attribute(attr, "");
    }
    // WebKit may pause muted media that is not rendered, so keep the elements "visible" but
    // effectively invisible (tiny, transparent, behind the canvas).
    let _ = el.set_attribute(
        "style",
        "position:fixed;left:0;top:0;width:4px;height:4px;opacity:0.01;pointer-events:none;z-index:-1",
    );
    el.set_src(src);
    if let Some(body) = document.body() {
        let _ = body.append_child(&el);
    }

    let presented = Rc::new(Cell::new(0u32));
    let callback: Rc<RefCell<Option<FrameCallback>>> = Rc::default();
    let (count, el2, cb2) = (presented.clone(), el.clone(), callback.clone());
    *callback.borrow_mut() = Some(Closure::new(move |_now: JsValue, _meta: JsValue| {
        count.set(count.get().wrapping_add(1));
        PRESENTED.with(|p| p.set(p.get() + 1));
        if let Some(cb) = cb2.borrow().as_ref() {
            request_video_frame(&el2, cb);
        }
    }));
    if let Some(cb) = callback.borrow().as_ref() {
        request_video_frame(&el, cb);
    }
    Video { el, presented, seen: 0, last_time: -1.0, held: None, refresh: false, texture: None, _callback: callback }
}

/// `HTMLVideoElement.requestVideoFrameCallback` (no web-sys binding yet).
fn request_video_frame(el: &web_sys::HtmlVideoElement, cb: &FrameCallback) {
    if let Ok(f) = js_sys::Reflect::get(el, &"requestVideoFrameCallback".into())
        && let Some(f) = f.dyn_ref::<js_sys::Function>()
    {
        let _ = f.call1(el, cb.as_ref().unchecked_ref());
    }
}

impl VideoSource for WebVideos {
    fn set_active(&mut self, id: Handle, active: bool) {
        let mut inner = self.inner.borrow_mut();
        if active {
            let Some(src) = inner.srcs.get(&id).cloned() else { return };
            let v = inner.videos.entry(id).or_insert_with(|| create_video(&src));
            if v.held.is_some() {
                return;
            }
            if let Ok(p) = v.el.play() {
                // Report rejections (autoplay policy, interrupted by pause) instead of throwing.
                let _ = p.catch(&self.on_play_error);
            }
        } else if let Some(v) = inner.videos.get(&id) {
            let _ = v.el.pause();
        }
    }

    fn update(
        &mut self,
        id: Handle,
        force: bool,
        target: &SlotTarget,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> FrameUpdate<'_> {
        let mode = self.mode;
        let mut inner = self.inner.borrow_mut();
        let Some(src) = inner.srcs.get(&id).cloned() else { return FrameUpdate::None };
        let v = inner.videos.entry(id).or_insert_with(|| create_video(&src));
        let (w, h) = (v.el.video_width(), v.el.video_height());
        if v.el.ready_state() < HAVE_CURRENT_DATA || w == 0 || h == 0 || v.el.seeking() {
            return FrameUpdate::None;
        }
        // Prefer presented-frame counts; fall back to the media clock if rVFC never fires.
        let presented = v.presented.get();
        let time = v.el.current_time();
        let newer = if presented > 0 { presented != v.seen } else { time != v.last_time };
        if !newer && !force && !v.refresh {
            return FrameUpdate::None;
        }
        v.seen = presented;
        v.last_time = time;
        v.refresh = false;

        if mode == IngestMode::Import {
            let (Some(dev), Some(tex)) = (device.as_webgpu(), target.texture.as_webgpu()) else {
                return FrameUpdate::None;
            };
            let [x, y] = target.origin;
            return match import_blit(dev, tex, &v.el, target.layer, x, y, target.size, target.mips) {
                Ok(()) => FrameUpdate::Written,
                Err(e) => {
                    log::warn!("importExternalTexture blit failed: {e:?}");
                    FrameUpdate::None
                }
            };
        }

        if v.texture.as_ref().is_none_or(|(t, _)| t.width() != w || t.height() != h) {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("video frame"),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            v.texture = Some((texture, view));
        }
        let Some((texture, _)) = v.texture.as_ref() else { return FrameUpdate::None };
        queue.copy_external_image_to_texture(
            &wgpu::CopyExternalImageSourceInfo {
                source: wgpu::ExternalImageSource::HTMLVideoElement(v.el.clone()),
                origin: wgpu::Origin2d::ZERO,
                flip_y: false,
            },
            wgpu::CopyExternalImageDestInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
                color_space: wgpu::PredefinedColorSpace::Srgb,
                premultiplied_alpha: false,
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        drop(inner);
        self.last_view = self.inner.borrow().videos.get(&id).and_then(|v| v.texture.as_ref().map(|(_, view)| view.clone()));
        match self.last_view.as_ref() {
            Some(view) => FrameUpdate::Texture(view),
            None => FrameUpdate::None,
        }
    }
}
