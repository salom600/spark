//! Editor-only state: tools, selection, camera, snapping, gizmo drags,
//! play snapshots.

use spark::math::{Vec2, Vec3};
use spark::prelude::*;
use spark::reexport::{egui, hecs};
use spark::scene::Dimension;

// ---------------------------------------------------------------------------
// Editor camera
// ---------------------------------------------------------------------------

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

    /// Wheel zoom. Dimension-aware: 2D zooms the ortho height only (dollying
    /// an ortho camera does nothing and used to corrupt the pan position);
    /// 3D dollies along the view direction, step scaled by distance.
    pub fn zoom(&mut self, dy: f32, dimension: Dimension) {
        match dimension {
            Dimension::D2 => {
                self.ortho_height = (self.ortho_height * (1.0 - dy * 0.1)).clamp(0.2, 500.0);
            }
            Dimension::D3 => {
                let forward = self.forward();
                let dist = self.pos.length();
                let step = -dy * 0.4 * dist.clamp(0.5, 60.0);
                let mut pos = self.pos + forward * step;
                if pos.length() < 0.5 {
                    pos = forward * 0.5;
                }
                self.pos = pos;
            }
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

    /// Move the camera so it looks at `target`, keeping its distance and
    /// orientation (Focus Selected).
    pub fn focus_on(&mut self, target: Vec3) {
        let dist = (self.pos - target).length().max(0.5);
        self.pos = target - self.forward() * dist;
    }

    /// Frame a sphere (`center`, `radius`) — Focus All. Keeps orientation in
    /// 3D; fits the ortho height in 2D.
    pub fn frame(&mut self, center: Vec3, radius: f32, dimension: Dimension) {
        let radius = radius.max(0.5);
        match dimension {
            Dimension::D2 => {
                self.pos.x = center.x;
                self.pos.y = center.y;
                self.ortho_height = radius * 2.6;
            }
            Dimension::D3 => {
                let dist = radius / (self.fov.to_radians() * 0.5).tan() + radius;
                self.pos = center - self.forward() * dist;
            }
        }
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

// ---------------------------------------------------------------------------
// Tools & snapping
// ---------------------------------------------------------------------------

/// Active editor tool (toolbar + Q/W/E/R/T/Y shortcuts).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Tool {
    /// Move the editor camera only; never touches the selection.
    #[default]
    Hand,
    Move,
    Rotate,
    Scale,
    /// 2D rectangular editing: move + resize sprites by their corners.
    Rect,
    /// Unified gizmo: move arrows + rotate rings + scale boxes at once.
    Transform,
}

impl Tool {
    pub fn label(self) -> &'static str {
        match self {
            Tool::Hand => "Hand",
            Tool::Move => "Move",
            Tool::Rotate => "Rotate",
            Tool::Scale => "Scale",
            Tool::Rect => "Rect",
            Tool::Transform => "Transform",
        }
    }
    pub fn key(self) -> Option<egui::KeyboardShortcut> {
        use egui::{Key, KeyboardShortcut, Modifiers};
        let key = match self {
            Tool::Hand => Key::Q,
            Tool::Move => Key::W,
            Tool::Rotate => Key::E,
            Tool::Scale => Key::R,
            Tool::Rect => Key::T,
            Tool::Transform => Key::Y,
        };
        Some(KeyboardShortcut::new(Modifiers::NONE, key))
    }
}

/// Snapping settings (applied to gizmo drags when enabled).
#[derive(Clone, Copy, Debug)]
pub struct SnapSettings {
    pub enabled: bool,
    /// World units for translate / rect size.
    pub translate: f32,
    /// Degrees for rotate.
    pub rotate_deg: f32,
    /// Multiples for scale factors.
    pub scale: f32,
}

impl Default for SnapSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            translate: 0.5,
            rotate_deg: 15.0,
            scale: 0.1,
        }
    }
}

// ---------------------------------------------------------------------------
// Gizmo drags
// ---------------------------------------------------------------------------

/// Which part of a gizmo is being dragged.
#[derive(Clone, Copy, Debug)]
pub enum GizmoDrag {
    TranslateAxis {
        axis: usize,
    },
    /// 0 = XY, 1 = XZ, 2 = YZ.
    TranslatePlane {
        plane: usize,
    },
    /// Free move on the camera-facing plane through the gizmo (or exact 2D).
    TranslateScreen,
    RotateAxis {
        axis: usize,
    },
    ScaleAxis {
        axis: usize,
    },
    ScaleUniform,
    /// Rect tool corner (0..3, counter-clockwise from (-x,-y) in local space).
    RectCorner {
        corner: usize,
    },
}

/// Everything captured when a drag starts, plus per-entity snapshots.
pub struct DragState {
    pub drag: GizmoDrag,
    pub start_mouse: egui::Pos2,
    /// Gizmo origin in world space (centroid of the selection).
    pub start_world: Vec3,
    /// Ray∩plane (or closest-point-on-axis) at drag start.
    pub start_hit: Vec3,
    /// Plane/axis normal at drag start (gizmo space).
    pub axis_dir: Vec3,
    /// Angle at drag start (rotate).
    pub start_angle: f32,
    /// Signed distance along the axis at drag start (scale).
    pub start_t: f32,
    /// Screen distance from the gizmo center at drag start (uniform scale).
    pub start_px_dist: f32,
    /// Opposite corner (world) for rect resize.
    pub rect_anchor: Vec3,
    /// Serialized `Sprite` before a rect resize (undo baseline).
    pub rect_sprite_before: Option<String>,
    /// Per-entity: id, local transform before the drag, world transform
    /// before the drag.
    pub entities: Vec<(hecs::Entity, Transform, Transform)>,
}

impl DragState {
    /// World-position delta for translate drags.
    pub fn translation(&self, now_hit: Vec3) -> Vec3 {
        now_hit - self.start_hit
    }

    /// Rotation delta in degrees (wrapped to one turn, snapped by caller).
    pub fn rotation_deg(&self, now_angle: f32) -> f32 {
        let mut d = now_angle - self.start_angle;
        while d > 180.0 {
            d -= 360.0;
        }
        while d < -180.0 {
            d += 360.0;
        }
        d
    }
}

// ---------------------------------------------------------------------------
// Editor state
// ---------------------------------------------------------------------------

/// Snapshot for play-in-editor (scene serialized before play).
pub struct PlaySnapshot {
    pub scene_text: String,
}

/// All transient editor state.
pub struct EditorState {
    /// Multi-selection; the **last** entry is the primary (inspector target).
    pub selected: Vec<hecs::Entity>,
    pub viewport_px: [u32; 4],
    pub show_new_project: bool,
    pub show_open_project: bool,
    pub new_project_name: String,
    pub new_project_dim: Dimension,
    pub open_path: String,
    pub tool: Tool,
    /// Gizmo axes in entity-local space (true) or world space (false).
    pub local_space: bool,
    pub snap: SnapSettings,
    pub drag: Option<DragState>,
    /// Gizmo part under the pointer this frame (set while drawing the
    /// overlay, consumed when a press starts a drag).
    pub hovered: Option<crate::gizmo::GizmoHit>,
    /// Hierarchy rows the user expanded.
    pub tree_open: std::collections::HashSet<hecs::Entity>,
    /// Hierarchy row being inline-renamed.
    pub renaming: Option<hecs::Entity>,
    /// Hierarchy drag & drop source (reparenting).
    pub hierarchy_drag: Option<hecs::Entity>,
}

impl Default for EditorState {
    fn default() -> Self {
        Self {
            selected: Vec::new(),
            viewport_px: [0, 0, 1280, 720],
            show_new_project: false,
            show_open_project: false,
            new_project_name: "MyGame".into(),
            new_project_dim: Dimension::D2,
            open_path: ".".into(),
            tool: Tool::Move,
            local_space: false,
            snap: SnapSettings::default(),
            drag: None,
            hovered: None,
            tree_open: std::collections::HashSet::new(),
            renaming: None,
            hierarchy_drag: None,
        }
    }
}

impl EditorState {
    /// Full-window viewport (unused in embedded play, kept for clarity).
    pub fn full_viewport(&self) -> (u32, u32, u32, u32) {
        (0, 0, self.viewport_px[2], self.viewport_px[3])
    }

    /// Central-panel viewport in physical pixels (edit mode).
    pub fn viewport_rect_px(&self) -> (u32, u32, u32, u32) {
        let [x, y, w, h] = self.viewport_px;
        (x, y, w, h)
    }

    /// The primary (last-selected) entity, if any.
    pub fn primary(&self) -> Option<hecs::Entity> {
        self.selected.last().copied()
    }

    pub fn is_selected(&self, e: hecs::Entity) -> bool {
        self.selected.contains(&e)
    }

    /// Replace the selection with a single entity.
    pub fn select(&mut self, e: hecs::Entity) {
        self.selected = vec![e];
    }

    /// Ctrl-click behavior: toggle membership, keeping `e` primary.
    pub fn toggle_select(&mut self, e: hecs::Entity) {
        match self.selected.iter().position(|&s| s == e) {
            Some(i) => {
                self.selected.remove(i);
            }
            None => self.selected.push(e),
        }
    }

    /// Drop selections that no longer exist in the world.
    pub fn retain_existing(&mut self, world: &hecs::World) {
        self.selected.retain(|e| world.contains(*e));
        self.tree_open.retain(|e| world.contains(*e));
        if let Some(e) = self.renaming
            && !world.contains(e)
        {
            self.renaming = None;
        }
        if let Some(e) = self.hierarchy_drag
            && !world.contains(e)
        {
            self.hierarchy_drag = None;
        }
    }

    pub fn mark_all_fresh(&mut self, engine: &mut Engine<'static>) {
        let ents: Vec<hecs::Entity> = engine.scene.world.iter().map(|er| er.entity()).collect();
        for e in ents {
            engine.rules.mark_fresh(e);
        }
    }
}
