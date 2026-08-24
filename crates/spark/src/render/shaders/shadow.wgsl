// spark — directional shadow pass (depth only).
// Renders mesh instances from the light's perspective into the shadow map.

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

struct VertIn {
    @location(0) position: vec3<f32>,
    @location(3) m0: vec4<f32>,
    @location(4) m1: vec4<f32>,
    @location(5) m2: vec4<f32>,
    @location(6) m3: vec4<f32>,
};

struct VertOut {
    @builtin(position) clip: vec4<f32>,
};

@vertex
fn vs_main(v: VertIn) -> VertOut {
    let model = mat4x4<f32>(v.m0, v.m1, v.m2, v.m3);
    var out: VertOut;
    out.clip = globals.light_view_proj * model * vec4<f32>(v.position, 1.0);
    return out;
}
