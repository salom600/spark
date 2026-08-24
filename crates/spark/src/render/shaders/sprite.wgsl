// spark — 2D sprite pass.
// Instanced quads: position/rotation/scale/tint per instance, projected by
// the shared globals (orthographic or perspective camera).

struct Globals {
    view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
    light_view_proj: mat4x4<f32>,
    dir_light: vec4<f32>,
    dir_light_color: vec4<f32>,
    light_meta: vec4<f32>,
    point_lights: array<vec4<f32>, 32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;

struct VertIn {
    @location(0) pos: vec2<f32>,
    @location(1) uv: vec2<f32>,
    // Instance attributes.
    @location(2) ipos: vec3<f32>,
    @location(3) irot: f32,
    @location(4) iscale: vec2<f32>,
    @location(5) icolor: vec4<f32>,
};

struct VertOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_main(v: VertIn) -> VertOut {
    let c = cos(v.irot);
    let s = sin(v.irot);
    let rotated = vec2<f32>(
        v.pos.x * c - v.pos.y * s,
        v.pos.x * s + v.pos.y * c
    ) * v.iscale;
    let world = vec3<f32>(v.ipos.xy + rotated, v.ipos.z);
    var out: VertOut;
    out.clip = globals.view_proj * vec4<f32>(world, 1.0);
    out.uv = v.uv;
    out.color = v.icolor;
    return out;
}

@fragment
fn fs_main(in: VertOut) -> @location(0) vec4<f32> {
    let texel = textureSample(tex, samp, in.uv);
    return texel * in.color;
}
