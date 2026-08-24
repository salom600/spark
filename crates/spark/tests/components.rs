//! Phase B component tests: every built-in component must actually change
//! what the renderer sees (FrameDraw) or how the engine behaves — not just
//! exist on the entity.

use spark::app::Engine;
use spark::components::{
    Camera, CameraKind, Light, LightKind, MeshRenderer, Sprite, Transform, Visible,
};
use spark::ecs;
use spark::math::Vec3;
use spark::render::build_frame_draw;

fn engine() -> Engine<'static> {
    Engine::headless_empty()
}

/// Visible(false) must exclude the entity from the frame's draw list.
#[test]
fn visible_false_excludes_from_draw() {
    let mut e = engine();
    let a = e.scene.world.spawn((
        ecs::Name("shown".into()),
        Transform::default(),
        MeshRenderer {
            mesh: "cube".into(),
            ..Default::default()
        },
    ));
    let b = e.scene.world.spawn((
        ecs::Name("hidden".into()),
        Transform {
            position: Vec3::new(5.0, 0.0, 0.0),
            ..Default::default()
        },
        MeshRenderer {
            mesh: "cube".into(),
            ..Default::default()
        },
        Visible(false),
    ));
    e.scene.world.spawn((
        ecs::Name("Camera".into()),
        Transform::default(),
        Camera::default(),
    ));
    let _ = (a, b);
    let draw = build_frame_draw(&e.scene, &mut e.assets, 1.0, None);
    let total_instances: usize = draw.meshes.iter().map(|(_, _, v)| v.len()).sum();
    assert_eq!(total_instances, 1, "only the visible mesh renders");
}

/// A directional light must land in the frame's globals (dir_light.w = 1).
#[test]
fn directional_light_reaches_globals() {
    let mut e = engine();
    e.scene.world.spawn((
        ecs::Name("Camera".into()),
        Transform::default(),
        Camera::default(),
    ));
    let draw = build_frame_draw(&e.scene, &mut e.assets, 1.0, None);
    assert_eq!(draw.globals.dir_light[3], 0.0, "no light → dir flag 0");
    assert!(!draw.has_directional);

    e.scene.world.spawn((
        ecs::Name("Sun".into()),
        Transform::default(),
        Light::default(),
    ));
    let draw = build_frame_draw(&e.scene, &mut e.assets, 1.0, None);
    assert_eq!(draw.globals.dir_light[3], 1.0, "sun → dir flag 1");
    assert!(draw.has_directional);
}

/// Point lights fill the point-light buffer (count in light_meta.x).
#[test]
fn point_lights_fill_buffer() {
    let mut e = engine();
    e.scene.world.spawn((
        ecs::Name("Camera".into()),
        Transform::default(),
        Camera::default(),
    ));
    for i in 0..3 {
        e.scene.world.spawn((
            ecs::Name(format!("P{i}")),
            Transform {
                position: Vec3::new(i as f32, 2.0, 0.0),
                ..Default::default()
            },
            Light {
                kind: LightKind::Point { range: 8.0 },
                ..Default::default()
            },
        ));
    }
    let draw = build_frame_draw(&e.scene, &mut e.assets, 1.0, None);
    // The buffer is zero-padded to MAX_POINT_LIGHTS; count non-zero entries.
    let live = draw
        .globals
        .point_lights
        .iter()
        .filter(|l| l[3] > 0.0)
        .count();
    assert_eq!(
        live, 3,
        "three point lights counted (got {live} live entries)"
    );
    // First light position must be the world position of the first entity.
    assert_eq!(draw.globals.point_lights[0][0], 0.0);
}

/// A spot light populates the spot uniforms and the presence flag.
#[test]
fn spot_light_populates_uniforms() {
    let mut e = engine();
    e.scene.world.spawn((
        ecs::Name("Camera".into()),
        Transform::default(),
        Camera::default(),
    ));
    e.scene.world.spawn((
        ecs::Name("Spot".into()),
        Transform {
            position: Vec3::new(0.0, 5.0, 0.0),
            ..Default::default()
        },
        Light {
            kind: LightKind::Spot {
                direction: Vec3::new(0.0, -1.0, 0.0),
                angle_deg: 60.0,
                range: 12.0,
            },
            intensity: 2.0,
            ..Default::default()
        },
    ));
    let draw = build_frame_draw(&e.scene, &mut e.assets, 1.0, None);
    assert_eq!(draw.globals.light_meta[2], 1.0, "spot presence flag set");
    assert_eq!(draw.globals.spot_pos[1], 5.0, "spot y position");
    assert_eq!(draw.globals.spot_pos[3], 12.0, "spot range");
    assert!(draw.globals.spot_dir[1] < -0.99, "spot points down");
    // 60° full cone → 30° half angle → cos(30°) ≈ 0.866.
    assert!((draw.globals.spot_dir[3] - 30.0_f32.to_radians().cos()).abs() < 1e-4);
}

/// The *active* camera renders the frame; inactive ones are skipped
/// (fallback: first camera when none is active).
#[test]
fn active_camera_selection() {
    let mut e = engine();
    let cam1 = e.scene.world.spawn((
        ecs::Name("Cam1".into()),
        Transform::default(),
        Camera::default(),
    ));
    let cam2 = e.scene.world.spawn((
        ecs::Name("Cam2".into()),
        Transform {
            position: Vec3::new(100.0, 0.0, 0.0),
            ..Default::default()
        },
        Camera::default(),
    ));
    let _ = (cam1, cam2);
    // Both active → first one wins.
    let (cam, tr) = e.primary_camera().unwrap();
    assert!(cam.active);
    assert_eq!(tr.position, Vec3::ZERO);

    // Deactivate cam1 → cam2 renders.
    if let Ok(mut c) = e.scene.world.get::<&mut Camera>(cam1) {
        c.active = false;
    }
    let (_, tr) = e.primary_camera().unwrap();
    assert_eq!(tr.position.x, 100.0, "active camera wins");

    // Deactivate everything → fallback to the first camera (no black screen).
    if let Ok(mut c) = e.scene.world.get::<&mut Camera>(cam2) {
        c.active = false;
    }
    assert!(e.primary_camera().is_some(), "fallback to first camera");
}

/// A parented sprite's world position (not its local offset) reaches the
/// draw list.
#[test]
fn sprite_world_transform_reaches_draw() {
    let mut e = engine();
    e.scene.world.spawn((
        ecs::Name("Camera".into()),
        Transform::default(),
        Camera::default(),
    ));
    let parent = e.scene.world.spawn((
        ecs::Name("Parent".into()),
        Transform {
            position: Vec3::new(4.0, 2.0, 0.0),
            ..Default::default()
        },
    ));
    e.scene.world.spawn((
        ecs::Name("Sprite".into()),
        Transform {
            position: Vec3::new(1.0, 0.0, 0.0),
            ..Default::default()
        },
        Sprite::default(),
    ));
    let child = ecs::find_by_name(&e.scene.world, "Sprite").unwrap();
    ecs::set_parent(&mut e.scene.world, child, Some(parent));
    let draw = build_frame_draw(&e.scene, &mut e.assets, 1.0, None);
    let inst = &draw.sprites[0].1[0];
    assert_eq!(inst.pos[0], 5.0, "parent + local x");
    assert_eq!(inst.pos[1], 2.0, "parent y");
}

/// Perspective vs orthographic cameras produce different projections for
/// the same scene (catches "3D scene rendered through a 2D camera" bugs).
#[test]
fn camera_kind_changes_projection() {
    let mut e = engine();
    let cam = e.scene.world.spawn((
        ecs::Name("Camera".into()),
        Transform::default(),
        Camera {
            kind: CameraKind::Perspective { fov_deg: 60.0 },
            ..Default::default()
        },
    ));
    let draw = build_frame_draw(&e.scene, &mut e.assets, 1.0, None);
    let persp = draw.globals.view_proj;
    if let Ok(mut c) = e.scene.world.get::<&mut Camera>(cam) {
        c.kind = CameraKind::Ortho2D { height: 10.0 };
    }
    let draw = build_frame_draw(&e.scene, &mut e.assets, 1.0, None);
    let ortho = draw.globals.view_proj;
    assert_ne!(persp, ortho, "projection must follow the camera kind");
}
