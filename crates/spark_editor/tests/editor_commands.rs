//! Editor command battery: these tests drive the *real* editor (headless —
//! no window, no GPU) through the same methods the UI calls, and verify
//! undo/redo actually restores the scene — not just that commands exist.

use spark::ecs;
use spark::math::Vec3;
use spark::prelude::*;
use spark::reexport::hecs;
use spark_editor::Editor;

fn pos_of(ed: &Editor, e: hecs::Entity) -> Vec3 {
    ed.engine
        .scene
        .world
        .get::<&Transform>(e)
        .map(|t| t.position)
        .unwrap()
}

fn name_of(ed: &Editor, e: hecs::Entity) -> String {
    ed.engine
        .scene
        .world
        .get::<&ecs::Name>(e)
        .map(|n| n.0.clone())
        .unwrap_or_default()
}

fn count(ed: &Editor) -> usize {
    ed.engine.scene.world.iter().count()
}

#[test]
fn create_entity_undo_redo() {
    let mut ed = Editor::headless();
    ed.add_entity("MyCube", None);
    assert_eq!(count(&ed), 1);
    assert_eq!(
        ed.state.primary().map(|e| name_of(&ed, e)),
        Some("MyCube".into())
    );

    // Undo removes it.
    ed.apply_undo();
    assert_eq!(count(&ed), 0, "undo must despawn the created entity");

    // Redo brings it back (with a fresh entity id; selection is not
    // resurrected — the same behavior as destructive undo in other editors).
    ed.apply_redo();
    assert_eq!(count(&ed), 1, "redo must respawn the entity");
    assert!(
        ecs::find_by_name(&ed.engine.scene.world, "MyCube").is_some(),
        "respawned entity must carry its name/components"
    );
}

#[test]
fn delete_undo_restores_subtree_hierarchy() {
    let mut ed = Editor::headless();
    ed.add_entity("Parent", None);
    let parent = ed.state.primary().unwrap();
    ed.add_entity("Child", Some(parent));
    let child = ed.state.primary().unwrap();
    // Move the child so we can verify the transform survives the round trip.
    ed.engine
        .scene
        .world
        .insert_one(
            child,
            Transform {
                position: Vec3::new(3.0, 1.0, 0.0),
                ..Default::default()
            },
        )
        .ok();
    assert_eq!(count(&ed), 2);

    // Select the parent and delete the subtree.
    ed.state.select(parent);
    ed.despawn_selected();
    assert_eq!(count(&ed), 0, "delete must remove the whole subtree");

    // Undo restores BOTH entities and their parent/child link.
    ed.apply_undo();
    assert_eq!(count(&ed), 2, "undo must restore the whole subtree");
    let parent2 = ecs::find_by_name(&ed.engine.scene.world, "Parent").unwrap();
    let child2 = ecs::find_by_name(&ed.engine.scene.world, "Child").unwrap();
    let link = ed
        .engine
        .scene
        .world
        .get::<&ecs::Parent>(child2)
        .map(|p| p.0)
        .unwrap();
    assert_eq!(link, parent2, "child must be re-parented to its old parent");
    assert_eq!(pos_of(&ed, child2), Vec3::new(3.0, 1.0, 0.0));

    // Redo deletes again.
    ed.apply_redo();
    assert_eq!(count(&ed), 0);
}

#[test]
fn duplicate_undo() {
    let mut ed = Editor::headless();
    ed.add_entity("Cube", None);
    let orig = ed.state.primary().unwrap();
    ed.duplicate_selected();
    assert_eq!(count(&ed), 2);
    let copy = ed.state.primary().unwrap();
    assert_ne!(copy, orig);
    assert_eq!(name_of(&ed, copy), "Cube copy");

    ed.apply_undo();
    assert_eq!(count(&ed), 1, "undo must remove the duplicate");
    // The original survives.
    assert_eq!(name_of(&ed, orig), "Cube");
}

#[test]
fn rename_undo_redo() {
    let mut ed = Editor::headless();
    ed.add_entity("Bob", None);
    let e = ed.state.primary().unwrap();
    ed.rename_entity(e, "Alice");
    assert_eq!(name_of(&ed, e), "Alice");
    ed.apply_undo();
    assert_eq!(name_of(&ed, e), "Bob");
    ed.apply_redo();
    assert_eq!(name_of(&ed, e), "Alice");
}

#[test]
fn reparent_preserves_world_transform_and_undoes() {
    let mut ed = Editor::headless();
    // Parent A: position (10,0,0), yaw 90° (local +X → world -Z), scale 2.
    let a = ed.engine.scene.world.spawn((
        ecs::Name("A".into()),
        Transform {
            position: Vec3::new(10.0, 0.0, 0.0),
            rotation: Vec3::new(0.0, 90.0, 0.0),
            scale: Vec3::new(2.0, 2.0, 2.0),
        },
    ));
    // Child B: local (1,0,0) → world (10,0,-2).
    let b = ed.engine.scene.world.spawn((
        ecs::Name("B".into()),
        Transform {
            position: Vec3::new(1.0, 0.0, 0.0),
            ..Default::default()
        },
    ));
    ecs::set_parent(&mut ed.engine.scene.world, b, Some(a));
    let wt = ecs::world_transform(&ed.engine.scene.world, b);
    assert!((wt.position.z + 2.0).abs() < 1e-4);

    // Reparent B to root: its world transform must be preserved.
    ed.reparent(b, None);
    let wt = ecs::world_transform(&ed.engine.scene.world, b);
    assert!((wt.position.x - 10.0).abs() < 1e-4, "x={}", wt.position.x);
    assert!((wt.position.z + 2.0).abs() < 1e-4, "z={}", wt.position.z);

    // Undo restores the hierarchy and the original local transform.
    ed.apply_undo();
    let parent = ed
        .engine
        .scene
        .world
        .get::<&ecs::Parent>(b)
        .map(|p| p.0)
        .unwrap();
    assert_eq!(parent, a);
    let local = ed
        .engine
        .scene
        .world
        .get::<&Transform>(b)
        .map(|t| *t)
        .unwrap();
    assert!((local.position.x - 1.0).abs() < 1e-4);
}

#[test]
fn reparent_rejects_cycles() {
    let mut ed = Editor::headless();
    let a = ed
        .engine
        .scene
        .world
        .spawn((ecs::Name("A".into()), Transform::default()));
    let b = ed
        .engine
        .scene
        .world
        .spawn((ecs::Name("B".into()), Transform::default()));
    ecs::set_parent(&mut ed.engine.scene.world, b, Some(a));
    // Parenting A under its own child B must be refused.
    ed.reparent(a, Some(b));
    let parent_of_a = ed
        .engine
        .scene
        .world
        .get::<&ecs::Parent>(a)
        .ok()
        .map(|p| p.0);
    assert_ne!(parent_of_a, Some(b), "cycle reparent must be rejected");
}

#[test]
fn multi_delete_undo() {
    let mut ed = Editor::headless();
    ed.add_entity("One", None);
    let one = ed.state.primary().unwrap();
    ed.add_entity("Two", None);
    let two = ed.state.primary().unwrap();
    ed.add_entity("Three", None);
    let three = ed.state.primary().unwrap();
    // Multi-select One and Three (ctrl-click style).
    ed.state.select(one);
    ed.state.toggle_select(three);
    ed.despawn_selected();
    assert_eq!(count(&ed), 1, "only Two survives");
    assert_eq!(name_of(&ed, two), "Two");

    ed.apply_undo();
    assert_eq!(count(&ed), 3, "undo restores both deleted entities");
}

#[test]
fn component_swap_undo_restores_transform() {
    let mut ed = Editor::headless();
    ed.add_entity("Moved", None);
    let e = ed.state.primary().unwrap();
    let before = ed.snapshot_component(e, "Transform");
    ed.engine
        .scene
        .world
        .insert_one(
            e,
            Transform {
                position: Vec3::new(5.0, 5.0, 5.0),
                ..Default::default()
            },
        )
        .ok();
    let after = ed.snapshot_component(e, "Transform");
    ed.push_component_cmd(e, "Transform", before, after, "Edit");
    assert_eq!(pos_of(&ed, e), Vec3::new(5.0, 5.0, 5.0));

    ed.apply_undo();
    assert_eq!(
        pos_of(&ed, e),
        Vec3::ZERO,
        "undo restores the old transform"
    );
    ed.apply_redo();
    assert_eq!(pos_of(&ed, e), Vec3::new(5.0, 5.0, 5.0));
}

#[test]
fn visibility_toggle_undo() {
    let mut ed = Editor::headless();
    ed.add_entity("Ghost", None);
    let e = ed.state.primary().unwrap();
    ed.toggle_visibility(e, true);
    let vis = ed
        .engine
        .scene
        .world
        .get::<&Visible>(e)
        .map(|v| v.0)
        .unwrap();
    assert!(!vis, "toggle must hide the entity");
    ed.apply_undo();
    // After undo the Visible component is removed again (default = visible).
    assert!(ed.engine.scene.world.get::<&Visible>(e).is_err());
}

#[test]
fn add_mesh_and_sprite_wire_components() {
    let mut ed = Editor::headless();
    ed.add_mesh("cube");
    let cube = ed.state.primary().unwrap();
    assert!(ed.engine.scene.world.get::<&MeshRenderer>(cube).is_ok());
    let mesh_name = ed
        .engine
        .scene
        .world
        .get::<&MeshRenderer>(cube)
        .map(|mr| mr.mesh.clone())
        .unwrap();
    assert_eq!(mesh_name, "cube");

    ed.add_sprite();
    let sp = ed.state.primary().unwrap();
    assert!(ed.engine.scene.world.get::<&Sprite>(sp).is_ok());

    // Both are undoable.
    ed.apply_undo();
    ed.apply_undo();
    assert_eq!(count(&ed), 0);
}

#[test]
fn undo_history_is_bounded_and_redo_clears() {
    let mut ed = Editor::headless();
    for i in 0..5 {
        ed.add_entity(&format!("E{i}"), None);
    }
    assert_eq!(count(&ed), 5);
    // A new action clears the redo stack.
    ed.apply_undo();
    assert_eq!(count(&ed), 4);
    ed.add_entity("Interrupt", None);
    assert!(!ed.undo.can_redo(), "new action must clear redo");
}

#[test]
fn scene_save_load_roundtrip_after_edits() {
    let mut ed = Editor::headless();
    ed.engine.scene.dimension = Dimension::D3;
    ed.add_mesh("cube");
    let cube = ed.state.primary().unwrap();
    ed.engine
        .scene
        .world
        .insert_one(
            cube,
            Transform {
                position: Vec3::new(1.0, 2.0, 3.0),
                rotation: Vec3::new(10.0, 20.0, 30.0),
                scale: Vec3::new(2.0, 2.0, 2.0),
            },
        )
        .ok();
    let text = ed.engine.scene.save(&ed.engine.registry);
    let loaded = Scene::load(&text, &ed.engine.registry).unwrap();
    let cube2 = ecs::find_by_name(&loaded.world, "cube").unwrap();
    let t = loaded.world.get::<&Transform>(cube2).unwrap();
    assert_eq!(t.position, Vec3::new(1.0, 2.0, 3.0));
    assert_eq!(t.rotation, Vec3::new(10.0, 20.0, 30.0));
    assert_eq!(t.scale, Vec3::new(2.0, 2.0, 2.0));
    assert_eq!(loaded.dimension, Dimension::D3);
}

/// Drag-and-drop asset spawning: a texture path becomes a real Sprite
/// entity (undoable), a model path becomes a MeshRenderer.
#[test]
fn drop_asset_spawns_component_backed_entity() {
    let mut ed = Editor::headless();
    ed.spawn_asset_entity("assets/player.png", Some(Vec3::new(2.0, 1.0, 0.0)));
    let e = ed.state.primary().unwrap();
    let sprite_image = ed
        .engine
        .scene
        .world
        .get::<&Sprite>(e)
        .map(|sp| sp.image.clone())
        .unwrap();
    assert_eq!(sprite_image, "assets/player.png");
    let t = ed
        .engine
        .scene
        .world
        .get::<&Transform>(e)
        .map(|t| t.position)
        .unwrap();
    assert_eq!(t, Vec3::new(2.0, 1.0, 0.0));

    ed.spawn_asset_entity("assets/robot.glb", None);
    let m = ed.state.primary().unwrap();
    let mesh_name = ed
        .engine
        .scene
        .world
        .get::<&MeshRenderer>(m)
        .map(|mr| mr.mesh.clone())
        .unwrap();
    assert_eq!(mesh_name, "assets/robot.glb#0");

    // Unsupported kinds refuse to spawn.
    let before = ed.engine.scene.world.iter().count();
    ed.spawn_asset_entity("assets/notes.txt", None);
    assert_eq!(ed.engine.scene.world.iter().count(), before);

    // The texture spawn is undoable.
    ed.state.select(e);
    ed.apply_undo();
    assert_eq!(
        ed.engine.scene.world.iter().count(),
        1,
        "only the mesh entity remains"
    );
}

/// Import copies a real file into assets/ and rescan indexes it.
#[test]
fn import_asset_copies_and_indexes() {
    let dir = std::env::temp_dir().join(format!("spark_import_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    Project::create_from_template(&dir, "ImportTest", Dimension::D2).unwrap();
    // A source file outside the project.
    let src = dir.join("source.png");
    std::fs::write(&src, b"not-really-png").unwrap();

    let mut ed = Editor::headless();
    ed.open_project(&dir);
    ed.import_asset(src.to_str().unwrap());
    assert!(
        dir.join("assets/source.png").is_file(),
        "the file must be copied into assets/"
    );
    assert!(
        ed.engine
            .assets
            .list(AssetKind::Texture)
            .contains(&"assets/source.png".to_string()),
        "rescan must index the imported file"
    );
    std::fs::remove_dir_all(&dir).ok();
}
