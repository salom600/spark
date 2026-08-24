//! Offline WGSL validation — regression test for the mesh.wgsl `meta`
//! reserved-keyword crash. Parses each shader with naga (the same front-end
//! wgpu uses at `create_shader_module` time), so CI catches parse/reserved
//! errors on every commit without a GPU.

macro_rules! wgsl_parses {
    ($name:ident, $path:literal) => {
        #[test]
        fn $name() {
            let src = include_str!($path);
            naga::front::wgsl::parse_str(src).unwrap_or_else(|e| panic!("{}: {e:?}", $path));
        }
    };
}

wgsl_parses!(sprite_wgsl_parses, "../src/render/shaders/sprite.wgsl");
wgsl_parses!(mesh_wgsl_parses, "../src/render/shaders/mesh.wgsl");
wgsl_parses!(shadow_wgsl_parses, "../src/render/shaders/shadow.wgsl");

/// The `Globals` uniform struct is declared **twice**: as a `#[repr(C)]`
/// Rust struct and as a WGSL struct. Their memory layouts must match
/// exactly, or the GPU reads garbage (lighting flicker, wrong shadows).
/// This test lays the WGSL struct out with naga's WGSL-layout algorithm
/// and compares the byte size against the Rust side — a real
/// regression guard for every future field added to either side.
#[test]
fn globals_uniform_layout_matches_rust() {
    let src = include_str!("../src/render/shaders/mesh.wgsl");
    let module = naga::front::wgsl::parse_str(src).expect("mesh.wgsl parses");
    let mut layouter = naga::proc::Layouter::default();
    layouter
        .update(naga::proc::GlobalCtx {
            types: &module.types,
            constants: &module.constants,
            global_expressions: &module.global_expressions,
            overrides: &module.overrides,
        })
        .expect("layouter runs");
    let (handle, _ty) = module
        .types
        .iter()
        .find(|(_, t)| t.name.as_deref() == Some("Globals"))
        .expect("Globals struct declared in mesh.wgsl");
    let wgsl_size = layouter[handle].size as usize;
    let rust_size = std::mem::size_of::<spark::render::Globals>();
    assert_eq!(
        wgsl_size, rust_size,
        "Globals layout mismatch: WGSL {wgsl_size} bytes vs Rust {rust_size} bytes — \
         the uniform struct declarations have drifted apart"
    );
}
