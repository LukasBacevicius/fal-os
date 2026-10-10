// Canvas-level UI drawn by the engine on top of the scene: selection outlines and handles,
// marquee, annotation pins and boxes, remote cursors. One instanced draw of SDF primitives.
// Each primitive is anchored in world space with a world-space size (scales with zoom) plus a
// screen-space offset/size (constant on screen), so handles and pins keep their pixel size.

struct Camera {
    offset: vec2<f32>,
    viewport: vec2<f32>,
    zoom: f32,
    time: f32,
    dpr: f32,
    _pad: f32,
};

struct Prim {
    @location(0) world: vec2<f32>,
    @location(1) world_size: vec2<f32>,
    @location(2) px_offset: vec2<f32>,
    @location(3) px_size: vec2<f32>,
    @location(4) fill: u32,
    @location(5) stroke: u32,
    @location(6) stroke_px: f32,
    @location(7) radius_px: f32,
    @location(8) shape: u32,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) p: vec2<f32>,      // px relative to the primitive's center
    @location(1) @interpolate(flat) half: vec2<f32>,
    @location(2) @interpolate(flat) fill: vec4<f32>,
    @location(3) @interpolate(flat) stroke: vec4<f32>,
    @location(4) @interpolate(flat) params: vec3<f32>, // stroke_px, radius_px, shape
    @location(5) screen: vec2<f32>,
};

const SHAPE_RECT: u32 = 0u;
const SHAPE_CIRCLE: u32 = 1u;
const SHAPE_CURSOR: u32 = 2u;
const SHAPE_DASHED: u32 = 3u;
const SHAPE_PIN: u32 = 4u;

@group(0) @binding(0) var<uniform> cam: Camera;

fn unpack_color(c: u32) -> vec4<f32> {
    return vec4<f32>(f32(c & 255u), f32((c >> 8u) & 255u), f32((c >> 16u) & 255u), f32(c >> 24u)) / 255.0;
}

@vertex
fn vs(@builtin(vertex_index) vi: u32, prim: Prim) -> VsOut {
    let c = vec2(f32(vi & 1u), f32(vi >> 1u));
    let origin = (prim.world - cam.offset) * cam.zoom + prim.px_offset;
    let size = prim.world_size * cam.zoom + prim.px_size;
    let pad = prim.stroke_px + 2.0;
    let screen = origin - vec2(pad) + c * (size + vec2(2.0 * pad));
    let ndc = screen / cam.viewport * 2.0 - 1.0;
    var out: VsOut;
    out.clip = vec4(ndc.x, -ndc.y, 0.0, 1.0);
    out.p = screen - (origin + size * 0.5);
    out.half = size * 0.5;
    out.fill = unpack_color(prim.fill);
    out.stroke = unpack_color(prim.stroke);
    out.params = vec3(prim.stroke_px, prim.radius_px, f32(prim.shape));
    out.screen = screen;
    return out;
}

fn rounded_box_sdf(p: vec2<f32>, half: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - half + vec2(r);
    return length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

fn tri_sdf(p: vec2<f32>, p0: vec2<f32>, p1: vec2<f32>, p2: vec2<f32>) -> f32 {
    let e0 = p1 - p0; let e1 = p2 - p1; let e2 = p0 - p2;
    let v0 = p - p0; let v1 = p - p1; let v2 = p - p2;
    let pq0 = v0 - e0 * clamp(dot(v0, e0) / dot(e0, e0), 0.0, 1.0);
    let pq1 = v1 - e1 * clamp(dot(v1, e1) / dot(e1, e1), 0.0, 1.0);
    let pq2 = v2 - e2 * clamp(dot(v2, e2) / dot(e2, e2), 0.0, 1.0);
    let s = sign(e0.x * e2.y - e0.y * e2.x);
    let d = min(min(vec2(dot(pq0, pq0), s * (v0.x * e0.y - v0.y * e0.x)),
                    vec2(dot(pq1, pq1), s * (v1.x * e1.y - v1.y * e1.x))),
                    vec2(dot(pq2, pq2), s * (v2.x * e2.y - v2.y * e2.x)));
    return -sqrt(d.x) * sign(d.y);
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let stroke_px = in.params.x;
    let shape = u32(in.params.z);
    var d: f32;
    switch shape {
        case SHAPE_CIRCLE: {
            d = length(in.p) - min(in.half.x, in.half.y);
        }
        case SHAPE_CURSOR: {
            // Arrow with its tip at the primitive's top-left corner.
            let s = min(in.half.x, in.half.y) * 2.0;
            let q = in.p + in.half;
            d = tri_sdf(q, vec2(0.0, 0.0), vec2(0.0, s), vec2(s * 0.72, s * 0.72)) - 0.5;
        }
        case SHAPE_PIN: {
            // Speech-bubble pin: a circle whose bottom-left corner is pointed (the anchor).
            let r = min(in.half.x, in.half.y);
            let circle = length(in.p) - r;
            let corner = rounded_box_sdf(in.p - vec2(-r * 0.5, r * 0.5), vec2(r * 0.5), 0.0);
            d = min(circle, corner);
        }
        default: {
            d = rounded_box_sdf(in.p, in.half, min(in.params.y, min(in.half.x, in.half.y)));
        }
    }
    let fill_a = clamp(0.5 - d, 0.0, 1.0) * in.fill.a;
    var out = vec4(in.fill.rgb * fill_a, fill_a);
    if stroke_px > 0.0 {
        var sa = clamp(0.5 - (abs(d) - stroke_px * 0.5), 0.0, 1.0) * in.stroke.a;
        if shape == SHAPE_DASHED && fract((in.screen.x + in.screen.y) / 10.0) > 0.5 {
            sa = 0.0;
        }
        out = vec4(in.stroke.rgb * sa, sa) + out * (1.0 - sa);
    }
    return out;
}
