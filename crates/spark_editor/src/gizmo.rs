//! Viewport helpers: camera math, grid/axes overlays, selection outlines,
//! entity picking, and the gizmo suite (translate axes+planes, rotate rings,
//! scale boxes, 2D move, rect handles).
//!
//! All overlays are drawn through egui's Painter (3D points are projected
//! with the same view_proj the GPU uses), so this stays CPU-side and adds no
//! wgpu pipelines. Every interaction primitive is a pure function — that is
//! what the unit tests pin down.

#![allow(float_literal_f32_fallback)]

use spark::math::{Mat4, Vec2, Vec3, Vec4};
use spark::prelude::*;
use spark::reexport::{egui, hecs};
use spark::scene::Dimension;

use crate::state::{DragState, EditorCamera, SnapSettings, Tool};

/// Which gizmo part the pointer is over (hit-test result).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GizmoHit {
    Axis(usize),
    Plane(usize),
    /// Camera-plane free move / gizmo center.
    Screen,
    Ring(usize),
    ScaleBox(usize),
    ScaleUniform,
    RectCorner(usize),
    /// Inside the rect-tool outline (drag to move).
    RectInside,
}

// ---------------------------------------------------------------------------
// Camera / projection math
// ---------------------------------------------------------------------------

/// The view-projection matrix the editor camera produces. Replicates the
/// math in `render::build_frame_draw` so picking + overlay coordinates agree
/// with the GPU-rendered frame. The quaternion here is constructed as
/// `Quat::from_rotation_x(pitch) * Quat::from_rotation_y(yaw)` — equivalent
/// to `Quat::from_euler(XYZ, pitch, yaw, 0)` used by `Transform::quat()`.
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

/// Project a 3D world point to screen pixels (egui points). Returns None if
/// the point is behind the camera (w <= 0).
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
/// in egui points. For 2D the "ray" points down -Z at the mouse x/y.
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

/// The 2D world position under the mouse (orthographic).
pub fn mouse_world_2d(
    cam: &EditorCamera,
    mouse_px: egui::Pos2,
    viewport_px: [u32; 4],
    ppp: f32,
) -> Vec2 {
    let (o, _) = pick_ray(cam, Dimension::D2, 1.0, mouse_px, viewport_px, ppp);
    Vec2::new(o.x, o.y)
}

/// Slab intersection test for picking. Returns the ray distance `t` if hit.
pub fn ray_aabb(origin: Vec3, dir: Vec3, center: Vec3, half_extents: Vec3) -> Option<f32> {
    let inv = |d: f32| {
        if d.abs() > 1e-6_f32 {
            1.0_f32 / d
        } else {
            f32::INFINITY
        }
    };
    let inv = Vec3::new(inv(dir.x), inv(dir.y), inv(dir.z));
    let t1 = (center - half_extents - origin) * inv;
    let t2 = (center + half_extents - origin) * inv;
    let tmin = t1.min(t2);
    let tmax = t1.max(t2);
    let t_enter = tmin.x.max(tmin.y).max(tmin.z).max(0.0_f32);
    let t_exit = tmax.x.min(tmax.y).min(tmax.z);
    if t_exit >= t_enter && t_exit >= 0.0_f32 {
        Some(t_enter)
    } else {
        None
    }
}

/// Closest point on an infinite line (`line_o + t * line_d`, unit `line_d`)
/// to a ray (`ray_o + s * ray_d`, unit). The ray parameter is clamped to
/// the visible half-space. Used for axis drags: dragging maps to the point
/// on the axis line nearest the mouse ray, which is stable at any angle.
pub fn ray_line_closest(ray_o: Vec3, ray_d: Vec3, line_o: Vec3, line_d: Vec3) -> Vec3 {
    let w0 = ray_o - line_o;
    let b = ray_d.dot(line_d);
    let d = ray_d.dot(w0);
    let e = line_d.dot(w0);
    let denom = 1.0 - b * b;
    let (s, t) = if denom.abs() > 1e-8 {
        let s = (e * b - d) / denom;
        let t = (e - d * b) / denom;
        (s.max(0.0), t)
    } else {
        // Parallel: pin the line point to the ray's closest approach.
        (0.0, -e)
    };
    let _ = s;
    line_o + line_d * t
}

/// Intersect a ray with a plane (`point` on it, unit `normal`). Returns the
/// hit point, or the ray origin projected onto the plane when (nearly)
/// parallel. Used for plane drags, rotate rings and screen drags.
pub fn ray_plane(ray_o: Vec3, ray_d: Vec3, point: Vec3, normal: Vec3) -> Vec3 {
    let denom = normal.dot(ray_d);
    if denom.abs() < 1e-6 {
        // Fallback: keep the plane's reference point (no movement).
        return point;
    }
    let t = normal.dot(point - ray_o) / denom;
    ray_o + ray_d * t.max(0.0)
}

/// Signed angle (degrees) of `v` in the plane spanned by unit `u`, `w`
/// (both perpendicular to the rotation axis), measured from `u` toward
/// `w`. Used by rotate drags.
pub fn angle_in_plane(v: Vec3, u: Vec3, w: Vec3) -> f32 {
    v.dot(w).atan2(v.dot(u)).to_degrees()
}

// ---------------------------------------------------------------------------
// Gizmo geometry
// ---------------------------------------------------------------------------

/// Gizmo axis basis: world axes, or the entity's local axes when `local`.
pub fn gizmo_basis(local: bool, world_t: &Transform) -> [Vec3; 3] {
    if !local {
        [Vec3::X, Vec3::Y, Vec3::Z]
    } else {
        let q = world_t.quat();
        [q * Vec3::X, q * Vec3::Y, q * Vec3::Z]
    }
}

/// Arrow length in world units so gizmos keep a constant on-screen size.
pub fn gizmo_scale(
    cam: &EditorCamera,
    origin: Vec3,
    dimension: Dimension,
    viewport_h_px: f32,
) -> f32 {
    match dimension {
        Dimension::D2 => cam.ortho_height * 0.14,
        Dimension::D3 => {
            let dist = (cam.pos - origin).length().max(0.01);
            let world_per_px =
                (cam.fov.to_radians() * 0.5).tan() * 2.0 * dist / viewport_h_px.max(1.0);
            world_per_px * 95.0
        }
    }
}

const AXIS_COLORS: [egui::Color32; 3] = [
    egui::Color32::from_rgb(225, 55, 55),
    egui::Color32::from_rgb(70, 210, 70),
    egui::Color32::from_rgb(60, 120, 230),
];

fn stroke_for(color: egui::Color32, hovered: bool) -> egui::Stroke {
    if hovered {
        egui::Stroke::new(4.5, color)
    } else {
        egui::Stroke::new(2.5, color)
    }
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

fn dist_to_polyline(p: egui::Pos2, pts: &[egui::Pos2]) -> f32 {
    pts.windows(2)
        .map(|w| dist_to_segment(p, w[0], w[1]))
        .fold(f32::INFINITY, f32::min)
}

/// Plane (0=XY, 1=XZ, 2=YZ) → (u axis, v axis, normal) indices.
fn plane_axes(plane: usize) -> (usize, usize, usize) {
    match plane {
        0 => (0, 1, 2), // XY, normal Z
        1 => (0, 2, 1), // XZ, normal Y
        _ => (1, 2, 0), // YZ, normal X
    }
}

/// Draw the translate gizmo (axis arrows + optional plane quads). Returns
/// the part under the mouse.
#[allow(clippy::too_many_arguments)]
pub fn draw_translate_gizmo(
    painter: &egui::Painter,
    vp: Mat4,
    origin: Vec3,
    basis: [Vec3; 3],
    len: f32,
    viewport_px: [u32; 4],
    ppp: f32,
    mouse: egui::Pos2,
    with_planes: bool,
) -> Option<GizmoHit> {
    let o = project(vp, origin, viewport_px, ppp)?;
    // Screen segments per axis.
    let tips: [Option<egui::Pos2>; 3] = [
        project(vp, origin + basis[0] * len, viewport_px, ppp),
        project(vp, origin + basis[1] * len, viewport_px, ppp),
        project(vp, origin + basis[2] * len, viewport_px, ppp),
    ];
    // Hit-test: nearest axis arrow wins; then plane quads; then center.
    let mut hit = None;
    let mut best = 8.0_f32;
    for (i, tip) in tips.iter().enumerate() {
        if let Some(t) = tip {
            let d = dist_to_segment(mouse, o, *t);
            if d < best {
                best = d;
                hit = Some(GizmoHit::Axis(i));
            }
        }
    }
    if hit.is_none() && with_planes {
        for plane in 0..3 {
            let (ui, vi, _) = plane_axes(plane);
            let p = origin + basis[ui] * len * 0.5 + basis[vi] * len * 0.5;
            if let Some(pc) = project(vp, p, viewport_px, ppp)
                && (pc - mouse).length() < 9.0
            {
                hit = Some(GizmoHit::Plane(plane));
                break;
            }
        }
    }
    if hit.is_none() && (mouse - o).length() < 7.0 {
        hit = Some(GizmoHit::Screen);
    }
    // Draw plane quads (translucent) under the arrows.
    if with_planes {
        for (plane, axis_color) in AXIS_COLORS.iter().enumerate() {
            let (ui, vi, _) = plane_axes(plane);
            let a = project(vp, origin + basis[ui] * len * 0.55, viewport_px, ppp);
            let b = project(
                vp,
                origin + basis[ui] * len * 0.55 + basis[vi] * len * 0.55,
                viewport_px,
                ppp,
            );
            let c = project(vp, origin + basis[vi] * len * 0.55, viewport_px, ppp);
            if let (Some(a), Some(b), Some(c)) = (a, b, c) {
                let color = if hit == Some(GizmoHit::Plane(plane)) {
                    axis_color.linear_multiply(0.10)
                } else {
                    axis_color.linear_multiply(0.04)
                };
                painter.add(egui::Shape::convex_polygon(
                    vec![o, a, b, c],
                    color,
                    egui::Stroke::NONE,
                ));
            }
        }
    }
    // Draw arrows.
    for (i, tip) in tips.iter().enumerate() {
        if let Some(t) = tip {
            painter.line_segment(
                [o, *t],
                stroke_for(AXIS_COLORS[i], hit == Some(GizmoHit::Axis(i))),
            );
            // Arrow head: small square at the tip.
            painter.rect_filled(
                egui::Rect::from_center_size(*t, egui::vec2(7.0, 7.0)),
                1.0,
                AXIS_COLORS[i],
            );
        }
    }
    painter.circle_filled(o, 3.5, egui::Color32::WHITE);
    hit
}

/// Draw the rotate gizmo (three rings around the axes). Returns the ring
/// under the mouse.
#[allow(clippy::too_many_arguments)]
pub fn draw_rotate_gizmo(
    painter: &egui::Painter,
    vp: Mat4,
    origin: Vec3,
    basis: [Vec3; 3],
    len: f32,
    viewport_px: [u32; 4],
    ppp: f32,
    mouse: egui::Pos2,
) -> Option<GizmoHit> {
    let mut hit = None;
    let mut best = 7.0_f32;
    let mut ring_polylines: [Vec<egui::Pos2>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    for axis in 0..3 {
        let u = basis[(axis + 1) % 3];
        let w = basis[(axis + 2) % 3];
        let mut pts = Vec::with_capacity(41);
        for i in 0..=40 {
            let a = (i as f32 / 40.0) * std::f32::consts::TAU;
            let p = origin + (u * a.cos() + w * a.sin()) * len;
            if let Some(sp) = project(vp, p, viewport_px, ppp) {
                pts.push(sp);
            }
        }
        let d = dist_to_polyline(mouse, &pts);
        if d < best {
            best = d;
            hit = Some(GizmoHit::Ring(axis));
        }
        ring_polylines[axis] = pts;
    }
    for axis in 0..3 {
        let hovered = hit == Some(GizmoHit::Ring(axis));
        let stroke = if hovered {
            egui::Stroke::new(4.0, AXIS_COLORS[axis])
        } else {
            egui::Stroke::new(2.0, AXIS_COLORS[axis].linear_multiply(0.85))
        };
        if ring_polylines[axis].len() > 1 {
            painter.add(egui::Shape::line(ring_polylines[axis].clone(), stroke));
        }
    }
    hit
}

/// Draw the scale gizmo (boxes at the axis tips + a uniform center box).
#[allow(clippy::too_many_arguments)]
pub fn draw_scale_gizmo(
    painter: &egui::Painter,
    vp: Mat4,
    origin: Vec3,
    basis: [Vec3; 3],
    len: f32,
    viewport_px: [u32; 4],
    ppp: f32,
    mouse: egui::Pos2,
) -> Option<GizmoHit> {
    let o = project(vp, origin, viewport_px, ppp)?;
    let tips: [Option<egui::Pos2>; 3] = [
        project(vp, origin + basis[0] * len, viewport_px, ppp),
        project(vp, origin + basis[1] * len, viewport_px, ppp),
        project(vp, origin + basis[2] * len, viewport_px, ppp),
    ];
    let mut hit = None;
    let mut best = 9.0_f32;
    for (i, tip) in tips.iter().enumerate() {
        if let Some(t) = tip {
            let d = (*t - mouse).length();
            if d < best {
                best = d;
                hit = Some(GizmoHit::ScaleBox(i));
            }
        }
    }
    if hit.is_none() && (mouse - o).length() < 8.0 {
        hit = Some(GizmoHit::ScaleUniform);
    }
    for (i, tip) in tips.iter().enumerate() {
        if let Some(t) = tip {
            let size = if hit == Some(GizmoHit::ScaleBox(i)) {
                12.0
            } else {
                9.0
            };
            painter.rect_filled(
                egui::Rect::from_center_size(*t, egui::vec2(size, size)),
                1.5,
                AXIS_COLORS[i],
            );
            painter.line_segment(
                [o, *t],
                egui::Stroke::new(1.5, AXIS_COLORS[i].linear_multiply(0.6)),
            );
        }
    }
    let csize = if hit == Some(GizmoHit::ScaleUniform) {
        12.0
    } else {
        9.0
    };
    painter.rect_filled(
        egui::Rect::from_center_size(o, egui::vec2(csize, csize)),
        2.0,
        egui::Color32::WHITE,
    );
    hit
}

/// 2D move gizmo: X/Y arrows + center square (free move).
pub fn draw_2d_move_gizmo(
    painter: &egui::Painter,
    vp: Mat4,
    origin: Vec3,
    len: f32,
    viewport_px: [u32; 4],
    ppp: f32,
    mouse: egui::Pos2,
) -> Option<GizmoHit> {
    let o = project(vp, origin, viewport_px, ppp)?;
    let tx = project(vp, origin + Vec3::new(len, 0.0, 0.0), viewport_px, ppp);
    let ty = project(vp, origin + Vec3::new(0.0, len, 0.0), viewport_px, ppp);
    let mut hit = None;
    let mut best = 8.0_f32;
    for (i, t) in [tx, ty].iter().enumerate() {
        if let Some(t) = t {
            let d = dist_to_segment(mouse, o, *t);
            if d < best {
                best = d;
                hit = Some(GizmoHit::Axis(i));
            }
        }
    }
    if hit.is_none() && (mouse - o).length() < 9.0 {
        hit = Some(GizmoHit::Screen);
    }
    if let Some(t) = tx {
        painter.line_segment(
            [o, t],
            stroke_for(AXIS_COLORS[0], hit == Some(GizmoHit::Axis(0))),
        );
    }
    if let Some(t) = ty {
        painter.line_segment(
            [o, t],
            stroke_for(AXIS_COLORS[1], hit == Some(GizmoHit::Axis(1))),
        );
    }
    let sz = if hit == Some(GizmoHit::Screen) {
        12.0
    } else {
        9.0
    };
    painter.rect_filled(
        egui::Rect::from_center_size(o, egui::vec2(sz, sz)),
        2.0,
        egui::Color32::WHITE,
    );
    hit
}

/// Rect tool: outline of the sprite's world rect + corner handles.
/// `center`/`half`/`rot_z_deg` describe the rect in world space;
/// `mouse_world` is the pointer's 2D world position (for the inside test).
#[allow(clippy::too_many_arguments)]
pub fn draw_rect_gizmo(
    painter: &egui::Painter,
    vp: Mat4,
    center: Vec3,
    half: Vec2,
    rot_z_deg: f32,
    viewport_px: [u32; 4],
    ppp: f32,
    mouse: egui::Pos2,
    mouse_world: Vec2,
) -> Option<GizmoHit> {
    let q = Quat::from_rotation_z(rot_z_deg.to_radians());
    let corners_local = [
        Vec2::new(-half.x, -half.y),
        Vec2::new(half.x, -half.y),
        Vec2::new(half.x, half.y),
        Vec2::new(-half.x, half.y),
    ];
    let corners_world: Vec<Vec3> = corners_local
        .iter()
        .map(|c| center + q * Vec3::new(c.x, c.y, 0.0))
        .collect();
    let corners_screen: Vec<Option<egui::Pos2>> = corners_world
        .iter()
        .map(|c| project(vp, *c, viewport_px, ppp))
        .collect();
    // Corner hit-test first (they win over inside).
    let mut hit = None;
    let mut best = 11.0_f32;
    for (i, c) in corners_screen.iter().enumerate() {
        if let Some(c) = c {
            let d = (*c - mouse).length();
            if d < best {
                best = d;
                hit = Some(GizmoHit::RectCorner(i));
            }
        }
    }
    if hit.is_none() {
        // Inside test in rect-local space.
        let inv_q = q.conjugate();
        let rel = Vec3::new(mouse_world.x - center.x, mouse_world.y - center.y, 0.0);
        let local = inv_q * rel;
        if local.x.abs() <= half.x && local.y.abs() <= half.y {
            hit = Some(GizmoHit::RectInside);
        }
    }
    // Draw outline.
    let stroke = egui::Stroke::new(1.5, egui::Color32::from_rgb(255, 170, 40));
    for i in 0..4 {
        if let (Some(a), Some(b)) = (corners_screen[i], corners_screen[(i + 1) % 4]) {
            painter.line_segment([a, b], stroke);
        }
    }
    // Corner handles.
    for (i, c) in corners_screen.iter().enumerate() {
        if let Some(c) = c {
            let hovered = hit == Some(GizmoHit::RectCorner(i));
            let sz = if hovered { 10.0 } else { 7.0 };
            painter.rect_filled(
                egui::Rect::from_center_size(*c, egui::vec2(sz, sz)),
                1.5,
                if hovered {
                    egui::Color32::WHITE
                } else {
                    egui::Color32::from_rgb(255, 170, 40)
                },
            );
        }
    }
    hit
}

// ---------------------------------------------------------------------------
// Overlays: grid, axes, selection
// ---------------------------------------------------------------------------

/// Draw the 3D ground grid (XZ plane) + world-origin axes.
pub fn draw_grid_and_axes(
    painter: &egui::Painter,
    view_proj: Mat4,
    viewport_px: [u32; 4],
    ppp: f32,
) {
    for i in -10..=10 {
        let major = i % 5 == 0;
        let a = if i == 0 {
            egui::Color32::from_rgb(110, 110, 110)
        } else if major {
            egui::Color32::from_rgb(75, 75, 75)
        } else {
            egui::Color32::from_rgb(48, 48, 48)
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
        (Vec3::new(len, 0.0, 0.0), AXIS_COLORS[0]),
        (Vec3::new(0.0, len, 0.0), AXIS_COLORS[1]),
        (Vec3::new(0.0, 0.0, len), AXIS_COLORS[2]),
    ];
    for (end, color) in axes {
        let p1 = project(view_proj, Vec3::ZERO, viewport_px, ppp);
        let p2 = project(view_proj, end, viewport_px, ppp);
        if let (Some(p1), Some(p2)) = (p1, p2) {
            painter.line_segment([p1, p2], egui::Stroke::new(2.5, color));
        }
    }
}

/// Draw the 2D grid + axes (XY plane, world units).
pub fn draw_grid_2d(
    painter: &egui::Painter,
    cam: &EditorCamera,
    vp: Mat4,
    viewport_px: [u32; 4],
    ppp: f32,
) {
    let step = if cam.ortho_height > 60.0 {
        10.0
    } else if cam.ortho_height > 25.0 {
        5.0
    } else {
        1.0
    };
    let half_h = cam.ortho_height * 0.6;
    let half_w = half_h * 1.8;
    let cx = cam.pos.x;
    let cy = cam.pos.y;
    let start_x = ((cx - half_w) / step).floor() * step;
    let start_y = ((cy - half_h) / step).floor() * step;
    let mut x = start_x;
    while x <= cx + half_w {
        let major = (x / step).round().rem_euclid(5.0) < 0.5;
        let a = if x.abs() < 1e-6 {
            AXIS_COLORS[1].linear_multiply(0.8)
        } else if major {
            egui::Color32::from_rgb(70, 70, 70)
        } else {
            egui::Color32::from_rgb(45, 45, 45)
        };
        let p1 = project(vp, Vec3::new(x, cy - half_h, 0.0), viewport_px, ppp);
        let p2 = project(vp, Vec3::new(x, cy + half_h, 0.0), viewport_px, ppp);
        if let (Some(p1), Some(p2)) = (p1, p2) {
            painter.line_segment([p1, p2], egui::Stroke::new(1.0, a));
        }
        x += step;
    }
    let mut y = start_y;
    while y <= cy + half_h {
        let major = (y / step).round().rem_euclid(5.0) < 0.5;
        let a = if y.abs() < 1e-6 {
            AXIS_COLORS[0].linear_multiply(0.8)
        } else if major {
            egui::Color32::from_rgb(70, 70, 70)
        } else {
            egui::Color32::from_rgb(45, 45, 45)
        };
        let p1 = project(vp, Vec3::new(cx - half_w, y, 0.0), viewport_px, ppp);
        let p2 = project(vp, Vec3::new(cx + half_w, y, 0.0), viewport_px, ppp);
        if let (Some(p1), Some(p2)) = (p1, p2) {
            painter.line_segment([p1, p2], egui::Stroke::new(1.0, a));
        }
        y += step;
    }
    // Axes cross at the origin.
    let o = project(vp, Vec3::ZERO, viewport_px, ppp);
    let ax = project(vp, Vec3::new(1.2, 0.0, 0.0), viewport_px, ppp);
    let ay = project(vp, Vec3::new(0.0, 1.2, 0.0), viewport_px, ppp);
    if let (Some(o), Some(ax)) = (o, ax) {
        painter.line_segment([o, ax], egui::Stroke::new(2.0, AXIS_COLORS[0]));
    }
    if let (Some(o), Some(ay)) = (o, ay) {
        painter.line_segment([o, ay], egui::Stroke::new(2.0, AXIS_COLORS[1]));
    }
}

/// Picking half-extents for an entity: collider shape > sprite size >
/// default cube. Uses the entity's *world* scale.
pub fn pick_half_extents(world: &hecs::World, e: hecs::Entity) -> Vec3 {
    let wt = spark::ecs::world_transform(world, e);
    let half = if let Ok(c) = world.get::<&Collider>(e) {
        match c.shape {
            ColliderShape::Box { half } => half,
            ColliderShape::Ball { r } => Vec3::new(r, r, r),
            ColliderShape::Capsule { half_height, r } => Vec3::new(r, half_height + r, r),
        }
    } else if let Ok(sp) = world.get::<&Sprite>(e) {
        Vec3::new(sp.size.x * 0.5, sp.size.y * 0.5, 0.25)
    } else {
        Vec3::new(0.5, 0.5, 0.5)
    };
    Vec3::new(
        (half.x * wt.scale.x).abs().max(0.05),
        (half.y * wt.scale.y).abs().max(0.05),
        (half.z * wt.scale.z).abs().max(0.05),
    )
}

/// Picking: cast a ray and find the nearest entity whose world AABB it
/// intersects. Cameras/lights are pickable too (like Unity's icons).
pub fn pick_entity(world: &hecs::World, origin: Vec3, dir: Vec3) -> Option<(hecs::Entity, f32)> {
    let mut best: Option<(hecs::Entity, f32)> = None;
    for (e, _) in world.query::<&Transform>().iter() {
        let center = spark::ecs::world_transform(world, e).position;
        let half = pick_half_extents(world, e);
        if let Some(t_hit) = ray_aabb(origin, dir, center, half)
            && best.map(|(_, d)| t_hit < d).unwrap_or(true)
        {
            best = Some((e, t_hit));
        }
    }
    best
}

/// Wireframe selection outline (3D box) or rect outline (2D) around each
/// selected entity. Primary selection is orange; others yellow.
pub fn draw_selection(
    painter: &egui::Painter,
    world: &hecs::World,
    selected: &[hecs::Entity],
    vp: Mat4,
    dimension: Dimension,
    viewport_px: [u32; 4],
    ppp: f32,
) {
    for (idx, e) in selected.iter().enumerate() {
        if !world.contains(*e) {
            continue;
        }
        let wt = spark::ecs::world_transform(world, *e);
        let half = pick_half_extents(world, *e);
        let primary = idx + 1 == selected.len();
        let color = if primary {
            egui::Color32::from_rgb(255, 160, 40)
        } else {
            egui::Color32::from_rgb(240, 220, 60)
        };
        let stroke = egui::Stroke::new(1.5, color);
        match dimension {
            Dimension::D2 => {
                let (p1, p2, p3, p4) = (
                    project(
                        vp,
                        wt.position + Vec3::new(-half.x, -half.y, 0.0),
                        viewport_px,
                        ppp,
                    ),
                    project(
                        vp,
                        wt.position + Vec3::new(half.x, -half.y, 0.0),
                        viewport_px,
                        ppp,
                    ),
                    project(
                        vp,
                        wt.position + Vec3::new(half.x, half.y, 0.0),
                        viewport_px,
                        ppp,
                    ),
                    project(
                        vp,
                        wt.position + Vec3::new(-half.x, half.y, 0.0),
                        viewport_px,
                        ppp,
                    ),
                );
                if let (Some(p1), Some(p2), Some(p3), Some(p4)) = (p1, p2, p3, p4) {
                    for seg in [[p1, p2], [p2, p3], [p3, p4], [p4, p1]] {
                        painter.line_segment(seg, stroke);
                    }
                }
            }
            Dimension::D3 => {
                // 8 corners, 12 edges.
                let mut c = Vec::with_capacity(8);
                for sx in [-1.0, 1.0] {
                    for sy in [-1.0, 1.0] {
                        for sz in [-1.0, 1.0] {
                            c.push(project(
                                vp,
                                wt.position + Vec3::new(sx * half.x, sy * half.y, sz * half.z),
                                viewport_px,
                                ppp,
                            ));
                        }
                    }
                }
                if c.iter().any(|p| p.is_none()) {
                    continue;
                }
                let c: Vec<egui::Pos2> = c.into_iter().map(|p| p.unwrap()).collect();
                // Corner order: x slow, y mid, z fast (see loops above).
                let edges = [
                    (0, 1),
                    (1, 3),
                    (3, 2),
                    (2, 0), // z = -1 face
                    (4, 5),
                    (5, 7),
                    (7, 6),
                    (6, 4), // z = +1 face
                    (0, 4),
                    (1, 5),
                    (2, 6),
                    (3, 7), // connectors
                ];
                for (a, b) in edges {
                    painter.line_segment([c[a], c[b]], stroke);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Drag math (pure functions used by the interaction handler)
// ---------------------------------------------------------------------------

/// The "now" hit point for a translate drag.
pub fn translate_now(
    cam: &EditorCamera,
    dimension: Dimension,
    drag: &DragState,
    mouse_px: egui::Pos2,
    viewport_px: [u32; 4],
    ppp: f32,
    aspect: f32,
) -> Vec3 {
    let (ray_o, ray_d) = pick_ray(cam, dimension, aspect, mouse_px, viewport_px, ppp);
    match drag.drag {
        crate::state::GizmoDrag::TranslateAxis { .. } => {
            ray_line_closest(ray_o, ray_d, drag.start_world, drag.axis_dir)
        }
        crate::state::GizmoDrag::TranslatePlane { .. } => {
            // `axis_dir` holds the plane normal in gizmo space.
            ray_plane(ray_o, ray_d, drag.start_world, drag.axis_dir)
        }
        crate::state::GizmoDrag::TranslateScreen => match dimension {
            Dimension::D2 => Vec3::new(ray_o.x, ray_o.y, drag.start_world.z),
            Dimension::D3 => {
                let normal = cam.forward() * -1.0;
                ray_plane(ray_o, ray_d, drag.start_world, normal)
            }
        },
        crate::state::GizmoDrag::RectCorner { .. } => {
            Vec3::new(ray_o.x, ray_o.y, drag.start_world.z)
        }
        _ => drag.start_world,
    }
}

/// The current rotation angle (degrees) for a rotate drag.
pub fn rotate_now_deg(
    cam: &EditorCamera,
    dimension: Dimension,
    drag: &DragState,
    mouse_px: egui::Pos2,
    viewport_px: [u32; 4],
    ppp: f32,
    aspect: f32,
) -> f32 {
    let (ray_o, ray_d) = pick_ray(cam, dimension, aspect, mouse_px, viewport_px, ppp);
    let axis = drag.axis_dir;
    // Plane basis perpendicular to the axis.
    let u = if axis.x.abs() < 0.9 { Vec3::X } else { Vec3::Y };
    let u = (u - axis * u.dot(axis)).normalize_or_zero();
    let w = axis.cross(u);
    let hit = ray_plane(ray_o, ray_d, drag.start_world, axis);
    angle_in_plane(hit - drag.start_world, u, w)
}

/// The current scale factor for a scale drag. `center_px` is the gizmo's
/// current screen position (uniform scale measures pointer distance to it).
pub fn scale_factor_now(
    cam: &EditorCamera,
    drag: &DragState,
    mouse_px: egui::Pos2,
    viewport_px: [u32; 4],
    ppp: f32,
    aspect: f32,
    center_px: egui::Pos2,
) -> f32 {
    match drag.drag {
        crate::state::GizmoDrag::ScaleAxis { .. } => {
            let (ray_o, ray_d) = pick_ray(cam, Dimension::D3, aspect, mouse_px, viewport_px, ppp);
            let hit = ray_line_closest(ray_o, ray_d, drag.start_world, drag.axis_dir);
            let t_now = (hit - drag.start_world).dot(drag.axis_dir);
            if drag.start_t.abs() < 1e-4 {
                1.0
            } else {
                t_now / drag.start_t
            }
        }
        _ => {
            let d = (mouse_px - center_px).length();
            if drag.start_px_dist < 1e-3 {
                1.0
            } else {
                // Uniform scale: ratio of pointer distance to the gizmo
                // center on screen.
                d / drag.start_px_dist
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Snapping
// ---------------------------------------------------------------------------

pub fn snap_f32(snap: &SnapSettings, v: f32, step: f32) -> f32 {
    if !snap.enabled || step <= 1e-4 {
        v
    } else {
        (v / step).round() * step
    }
}

pub fn snap_translate(snap: &SnapSettings, v: Vec3) -> Vec3 {
    Vec3::new(
        snap_f32(snap, v.x, snap.translate),
        snap_f32(snap, v.y, snap.translate),
        snap_f32(snap, v.z, snap.translate),
    )
}

pub fn snap_scale_val(snap: &SnapSettings, s: f32) -> f32 {
    snap_f32(snap, s, snap.scale).max(0.01)
}

/// Tool → which gizmo set to draw/hit-test.
pub fn tool_parts(tool: Tool) -> (bool, bool, bool) {
    // (translate, rotate, scale)
    match tool {
        Tool::Move => (true, false, false),
        Tool::Rotate => (false, true, false),
        Tool::Scale => (false, false, true),
        Tool::Transform => (true, true, true),
        _ => (false, false, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{DragState, GizmoDrag};

    #[test]
    fn ray_aabb_hit_and_miss() {
        let origin = Vec3::new(0.0, 0.0, -5.0);
        let dir = Vec3::new(0.0, 0.0, 1.0);
        let hit = ray_aabb(origin, dir, Vec3::ZERO, Vec3::new(0.5, 0.5, 0.5));
        assert!(hit.is_some());
        assert!((hit.unwrap() - 4.5).abs() < 0.01);
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
        let cam = EditorCamera {
            yaw: 0.0,
            pitch: 0.0,
            pos: Vec3::new(0.0, 0.0, 10.0),
            ..Default::default()
        };
        let vp = view_proj(&cam, Dimension::D3, 1.0);
        let p = project(vp, Vec3::new(0.0, 0.0, 0.0), [0, 0, 100, 100], 1.0);
        assert!(p.is_some());
        let p = p.unwrap();
        assert!((p.x - 50.0).abs() < 0.5, "x = {}", p.x);
        assert!((p.y - 50.0).abs() < 0.5, "y = {}", p.y);
    }

    #[test]
    fn ray_line_closest_is_on_the_line() {
        // Ray down -Z from (0,0,-5); X axis line through origin.
        let hit = ray_line_closest(
            Vec3::new(0.0, 0.0, -5.0),
            Vec3::new(0.0, 0.0, 1.0),
            Vec3::ZERO,
            Vec3::X,
        );
        // Closest point on the X line to the ray: (0,0,0).
        assert!(hit.x.abs() < 1e-5, "x = {}", hit.x);
        assert!(hit.y.abs() < 1e-5 && hit.z.abs() < 1e-5);
        // Ray aimed at (3, 0, 0) from (0, 0, -5): closest point ≈ x=3.
        let hit = ray_line_closest(
            Vec3::new(3.0, 0.0, -5.0),
            Vec3::new(0.0, 0.0, 1.0),
            Vec3::ZERO,
            Vec3::X,
        );
        assert!((hit.x - 3.0).abs() < 1e-4, "x = {}", hit.x);
    }

    #[test]
    fn ray_plane_intersects() {
        // z=0 plane, ray from (2, 3, 10) looking down -Z.
        let hit = ray_plane(
            Vec3::new(2.0, 3.0, 10.0),
            Vec3::new(0.0, 0.0, -1.0),
            Vec3::ZERO,
            Vec3::Z,
        );
        assert!((hit.x - 2.0).abs() < 1e-5 && (hit.y - 3.0).abs() < 1e-5);
        assert!(hit.z.abs() < 1e-5);
    }

    #[test]
    fn angle_in_plane_quadrants() {
        let u = Vec3::X;
        let w = Vec3::Y;
        assert!((angle_in_plane(Vec3::new(1.0, 0.0, 0.0), u, w)).abs() < 1e-4);
        assert!((angle_in_plane(Vec3::new(0.0, 1.0, 0.0), u, w) - 90.0).abs() < 1e-3);
        assert!((angle_in_plane(Vec3::new(-1.0, 0.0, 0.0), u, w) - 180.0).abs() < 1e-3);
        assert!((angle_in_plane(Vec3::new(0.0, -1.0, 0.0), u, w) + 90.0).abs() < 1e-3);
    }

    #[test]
    fn snapping_quantizes() {
        let snap = SnapSettings {
            enabled: true,
            ..Default::default()
        };
        assert_eq!(snap_f32(&snap, 0.24, 0.5), 0.0);
        assert_eq!(snap_f32(&snap, 0.26, 0.5), 0.5);
        assert_eq!(snap_f32(&snap, 14.9, 15.0), 15.0);
        assert_eq!(snap_f32(&snap, 7.4, 15.0), 0.0);
        // Disabled → unchanged.
        let snap = SnapSettings::default();
        assert_eq!(snap_f32(&snap, 0.26, 0.5), 0.26);
    }

    #[test]
    fn rotation_delta_wraps() {
        let drag = DragState {
            drag: GizmoDrag::RotateAxis { axis: 1 },
            start_mouse: egui::Pos2::ZERO,
            start_world: Vec3::ZERO,
            start_hit: Vec3::ZERO,
            axis_dir: Vec3::Y,
            start_angle: 170.0,
            start_t: 1.0,
            start_px_dist: 100.0,
            rect_anchor: Vec3::ZERO,
            rect_sprite_before: None,
            entities: Vec::new(),
        };
        // 340° of motion the long way round should wrap to +20°.
        assert!((drag.rotation_deg(-170.0) - 20.0).abs() < 1e-4);
        // A half turn stays at ±180 (boundary).
        assert!((drag.rotation_deg(350.0) - 180.0).abs() < 1e-4);
    }
}
