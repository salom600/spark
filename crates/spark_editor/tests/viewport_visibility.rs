//! Viewport visibility regression: the 3D scene must not be hidden behind
//! egui panel backgrounds.
//!
//! Bug guarded (reported on Windows): the cube was selectable and its
//! gizmo/outline worked, but the shaded mesh was invisible in the editor
//! viewport — only the orange selection wireframe showed.
//!
//! Root cause: the editor renders the scene in a scissored wgpu pass and
//! then draws egui on top (`LoadOp::Load`). egui's `CentralPanel::default()`
//! paints an **opaque** `visuals.panel_fill` rectangle over the whole
//! viewport, so every GPU-drawn pixel was overdrawn by the panel background.
//! Only egui-painted content (the selection outline, grid, gizmos — drawn by
//! `ui.painter()` after the panel background within the same egui frame)
//! stayed visible. Standalone games were unaffected (their HUD is an
//! `egui::Area`, which paints no background).
//!
//! This test runs the real editor UI headlessly (egui needs no GPU), then
//! inspects the tessellated meshes: no *large opaque* triangle may cover
//! the viewport region — otherwise whatever the renderer drew underneath is
//! invisible, no matter how correct the mesh/ Material/camera pipeline is.

use spark_editor::Editor;

#[test]
fn viewport_scene_is_not_covered_by_opaque_egui_background() {
    let mut ed = Editor::headless();
    // Scene content so the viewport has something to show.
    ed.add_mesh("cube");

    let ctx = egui::Context::default();
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(1600.0, 900.0),
        )),
        ..Default::default()
    };
    let output = ctx.run(raw, |ctx| ed.ui(ctx));
    let [vx, vy, vw, vh] = ed.state.viewport_px;
    assert!(
        vw > 400 && vh > 300,
        "viewport rect must be substantial, got {vx},{vy},{vw}x{vh}"
    );

    let tess = ctx.tessellate(output.shapes, 1.0);

    // Sample the viewport interior (inset so toolbar edges/gizmo handles
    // at the border can't false-positive).
    let (vx, vy, vw, vh) = (vx as f32, vy as f32, vw as f32, vh as f32);
    let inset = 40.0;
    let mut covered = Vec::new();
    for i in 0..5 {
        for j in 0..5 {
            let p = egui::pos2(
                vx + inset + (vw - 2.0 * inset) * i as f32 / 4.0,
                vy + inset + (vh - 2.0 * inset) * j as f32 / 4.0,
            );
            if let Some(color) = big_opaque_cover(&tess, p) {
                covered.push((p, color));
            }
        }
    }
    assert!(
        covered.is_empty(),
        "the editor viewport is overdrawn by an opaque egui background \
         ({} of 25 sample points covered, e.g. {:?} by color {:?}) — \
         the GPU-rendered scene cannot be seen through it. The central \
         panel must use a transparent frame.",
        covered.len(),
        covered.first().map(|(p, _)| *p),
        covered.first().map(|(_, c)| *c),
    );
}

/// The color of the first *large, fully opaque* mesh triangle containing
/// `p`, if any. Threshold 5000 px² separates real panel backgrounds
/// (hundreds of thousands of px²) from grid lines, gizmo strokes and
/// widget glyphs (all well under 1000 px²).
fn big_opaque_cover(tess: &[egui::ClippedPrimitive], p: egui::Pos2) -> Option<egui::Color32> {
    for cp in tess {
        let mesh = match &cp.primitive {
            egui::epaint::Primitive::Mesh(m) => m,
            _ => continue,
        };
        for tri in mesh.indices.as_chunks::<3>().0 {
            let a = &mesh.vertices[tri[0] as usize];
            let b = &mesh.vertices[tri[1] as usize];
            let c = &mesh.vertices[tri[2] as usize];
            if a.color.a() != 255 || b.color.a() != 255 || c.color.a() != 255 {
                continue; // translucent (gizmo planes, grid AA) — never hides.
            }
            if triangle_area(a.pos, b.pos, c.pos) < 5000.0 {
                continue; // thin strokes, text, small widgets.
            }
            if point_in_triangle(p, a.pos, b.pos, c.pos) {
                return Some(a.color);
            }
        }
    }
    None
}

fn triangle_area(a: egui::Pos2, b: egui::Pos2, c: egui::Pos2) -> f32 {
    ((b.x - a.x) * (c.y - a.y) - (c.x - a.x) * (b.y - a.y)).abs() * 0.5
}

fn point_in_triangle(p: egui::Pos2, a: egui::Pos2, b: egui::Pos2, c: egui::Pos2) -> bool {
    let d1 = sign(p, a, b);
    let d2 = sign(p, b, c);
    let d3 = sign(p, c, a);
    let has_neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let has_pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(has_neg && has_pos)
}

fn sign(p: egui::Pos2, a: egui::Pos2, b: egui::Pos2) -> f32 {
    (p.x - b.x) * (a.y - b.y) - (a.x - b.x) * (p.y - b.y)
}
