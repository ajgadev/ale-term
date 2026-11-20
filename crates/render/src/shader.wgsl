// One shader draws everything: solid rectangles, text glyphs and color emoji.
// Each instance is a screen-space rectangle in physical pixels.

struct Globals {
    screen: vec2<f32>,
    atlas: vec2<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;
@group(0) @binding(1) var atlas_texture: texture_2d<f32>;
@group(0) @binding(2) var atlas_sampler: sampler;

struct Instance {
    @location(0) pos: vec2<f32>,
    @location(1) size: vec2<f32>,
    // x, y, width, height inside the atlas, in pixels.
    @location(2) uv: vec4<f32>,
    @location(3) color: vec4<f32>,
    // 0 = solid rectangle, 1 = glyph mask tinted with `color`, 2 = color glyph,
    // 3 = image (its own texture; `uv` is already in the 0..1 range).
    @location(4) kind: u32,
};

struct Varyings {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) @interpolate(flat) kind: u32,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32, instance: Instance) -> Varyings {
    // The four corners of a quad, generated from the vertex index.
    let corner = vec2<f32>(f32(index & 1u), f32((index >> 1u) & 1u));
    let pixel = instance.pos + corner * instance.size;

    var out: Varyings;
    out.clip = vec4<f32>(
        pixel.x / globals.screen.x * 2.0 - 1.0,
        1.0 - pixel.y / globals.screen.y * 2.0,
        0.0,
        1.0,
    );
    let uv = instance.uv.xy + corner * instance.uv.zw;
    if instance.kind == 3u {
        out.uv = uv;
    } else {
        out.uv = uv / globals.atlas;
    }
    out.color = instance.color;
    out.kind = instance.kind;
    return out;
}

@fragment
fn fs_main(in: Varyings) -> @location(0) vec4<f32> {
    let texel = textureSampleLevel(atlas_texture, atlas_sampler, in.uv, 0.0);
    if in.kind == 1u {
        return vec4<f32>(in.color.rgb, in.color.a * texel.a);
    }
    if in.kind == 2u || in.kind == 3u {
        return vec4<f32>(texel.rgb, texel.a * in.color.a);
    }
    return in.color;
}
