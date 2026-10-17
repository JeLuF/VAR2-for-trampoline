// NV12 → RGB. Draws a quad filling the viewport, which egui sets to the view's rectangle.

const LINEAR_TARGET: bool = false;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) tex_coord: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOutput {
    // Triangle strip: top left, top right, bottom left, bottom right.
    let uv = vec2<f32>(f32(index & 1u), f32(index >> 1u));
    var out: VertexOutput;
    out.position = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    out.tex_coord = uv;
    return out;
}

@group(0) @binding(0) var y_texture: texture_2d<f32>;
@group(0) @binding(1) var uv_texture: texture_2d<f32>;
@group(0) @binding(2) var video_sampler: sampler;
@group(0) @binding(3) var<uniform> color_matrix: mat4x4<f32>;

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let low = c / 12.92;
    let high = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
    return select(high, low, c <= vec3<f32>(0.04045));
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let y = textureSample(y_texture, video_sampler, in.tex_coord).r;
    let cbcr = textureSample(uv_texture, video_sampler, in.tex_coord).rg;
    var rgb = clamp((color_matrix * vec4<f32>(y, cbcr, 1.0)).rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    if LINEAR_TARGET {
        rgb = srgb_to_linear(rgb);
    }
    return vec4<f32>(rgb, 1.0);
}
