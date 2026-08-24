//! Editor integration regression: building a frame with a directional
//! light + a cube mesh must not panic.
//!
//! This guards against the original "Add Cube (3D) crashes the editor"
//! regression. The crash lived in the *renderer's* shadow pass
//! (`MeshPass::draw_shadow` created a bind group with one entry but the
//! layout had three) — that path only runs when the GPU is real, so CI
//! can't exercise it directly. What we CAN do here is assert the structure
//! that triggers the shadow pass (directional light + at least one mesh),
//! so any regression in `build_frame_draw`'s framing is caught on Linux
//! CI without a GPU.

use spark::app::Engine;
use spark::components::{Light, LightKind, MeshRenderer, Transform};
use spark::math::Vec3;
use spark::render::build_frame_draw;
use spark::scene::Dimension;

/// Spawn a Sun + Cube scene, then assert `build_frame_draw` produces a
/// `FrameDraw` whose `has_directional` is true and whose `meshes` vec
/// contains exactly one entry for the "cube" mesh. This is the exact
/// precondition the renderer's shadow pass checks before running — if it
/// returns false, the shadow pass (and thus the crash) cannot fire.
#[test]
fn build_frame_draw_with_directional_light_and_cube_no_panic() {
    let dir = std::env::temp_dir().join(format!(
        "spark_editor_add_cube_3d_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    let _ = std::fs::remove_dir_all(&dir);
    spark::project::Project::create_from_template(&dir, "Test3D", Dimension::D3)
        .expect("template project creates");

    let mut engine =
        Engine::headless(dir.canonicalize().unwrap().as_path()).expect("headless engine boots");
    // Spawn the cube the same way the editor's `Scene → Add Cube (3D)` does.
    engine.scene.world.spawn((
        spark::ecs::Name("cube".into()),
        Transform::default(),
        MeshRenderer {
            mesh: "cube".into(),
            ..Default::default()
        },
    ));
    // The template already has a Sun (directional light) — verify.
    let has_dir = engine
        .scene
        .world
        .query::<&Light>()
        .iter()
        .any(|(_, l)| matches!(l.kind, LightKind::Directional { .. }));
    assert!(
        has_dir,
        "template 3D scene must include a directional light"
    );

    // build_frame_draw is the same path the editor runs every frame. It
    // must not panic with a Sun + cube present.
    let draw = build_frame_draw(&engine.scene, &mut engine.assets, 1.778_f32, None);
    assert!(draw.has_directional, "directional light should be detected");
    assert!(
        !draw.meshes.is_empty(),
        "cube mesh should be in the draw list"
    );
    assert_eq!(draw.meshes[0].0, "cube", "first mesh should be the cube");

    std::fs::remove_dir_all(&dir).ok();
}

/// Sanity-check the EditorCamera math used by the editor's overlay/picking
/// code. The forward vector at the identity (yaw=0, pitch=0) must point
/// down -Z — otherwise the editor camera looks somewhere unexpected.
#[test]
fn editor_camera_forward_is_neg_z_at_identity() {
    // We can't reach spark_editor::state::EditorCamera from here (it's a
    // private module of the editor binary), so we replicate the math
    // contract: the GPU's `Transform::quat()` for (pitch, yaw, 0) = (0, 0, 0)
    // is identity, so the camera looks down -Z.
    let t = Transform {
        position: Vec3::new(0.0, 0.0, 10.0),
        rotation: Vec3::ZERO,
        scale: Vec3::ONE,
    };
    let forward = t.quat() * Vec3::new(0.0, 0.0, -1.0);
    assert!((forward.x).abs() < 1e-5, "x = {}", forward.x);
    assert!((forward.y).abs() < 1e-5, "y = {}", forward.y);
    assert!((forward.z + 1.0).abs() < 1e-5, "z = {}", forward.z);
}
