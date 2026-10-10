// Copies a decoded video frame into one detail-tier slot (one mip level per draw).
// The viewport selects the slot rect; the source has no mips, so each output texel
// box-filters its footprint with 4x4 taps.

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> VsOut {
    let c = vec2(f32(vi & 1u), f32(vi >> 1u));
    var out: VsOut;
    out.pos = vec4(c.x * 2.0 - 1.0, 1.0 - c.y * 2.0, 0.0, 1.0);
    out.uv = c;
    return out;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let step = vec2(dpdx(in.uv).x, dpdy(in.uv).y);
    var acc = vec3(0.0);
    for (var y = 0; y < 4; y++) {
        for (var x = 0; x < 4; x++) {
            let o = (vec2(f32(x), f32(y)) + 0.5) * 0.25 - 0.5;
            acc += textureSampleLevel(src, samp, in.uv + o * step, 0.0).rgb;
        }
    }
    return vec4(acc / 16.0, 1.0);
}
