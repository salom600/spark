//! Editor-only state: selection, viewport tracking, camera, play snapshots.

use spark::math::{Vec2, Vec3};
use spark::prelude::*;
use spark::reexport::hecs;
use spark::scene::Dimension;

/// Editor scene camera (not part of the saved scene).
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
            pos: Vec3::new(0.0, 4.0, 10.0),
            yaw: -90.0,
            pitch: -15.0,
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
                self.yaw = -90.0;
                self.pitch = 0.0;
            }
            Dimension::D3 => {
                self.pos = Vec3::new(0.0, 4.0, 10.0);
                self.yaw = -90.0;
                self.pitch = -15.0;
            }
        }
    }

    /// Wheel zoom: ortho height (2D) + dolly along the view direction (3D).
    pub fn zoom(&mut self, dy: f32) {
        self.ortho_height = (self.ortho_height - dy).clamp(0.5, 200.0);
        self.pos += self.forward() * (-dy * 0.5);
    }

    pub fn forward(&self) -> Vec3 {
        let yaw = self.yaw.to_radians();
        let pitch = self.pitch.to_radians();
        Vec3::new(
            yaw.cos() * pitch.cos(),
            pitch.sin(),
            yaw.sin() * pitch.cos(),
        )
    }

    pub fn pan(&mut self, delta: Vec2, dimension: Dimension) {
        match dimension {
            Dimension::D2 => {
                let scale = self.ortho_height * 0.0016;
                self.pos.x -= delta.x * scale;
                self.pos.y += delta.y * scale;
            }
            Dimension::D3 => {
                let yaw = self.yaw.to_radians();
                let right = Vec3::new(-yaw.sin(), 0.0, yaw.cos());
                let scale = 0.003 * self.pos.length().max(1.0);
                self.pos += right * (-delta.x * scale);
                self.pos += Vec3::Y * (delta.y * scale);
            }
        }
    }

    pub fn look(&mut self, delta: Vec2) {
        self.yaw -= delta.x * 0.4;
        self.pitch = (self.pitch - delta.y * 0.4).clamp(-89.0, 89.0);
    }

    /// Camera override tuple for `build_frame_draw`.
    pub fn as_override(&self, dimension: Dimension) -> (Transform, Camera) {
        let tr = Transform {
            position: self.pos,
            rotation: Vec3::new(0.0, 0.0, 0.0),
            scale: Vec3::ONE,
        };
        let kind = match dimension {
            Dimension::D2 => {
                // Look straight down -Z for 2D.
                CameraKind::Ortho2D { height: self.ortho_height }
            }
            Dimension::D3 => CameraKind::Perspective { fov_deg: self.fov },
        };
        // 3D: build rotation from yaw/pitch (YXZ order suits FPS-style cams).
        let rot = match dimension {
            Dimension::D2 => Vec3::ZERO,
            Dimension::D3 => Vec3::new(self.pitch, self.yaw, 0.0),
        };
        (Transform { rotation: rot, ..tr }, Camera { kind, ..Default::default() })
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
    pub renaming: Option<(hecs::Entity, String)>,
    pub rules_edit: Option<(hecs::Entity, usize)>,
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
            renaming: None,
            rules_edit: None,
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
