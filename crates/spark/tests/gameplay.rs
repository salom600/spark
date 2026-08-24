//! Gameplay rules tests: data-driven behavior must actually change runtime
//! state through the full Engine tick pipeline — input events, collisions,
//! timers, messages and camera switching.

use spark::app::Engine;
use spark::components::{
    Camera, Collider, ColliderShape, RigidBody, RulesComp, Sprite, Transform, Vars,
};
use spark::ecs;
use spark::math::Vec3;
use spark::rules::{Action, Cond, Rule, RuleEvent, VarScope};

fn engine() -> Engine<'static> {
    let mut e = Engine::headless_empty();
    e.scene.world.spawn((
        ecs::Name("Camera".into()),
        Transform::default(),
        Camera::default(),
    ));
    e
}

fn key(name: &str) -> winit::keyboard::KeyCode {
    use winit::keyboard::KeyCode as K;
    match name {
        "Space" => K::Space,
        "KeyA" => K::KeyA,
        "KeyB" => K::KeyB,
        _ => panic!("unknown key {name}"),
    }
}

/// On key press → set velocity (the classic jump rule, through the whole
/// physics pipeline).
#[test]
fn key_press_rule_sets_velocity() {
    let mut e = engine();
    e.scene.world.spawn((
        ecs::Name("Player".into()),
        ecs::Tag("player".into()),
        Transform::default(),
        RigidBody {
            gravity_scale: 0.0,
            ..Default::default()
        },
        Collider {
            shape: ColliderShape::Ball { r: 0.5 },
            ..Default::default()
        },
        RulesComp {
            rules: vec![Rule {
                on: RuleEvent::KeyPressed("Space".into()),
                when: vec![],
                run: vec![Action::SetVelocity {
                    v: Vec3::new(0.0, 5.0, 0.0),
                    relative: false,
                }],
                enabled: true,
            }],
        },
    ));
    // First tick: physics creates the body.
    e.tick(1.0 / 60.0);
    // Press Space.
    e.input
        .on_key(key("Space"), winit::event::ElementState::Pressed);
    e.tick(1.0 / 60.0);
    // From the next tick the body moves up.
    e.tick(1.0 / 60.0);
    let player = ecs::find_by_tag(&e.scene.world, "player").unwrap();
    let y = e.scene.world.get::<&Transform>(player).unwrap().position.y;
    assert!(y > 0.01, "jump rule must move the player, got y={y}");
}

/// Collision → variable change (damage on hit).
#[test]
fn collision_rule_changes_var() {
    let mut e = engine();
    e.scene.world.spawn((
        ecs::Name("Player".into()),
        ecs::Tag("player".into()),
        Transform::default(),
        Vars([("hp".to_string(), 100.0)].into_iter().collect()),
        RigidBody::default(),
        Collider {
            shape: ColliderShape::Ball { r: 0.5 },
            ..Default::default()
        },
        RulesComp {
            rules: vec![Rule {
                on: RuleEvent::CollisionEnter {
                    other: Some("enemy".into()),
                },
                when: vec![],
                run: vec![Action::AddVar {
                    scope: VarScope::Entity,
                    name: "hp".into(),
                    delta: -10.0,
                }],
                enabled: true,
            }],
        },
    ));
    e.scene.world.spawn((
        ecs::Name("Enemy".into()),
        ecs::Tag("enemy".into()),
        Transform {
            position: Vec3::new(0.01, 0.0, 0.0),
            ..Default::default()
        },
        RigidBody {
            kind: spark::components::BodyKind::Static,
            ..Default::default()
        },
        Collider {
            shape: ColliderShape::Ball { r: 0.5 },
            ..Default::default()
        },
    ));
    let player = ecs::find_by_tag(&e.scene.world, "player").unwrap();
    for _ in 0..60 {
        e.tick(1.0 / 60.0);
    }
    let hp = e
        .scene
        .world
        .get::<&Vars>(player)
        .unwrap()
        .0
        .get("hp")
        .copied()
        .unwrap_or(-1.0);
    assert!(
        hp < 100.0,
        "collision rule must damage the player (hp={hp})"
    );
}

/// Timer rules fire repeatedly; conditions gate them.
#[test]
fn timer_rule_repeats_and_conditions_gate() {
    let mut e = engine();
    e.scene.world.spawn((
        ecs::Name("Ticker".into()),
        Transform::default(),
        RulesComp {
            rules: vec![Rule {
                on: RuleEvent::Timer {
                    secs: 0.05,
                    repeat: true,
                },
                when: vec![Cond::Var {
                    scope: VarScope::Global,
                    name: "armed".into(),
                    op: spark::rules::CmpOp::Eq,
                    value: 1.0,
                }],
                run: vec![Action::AddVar {
                    scope: VarScope::Global,
                    name: "ticks".into(),
                    delta: 1.0,
                }],
                enabled: true,
            }],
        },
    ));
    // Not armed: no ticks accumulate.
    for _ in 0..30 {
        e.tick(1.0 / 60.0);
    }
    assert_eq!(
        e.scene.globals.get("ticks"),
        None,
        "condition gates the rule"
    );
    // Arm it.
    e.scene.globals.insert("armed".into(), 1.0);
    for _ in 0..30 {
        e.tick(1.0 / 60.0);
    }
    let ticks = e.scene.globals.get("ticks").copied().unwrap_or(0.0);
    assert!(ticks >= 2.0, "timer must fire repeatedly, got {ticks}");
}

/// Messages broadcast on one tick arrive on the next.
#[test]
fn message_rule_delivers_next_tick() {
    let mut e = engine();
    e.scene.world.spawn((
        ecs::Name("Sender".into()),
        Transform::default(),
        RulesComp {
            rules: vec![Rule {
                on: RuleEvent::Update,
                when: vec![Cond::Once],
                run: vec![Action::SendMessage("boom".into())],
                enabled: true,
            }],
        },
    ));
    let receiver = e.scene.world.spawn((
        ecs::Name("Receiver".into()),
        Transform::default(),
        Vars([("heard".to_string(), 0.0)].into_iter().collect()),
        RulesComp {
            rules: vec![Rule {
                on: RuleEvent::Message("boom".into()),
                when: vec![],
                run: vec![Action::SetVar {
                    scope: VarScope::Entity,
                    name: "heard".into(),
                    value: 1.0,
                }],
                enabled: true,
            }],
        },
    ));
    e.tick(1.0 / 60.0); // sends
    e.tick(1.0 / 60.0); // receives
    let heard = e
        .scene
        .world
        .get::<&Vars>(receiver)
        .unwrap()
        .0
        .get("heard")
        .copied()
        .unwrap_or(0.0);
    assert_eq!(heard, 1.0, "message must arrive the next tick");
}

/// Action::UseCamera switches the active gameplay camera.
#[test]
fn use_camera_action_switches_camera() {
    let mut e = engine();
    let cam1 = ecs::find_by_name(&e.scene.world, "Camera").unwrap();
    let cam2 = e.scene.world.spawn((
        ecs::Name("BossCam".into()),
        Transform {
            position: Vec3::new(50.0, 0.0, 0.0),
            ..Default::default()
        },
        Camera::default(),
    ));
    // A switcher entity: after Start, use the boss camera.
    e.scene.world.spawn((
        ecs::Name("Switcher".into()),
        Transform::default(),
        RulesComp {
            rules: vec![Rule {
                on: RuleEvent::Start,
                when: vec![],
                run: vec![Action::UseCamera("BossCam".into())],
                enabled: true,
            }],
        },
    ));
    e.tick(1.0 / 60.0);
    let (_, tr) = e.primary_camera().unwrap();
    assert_eq!(tr.position.x, 50.0, "the boss camera must be active");
    // Cam1 was deactivated.
    let active1 = e
        .scene
        .world
        .get::<&Camera>(cam1)
        .map(|c| c.active)
        .unwrap();
    assert!(!active1);
    let active2 = e
        .scene
        .world
        .get::<&Camera>(cam2)
        .map(|c| c.active)
        .unwrap();
    assert!(active2);
}

/// Clicked rule: a sprite under the (2D) mouse pointer reacts.
#[test]
fn clicked_rule_hits_sprite_under_mouse() {
    let mut e = Engine::headless_empty();
    e.scene.world.spawn((
        ecs::Name("Camera".into()),
        Transform::default(),
        Camera::default(),
    ));
    e.scene.world.spawn((
        ecs::Name("Button".into()),
        Transform {
            position: Vec3::new(2.0, 1.0, 0.0),
            ..Default::default()
        },
        Sprite {
            size: spark::math::Vec2::new(2.0, 2.0),
            ..Default::default()
        },
        RulesComp {
            rules: vec![Rule {
                on: RuleEvent::Clicked,
                when: vec![],
                run: vec![Action::SetVisible(false)],
                enabled: true,
            }],
        },
    ));
    // Default viewport 1280x720, ortho height 10 (width 17.78): screen
    // (784, 288) maps to world (2, 1) — the sprite's center.
    e.input.on_mouse_move(spark::math::Vec2::new(784.0, 288.0));
    e.tick(1.0 / 60.0);
    let button = ecs::find_by_name(&e.scene.world, "Button").unwrap();
    let vis = e
        .scene
        .world
        .get::<&spark::components::Visible>(button)
        .map(|v| v.0)
        .ok();
    assert_eq!(vis, Some(false), "clicked rule must fire on the sprite");
}

/// Regression: `Start` rules must fire EXACTLY ONCE per entity (the fresh
/// list used to never drain, so Start fired every tick), including for
/// entities spawned directly rather than loaded from a scene.
#[test]
fn start_rule_fires_exactly_once() {
    let mut e = engine();
    e.scene.world.spawn((
        ecs::Name("Counter".into()),
        Transform::default(),
        RulesComp {
            rules: vec![Rule {
                on: RuleEvent::Start,
                when: vec![],
                run: vec![Action::AddVar {
                    scope: VarScope::Global,
                    name: "starts".into(),
                    delta: 1.0,
                }],
                enabled: true,
            }],
        },
    ));
    for _ in 0..10 {
        e.tick(1.0 / 60.0);
    }
    let starts = e.scene.globals.get("starts").copied().unwrap_or(0.0);
    assert_eq!(starts, 1.0, "Start must fire exactly once, got {starts}");
}
