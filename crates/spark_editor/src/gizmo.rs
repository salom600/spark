//! 3D viewport helpers: camera math, grid/axes overlay, entity picking,
//! and a translate gizmo. All overlays are drawn through egui's Painter
//! (3D points are projected to screen pixels using the same view_proj the
//! GPU uses), so this stays entirely on the CPU side and adds no wgpu
//! pipelines.

// Strict Rust 2024 float-literal inference rejects untyped `0.0`/`1.0`
// literals where the inference is ambiguous (some glam/egui call sites
// end up at f64 fallback). Suffixing every literal is noisy; the math is
// unambiguously f32 at runtime, so allow the lint module-wide.
#![allow(float_literal_f32_fallback)]

use spark::math::{Mat4, Vec3, Vec4};
use spark::prelude::*;
use spark::reexport::{egui, hecs};
use spark::scene::Dimension;

use crate::state::EditorCamera;

/// The view-projection matrix the editor camera produces. Replicates the
/// math in `render::build_frame_draw` so picking + overlay coordinates
/// agree with the GPU-rendered frame. The quaternion here is constructed
/// as `Quat::from_rotation_x(pitch) * Quat::from_rotation_y(yaw)` — which
/// is equivalent to `Quat::from_euler(XYZ, pitch, yaw, 0)` used by
/// `Transform::quat()`.
pub fn view_proj(cam: &EditorCamera, dimension: Dimension, aspect: f32) -> Mat4 {
    let pos = cam.pos;
    let quat = match dimension {
        Dimension::D2 => Quat::IDENTITY,
        Dimension::D3 => {
            Quat::from_rotation_x(cam.pitch.to_radians())
                * Quat::from_rotation_y(cam.yaw.to_radians())
        }
    };
    let view = Mat4::from_translation(pos) * Mat4::from_quat(quat);
    let proj = match dimension {
        Dimension::D2 => {
            let h = cam.ortho_height.max(0.001);
            let w = h * aspect.max(0.001);
            Mat4::orthographic_rh(-w / 2.0, w / 2.0, -h / 2.0, h / 2.0, -1000.0, 1000.0)
        }
        Dimension::D3 => Mat4::perspective_rh(
            cam.fov.to_radians().max(0.01),
            aspect.max(0.01),
            0.1,
            1000.0,
        ),
    };
    proj * view.inverse()
}

/// Project a 3D world point to screen pixels. Returns None if the point is
/// behind the camera (w <= 0).
pub fn project(view_proj: Mat4, p: Vec3, viewport_px: [u32; 4], ppp: f32) -> Option<egui::Pos2> {
    let clip = view_proj * Vec4::new(p.x, p.y, p.z, 1.0);
    if clip.w <= 0.0 {
        return None;
    }
    let ndc_x = clip.x / clip.w;
    let ndc_y = clip.y / clip.w;
    let [vx, vy, vw, vh] = viewport_px;
    let sx = vx as f32 + (ndc_x * 0.5 + 0.5) * vw as f32;
    let sy = vy as f32 + (0.5 - ndc_y * 0.5) * vh as f32;
    Some(egui::pos2(sx / ppp, sy / ppp))
}

/// Build a picking ray (origin, normalized direction) from a mouse position
/// in pixels. For 2D the "ray" is a straight-down -Z ray at the mouse x/y;
/// for 3D it unprojects through the perspective frustum.
pub fn pick_ray(
    cam: &EditorCamera,
    dimension: Dimension,
    aspect: f32,
    mouse_px: egui::Pos2,
    viewport_px: [u32; 4],
    ppp: f32,
) -> (Vec3, Vec3) {
    let [vx, vy, vw, vh] = viewport_px;
    let mx = mouse_px.x * ppp - vx as f32;
    let my = mouse_px.y * ppp - vy as f32;
    match dimension {
        Dimension::D2 => {
            let h = cam.ortho_height.max(0.001);
            let w = h * aspect.max(0.001);
            let world_x = (mx / vw as f32 - 0.5) * w + cam.pos.x;
            let world_y = (0.5 - my / vh as f32) * h + cam.pos.y;
            (Vec3::new(world_x, world_y, 0.0), Vec3::new(0.0, 0.0, -1.0))
        }
        Dimension::D3 => {
            let ndc_x = (mx / vw as f32) * 2.0 - 1.0;
            let ndc_y = 1.0 - (my / vh as f32) * 2.0;
            let proj = Mat4::perspective_rh(
                cam.fov.to_radians().max(0.01),
                aspect.max(0.01),
                0.1,
                1000.0,
            );
            let view = Mat4::from_translation(cam.pos)
                * Mat4::from_quat(
                    Quat::from_rotation_x(cam.pitch.to_radians())
                        * Quat::from_rotation_y(cam.yaw.to_radians()),
                );
            let inv = (proj * view.inverse()).inverse();
            let near = inv * Vec4::new(ndc_x, ndc_y, -1.0, 1.0);
            let far = inv * Vec4::new(ndc_x, ndc_y, 1.0, 1.0);
            let origin = Vec3::new(near.x / near.w, near.y / near.w, near.z / near.w);
            let far_pt = Vec3::new(far.x / far.w, far.y / far.w, far.z / far.w);
            (origin, (far_pt - origin).normalize_or_zero())
        }
    }
}

/// Slab intersection test for picking. Returns the ray distance `t` if hit,
/// else None. AABB defined by `center` + `half_extents`.
pub fn ray_aabb(origin: Vec3, dir: Vec3, center: Vec3, half_extents: Vec3) -> Option<f32> {
    let inv = Vec3::new(
        if dir.x.abs() > 1e-6_f32 {
            1.0_f32 / dir.x
        } else {
            f32::INFINITY
        },
        if dir.y.abs() > 1e-6_f32 {
            1.0_f32 / dir.y
        } else {
            f32::INFINITY
        },
        if dir.z.abs() > 1e-6_f32 {
            1.0_f32 / dir.z
        } else {
            f32::INFINITY
        },
    );
    let t1 = (center - half_extents - origin) * inv;
    let t2 = (center + half_extents - origin) * inv;
    // Per-axis min/max (Vec3::min/max are component-wise).
    let tmin = t1.min(t2);
    let tmax = t1.max(t2);
    // Slab algorithm: latest entry = max of per-axis mins; earliest exit
    // = min of per-axis maxes. Hit iff exit >= entry and exit >= 0.
    let t_enter = tmin.x.max(tmin.y).max(tmin.z).max(0.0_f32);
    let t_exit = tmax.x.min(tmax.y).min(tmax.z);
    if t_exit >= t_enter && t_exit >= 0.0_f32 {
        Some(t_enter)
    } else {
        None
    }
}

/// Draw the ground grid + world-origin XYZ axes via egui's Painter. Lines
/// are projected from 3D world space to screen pixels.
pub fn draw_grid_and_axes(
    painter: &egui::Painter,
    view_proj: Mat4,
    viewport_px: [u32; 4],
    ppp: f32,
) {
    // Grid on the XZ plane (y=0): -10..10, every 1 unit.
    for i in -10..=10 {
        let a = if i == 0 {
            egui::Color32::from_rgb(110, 110, 110)
        } else {
            egui::Color32::from_rgb(55, 55, 55)
        };
        let p1 = project(view_proj, Vec3::new(i as f32, 0.0, -10.0), viewport_px, ppp);
        let p2 = project(view_proj, Vec3::new(i as f32, 0.0, 10.0), viewport_px, ppp);
        if let (Some(p1), Some(p2)) = (p1, p2) {
            painter.line_segment([p1, p2], egui::Stroke::new(1.0, a));
        }
        let p1 = project(view_proj, Vec3::new(-10.0, 0.0, i as f32), viewport_px, ppp);
        let p2 = project(view_proj, Vec3::new(10.0, 0.0, i as f32), viewport_px, ppp);
        if let (Some(p1), Some(p2)) = (p1, p2) {
            painter.line_segment([p1, p2], egui::Stroke::new(1.0, a));
        }
    }
    // World axes (length 1.5) at the origin.
    let len = 1.5;
    let axes = [
        (
            Vec3::new(len, 0.0, 0.0),
            egui::Color32::from_rgb(220, 60, 60),
        ),
        (
            Vec3::new(0.0, len, 0.0),
            egui::Color32::from_rgb(60, 220, 60),
        ),
        (
            Vec3::new(0.0, 0.0, len),
            egui::Color32::from_rgb(60, 120, 220),
        ),
    ];
    for (end, color) in axes {
        let p1 = project(view_proj, Vec3::ZERO, viewport_px, ppp);
        let p2 = project(view_proj, end, viewport_px, ppp);
        if let (Some(p1), Some(p2)) = (p1, p2) {
            painter.line_segment([p1, p2], egui::Stroke::new(2.5, color));
        }
    }
}

/// Translate gizmo: draws three axis arrows at `origin`, returns the axis
/// index (0=X, 1=Y, 2=Z) hit by the mouse if any (within an 8-pixel
/// tolerance).
pub fn draw_translate_gizmo(
    painter: &egui::Painter,
    view_proj: Mat4,
    origin: Vec3,
    viewport_px: [u32; 4],
    ppp: f32,
    mouse: egui::Pos2,
) -> Option<usize> {
    let len = 1.5;
    let axes = [
        (
            Vec3::new(len, 0.0, 0.0),
            egui::Color32::from_rgb(220, 60, 60),
        ),
        (
            Vec3::new(0.0, len, 0.0),
            egui::Color32::from_rgb(60, 220, 60),
        ),
        (
            Vec3::new(0.0, 0.0, len),
            egui::Color32::from_rgb(60, 120, 220),
        ),
    ];
    let mut hit = None;
    let mut best_dist = 8.0_f32; // pixels
    let origin_screen = project(view_proj, origin, viewport_px, ppp);
    for (i, (end, color)) in axes.iter().enumerate() {
        let p_end = project(view_proj, origin + *end, viewport_px, ppp);
        if let (Some(p0), Some(p1)) = (origin_screen, p_end) {
            painter.line_segment([p0, p1], egui::Stroke::new(3.0, *color));
            let d = dist_to_segment(mouse, p0, p1);
            if d < best_dist {
                best_dist = d;
                hit = Some(i);
            }
        }
    }
    hit
}

fn dist_to_segment(p: egui::Pos2, a: egui::Pos2, b: egui::Pos2) -> f32 {
    let ab = b - a;
    let len2 = ab.length_sq();
    if len2 < 1e-6 {
        return (p - a).length();
    }
    let t = ((p - a).dot(ab) / len2).clamp(0.0, 1.0);
    let proj = a + ab * t;
    (p - proj).length()
}

/// Picking: cast a ray and find the nearest entity with a Transform whose
/// AABB (a unit-ish box around the entity's position) intersects. Returns
/// the entity and the hit distance.
pub fn pick_entity(world: &hecs::World, origin: Vec3, dir: Vec3) -> Option<(hecs::Entity, f32)> {
    let mut best: Option<(hecs::Entity, f32)> = None;
    for (e, t) in world.query::<&Transform>().iter() {
        // Default half-extents: 0.5 in each axis (covers a unit cube).
        // For sprites/meshes this is approximate but good enough for editor
        // picking.
        let half = Vec3::new(0.5, 0.5, 0.5);
        if let Some(t_hit) = ray_aabb(origin, dir, t.position, half)
            && best.map(|(_, d)| t_hit < d).unwrap_or(true)
        {
            best = Some((e, t_hit));
        }
    }
    best
}

/// Drag delta in world units along the gizmo's drag axis. Projects the
/// current mouse position onto the gizmo axis line in screen space, then
/// scales by an approximate world-unit-per-pixel factor based on the
/// camera's distance to the gizmo and the perspective FOV.
#[allow(clippy::too_many_arguments)]
pub fn axis_drag_delta(
    cam: &EditorCamera,
    aspect: f32,
    gizmo_origin: Vec3,
    axis: usize,
    mouse_px: egui::Pos2,
    viewport_px: [u32; 4],
    ppp: f32,
    start_mouse: egui::Pos2,
) -> Vec3 {
    let vp = view_proj(cam, Dimension::D3, aspect);
    let p0 = project(vp, gizmo_origin, viewport_px, ppp).unwrap_or(mouse_px);
    let axis_vec = match axis {
        0 => Vec3::new(1.0, 0.0, 0.0),
        1 => Vec3::new(0.0, 1.0, 0.0),
        _ => Vec3::new(0.0, 0.0, 1.0),
    };
    let p1 = project(vp, gizmo_origin + axis_vec, viewport_px, ppp).unwrap_or(mouse_px);
    let screen_axis = p1 - p0;
    if screen_axis.length_sq() < 1e-6 {
        return Vec3::ZERO;
    }
    let screen_axis_dir = screen_axis.normalized();
    let mouse_delta_px = (mouse_px - start_mouse).dot(screen_axis_dir);
    let dist = (cam.pos - gizmo_origin).length();
    let world_per_px = (cam.fov.to_radians().tan() * dist) / (viewport_px[3] as f32 / ppp).max(1.0);
    let world_delta = mouse_delta_px * world_per_px;
    axis_vec * world_delta
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ray_aabb_hit_and_miss() {
        let origin = Vec3::new(0.0, 0.0, -5.0);
        let dir = Vec3::new(0.0, 0.0, 1.0);
        // Box at origin, half 0.5 — should hit at t≈4.5.
        let hit = ray_aabb(origin, dir, Vec3::ZERO, Vec3::new(0.5, 0.5, 0.5));
        assert!(hit.is_some());
        assert!((hit.unwrap() - 4.5).abs() < 0.01);
        // Box far to the side — should miss.
        let miss = ray_aabb(
            origin,
            dir,
            Vec3::new(10.0, 0.0, 0.0),
            Vec3::new(0.5, 0.5, 0.5),
        );
        assert!(miss.is_none());
    }

    #[test]
    fn forward_matches_view_proj_for_yaw_pitch_zero() {
        // With yaw=0, pitch=0, the camera looks down -Z. Projecting a point
        // at (0, 0, -5) should land at the viewport center horizontally.
        let cam = EditorCamera {
            yaw: 0.0,
            pitch: 0.0,
            pos: Vec3::new(0.0, 0.0, 10.0),
            ..Default::default()
        };
        let vp = view_proj(&cam, Dimension::D3, 1.0);
        let p = project(vp, Vec3::new(0.0, 0.0, 0.0), [0, 0, 100, 100], 1.0);
        // Point at world origin should project to viewport center (50, 50).
        assert!(p.is_some());
        let p = p.unwrap();
        assert!((p.x - 50.0).abs() < 0.5, "x = {}", p.x);
        assert!((p.y - 50.0).abs() < 0.5, "y = {}", p.y);
    }
}
