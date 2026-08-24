//! The built-in component set.
//!
//! Every component is declared **once** with `#[derive(ComponentDef, Clone,
//! Default, Serialize, Deserialize)]` — the derive macro generates the editor
//! inspector, the `Inspect` impl (so it also edits nested), and the registry
//! provides save/load/clone. Only `Rules` and `Vars` (which contain
//! collections) get small hand-written inspectors.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use spark_macros::ComponentDef;

use crate::ecs::{ComponentDef, Inspect};
use crate::math::{Color, Vec2, Vec3};

// ---------------------------------------------------------------------------
// Core structural
// ---------------------------------------------------------------------------

/// Position, rotation (Euler degrees, XYZ) and scale. Universal across 2D and
/// 3D: 2D scenes use x/y with z as the draw layer.
#[derive(ComponentDef, Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Transform {
    pub position: Vec3,
    /// Euler angles in degrees (applied X then Y then Z).
    #[serde(default)]
    pub rotation: Vec3,
    #[serde(default = "default_scale")]
    pub scale: Vec3,
}

fn default_scale() -> Vec3 {
    Vec3::ONE
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            position: Vec3::ZERO,
            rotation: Vec3::ZERO,
            scale: Vec3::ONE,
        }
    }
}

impl Transform {
    /// Orientation as a quaternion (from Euler degrees).
    pub fn quat(&self) -> glam::Quat {
        glam::Quat::from_euler(
            glam::EulerRot::XYZ,
            self.rotation.x.to_radians(),
            self.rotation.y.to_radians(),
            self.rotation.z.to_radians(),
        )
    }
}

/// Visibility toggle driven by rules (`ToggleVisible`) and the editor.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Visible(pub bool);

impl Default for Visible {
    fn default() -> Self {
        Self(true)
    }
}

// Tuple structs are skipped by the derive macro; hand-write the tiny impl.
impl ComponentDef for Visible {
    const NAME: &'static str = "Visible";
    fn inspect(&mut self, ui: &mut egui::Ui) -> bool {
        self.0.inspect(ui)
    }
}

// ---------------------------------------------------------------------------
// 2D rendering
// ---------------------------------------------------------------------------

/// A textured quad. The engine's entire 2D story: sprites are instanced quads
/// positioned by [`Transform`] and tinted by `color`.
#[derive(ComponentDef, Clone, Debug, Serialize, Deserialize)]
pub struct Sprite {
    /// Asset path, e.g. `"assets/player.png"` (see `assets::Assets`).
    pub image: String,
    #[serde(default)]
    pub color: Color,
    /// Size in world units (height maps to the ortho camera height).
    #[serde(default = "default_sprite_size")]
    pub size: Vec2,
}

fn default_sprite_size() -> Vec2 {
    Vec2::ONE
}

impl Default for Sprite {
    fn default() -> Self {
        Self {
            image: String::new(),
            color: Color::WHITE,
            size: Vec2::ONE,
        }
    }
}

// ---------------------------------------------------------------------------
// 3D rendering
// ---------------------------------------------------------------------------

/// Surface parameters shared by 3D meshes (and glTF imports).
#[derive(ComponentDef, Clone, Debug, Serialize, Deserialize)]
pub struct Material {
    #[serde(default)]
    pub albedo: Color,
    /// Optional albedo texture asset path.
    #[serde(default)]
    pub texture: Option<String>,
    #[serde(default)]
    pub emissive: Color,
    #[serde(default = "default_metallic")]
    pub metallic: f32,
    #[serde(default = "default_roughness")]
    pub roughness: f32,
    /// Skip lighting entirely (UI quads, gizmos).
    #[serde(default)]
    pub unlit: bool,
}

fn default_metallic() -> f32 {
    0.0
}
fn default_roughness() -> f32 {
    0.8
}

impl Default for Material {
    fn default() -> Self {
        Self {
            albedo: Color::WHITE,
            texture: None,
            emissive: Color::BLACK,
            metallic: 0.0,
            roughness: 0.8,
            unlit: false,
        }
    }
}

/// Draws a mesh (builtin `"cube"`, `"sphere"`, `"plane"`, `"quad"` or
/// `"assets/model.glb#0"` glTF primitives) with `material`.
#[derive(ComponentDef, Clone, Debug, Default, Serialize, Deserialize)]
pub struct MeshRenderer {
    pub mesh: String,
    #[serde(default)]
    pub material: Material,
}

/// Camera projection. Also editable nested inside [`Camera`].
#[derive(ComponentDef, Clone, Copy, Debug, Serialize, Deserialize)]
pub enum CameraKind {
    /// Orthographic camera sized by world `height` (width follows aspect).
    Ortho2D {
        #[serde(default = "default_cam_height")]
        height: f32,
    },
    Perspective {
        #[serde(default = "default_fov")]
        fov_deg: f32,
    },
}

fn default_cam_height() -> f32 {
    10.0
}
fn default_fov() -> f32 {
    60.0
}

impl Default for CameraKind {
    fn default() -> Self {
        Self::Ortho2D {
            height: default_cam_height(),
        }
    }
}

/// Scene camera. The first entity with this component renders the frame.
#[derive(ComponentDef, Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Camera {
    pub kind: CameraKind,
    #[serde(default)]
    pub clear: Color,
    #[serde(default = "default_near")]
    pub near: f32,
    #[serde(default = "default_far")]
    pub far: f32,
}

fn default_near() -> f32 {
    0.1
}
fn default_far() -> f32 {
    1000.0
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            kind: CameraKind::default(),
            clear: Color::default(),
            near: default_near(),
            far: default_far(),
        }
    }
}

/// Light projection variants.
#[derive(ComponentDef, Clone, Copy, Debug, Serialize, Deserialize)]
pub enum LightKind {
    /// Sun-like directional light; `direction` points *from* light to scene.
    Directional {
        #[serde(default = "default_light_dir")]
        direction: Vec3,
    },
    Point {
        #[serde(default = "default_point_range")]
        range: f32,
    },
}

fn default_light_dir() -> Vec3 {
    Vec3::new(-0.5, -1.0, -0.3)
}
fn default_point_range() -> f32 {
    10.0
}

impl Default for LightKind {
    fn default() -> Self {
        Self::Directional {
            direction: default_light_dir(),
        }
    }
}

/// A light. One shadow-casting directional light is supported in v1.
#[derive(ComponentDef, Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Light {
    pub kind: LightKind,
    #[serde(default)]
    pub color: Color,
    #[serde(default = "default_light_intensity")]
    pub intensity: f32,
}

fn default_light_intensity() -> f32 {
    1.0
}

impl Default for Light {
    fn default() -> Self {
        Self {
            kind: LightKind::default(),
            color: Color::default(),
            intensity: default_light_intensity(),
        }
    }
}

// ---------------------------------------------------------------------------
// Physics
// ---------------------------------------------------------------------------

/// Rigid body motion type.
#[derive(ComponentDef, Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BodyKind {
    #[default]
    Static,
    Dynamic,
    Kinematic,
}

/// Rigid body driving (or driven by) the entity's [`Transform`].
#[derive(ComponentDef, Clone, Debug, Serialize, Deserialize)]
pub struct RigidBody {
    pub kind: BodyKind,
    #[serde(default)]
    pub linear_damping: f32,
    #[serde(default)]
    pub angular_damping: f32,
    #[serde(default = "default_restitution")]
    pub restitution: f32,
    #[serde(default = "default_friction")]
    pub friction: f32,
    #[serde(default = "one")]
    pub gravity_scale: f32,
    #[serde(default)]
    pub lock_rotation: bool,
}

fn default_restitution() -> f32 {
    0.3
}
fn default_friction() -> f32 {
    0.6
}
fn one() -> f32 {
    1.0
}

// NOTE: serde field-defaults don't apply to derive(Default); these manual
// impls keep `Default` consistent with what a fresh scene file would contain.
impl Default for RigidBody {
    fn default() -> Self {
        Self {
            kind: BodyKind::default(),
            linear_damping: 0.0,
            angular_damping: 0.0,
            restitution: default_restitution(),
            friction: default_friction(),
            gravity_scale: one(),
            lock_rotation: false,
        }
    }
}

/// Collider volume shapes, shared by 2D (x/y) and 3D physics.
#[derive(ComponentDef, Clone, Copy, Debug, Serialize, Deserialize)]
pub enum ColliderShape {
    /// Half-extents (3D) or half-width/half-height (2D, z ignored).
    Box {
        #[serde(default = "default_box_half")]
        half: Vec3,
    },
    Ball {
        #[serde(default = "default_ball_r")]
        r: f32,
    },
    /// Vertical capsule (2D) / Y capsule (3D).
    Capsule {
        #[serde(default = "default_capsule_h")]
        half_height: f32,
        #[serde(default = "default_ball_r")]
        r: f32,
    },
}

fn default_box_half() -> Vec3 {
    Vec3::new(0.5, 0.5, 0.5)
}
fn default_ball_r() -> f32 {
    0.5
}
fn default_capsule_h() -> f32 {
    0.5
}

impl Default for ColliderShape {
    fn default() -> Self {
        Self::Box {
            half: default_box_half(),
        }
    }
}

/// Collider attached to a [`RigidBody`] (or a static sensor).
#[derive(ComponentDef, Clone, Debug, Default, Serialize, Deserialize)]
pub struct Collider {
    pub shape: ColliderShape,
    /// Sensors raise collision events for the rules system without forces.
    #[serde(default)]
    pub sensor: bool,
}

// ---------------------------------------------------------------------------
// Audio
// ---------------------------------------------------------------------------

/// Background music track that auto-plays (looped) when the scene runs.
#[derive(ComponentDef, Clone, Debug, Serialize, Deserialize)]
pub struct Music {
    /// Sound asset path, e.g. `"assets/music/theme.ogg"`.
    pub track: String,
    #[serde(default = "default_music_vol")]
    pub volume: f32,
}

fn default_music_vol() -> f32 {
    0.6
}

impl Default for Music {
    fn default() -> Self {
        Self {
            track: String::new(),
            volume: default_music_vol(),
        }
    }
}

// ---------------------------------------------------------------------------
// Rules & state
// ---------------------------------------------------------------------------

/// Data-driven behavior: event → conditions → actions (see `rules.rs`).
/// The inspector is hand-written (rule lists get a bespoke editor in the
/// editor binary); everything else derives normally.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RulesComp {
    pub rules: Vec<crate::rules::Rule>,
}

impl ComponentDef for RulesComp {
    const NAME: &'static str = "Rules";
    fn inspect(&mut self, ui: &mut egui::Ui) -> bool {
        let changed = false;
        ui.weak(format!("{} rule(s)", self.rules.len()));
        for (i, r) in self.rules.iter().enumerate() {
            ui.push_id(i, |ui| {
                ui.label(format!("#{} {}", i + 1, r.summary()));
            });
        }
        ui.weak("Edit rules in the Rules panel");
        changed
    }
}
/// Entity-local numeric variables used by rules conditions/actions.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Vars(pub HashMap<String, f64>);

impl ComponentDef for Vars {
    const NAME: &'static str = "Vars";
    fn inspect(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;
        egui::Grid::new("Vars").num_columns(2).show(ui, |ui| {
            let keys: Vec<String> = self.0.keys().cloned().collect();
            for k in keys {
                ui.strong(k.clone());
                if let Some(v) = self.0.get_mut(&k) {
                    changed |= v.inspect(ui);
                }
                ui.end_row();
            }
        });
        changed
    }
}

/// Register every built-in component. Games call this first, then register
/// their own types before loading scenes.
pub fn register_core(registry: &mut crate::ecs::Registry) {
    registry
        .register::<Transform>()
        .register::<Visible>()
        .register::<Sprite>()
        .register::<MeshRenderer>()
        .register::<Camera>()
        .register::<Light>()
        .register::<RigidBody>()
        .register::<Collider>()
        .register::<Music>()
        .register::<RulesComp>()
        .register::<Vars>();
}
