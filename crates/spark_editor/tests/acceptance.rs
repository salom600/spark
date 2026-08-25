//! Final acceptance scenarios — the editor workflows a user actually
//! performs, executed end-to-end against the real Editor + Engine
//! (headless): project creation, entity construction via the same methods
//! the menus call, editing, serialization, play mode, physics, restore.
//!
//! These are the contract tests from the engine's development plan:
//! **3D:** create → scene → camera → light → cube → transform → rigidbody →
//! collider → ground → save → reload → play → physics → stop.
//! **2D:** create → scene → camera → sprite → texture import →
//! move/rotate/rect → collider → play.

mod audio_helpers;

use spark::components::{
    BodyKind, Camera, CameraKind, Collider, Music, RigidBody, Sprite, Transform,
};
use spark::ecs;
use spark::math::{Vec2, Vec3};
use spark::prelude::*;
use spark::reexport::hecs;
use spark::scene::Dimension;
use spark_editor::{Editor, PlayState};

/// Write a small valid PNG (8×8 solid magenta) through the image crate —
/// the same decoder the engine's asset pipeline uses.
fn write_png(path: &std::path::Path) {
    let img = image::RgbaImage::from_pixel(8, 8, image::Rgba([255, 0, 255, 255]));
    img.save(path).unwrap();
}

fn pos(ed: &Editor, e: hecs::Entity) -> Vec3 {
    ed.engine
        .scene
        .world
        .get::<&Transform>(e)
        .map(|t| t.position)
        .unwrap()
}

/// The full 3D acceptance scenario.
#[test]
fn acceptance_3d_full_workflow() {
    // New 3D project (real template on disk).
    let dir = std::env::temp_dir().join(format!("spark_acc3d_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    Project::create_from_template(&dir, "Accept3D", Dimension::D3).unwrap();
    let mut ed = Editor::headless();
    ed.open_project(&dir);

    // Template guarantees: camera (perspective) + sun (directional).
    let cam = ecs::find_by_name(&ed.engine.scene.world, "Camera").unwrap();
    let kind = ed.engine.scene.world.get::<&Camera>(cam).unwrap().kind;
    assert!(matches!(kind, CameraKind::Perspective { .. }));
    assert!(ecs::find_by_name(&ed.engine.scene.world, "Sun").is_some());

    // Add Cube (the Scene menu path).
    ed.add_mesh("cube");
    let cube = ed.state.primary().unwrap();
    // Transform it like a user would (inspector edit + undoable).
    let before = ed.snapshot_component(cube, "Transform").unwrap();
    ed.engine
        .scene
        .world
        .insert_one(
            cube,
            Transform {
                position: Vec3::new(0.0, 4.0, 0.0),
                rotation: Vec3::new(15.0, 30.0, 0.0),
                scale: Vec3::new(1.0, 1.0, 1.0),
            },
        )
        .ok();
    let after = ed.snapshot_component(cube, "Transform").unwrap();
    ed.push_component_cmd(cube, "Transform", Some(before), Some(after), "Edit");

    // Undo/redo round trip on the edit (before more commands stack up).
    ed.apply_undo();
    assert_eq!(
        pos(&ed, cube),
        Vec3::ZERO,
        "undo restores the pre-edit transform"
    );
    ed.apply_redo();
    assert_eq!(
        pos(&ed, cube),
        Vec3::new(0.0, 4.0, 0.0),
        "redo re-applies it"
    );

    // Add RigidBody + Collider (the Add Component path).
    let entry = ed.engine.registry.get("RigidBody").unwrap();
    (entry.add_default)(&mut ed.engine.scene.world, cube);
    let entry = ed.engine.registry.get("Collider").unwrap();
    (entry.add_default)(&mut ed.engine.scene.world, cube);
    assert!(
        ed.engine
            .scene
            .world
            .get::<&RigidBody>(cube)
            .map(|r| r.kind == BodyKind::Dynamic)
            .unwrap_or(false)
    );

    // Add Ground.
    ed.add_ground();

    // Save → reload → same result.
    ed.save_scene();
    ed.load_scene();
    let cube2 = ecs::find_by_name(&ed.engine.scene.world, "cube").unwrap();
    let p = pos(&ed, cube2);
    assert_eq!(p, Vec3::new(0.0, 4.0, 0.0));
    assert!(ed.engine.scene.world.get::<&RigidBody>(cube2).is_ok());
    assert!(ed.engine.scene.world.get::<&Collider>(cube2).is_ok());
    assert!(ecs::find_by_name(&ed.engine.scene.world, "Ground").is_some());

    // Play → physics → cube falls and rests on the ground.
    ed.play();
    assert_eq!(ed.play_state, PlayState::Playing);
    for _ in 0..300 {
        ed.engine.tick(1.0 / 60.0);
    }
    let rest = pos(&ed, cube2).y;
    assert!(
        (rest - 0.5).abs() < 0.2,
        "cube rests on the ground after play (y={rest})"
    );

    // Stop → the editor state is not corrupted.
    ed.stop_play();
    let restored = pos(&ed, cube2);
    assert_eq!(restored, Vec3::new(0.0, 4.0, 0.0), "edit state intact");
    std::fs::remove_dir_all(&dir).ok();
}

/// The full 2D acceptance scenario.
#[test]
fn acceptance_2d_full_workflow() {
    let dir = std::env::temp_dir().join(format!("spark_acc2d_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    Project::create_from_template(&dir, "Accept2D", Dimension::D2).unwrap();
    let mut ed = Editor::headless();
    ed.open_project(&dir);

    // Template camera is orthographic.
    let cam = ecs::find_by_name(&ed.engine.scene.world, "Camera").unwrap();
    let kind = ed.engine.scene.world.get::<&Camera>(cam).unwrap().kind;
    assert!(matches!(kind, CameraKind::Ortho2D { .. }));

    // Import a texture (the real import path) and spawn a Sprite via the
    // drop path.
    let src = dir.join("player_src.png");
    write_png(&src);
    ed.import_asset(src.to_str().unwrap());
    assert!(
        ed.engine
            .assets
            .list(AssetKind::Texture)
            .contains(&"assets/player_src.png".to_string())
    );
    // The texture actually decodes (the preview path).
    assert!(
        ed.engine.assets.texture("assets/player_src.png").is_some(),
        "imported PNG must decode"
    );

    ed.spawn_asset_entity("assets/player_src.png", Some(Vec3::new(1.0, 2.0, 0.0)));
    let sprite_e = ed.state.primary().unwrap();
    let image = ed
        .engine
        .scene
        .world
        .get::<&Sprite>(sprite_e)
        .map(|s| s.image.clone())
        .unwrap();
    assert_eq!(image, "assets/player_src.png");

    // Move + rotate + resize (rect semantics via Sprite size).
    ed.engine
        .scene
        .world
        .insert_one(
            sprite_e,
            Transform {
                position: Vec3::new(3.0, -1.0, 0.0),
                rotation: Vec3::new(0.0, 0.0, 45.0),
                ..Default::default()
            },
        )
        .ok();
    if let Ok(mut sp) = ed.engine.scene.world.get::<&mut Sprite>(sprite_e) {
        sp.size = Vec2::new(2.0, 3.0);
    }

    // Collider → the physics pipeline owns it.
    let entry = ed.engine.registry.get("Collider").unwrap();
    (entry.add_default)(&mut ed.engine.scene.world, sprite_e);
    assert!(ed.engine.scene.world.get::<&Collider>(sprite_e).is_ok());

    // Save/reload round trip preserves everything.
    ed.save_scene();
    ed.load_scene();
    let s2 = ecs::find_by_name(&ed.engine.scene.world, "player_src.png").unwrap();
    let (t_pos, t_rot) = ed
        .engine
        .scene
        .world
        .get::<&Transform>(s2)
        .map(|t| (t.position, t.rotation))
        .unwrap();
    assert_eq!(t_pos, Vec3::new(3.0, -1.0, 0.0));
    assert_eq!(t_rot.z, 45.0);
    let sp_size = ed
        .engine
        .scene
        .world
        .get::<&Sprite>(s2)
        .map(|sp| sp.size)
        .unwrap();
    assert_eq!(sp_size, Vec2::new(2.0, 3.0));
    assert!(ed.engine.scene.world.get::<&Collider>(s2).is_ok());

    // Play: the sprite renders in the draw list with its transform.
    ed.play();
    let draw =
        spark::render::build_frame_draw(&ed.engine.scene, &mut ed.engine.assets, 1.778, None);
    let instances: usize = draw.sprites.iter().map(|(_, v)| v.len()).sum();
    assert_eq!(
        instances, 1,
        "the sprite must be in the play-mode draw list"
    );
    let sp_pos = draw.sprites[0].1[0].pos;
    assert_eq!(sp_pos[0], 3.0);
    assert_eq!(sp_pos[1], -1.0);
    ed.engine.tick(1.0 / 60.0);
    ed.stop_play();
    // The transform survives the play round trip.
    let t_pos = ed
        .engine
        .scene
        .world
        .get::<&Transform>(s2)
        .map(|t| t.position)
        .unwrap();
    assert_eq!(t_pos, Vec3::new(3.0, -1.0, 0.0));
    std::fs::remove_dir_all(&dir).ok();
}

/// A Music component assigned from the asset browser plays in play mode
/// (headless bookkeeping).
#[test]
fn acceptance_audio_assign_and_play() {
    let dir = std::env::temp_dir().join(format!("spark_acc_audio_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    Project::create_from_template(&dir, "AcceptAudio", Dimension::D2).unwrap();
    let mut ed = Editor::headless();
    ed.open_project(&dir);

    // Generate a WAV and import it.
    crate::audio_helpers::write_wav(&dir.join("loop_src.wav"));
    ed.import_asset(dir.join("loop_src.wav").to_str().unwrap());

    // Assign as Music on the selected entity (the asset browser path).
    ed.add_entity("Jukebox", None);
    ed.assign_asset("assets/loop_src.wav", "Music");
    let e = ed.state.primary().unwrap();
    assert!(
        ed.engine
            .scene
            .world
            .get::<&Music>(e)
            .map(|m| m.track == "assets/loop_src.wav")
            .unwrap_or(false)
    );

    ed.play();
    ed.engine.tick(1.0 / 60.0);
    assert_eq!(
        ed.engine.playing_track.as_deref(),
        Some("assets/loop_src.wav")
    );
    ed.stop_play();
    assert!(ed.engine.playing_track.is_none(), "stop halts music");
    std::fs::remove_dir_all(&dir).ok();
}
