//! Integration tests: run the bundled demo games headless for several
//! seconds and assert their rules/physics actually simulate.
//!
//! These run GPU-free and audio-free (CI has no display or sound server).

use std::path::Path;

use spark::app::Engine;

const FRAMES: u32 = 180; // ~3 seconds at 60fps

fn run_headless(project_dir: &str) -> Engine<'static> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../")
        .join(project_dir);
    Engine::headless(root.canonicalize().expect("demo dir exists").as_path())
        .expect("demo project loads")
}

#[test]
fn ember_run_boots_and_simulates() {
    let mut engine = run_headless("demos/ember_run");
    assert!(
        spark::ecs::find_by_tag(&engine.scene.world, "player").is_some(),
        "player exists"
    );
    assert!(
        spark::ecs::find_by_tag(&engine.scene.world, "goal").is_some(),
        "goal exists"
    );

    // Simulate: player falls onto the ground under gravity.
    let player = spark::ecs::find_by_tag(&engine.scene.world, "player").unwrap();
    let transform = engine
        .scene
        .world
        .get::<&spark::components::Transform>(player)
        .unwrap();
    let start_y = transform.position.y;
    drop(transform);
    for _ in 0..FRAMES {
        engine.tick(1.0 / 60.0);
    }
    let end_y = engine
        .scene
        .world
        .get::<&spark::components::Transform>(player)
        .unwrap()
        .position
        .y;
    assert!(
        end_y < start_y + 0.1,
        "player should settle on ground (start {start_y}, end {end_y})"
    );
    assert!(engine.scene.world.contains(player));
}

#[test]
fn ember_run_jump_rule_fires() {
    let mut engine = run_headless("demos/ember_run");
    let player = spark::ecs::find_by_tag(&engine.scene.world, "player").unwrap();

    // Let it land first.
    for _ in 0..90 {
        engine.tick(1.0 / 60.0);
    }
    // Press jump.
    let before = engine
        .scene
        .world
        .get::<&spark::components::Transform>(player)
        .unwrap()
        .position
        .y;
    // Press jump; rules run after physics within a tick, so the velocity
    // applies from the NEXT tick onward — simulate a few frames.
    engine
        .input
        .on_key(winit_key("Space"), winit::event::ElementState::Pressed);
    engine.tick(1.0 / 60.0);
    engine
        .input
        .on_key(winit_key("Space"), winit::event::ElementState::Released);
    for _ in 0..10 {
        engine.tick(1.0 / 60.0);
    }
    let after = engine
        .scene
        .world
        .get::<&spark::components::Transform>(player)
        .unwrap()
        .position
        .y;
    assert!(
        after > before + 0.5,
        "jump rule should raise the player ({before} -> {after})"
    );
}

#[test]
fn playground_boots_and_spawns() {
    let mut engine = run_headless("demos/playground");
    assert!(spark::ecs::find_by_tag(&engine.scene.world, "controller").is_some());

    // Drop a box.
    engine
        .input
        .on_key(winit_key("Space"), winit::event::ElementState::Pressed);
    engine.tick(1.0 / 60.0);
    engine
        .input
        .on_key(winit_key("Space"), winit::event::ElementState::Released);

    let spawned = spark::ecs::find_by_tag(&engine.scene.world, "spawned");
    assert!(spawned.is_some(), "box prefab should spawn");
    assert_eq!(engine.scene.globals.get("objects"), Some(&1.0));

    // Let it fall.
    let box_e = spawned.unwrap();
    let y0 = engine
        .scene
        .world
        .get::<&spark::components::Transform>(box_e)
        .unwrap()
        .position
        .y;
    for _ in 0..FRAMES {
        engine.tick(1.0 / 60.0);
    }
    let y1 = engine
        .scene
        .world
        .get::<&spark::components::Transform>(box_e)
        .unwrap()
        .position
        .y;
    assert!(y1 < y0, "box should fall ({y0} -> {y1})");

    // Clear.
    engine
        .input
        .on_key(winit_key("KeyC"), winit::event::ElementState::Pressed);
    engine.tick(1.0 / 60.0);
    engine
        .input
        .on_key(winit_key("KeyC"), winit::event::ElementState::Released);
    engine.tick(1.0 / 60.0);
    assert!(
        spark::ecs::find_by_tag(&engine.scene.world, "spawned").is_none(),
        "DestroyTagged should clear spawned objects"
    );
}

#[test]
fn playground_gravity_toggle() {
    let mut engine = run_headless("demos/playground");
    // Drop two balls.
    for key in ["KeyB", "KeyB"] {
        engine
            .input
            .on_key(winit_key(key), winit::event::ElementState::Pressed);
        engine.tick(1.0 / 60.0);
        engine
            .input
            .on_key(winit_key(key), winit::event::ElementState::Released);
    }
    // Gravity off (H).
    engine
        .input
        .on_key(winit_key("KeyH"), winit::event::ElementState::Pressed);
    engine.tick(1.0 / 60.0);
    engine
        .input
        .on_key(winit_key("KeyH"), winit::event::ElementState::Released);
    assert_eq!(engine.scene.globals.get("gravity_on"), Some(&0.0));

    let balls: Vec<_> = {
        let world = &engine.scene.world;
        world
            .query::<&spark::ecs::Tag>()
            .iter()
            .filter(|(_, t)| t.0 == "spawned")
            .map(|(e, _)| e)
            .collect()
    };
    assert_eq!(balls.len(), 2, "two balls spawned");

    // With gravity off, drift is minimal.
    for _ in 0..30 {
        engine.tick(1.0 / 60.0);
    }
    // Gravity back on.
    engine
        .input
        .on_key(winit_key("KeyG"), winit::event::ElementState::Pressed);
    engine.tick(1.0 / 60.0);
    engine
        .input
        .on_key(winit_key("KeyG"), winit::event::ElementState::Released);
    assert_eq!(engine.scene.globals.get("gravity_on"), Some(&1.0));
}

#[test]
fn template_project_loads() {
    let engine = run_headless("templates/blank");
    assert!(spark::ecs::find_by_name(&engine.scene.world, "Camera").is_some());
    assert!(spark::ecs::find_by_name(&engine.scene.world, "Sun").is_some());
}

fn winit_key(name: &str) -> winit::keyboard::KeyCode {
    use winit::keyboard::KeyCode as K;
    match name {
        "Space" => K::Space,
        "KeyB" => K::KeyB,
        "KeyC" => K::KeyC,
        "KeyG" => K::KeyG,
        "KeyH" => K::KeyH,
        _ => panic!("unknown key {name}"),
    }
}
