//! Offline WGSL validation — regression test for the mesh.wgsl `meta`
//! reserved-keyword crash. Parses each shader with naga (the same front-end
//! wgpu uses at `create_shader_module` time), so CI catches parse/reserved
//! errors on every commit without a GPU.

macro_rules! wgsl_parses {
    ($name:ident, $path:literal) => {
        #[test]
        fn $name() {
            let src = include_str!($path);
            naga::front::wgsl::parse_str(src)
                .unwrap_or_else(|e| panic!("{}: {e:?}", $path));
        }
    };
}

wgsl_parses!(sprite_wgsl_parses, "../src/render/shaders/sprite.wgsl");
wgsl_parses!(mesh_wgsl_parses, "../src/render/shaders/mesh.wgsl");
wgsl_parses!(shadow_wgsl_parses, "../src/render/shaders/shadow.wgsl");
