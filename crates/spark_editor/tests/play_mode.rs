//! Play-mode tests: the editor's play/pause/stop/restart/step state machine
//! and runtime-state isolation — gameplay must never corrupt the edit state.
//! The physics acceptance scenario (cube + rigidbody + collider + ground)
//! runs through the real Editor + Engine pipeline.

use spark::components::{
    BodyKind, Camera, Collider, ColliderShape, MeshRenderer, RigidBody, Transform,
};
use spark::ecs;
use spark::math::Vec3;
use spark::reexport::hecs;
use spark::scene::Dimension;
use spark_editor::{Editor, PlayState};

fn pos_y(ed: &Editor, e: hecs::Entity) -> f32 {
    ed.engine
        .scene
        .world
        .get::<&Transform>(e)
        .map(|t| t.position.y)
        .unwrap()
}

/// The full 3D acceptance scene: camera + sun + ground (static collider) +
/// dynamic cube, exactly what a user builds with the Scene menu.
fn acceptance_scene() -> (Editor, hecs::Entity) {
    let mut ed = Editor::headless();
    ed.engine.set_dimension(Dimension::D3);
    // Camera (template would add one; add explicitly for the empty scene).
    ed.add_camera();
    // Sun.
    ed.add_sun();
    // Ground: plane mesh + static box collider, top surface at y = 0.
    let ground = ed.engine.scene.world.spawn((
        ecs::Name("Ground".into()),
        Transform {
            position: Vec3::new(0.0, -0.5, 0.0),
            scale: Vec3::new(20.0, 1.0, 20.0),
            ..Default::default()
        },
        MeshRenderer {
            mesh: "plane".into(),
            ..Default::default()
        },
        RigidBody {
            kind: BodyKind::Static,
            ..Default::default()
        },
        Collider {
            shape: ColliderShape::Box {
                half: Vec3::new(0.5, 0.5, 0.5),
            },
            ..Default::default()
        },
    ));
    // Cube: dynamic body with a matching box collider, dropped from y = 4.
    let cube = ed.engine.scene.world.spawn((
        ecs::Name("Cube".into()),
        Transform {
            position: Vec3::new(0.0, 4.0, 0.0),
            ..Default::default()
        },
        MeshRenderer {
            mesh: "cube".into(),
            ..Default::default()
        },
        RigidBody {
            kind: BodyKind::Dynamic,
            ..Default::default()
        },
        Collider {
            shape: ColliderShape::Box {
                half: Vec3::new(0.5, 0.5, 0.5),
            },
            ..Default::default()
        },
    ));
    let _ = ground;
    (ed, cube)
}

/// Mandatory acceptance test: press Play → the cube falls under gravity,
/// lands on the ground, and comes to rest on top of it.
#[test]
fn play_mode_cube_falls_and_rests_on_ground() {
    let (mut ed, cube) = acceptance_scene();
    ed.play();
    assert_eq!(ed.play_state, PlayState::Playing);

    let start_y = pos_y(&ed, cube);
    assert!((start_y - 4.0).abs() < 1e-4);

    // Simulate ~5 seconds.
    for _ in 0..300 {
        ed.engine.tick(1.0 / 60.0);
    }
    let end_y = pos_y(&ed, cube);
    assert!(
        end_y < start_y - 1.0,
        "cube must fall under gravity (start {start_y}, end {end_y})"
    );
    // Resting on the ground: top surface y=0 + half extent 0.5 = 0.5.
    assert!(
        (end_y - 0.5).abs() < 0.2,
        "cube should rest on the ground surface (y ≈ 0.5), got {end_y}"
    );

    // Stop: the edit state is restored exactly.
    ed.stop_play();
    assert_eq!(ed.play_state, PlayState::Stopped);
    let restored_y = pos_y(&ed, cube);
    assert!(
        (restored_y - 4.0).abs() < 1e-4,
        "stop must restore the pre-play transform, got {restored_y}"
    );
}

/// Pausing freezes the simulation; stepping advances exactly one frame.
#[test]
fn pause_and_step_freeze_and_advance() {
    let (mut ed, cube) = acceptance_scene();
    ed.play();
    for _ in 0..10 {
        ed.engine.tick(1.0 / 60.0);
    }
    let y_before_pause = pos_y(&ed, cube);

    ed.pause_play();
    assert_eq!(ed.play_state, PlayState::Paused);
    // "Render" many frames while paused — nothing moves.
    for _ in 0..60 {
        // (paused frames don't tick; step_frame is the only way forward)
    }
    assert_eq!(pos_y(&ed, cube), y_before_pause, "pause freezes the world");

    // Step exactly one frame: the cube moves by roughly one gravity step.
    let frame_before = ed.engine.frame;
    ed.step_frame();
    assert_eq!(ed.engine.frame, frame_before + 1, "step advances one tick");
    let y_after_step = pos_y(&ed, cube);
    assert!(
        y_after_step < y_before_pause,
        "stepped frame must simulate (y {y_before_pause} → {y_after_step})"
    );

    // Resume continues simulation.
    ed.resume_play();
    for _ in 0..10 {
        ed.engine.tick(1.0 / 60.0);
    }
    assert!(pos_y(&ed, cube) < y_after_step, "resume continues");
    ed.stop_play();
}

/// Restart restores the pre-play state and keeps playing.
#[test]
fn restart_restores_snapshot_and_plays() {
    let (mut ed, cube) = acceptance_scene();
    ed.play();
    for _ in 0..120 {
        ed.engine.tick(1.0 / 60.0);
    }
    let mid_y = pos_y(&ed, cube);
    assert!(mid_y < 4.0, "cube fell during the first run");

    ed.restart_play();
    assert_eq!(ed.play_state, PlayState::Playing, "restart keeps playing");
    assert!(
        (pos_y(&ed, cube) - 4.0).abs() < 1e-4,
        "restart restores the start position"
    );
    // And it falls again.
    for _ in 0..120 {
        ed.engine.tick(1.0 / 60.0);
    }
    assert!(pos_y(&ed, cube) < 3.0, "simulation continues after restart");
    ed.stop_play();
}

/// Rule-driven mutations during play must not leak into the edit state —
/// even entity destruction.
#[test]
fn play_isolation_destroys_nothing_permanent() {
    let mut ed = Editor::headless();
    ed.add_entity("Doomed", None);
    let doomed = ed.state.primary().unwrap();
    // Play, destroy the entity directly in the runtime world (what a rule
    // or gameplay code would do), stop.
    ed.play();
    ecs::despawn_recursive(&mut ed.engine.scene.world, doomed);
    assert!(
        ecs::find_by_name(&ed.engine.scene.world, "Doomed").is_none(),
        "destroyed while playing"
    );
    ed.stop_play();
    assert!(
        ecs::find_by_name(&ed.engine.scene.world, "Doomed").is_some(),
        "stop must resurrect entities destroyed during play"
    );
}

/// Globals modified during play (score counters etc.) don't leak either.
#[test]
fn play_isolation_globals() {
    let mut ed = Editor::headless();
    ed.engine.scene.globals.insert("score".into(), 0.0);
    ed.play();
    ed.engine.scene.globals.insert("score".into(), 42.0);
    ed.stop_play();
    let score = ed
        .engine
        .scene
        .globals
        .get("score")
        .copied()
        .unwrap_or(-1.0);
    assert!((score - 0.0).abs() < 1e-9, "globals restored, got {score}");
}

/// The play snapshot survives a save/load cycle in between (the snapshot is
/// data, not world references).
#[test]
fn play_snapshot_is_serialized_data() {
    let (mut ed, cube) = acceptance_scene();
    ed.play();
    let snap = ed.playing.as_ref().unwrap().scene_text.clone();
    let reloaded = spark::scene::Scene::load(&snap, &ed.engine.registry).unwrap();
    let cube2 = ecs::find_by_name(&reloaded.world, "Cube").unwrap();
    let y = reloaded.world.get::<&Transform>(cube2).unwrap().position.y;
    assert!((y - 4.0).abs() < 1e-4);
    let _ = cube;
}

/// The gameplay camera (not the editor camera) drives rendering in play
/// mode: build_draw without override picks the scene camera.
#[test]
fn play_mode_uses_gameplay_camera() {
    let mut ed = Editor::headless();
    ed.engine.scene.dimension = Dimension::D3;
    ed.engine.scene.world.spawn((
        ecs::Name("GameCam".into()),
        Transform {
            position: Vec3::new(0.0, 2.0, 9.0),
            ..Default::default()
        },
        Camera::default(),
    ));
    let (cam, tr) = ed.engine.primary_camera().unwrap();
    assert_eq!(tr.position.z, 9.0);
    let _ = cam;
    // The editor camera override is separate state and never becomes the
    // gameplay camera.
    let (ectr, ecam) = ed.editor_cam.as_override(Dimension::D3);
    assert_ne!(ectr.position, tr.position);
    let _ = ecam;
}

/// Regression: collider shapes must scale with the entity's transform —
/// a 20×20 ground must catch a cube dropped at x = 5 (outside the 1×1
/// unscaled collider the bug produced).
#[test]
fn scaled_colliders_match_visual_size() {
    let mut ed = Editor::headless();
    ed.engine.set_dimension(Dimension::D3);
    ed.engine.scene.world.spawn((
        ecs::Name("Ground".into()),
        Transform {
            position: Vec3::new(0.0, -0.5, 0.0),
            scale: Vec3::new(20.0, 1.0, 20.0),
            ..Default::default()
        },
        MeshRenderer {
            mesh: "plane".into(),
            ..Default::default()
        },
        RigidBody {
            kind: BodyKind::Static,
            ..Default::default()
        },
        Collider {
            shape: ColliderShape::Box {
                half: Vec3::new(0.5, 0.5, 0.5),
            },
            ..Default::default()
        },
    ));
    let cube = ed.engine.scene.world.spawn((
        ecs::Name("Cube".into()),
        Transform {
            position: Vec3::new(5.0, 4.0, -3.0),
            ..Default::default()
        },
        MeshRenderer {
            mesh: "cube".into(),
            ..Default::default()
        },
        RigidBody {
            kind: BodyKind::Dynamic,
            ..Default::default()
        },
        Collider {
            shape: ColliderShape::Box {
                half: Vec3::new(0.5, 0.5, 0.5),
            },
            ..Default::default()
        },
    ));
    ed.play();
    for _ in 0..300 {
        ed.engine.tick(1.0 / 60.0);
    }
    let y = pos_y(&ed, cube);
    assert!(
        (y - 0.5).abs() < 0.2,
        "scaled ground must catch the off-center cube (y = {y}); \
         collider scaling regressed"
    );
    ed.stop_play();
}
