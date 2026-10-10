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
};

const FLAG_SELECTED: u32 = 1u;
const FLAG_LOW_READY: u32 = 4u;
const FLAG_HIDDEN: u32 = 8u;
const DETAIL_GRID: f32 = 8.0;

@group(0) @binding(0) var<uniform> cam: Camera;
@group(0) @binding(1) var low_tex: texture_2d_array<f32>;
@group(0) @binding(2) var detail_tex: texture_2d_array<f32>;
@group(0) @binding(3) var samp: sampler;

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
    let radius = min(6.0, min(half.x, half.y) * 0.25);
    let d = rounded_box_sdf(p, half, radius);
    let coverage = clamp(0.5 - d, 0.0, 1.0);

    // Sample both tiers unconditionally (uniform control flow), then select.
    let low_uv = mix(in.low_uv.xy, in.low_uv.zw, in.local);
    let low = textureSample(low_tex, samp, low_uv, in.low_layer);
    let tile = 1.0 / DETAIL_GRID;
    let inset = 1.0 / 64.0; // half a texel at the smallest detail mip
    let duv = (in.detail.yz + clamp(in.local, vec2(inset), vec2(1.0 - inset))) * tile;
    let detail = textureSample(detail_tex, samp, duv, i32(max(in.detail.x, 0.0)));

    var rgb = in.color.rgb;
    if (in.flags & FLAG_LOW_READY) != 0u { rgb = low.rgb; }
    if in.detail.x >= 0.0 { rgb = detail.rgb; }

    // Selection ring: 2px inside the edge.
    if (in.flags & FLAG_SELECTED) != 0u && d > -2.5 {
        rgb = vec3(0.23, 0.51, 0.96);
    }
    return vec4(rgb * coverage, coverage);
}
