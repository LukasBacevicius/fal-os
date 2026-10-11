// Canvas-level UI drawn by the engine on top of the scene: selection outlines and handles,
// marquee, annotation pins and boxes, remote cursors, and agent activity (pulsing outlines,
// read/write sweeps, edit flashes, delete ghosts, title-line dots). One instanced draw of SDF
// primitives. Activity shapes animate here from their start time and the camera clock, so
// the CPU uploads them once per event and only redraws.
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
    @location(9) t0: f32,
    @location(10) dur: f32,
    @location(11) seed: f32,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) p: vec2<f32>,      // px relative to the primitive's center
    @location(1) @interpolate(flat) half: vec2<f32>,
    @location(2) @interpolate(flat) fill: vec4<f32>,
    @location(3) @interpolate(flat) stroke: vec4<f32>,
    @location(4) @interpolate(flat) params: vec3<f32>, // stroke_px, radius_px, shape
    @location(5) screen: vec2<f32>,
    @location(6) @interpolate(flat) anim: vec3<f32>, // t0, dur, seed
};

const SHAPE_RECT: u32 = 0u;
const SHAPE_CIRCLE: u32 = 1u;
const SHAPE_CURSOR: u32 = 2u;
const SHAPE_DASHED: u32 = 3u;
const SHAPE_PIN: u32 = 4u;
const SHAPE_PULSE: u32 = 5u;
const SHAPE_SWEEP_UP: u32 = 6u;
const SHAPE_SWEEP_DOWN: u32 = 7u;
const SHAPE_FLASH: u32 = 8u;
const SHAPE_GHOST: u32 = 9u;
const SHAPE_RAIN: u32 = 10u;
const SHAPE_BLINK: u32 = 11u;
const SHAPE_GLYPH: u32 = 12u;

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
    out.anim = vec3(prim.t0, prim.dur, prim.seed);
    return out;
}

fn hash11(n: f32) -> f32 {
    return fract(sin(n * 127.1 + 311.7) * 43758.5453);
}

/// Activity shapes: everything derives from the clock, the start time and the rect.
fn activity(in: VsOut, shape: u32, d: f32) -> vec4<f32> {
    let t0 = in.anim.x;
    let dur = max(in.anim.y, 1e-3);
    let k = clamp((cam.time - t0) / dur, 0.0, 1.0);
    let stroke_px = in.params.x;
    let inside = clamp(0.5 - d, 0.0, 1.0);
    let edge = clamp(0.5 - (abs(d) - stroke_px * 0.5), 0.0, 1.0);
    // 0 at the top of the rect, 1 at the bottom.
    let v = (in.p.y + in.half.y) / max(2.0 * in.half.y, 1.0);
    var fa = 0.0; // fill alpha factor
    var sa = 0.0; // stroke alpha factor
    switch shape {
        case SHAPE_PULSE: {
            sa = 0.45 + 0.55 * (0.5 + 0.5 * sin((cam.time - t0) * 2.6));
        }
        case SHAPE_SWEEP_UP: {
            // A soft band rising from below the rect to above it.
            let c = 1.15 - k * 1.3;
            let b = (v - c) / 0.14;
            fa = exp(-b * b) * inside;
            // A trail under the band that fades as it passes.
            fa += 0.35 * smoothstep(c, c + 0.5, v) * (1.0 - smoothstep(c + 0.5, c + 0.9, v)) * inside * (1.0 - k);
            sa = 0.5 * (1.0 - k);
        }
        case SHAPE_SWEEP_DOWN: {
            // The band descends; above it the rect is tinted, and the tint fades out.
            let c = -0.15 + k * 1.3;
            let b = (v - c) / 0.12;
            fa = (exp(-b * b) + 0.35 * step(v, c) * (1.0 - k)) * inside;
            sa = smoothstep(0.0, 0.2, k) * (1.0 - smoothstep(0.6, 1.0, k));
        }
        case SHAPE_FLASH: {
            let e = (1.0 - k) * (1.0 - k);
            sa = e;
            fa = 0.6 * e * inside;
        }
        case SHAPE_GHOST: {
            let e = 1.0 - smoothstep(0.0, 1.0, k);
            fa = e * inside;
            sa = e;
        }
        case SHAPE_BLINK: {
            sa = smoothstep(0.0, 0.15, k) * (1.0 - smoothstep(0.65, 1.0, k));
        }
        case SHAPE_RAIN: {
            // Sparse 2 px squares in 10 px columns, each drifting down the strip on its own
            // phase and twinkling in and out at the ends.
            let col_w = 10.0 * cam.dpr;
            let x = in.p.x + in.half.x;
            let col = floor(x / col_w);
            let h = hash11(col + in.anim.z * 17.0);
            if h > 0.6 {
                let speed = 0.35 + 0.5 * hash11(col * 3.1 + 1.7);
                let ph = fract(h * 9.0 + (cam.time - t0) * speed);
                let y = (ph - 0.5) * 2.0 * in.half.y;
                let cx = (col + 0.5) * col_w - in.half.x;
                let s = 1.1 * cam.dpr;
                let q = abs(in.p - vec2(cx, y)) - vec2(s);
                let dot = clamp(0.5 - max(q.x, q.y), 0.0, 1.0);
                fa = dot * sin(ph * 3.14159);
            }
            return vec4(in.fill.rgb, 1.0) * in.fill.a * fa;
        }
        default: {}
    }
    let a = clamp(in.fill.a * fa, 0.0, 1.0);
    var out = vec4(in.fill.rgb * a, a);
    if stroke_px > 0.0 {
        let s = edge * in.stroke.a * sa;
        out = vec4(in.stroke.rgb * s, s) + out * (1.0 - s);
    }
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

fn seg(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    let pa = p - a;
    let ba = b - a;
    let h = clamp(dot(pa, ba) / dot(ba, ba), 0.0, 1.0);
    return length(pa - ba * h);
}

/// iq's star SDF: `n` points, outer radius `r`, sharpness `m` (2 = sharpest, n = polygon).
fn star_sdf(p0: vec2<f32>, r: f32, n: f32, m: f32) -> f32 {
    let an = 3.141593 / n;
    let en = 3.141593 / m;
    let acs = vec2(cos(an), sin(an));
    let ecs = vec2(cos(en), sin(en));
    let a = atan2(p0.x, p0.y);
    let bn = a - 2.0 * an * floor(a / (2.0 * an)) - an;
    var p = length(p0) * vec2(cos(bn), abs(sin(bn)));
    p = p - r * acs;
    p = p + ecs * clamp(-dot(p, ecs), 0.0, r * acs.y / ecs.y);
    return length(p) * sign(p.x);
}

/// Badge glyphs in normalized coordinates (-1..1, y down); `w`: half stroke width.
fn glyph_sdf(id: u32, q: vec2<f32>, w: f32) -> f32 {
    switch id {
        case 0u: { // ↓ import
            let d = min(seg(q, vec2(0.0, -0.62), vec2(0.0, 0.56)),
                    min(seg(q, vec2(-0.42, 0.14), vec2(0.0, 0.58)), seg(q, vec2(0.42, 0.14), vec2(0.0, 0.58))));
            return d - w;
        }
        case 1u: { // ✦ generated
            return star_sdf(q, 0.78, 4.0, 2.3);
        }
        case 2u: { // ƒ derived
            var d = seg(q, vec2(0.12, -0.42), vec2(-0.12, 0.6));
            d = min(d, seg(q, vec2(0.12, -0.42), vec2(0.24, -0.62)));
            d = min(d, seg(q, vec2(0.24, -0.62), vec2(0.48, -0.6)));
            d = min(d, seg(q, vec2(-0.12, 0.6), vec2(-0.38, 0.64)));
            d = min(d, seg(q, vec2(-0.34, -0.08), vec2(0.34, -0.08)));
            return d - w;
        }
        case 3u: { // ▶ render
            return tri_sdf(q, vec2(-0.42, -0.56), vec2(-0.42, 0.56), vec2(0.56, 0.0));
        }
        case 4u: { // ★ final
            return star_sdf(vec2(q.x, -q.y), 0.82, 5.0, 2.6);
        }
        default: { // ↑ dot: uploaded to fal
            return length(q) - 0.62;
        }
    }
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let stroke_px = in.params.x;
    let shape = u32(in.params.z);
    if shape == SHAPE_GLYPH {
        // Only on nodes big enough on screen (the client passes a huge size to force it).
        if in.anim.x * cam.zoom < 160.0 * cam.dpr || in.anim.y * cam.zoom < 72.0 * cam.dpr {
            discard;
        }
        let bd = rounded_box_sdf(in.p, in.half, 2.0 * cam.dpr);
        let ba = clamp(0.5 - bd, 0.0, 1.0) * in.fill.a;
        let s = max(min(in.half.x, in.half.y) - cam.dpr, 1.0);
        let gd = glyph_sdf(u32(in.anim.z + 0.5), in.p / s, stroke_px * 0.5 / s) * s;
        let ga = clamp(0.5 - gd, 0.0, 1.0) * in.stroke.a;
        return vec4(in.stroke.rgb * ga, ga) + vec4(in.fill.rgb * ba, ba) * (1.0 - ga);
    }
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
    if shape >= SHAPE_PULSE {
        return activity(in, shape, rounded_box_sdf(in.p, in.half, 0.0));
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
