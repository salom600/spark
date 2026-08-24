//! Editor-only state: selection, viewport tracking, camera, play snapshots.

use spark::math::{Vec2, Vec3};
use spark::prelude::*;
use spark::reexport::{egui, hecs};
use spark::scene::Dimension;

/// Editor scene camera (not part of the saved scene).
///
/// Convention (matches `Transform::quat()`'s `EulerRot::XYZ`):
/// - `yaw` rotates around the world Y axis. `yaw = 0` looks down `-Z`,
///   `yaw = 90°` looks toward `-X` (clockwise viewed from above).
/// - `pitch` rotates around the world X axis. Positive looks up, negative
///   looks down. Clamped to ±89°.
/// - `forward()` is derived from the **same** Euler angles stored in
///   `as_override`, so CPU-side picking/panning/zoom math agrees with the
///   GPU camera matrix.
#[derive(Clone)]
pub struct EditorCamera {
    pub pos: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub ortho_height: f32,
    pub fov: f32,
}

impl Default for EditorCamera {
    fn default() -> Self {
        Self {
            // Above and behind origin, looking at the world origin.
            pos: Vec3::new(0.0, 4.0, 10.0),
            yaw: 0.0,
            pitch: -20.0,
            ortho_height: 10.0,
            fov: 60.0,
        }
    }
}

impl EditorCamera {
    pub fn fit_dimension(&mut self, d: Dimension) {
        match d {
            Dimension::D2 => {
                self.pos = Vec3::new(0.0, 0.0, 10.0);
                self.yaw = 0.0;
                self.pitch = 0.0;
            }
            Dimension::D3 => {
                self.pos = Vec3::new(0.0, 4.0, 10.0);
                self.yaw = 0.0;
                self.pitch = -20.0;
            }
        }
    }

    /// Wheel zoom: ortho height (2D) + dolly along the view direction (3D).
    pub fn zoom(&mut self, dy: f32) {
        self.ortho_height = (self.ortho_height - dy).clamp(0.5, 200.0);
        // Dolly toward the look target; scale step by current distance so
        // nearby cameras move slowly and far cameras move fast.
        let forward = self.forward();
        let dist = self.pos.length();
        let step = -dy * 0.5 * dist.max(1.0).min(50.0);
        self.pos += forward * step;
        if self.pos.length() < 0.5 {
            self.pos = forward * 0.5;
        }
    }

    /// Unit vector the camera looks along. Derived from the same Euler
    /// angles used in `as_override`, so this matches the GPU camera's view
    /// direction. For `Transform::quat() = Quat::from_euler(XYZ, p, y, 0)`
    /// the matrix is `R = Rx(p)·Ry(y)` and applying it to `(0,0,-1)` gives
    /// the closed form below.
    pub fn forward(&self) -> Vec3 {
        let yaw = self.yaw.to_radians();
        let pitch = self.pitch.to_radians();
        Vec3::new(
            -yaw.sin(),
            pitch.sin() * yaw.cos(),
            -pitch.cos() * yaw.cos(),
        )
    }

    /// Right vector projected onto the world XZ plane (for panning).
    pub fn right(&self) -> Vec3 {
        let yaw = self.yaw.to_radians();
        Vec3::new(yaw.cos(), 0.0, -yaw.sin())
    }

    pub fn pan(&mut self, delta: Vec2, dimension: Dimension) {
        match dimension {
            Dimension::D2 => {
                let scale = self.ortho_height * 0.0016;
                self.pos.x -= delta.x * scale;
                self.pos.y += delta.y * scale;
            }
            Dimension::D3 => {
                let right = self.right();
                let scale = 0.003 * self.pos.length().max(1.0);
                self.pos += right * (-delta.x * scale);
                self.pos += Vec3::Y * (delta.y * scale);
            }
        }
    }

    /// Orbit: yaw/pitch changes from mouse delta (right-mouse drag).
    pub fn look(&mut self, delta: Vec2) {
        self.yaw -= delta.x * 0.4;
        self.pitch = (self.pitch - delta.y * 0.4).clamp(-89.0, 89.0);
    }

    /// Camera override tuple for `build_frame_draw`. The Transform's rotation
    /// is `(pitch, yaw, 0)` Euler XYZ — exactly what `Transform::quat()`
    /// expects — so the GPU side and `forward()` agree.
    pub fn as_override(&self, dimension: Dimension) -> (Transform, Camera) {
        let kind = match dimension {
            Dimension::D2 => CameraKind::Ortho2D {
                height: self.ortho_height,
            },
            Dimension::D3 => CameraKind::Perspective { fov_deg: self.fov },
        };
        let rot = match dimension {
            Dimension::D2 => Vec3::ZERO,
            Dimension::D3 => Vec3::new(self.pitch, self.yaw, 0.0),
        };
        (
            Transform {
                position: self.pos,
                rotation: rot,
                scale: Vec3::ONE,
            },
            Camera {
                kind,
                ..Default::default()
            },
        )
    }
}

/// Snapshot for play-in-editor (scene serialized before play).
pub struct PlaySnapshot {
    pub scene_text: String,
}

/// All transient editor state.
pub struct EditorState {
    pub selected: Option<hecs::Entity>,
    pub viewport_px: [u32; 4],
    pub show_new_project: bool,
    pub show_open_project: bool,
    pub new_project_name: String,
    pub new_project_dim: Dimension,
    pub open_path: String,
    /// Translate / Rotate / Scale — which gizmo is active.
    pub gizmo_mode: GizmoMode,
    /// When dragging a gizmo axis, which axis (0=X, 1=Y, 2=Z).
    pub gizmo_drag_axis: Option<usize>,
    /// Mouse position when the drag started (pixels, ppp-scaled).
    pub gizmo_drag_start_mouse: Option<egui::Pos2>,
    /// The entity's Transform when the drag started.
    pub gizmo_drag_start_transform: Option<Transform>,
}

/// Active gizmo mode (matches the toolbar buttons).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GizmoMode {
    Translate,
    Rotate,
    Scale,
}

impl Default for GizmoMode {
    fn default() -> Self {
        Self::Translate
    }
}

impl Default for EditorState {
    fn default() -> Self {
        Self {
            selected: None,
            viewport_px: [0, 0, 1280, 720],
            show_new_project: false,
            show_open_project: false,
            new_project_name: "MyGame".into(),
            new_project_dim: Dimension::D2,
            open_path: ".".into(),
            gizmo_mode: GizmoMode::Translate,
            gizmo_drag_axis: None,
            gizmo_drag_start_mouse: None,
            gizmo_drag_start_transform: None,
        }
    }
}

impl EditorState {
    /// Full-window viewport (play mode).
    pub fn full_viewport(&self) -> (u32, u32, u32, u32) {
        (0, 0, self.viewport_px[2], self.viewport_px[3])
    }

    /// Central-panel viewport in physical pixels (edit mode).
    pub fn viewport_rect_px(&self) -> (u32, u32, u32, u32) {
        let [x, y, w, h] = self.viewport_px;
        (x, y, w, h)
    }

    pub fn mark_all_fresh(&mut self, engine: &mut Engine<'static>) {
        let ents: Vec<hecs::Entity> = engine.scene.world.iter().map(|er| er.entity()).collect();
        for e in ents {
            engine.rules.mark_fresh(e);
        }
    }
}
