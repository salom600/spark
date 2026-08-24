// spark — 3D mesh pass (PBR-flavored lighting).
// One directional light with a PCF 3x3 shadow map + up to 16 point lights
// + ambient + emissive, with an `unlit` escape hatch.

struct Globals {
    view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
    light_view_proj: mat4x4<f32>,
    dir_light: vec4<f32>,       // xyz = direction (towards scene), w = present
    dir_light_color: vec4<f32>, // rgb * intensity, a = ambient
    light_meta: vec4<f32>,      // x = point count, y = shadow bias
    point_lights: array<vec4<f32>, 32>, // pairs: (pos.xyz, range), (color, pad)
};

@group(0) @binding(0) var<uniform> globals: Globals;
@group(0) @binding(1) var shadow_tex: texture_depth_2d;
@group(0) @binding(2) var shadow_samp: sampler_comparison;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;

struct VertIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    // Instance attributes.
    @location(3) m0: vec4<f32>,
    @location(4) m1: vec4<f32>,
    @location(5) m2: vec4<f32>,
    @location(6) m3: vec4<f32>,
    @location(7) color: vec4<f32>,
    @location(8) params: vec4<f32>,  // metallic, roughness, unlit, -
    @location(9) emissive: vec4<f32>,
};

struct VertOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) world_normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) color: vec4<f32>,
    @location(4) params: vec4<f32>,
    @location(5) emissive: vec4<f32>,
};

@vertex
fn vs_main(v: VertIn) -> VertOut {
    let model = mat4x4<f32>(v.m0, v.m1, v.m2, v.m3);
    let world = model * vec4<f32>(v.position, 1.0);
    // Upper-left 3x3 is fine for uniform-ish scale; normalize in fs.
    let n = (model * vec4<f32>(v.normal, 0.0)).xyz;
    var out: VertOut;
    out.clip = globals.view_proj * world;
    out.world_pos = world.xyz;
    out.world_normal = n;
    out.uv = v.uv;
    out.color = v.color;
    out.params = v.params;
    out.emissive = v.emissive;
    return out;
}

fn shadow_factor(world_pos: vec3<f32>, n_dot_l: f32) -> f32 {
    if (globals.dir_light.w < 0.5) {
        return 1.0;
    }
    let light_pos = globals.light_view_proj * vec4<f32>(world_pos, 1.0);
    var uv = light_pos.xy / light_pos.w;
    uv = uv * 0.5 + vec2<f32>(0.5, 0.5);
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0 || light_pos.w <= 0.0) {
        return 1.0;
    }
    let depth = (light_pos.z / light_pos.w) - globals.light_meta.y * (1.0 + (1.0 - n_dot_l) * 4.0);
    var lit: f32 = 0.0;
    let texel = 1.0 / 2048.0;
    // PCF 3x3.
    for (var dy: i32 = -1; dy <= 1; dy++) {
        for (var dx: i32 = -1; dx <= 1; dx++) {
            let offs = vec2<f32>(f32(dx), f32(dy)) * texel;
            lit += textureSampleCompare(
                shadow_tex, shadow_samp, uv + offs, depth
            );
        }
    }
    return lit / 9.0;
}

fn ggx_specular(n: vec3<f32>, l: vec3<f32>, v: vec3<f32>, roughness: f32) -> f32 {
    let h = normalize(l + v);
    let n_dot_h = max(dot(n, h), 0.0);
    let a = roughness * roughness;
    let a2 = a * a;
    let d = a2 / (3.14159 * pow(n_dot_h * n_dot_h * (a2 - 1.0) + 1.0, 2.0));
    return clamp(d, 0.0, 1.0);
}

@fragment
fn fs_main(in: VertOut) -> @location(0) vec4<f32> {
    let texel = textureSample(tex, samp, in.uv);
    let albedo = texel.rgb * in.color.rgb;
    let alpha = texel.a * in.color.a;

    if (in.params.z > 0.5) {
        return vec4<f32>(albedo, alpha);
    }

    let n = normalize(in.world_normal);
    let v = normalize(globals.camera_pos.xyz - in.world_pos);
    let metallic = in.params.x;
    let roughness = max(in.params.y, 0.04);

    var color = albedo * globals.dir_light_color.a; // ambient

    // Directional light (shadowed).
    if (globals.dir_light.w > 0.5) {
        let l = normalize(-globals.dir_light.xyz);
        let n_dot_l = max(dot(n, l), 0.0);
        if (n_dot_l > 0.0) {
            let shadow = shadow_factor(in.world_pos, n_dot_l);
            let diff = albedo * n_dot_l;
            let spec = mix(vec3<f32>(1.0), albedo, metallic) * ggx_specular(n, l, v, roughness) * (1.0 - roughness * 0.5);
            color += globals.dir_light_color.rgb * (diff + spec) * shadow;
        }
    }

    // Point lights.
    let count = i32(globals.light_meta.x);
    for (var i: i32 = 0; i < count; i++) {
        let meta = globals.point_lights[i * 2];
        let lcol = globals.point_lights[i * 2 + 1].rgb;
        let to_light = meta.xyz - in.world_pos;
        let dist = length(to_light);
        if (dist < 0.001 || dist > meta.w) {
            continue;
        }
        let l = to_light / dist;
        let n_dot_l = max(dot(n, l), 0.0);
        let falloff = pow(clamp(1.0 - dist / meta.w, 0.0, 1.0), 2.0);
        let diff = albedo * n_dot_l;
        let spec = mix(vec3<f32>(1.0), albedo, metallic) * ggx_specular(n, l, v, roughness) * (1.0 - roughness * 0.5);
        color += lcol * (diff + spec) * falloff;
    }

    color += in.emissive.rgb;
    return vec4<f32>(color, alpha);
}
