// One instanced draw for every media node on the canvas.
// Camera-only frames update `cam` and nothing else.

struct Camera {
    offset: vec2<f32>,   // world position of the viewport's top-left corner
    viewport: vec2<f32>, // physical pixels
    zoom: f32,           // physical pixels per world unit
    time: f32,
    _pad: vec2<f32>,
};

struct Instance {
    @location(0) pos: vec2<f32>,
    @location(1) size: vec2<f32>,
    @location(2) low_uv: vec4<f32>,
    @location(3) low_layer: u32,
    @location(4) detail_slot: i32,
    @location(5) color: u32,
    @location(6) flags: u32,
    @location(7) aspect: f32, // content w/h; 0 = node's own aspect (no crop)
    @location(8) radius: f32, // max corner radius, screen px
    @location(9) hires_layer: i32, // hires tier layer, -1 = none
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) local: vec2<f32>,           // 0..1 inside the node
    @location(1) @interpolate(flat) low_layer: u32,
    @location(2) low_uv: vec4<f32>,
    @location(3) @interpolate(flat) detail: vec3<f32>, // layer, col, row (layer < 0 = none)
    @location(4) color: vec4<f32>,
    @location(5) @interpolate(flat) flags: u32,
    @location(6) screen_size: vec2<f32>,
    @location(7) @interpolate(flat) crop: vec2<f32>, // uv scale around the center (cover crop)
    @location(8) @interpolate(flat) radius: f32,
    @location(9) @interpolate(flat) hires: i32,
};

const FLAG_LOW_READY: u32 = 4u;
const FLAG_HIDDEN: u32 = 8u;
const FLAG_FLAT: u32 = 16u;
const DETAIL_GRID: f32 = 8.0;

@group(0) @binding(0) var<uniform> cam: Camera;
@group(0) @binding(1) var low_tex: texture_2d_array<f32>;
@group(0) @binding(2) var detail_tex: texture_2d_array<f32>;
@group(0) @binding(3) var samp: sampler;
@group(0) @binding(4) var hires_tex: texture_2d_array<f32>;

fn unpack_color(c: u32) -> vec4<f32> {
    return vec4<f32>(f32(c & 255u), f32((c >> 8u) & 255u), f32((c >> 16u) & 255u), f32(c >> 24u)) / 255.0;
}

@vertex
fn vs(@builtin(vertex_index) vi: u32, inst: Instance) -> VsOut {
    var corners = array<vec2<f32>, 4>(vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(0.0, 1.0), vec2(1.0, 1.0));
    let c = corners[vi];
    var out: VsOut;
    let screen_size = inst.size * cam.zoom;
    // Sub-pixel and hidden nodes collapse to zero area (keeps the draw list shape stable).
    var hide = (inst.flags & FLAG_HIDDEN) != 0u || max(screen_size.x, screen_size.y) < 0.5;
    let world = inst.pos + c * inst.size;
    var screen = (world - cam.offset) * cam.zoom;
    if hide { screen = vec2(-10.0); }
    let ndc = screen / cam.viewport * 2.0 - 1.0;
    out.clip = vec4(ndc.x, -ndc.y, 0.0, 1.0);
    out.local = c;
    out.low_layer = inst.low_layer;
    out.low_uv = inst.low_uv;
    if inst.detail_slot >= 0 {
        let s = u32(inst.detail_slot);
        let per_layer = u32(DETAIL_GRID * DETAIL_GRID);
        let idx = s % per_layer;
        out.detail = vec3(f32(s / per_layer), f32(idx % 8u), f32(idx / 8u));
    } else {
        out.detail = vec3(-1.0, 0.0, 0.0);
    }
    out.color = unpack_color(inst.color);
    out.flags = inst.flags;
    out.screen_size = screen_size;
    // Aspect-correct sampling: tiles hold the content squeezed to a square; show it with a
    // centered cover crop when the node's aspect differs from the content's.
    var crop = vec2(1.0, 1.0);
    let na = inst.size.x / max(inst.size.y, 1e-6);
    if inst.aspect > 0.0 {
        if na > inst.aspect { crop.y = inst.aspect / na; } else { crop.x = na / inst.aspect; }
    }
    out.crop = crop;
    out.radius = inst.radius;
    out.hires = inst.hires_layer;
    return out;
}

fn rounded_box_sdf(p: vec2<f32>, half: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - half + vec2(r);
    return length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    // Rounded-corner SDF in screen pixels: AA is zoom-independent.
    let half = in.screen_size * 0.5;
    let p = (in.local - 0.5) * in.screen_size;
    let radius = min(in.radius, min(half.x, half.y) * 0.25);
    let d = rounded_box_sdf(p, half, radius);
    let coverage = clamp(0.5 - d, 0.0, 1.0);

    let uv = (in.local - 0.5) * in.crop + 0.5;
    // Sample both tiers unconditionally (uniform control flow), then select.
    let low_uv = mix(in.low_uv.xy, in.low_uv.zw, uv);
    let low = textureSample(low_tex, samp, low_uv, in.low_layer);
    let tile = 1.0 / DETAIL_GRID;
    let inset = 1.0 / 64.0; // half a texel at the smallest detail mip
    let duv = (in.detail.yz + clamp(uv, vec2(inset), vec2(1.0 - inset))) * tile;
    let detail = textureSample(detail_tex, samp, duv, i32(max(in.detail.x, 0.0)));
    let hires = textureSample(hires_tex, samp, uv, max(in.hires, 0));

    // Tiles are premultiplied; composite over the node's placeholder color so transparent
    // media stays visible.
    var px = vec4(0.0);
    if (in.flags & FLAG_LOW_READY) != 0u { px = low; }
    if in.detail.x >= 0.0 { px = detail; }
    if in.hires >= 0 { px = hires; }
    var rgb = px.rgb + in.color.rgb * (1.0 - px.a);
    var a = 1.0;
    if (in.flags & FLAG_FLAT) != 0u {
        rgb = in.color.rgb * in.color.a;
        a = in.color.a;
    }
    return vec4(rgb * coverage, a * coverage);
}
