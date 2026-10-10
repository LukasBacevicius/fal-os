//! Pixel sources. The spike synthesizes images and video frames procedurally so we can
//! measure the pipeline (decode → upload → mips → draw) without codec dependencies.
//! Real decoders (ImageIO/AVFoundation natively, WebCodecs on web) plug in here.

pub const LOW_TILE: u32 = 32;
pub const LOW_MIPS: u32 = 6; // 32 → 1 px per tile
pub const DETAIL_TILE: u32 = 256;
pub const DETAIL_MIPS: u32 = 3; // 256, 128, 64

fn hash(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846c_a68b);
    x ^ (x >> 16)
}

fn hsv(h: f32, s: f32, v: f32) -> [f32; 3] {
    let i = (h * 6.0).floor();
    let f = h * 6.0 - i;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - f * s), v * (1.0 - (1.0 - f) * s));
    match i as i32 % 6 {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}

/// Average color of a node's synthetic image, used before any pixels are resident.
pub fn base_color(seed: u32) -> u32 {
    let h = (hash(seed) % 1000) as f32 / 1000.0;
    let [r, g, b] = hsv(h, 0.45, 0.55);
    (r * 255.0) as u32 | ((g * 255.0) as u32) << 8 | ((b * 255.0) as u32) << 16 | 255 << 24
}

/// Writes a synthetic `size`×`size` RGBA image into `out` at row stride `stride` (in pixels).
/// `t` animates it (video frames); images pass 0.
pub fn synth(seed: u32, t: f32, size: u32, out: &mut [u8], stride: usize) {
    let hv = hash(seed);
    let h0 = (hv % 1000) as f32 / 1000.0;
    let h1 = h0 + 0.15 + (hash(hv) % 300) as f32 / 1000.0;
    let cx = 0.3 + (hash(hv ^ 1) % 400) as f32 / 1000.0 + (t * 0.9).sin() * 0.15;
    let cy = 0.3 + (hash(hv ^ 2) % 400) as f32 / 1000.0 + (t * 0.7).cos() * 0.15;
    let rad = 0.15 + (hash(hv ^ 3) % 200) as f32 / 1000.0;
    let stripes = 3.0 + (hash(hv ^ 4) % 9) as f32;
    let inv = 1.0 / size as f32;
    for y in 0..size {
        let fy = (y as f32 + 0.5) * inv;
        let row = &mut out[(y as usize * stride) * 4..][..size as usize * 4];
        for x in 0..size {
            let fx = (x as f32 + 0.5) * inv;
            let g = hsv((h0 + (h1 - h0) * fy).fract(), 0.55, 0.35 + 0.45 * fx);
            let dx = fx - cx;
            let dy = fy - cy;
            let inside = dx * dx + dy * dy < rad * rad;
            let band = ((fx + fy + t * 0.2) * stripes).fract() < 0.5;
            let [r, gg, b] = if inside {
                hsv((h1 + 0.5).fract(), 0.6, 0.9)
            } else if band {
                [g[0] * 0.85, g[1] * 0.85, g[2] * 0.85]
            } else {
                g
            };
            let px = &mut row[x as usize * 4..x as usize * 4 + 4];
            px[0] = (r * 255.0) as u8;
            px[1] = (gg * 255.0) as u8;
            px[2] = (b * 255.0) as u8;
            px[3] = 255;
        }
    }
}

/// Box-filters `src` (w×h RGBA) to half size.
pub fn downsample(src: &[u8], w: u32, h: u32) -> Vec<u8> {
    let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
    let mut dst = vec![0u8; (nw * nh * 4) as usize];
    for y in 0..nh {
        for x in 0..nw {
            for c in 0..4 {
                let s = |xx: u32, yy: u32| src[((yy.min(h - 1) * w + xx.min(w - 1)) * 4 + c) as usize] as u32;
                let v = s(2 * x, 2 * y) + s(2 * x + 1, 2 * y) + s(2 * x, 2 * y + 1) + s(2 * x + 1, 2 * y + 1);
                dst[((y * nw + x) * 4 + c) as usize] = (v / 4) as u8;
            }
        }
    }
    dst
}

/// The detail-tier slot a video frame lands in: a square region of one array layer, with
/// `mips` levels (each level's region is the mip-0 region shifted right by the level).
pub struct SlotTarget<'a> {
    pub texture: &'a wgpu::Texture,
    pub layer: u32,
    /// Top-left texel of the slot at mip 0.
    pub origin: [u32; 2],
    /// Edge length in texels at mip 0.
    pub size: u32,
    pub mips: u32,
}

/// What a [`VideoSource`] produced for one video this frame.
pub enum FrameUpdate<'a> {
    /// Nothing new (or not ready yet): keep showing what the slot holds.
    None,
    /// A decoded frame as a sampleable 2D view; the engine blits it into the slot (all mips).
    Texture(&'a wgpu::TextureView),
    /// The source already wrote the frame into the slot itself (all mips), e.g. zero-copy
    /// sampling of a platform-native frame.
    Written,
}

/// A platform video decoder feeding the detail tier.
///
/// The engine decides which videos play (the biggest visible ones, up to
/// `Budgets::max_active_videos`) and asks for each active video's latest frame:
/// - web: hidden `<video>` elements; zero-copy `importExternalTexture` drawn straight into the
///   slot (`Written`), or `copyExternalImageToTexture` into a per-video texture (`Texture`)
/// - native (later): VideoToolbox → IOSurface-backed Metal texture (`Texture`, zero-copy)
///
/// Without a source the engine falls back to synthesized frames.
pub trait VideoSource {
    /// Starts (`true`) or pauses decoding of video `id`, the index of the node among the
    /// scene's video nodes in node order. Paused videos keep their last frame on screen.
    fn set_active(&mut self, id: usize, active: bool);

    /// Delivers the latest decoded frame of video `id` if it is newer than the last one
    /// delivered, or whenever `force` is set (its slot was just assigned).
    fn update(
        &mut self,
        id: usize,
        force: bool,
        target: &SlotTarget,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> FrameUpdate<'_>;
}
