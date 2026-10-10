// Generates one mip level of a tile region from the level above it (2x2 box filter).
// The viewport selects the region; tiles are aligned so texel (x, y) of level k averages
// texels (2x..2x+1, 2y..2y+1) of level k-1.

@group(0) @binding(0) var src: texture_2d<f32>;

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    let c = vec2(f32(vi & 1u), f32(vi >> 1u));
    return vec4(c.x * 2.0 - 1.0, 1.0 - c.y * 2.0, 0.0, 1.0);
}

@fragment
fn fs(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    let b = vec2<i32>(floor(p.xy)) * 2;
    let s = textureLoad(src, b, 0) + textureLoad(src, b + vec2(1, 0), 0)
        + textureLoad(src, b + vec2(0, 1), 0) + textureLoad(src, b + vec2(1, 1), 0);
    return s * 0.25;
}
