//! Editor panels: hierarchy tree, inspector, viewport (tools + gizmos +
//! picking + shortcuts), asset browser, console, modals.

use spark::ecs;
use spark::prelude::*;
use spark::reexport::{egui, hecs};
use spark::rules::{Action, CmpOp, Cond, Rule, RuleEvent};

use crate::Editor;
use crate::gizmo::{self, GizmoHit};
use crate::state::{DragState, GizmoDrag, Tool};

impl Editor {
    // -----------------------------------------------------------------------
    // Hierarchy (left)
    // -----------------------------------------------------------------------

    pub(crate) fn hierarchy_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Hierarchy");
        ui.separator();
        if self.play_state != crate::PlayState::Stopped {
            ui.weak("(snapshot — edits disabled while playing)");
        }
        let released = ui.ctx().input(|i| i.pointer.any_released());
        egui::ScrollArea::vertical().show(ui, |ui| {
            let roots = ecs::roots(&self.engine.scene.world);
            let empty = roots.is_empty();
            for root in roots {
                self.entity_row(ui, root, 0, released);
            }
            if empty {
                ui.weak("(empty scene — Scene → Add Entity)");
            }
            // Root drop zone (unparent).
            if let Some(src) = self.state.hierarchy_drag {
                let label = ecs::entity_label(&self.engine.scene.world, src);
                let resp = ui
                    .button(format!("⇧ move \"{label}\" to root"))
                    .on_hover_text("release the mouse here to unparent");
                if resp.clicked() || (resp.hovered() && released) {
                    self.reparent(src, None);
                    self.state.hierarchy_drag = None;
                }
            }
        });
        if released && !ui.ctx().is_pointer_over_area() {
            // Dropped nowhere: cancel the drag.
            self.state.hierarchy_drag = None;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn entity_row(&mut self, ui: &mut egui::Ui, e: hecs::Entity, depth: usize, released: bool) {
        if !self.engine.scene.world.contains(e) {
            return;
        }
        let (label, children) = {
            let world = &self.engine.scene.world;
            (ecs::entity_label(world, e), ecs::children(world, e))
        };
        let selected = self.state.is_selected(e);
        let has_children = !children.is_empty();
        let open = depth < 2 || self.state.tree_open.contains(&e);
        let drop_target = self
            .state
            .hierarchy_drag
            .is_some_and(|src| src != e && !self.state.drag.is_some());

        ui.horizontal(|ui| {
            ui.add_space((depth * 14) as f32);
            // Expander.
            if has_children {
                let arrow = if open { "▾" } else { "▸" };
                if ui.small_button(arrow).clicked() {
                    if open {
                        self.state.tree_open.remove(&e);
                    } else {
                        self.state.tree_open.insert(e);
                    }
                }
            } else {
                ui.label("  ");
            }
            // Visibility eye.
            let vis = self
                .engine
                .scene
                .world
                .get::<&Visible>(e)
                .map(|v| v.0)
                .unwrap_or(true);
            let eye = if vis { "●" } else { "○" };
            if ui
                .small_button(eye)
                .on_hover_text("toggle visibility (Visible component)")
                .clicked()
            {
                self.toggle_visibility(e, vis);
            }
            // Label / rename field / drop target.
            if self.state.renaming == Some(e) {
                let mut name = label.clone();
                let resp = ui.text_edit_singleline(&mut name);
                if resp.lost_focus() || resp.clicked_elsewhere() {
                    self.rename_entity(e, name.trim());
                    self.state.renaming = None;
                }
            } else {
                let title = if selected {
                    egui::RichText::new(&label).strong()
                } else {
                    egui::RichText::new(&label)
                };
                let mut resp = ui
                    .selectable_label(selected, title)
                    .on_hover_text("click: select · double-click: rename · drag: reparent");
                if drop_target && resp.hovered() {
                    resp = resp.highlight();
                }
                if resp.clicked() {
                    let ctrl = ui.ctx().input(|i| i.modifiers.ctrl);
                    if ctrl {
                        self.state.toggle_select(e);
                    } else {
                        self.state.select(e);
                    }
                }
                if resp.double_clicked() {
                    self.state.renaming = Some(e);
                }
                if resp.dragged() {
                    self.state.hierarchy_drag = Some(e);
                }
                if drop_target
                    && resp.hovered()
                    && released
                    && let Some(src) = self.state.hierarchy_drag.take()
                {
                    self.reparent(src, Some(e));
                }
                resp.context_menu(|ui| self.entity_context_menu(ui, e));
            }
        });
        if open && has_children {
            for child in children {
                self.entity_row(ui, child, depth + 1, released);
            }
        }
    }

    /// Toggle an entity's `Visible` component (undoable).
    pub fn toggle_visibility(&mut self, e: hecs::Entity, currently: bool) {
        let before = self.snapshot_component(e, "Visible");
        let world = &mut self.engine.scene.world;
        let _ = world.insert_one(e, Visible(!currently));
        let after = self.snapshot_component(e, "Visible");
        if before != after {
            self.push_component_cmd(e, "Visible", before, after, "Toggle");
        }
    }

    fn entity_context_menu(&mut self, ui: &mut egui::Ui, e: hecs::Entity) {
        if ui.button("Add Child Entity").clicked() {
            self.add_entity("Entity", Some(e));
            ui.close();
        }
        ui.separator();
        if ui.button("Duplicate (Ctrl+D)").clicked() {
            self.state.select(e);
            self.duplicate_selected();
            ui.close();
        }
        if ui.button("Delete (Del)").clicked() {
            self.state.select(e);
            self.despawn_selected();
            ui.close();
        }
        ui.separator();
        if ui.button("Focus (F)").clicked() {
            self.state.select(e);
            self.focus_selection();
            ui.close();
        }
    }

    // -----------------------------------------------------------------------
    // Inspector (right)
    // -----------------------------------------------------------------------

    pub(crate) fn inspector_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Inspector");
        ui.separator();

        self.state.retain_existing(&self.engine.scene.world);
        if self.state.selected.is_empty() {
            ui.weak("Select an entity in the Hierarchy or Viewport");
            return;
        }
        if self.state.selected.len() > 1 {
            ui.colored_label(
                egui::Color32::from_rgb(255, 170, 40),
                format!(
                    "{} entities selected — showing primary",
                    self.state.selected.len()
                ),
            );
            ui.weak("gizmo drags and Del/Ctrl+D act on all");
            ui.separator();
        }
        let Some(e) = self.state.primary() else {
            return;
        };
        if !self.engine.scene.world.contains(e) {
            return;
        }

        // Name + tag.
        let mut name = self
            .engine
            .scene
            .world
            .get::<&ecs::Name>(e)
            .map(|n| n.0.clone())
            .unwrap_or_default();
        let mut tag = self
            .engine
            .scene
            .world
            .get::<&ecs::Tag>(e)
            .map(|t| t.0.clone())
            .unwrap_or_default();
        egui::Grid::new("meta").num_columns(2).show(ui, |ui| {
            ui.strong("Name");
            if ui.text_edit_singleline(&mut name).changed() {
                let _ = self
                    .engine
                    .scene
                    .world
                    .insert_one(e, ecs::Name(name.clone()));
            }
            ui.end_row();
            ui.strong("Tag");
            if ui.text_edit_singleline(&mut tag).changed() {
                let _ = self.engine.scene.world.insert_one(e, ecs::Tag(tag.clone()));
            }
            ui.end_row();
        });
        ui.separator();

        // Transform is always editable (auto-added on selection if missing).
        if self.engine.scene.world.get::<&Transform>(e).is_err() {
            let _ = self.engine.scene.world.insert_one(e, Transform::default());
        }
        self.inspect_component(ui, e, "Transform");

        // Rules get the bespoke editor; everything else the generated one.
        let names: Vec<&'static str> = self.engine.registry.names().collect();
        for comp_name in names {
            if comp_name == "Transform" {
                continue;
            }
            let has = self
                .engine
                .registry
                .get(comp_name)
                .map(|en| (en.has)(&self.engine.scene.world, e))
                .unwrap_or(false);
            if !has {
                continue;
            }
            if comp_name == "Rules" {
                self.rules_editor(ui, e);
            } else {
                self.inspect_component(ui, e, comp_name);
            }
        }

        ui.separator();
        self.add_component_button(ui, e);
    }

    /// One component: collapsing header + generated inspector + undo capture.
    fn inspect_component(&mut self, ui: &mut egui::Ui, e: hecs::Entity, name: &str) {
        let Some(entry) = self.engine.registry.get(name) else {
            return;
        };
        let static_name = entry.name;
        let before = self.snapshot_component(e, name);

        let mut changed = false;
        egui::CollapsingHeader::new(name)
            .default_open(true)
            .id_salt(("comp", e.to_bits(), static_name))
            .show(ui, |ui| {
                let Editor { engine, .. } = self;
                let entry = engine.registry.get(name).unwrap();
                changed = (entry.inspect)(&mut engine.scene.world, e, ui);
            })
            .header_response
            .context_menu(|ui| {
                if ui.button("Remove component").clicked() {
                    let before2 = before.clone();
                    self.push_component_cmd(e, static_name, before2, None, "Remove");
                    let entry = self.engine.registry.get(name).unwrap();
                    (entry.remove)(&mut self.engine.scene.world, e);
                    ui.close();
                }
                if ui.button("Reset to default").clicked() {
                    let before2 = before.clone();
                    let entry = self.engine.registry.get(name).unwrap();
                    (entry.add_default)(&mut self.engine.scene.world, e);
                    let after = self.snapshot_component(e, name);
                    if before2 != after {
                        self.push_component_cmd(e, static_name, before2, after, "Reset");
                    }
                    ui.close();
                }
            });

        if changed {
            let after = self.snapshot_component(e, name);
            if before.as_deref() != after.as_deref() {
                self.push_component_cmd(e, static_name, before, after, "Edit");
            }
            self.engine.physics.request_rebuild();
        }
    }

    fn add_component_button(&mut self, ui: &mut egui::Ui, e: hecs::Entity) {
        egui::ComboBox::from_id_salt("add_component")
            .selected_text("+ Add Component")
            .width(ui.available_width())
            .show_ui(ui, |ui| {
                let names: Vec<&'static str> = self.engine.registry.names().collect();
                for name in names {
                    let entry = self.engine.registry.get(name).unwrap();
                    if (entry.has)(&self.engine.scene.world, e) {
                        continue;
                    }
                    if ui.selectable_label(false, name).clicked() {
                        (entry.add_default)(&mut self.engine.scene.world, e);
                        let after = self.snapshot_component(e, name);
                        self.push_component_cmd(e, entry.name, None, after, "Add");
                    }
                }
            });
    }

    // -----------------------------------------------------------------------
    // Rules editor (bespoke UI over Rule lists; undo via text snapshots)
    // -----------------------------------------------------------------------

    fn rules_editor(&mut self, ui: &mut egui::Ui, e: hecs::Entity) {
        let before = self.snapshot_component(e, "Rules");
        egui::CollapsingHeader::new("Rules")
            .default_open(true)
            .id_salt(("comp", e.to_bits(), "Rules"))
            .show(ui, |ui| {
                let Editor { engine, .. } = self;
                let Ok(mut rc) = engine.scene.world.get::<&mut RulesComp>(e) else {
                    return;
                };
                if ui.button("+ Add Rule").clicked() {
                    rc.rules.push(Rule {
                        on: RuleEvent::Update,
                        when: vec![],
                        run: vec![Action::Log("hello".into())],
                        enabled: true,
                    });
                }
                let mut remove_idx = None;
                for (i, rule) in rc.rules.iter_mut().enumerate() {
                    ui.push_id(("rule", i), |ui| {
                        egui::CollapsingHeader::new(format!("#{} {}", i + 1, rule.summary()))
                            .default_open(false)
                            .show(ui, |ui| {
                                ui.checkbox(&mut rule.enabled, "enabled");
                                rule_event_ui(ui, &mut rule.on);
                                ui.label("When:");
                                rule_conditions_ui(ui, &mut rule.when);
                                ui.label("Then:");
                                rule_actions_ui(ui, &mut rule.run);
                                if ui.small_button("delete rule").clicked() {
                                    remove_idx = Some(i);
                                }
                            });
                    });
                }
                if let Some(i) = remove_idx {
                    rc.rules.remove(i);
                }
            });
        // The bespoke widgets mutate `RulesComp` directly; snapshot-compare
        // gives the same undo coverage as the generated inspectors.
        let after = self.snapshot_component(e, "Rules");
        if before.as_deref() != after.as_deref() {
            self.push_component_cmd(e, "Rules", before, after, "Edit");
        }
    }

    // -----------------------------------------------------------------------
    // Viewport (center): toolbar, overlays, gizmos, picking, shortcuts
    // -----------------------------------------------------------------------

    pub(crate) fn viewport_panel(&mut self, ui: &mut egui::Ui, _ctx: &egui::Context) {
        let ppp = ui.ctx().pixels_per_point();
        let playing = self.play_state != crate::PlayState::Stopped;
        let dimension = self.engine.scene.dimension;

        if !playing {
            self.viewport_toolbar(ui);
            self.viewport_shortcuts(ui.ctx());
        } else {
            self.play_controls(ui);
        }

        // Compute the viewport rect (below the toolbar if any) in physical
        // pixels — this is what the GPU scissor + overlays use.
        let rect = ui.available_rect_before_wrap();
        let x = (rect.min.x * ppp) as u32;
        let y = (rect.min.y * ppp) as u32;
        let w = (rect.width() * ppp) as u32;
        let h = (rect.height() * ppp) as u32;
        self.state.viewport_px = [x, y, w.max(1), h.max(1)];

        // Wheel zoom only when the pointer is over the viewport.
        if !playing && ui.rect_contains_pointer(rect) {
            let scroll = ui.ctx().input(|i| i.smooth_scroll_delta.y);
            if scroll.abs() > 1e-3 {
                self.editor_cam.zoom(scroll, dimension);
            }
        }

        // The overlay (grid + selection + gizmo) is drawn via egui's Painter,
        // projecting 3D points with the same view_proj the GPU uses.
        let painter = ui.painter().clone();
        if !playing {
            self.draw_overlay(ui, &painter, ppp);
            self.viewport_interaction(ui, rect);
        }
    }

    /// Play-mode control strip (Pause/Step/Restart/Stop + maximize toggle).
    fn play_controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            match self.play_state {
                crate::PlayState::Playing => {
                    if ui.button("⏸ Pause (F6)").clicked() {
                        self.pause_play();
                    }
                }
                crate::PlayState::Paused => {
                    if ui.button("▶ Resume (F6)").clicked() {
                        self.resume_play();
                    }
                    if ui.button("⏭ Step (F7)").clicked() {
                        self.step_frame();
                    }
                }
                crate::PlayState::Stopped => {}
            }
            if ui.button("↺ Restart (F8)").clicked() {
                self.restart_play();
            }
            if ui.button("⏹ Stop (F5)").clicked() {
                self.stop_play();
            }
            ui.separator();
            ui.checkbox(&mut self.state.maximize_on_play, "maximize on play")
                .on_hover_text("hide editor panels while playing (game view fills the window)");
            ui.separator();
            let state = match self.play_state {
                crate::PlayState::Playing => "PLAYING",
                crate::PlayState::Paused => "PAUSED",
                crate::PlayState::Stopped => "STOPPED",
            };
            ui.weak(format!(
                "game view · {state} · ESC-free camera, input goes to the game"
            ));
        });
    }

    fn viewport_toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            for tool in [
                Tool::Hand,
                Tool::Move,
                Tool::Rotate,
                Tool::Scale,
                Tool::Rect,
                Tool::Transform,
            ] {
                let selected = self.state.tool == tool;
                let key = match tool {
                    Tool::Hand => "Q",
                    Tool::Move => "W",
                    Tool::Rotate => "E",
                    Tool::Scale => "R",
                    Tool::Rect => "T",
                    Tool::Transform => "Y",
                };
                let btn = ui.selectable_label(selected, tool.label());
                if btn.clicked() {
                    self.state.tool = tool;
                }
                btn.on_hover_text(format!("shortcut: {key}"));
            }
            ui.separator();
            let dim3 = matches!(self.engine.scene.dimension, spark::scene::Dimension::D3);
            if dim3 {
                let local = self.state.local_space;
                if ui
                    .selectable_label(!local, "Global")
                    .on_hover_text("gizmo axes in world space")
                    .clicked()
                {
                    self.state.local_space = false;
                }
                if ui
                    .selectable_label(local, "Local")
                    .on_hover_text("gizmo axes follow the entity's rotation")
                    .clicked()
                {
                    self.state.local_space = true;
                }
                ui.separator();
            }
            let snap = &mut self.state.snap;
            ui.checkbox(&mut snap.enabled, "Snap").on_hover_text(
                "quantize drags: translate/rect to the grid step, rotate to the angle step, scale to the factor step",
            );
            if snap.enabled {
                ui.add(egui::DragValue::new(&mut snap.translate).range(0.01..=10.0).speed(0.05))
                    .on_hover_text("grid step (world units)");
                ui.add(egui::DragValue::new(&mut snap.rotate_deg).range(1.0..=90.0).speed(1.0))
                    .on_hover_text("rotate step (degrees)");
                ui.add(egui::DragValue::new(&mut snap.scale).range(0.01..=1.0).speed(0.01))
                    .on_hover_text("scale step");
            }
            ui.separator();
            ui.weak("RMB orbit · MMB pan · Wheel zoom · F focus · Home frame all");
        });
    }

    /// Keyboard shortcuts (egui-level so text fields consume keys first).
    fn viewport_shortcuts(&mut self, ctx: &egui::Context) {
        use egui::Key;
        if ctx.wants_keyboard_input() {
            return;
        }
        let input = ctx.input(|i| {
            (
                i.key_pressed(Key::Q),
                i.key_pressed(Key::W),
                i.key_pressed(Key::E),
                i.key_pressed(Key::R),
                i.key_pressed(Key::T),
                i.key_pressed(Key::Y),
                i.key_pressed(Key::F),
                i.key_pressed(Key::Home),
                i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace),
                i.modifiers.command && i.key_pressed(Key::S),
                i.modifiers.command && i.key_pressed(Key::D),
                i.modifiers.command && !i.modifiers.shift && i.key_pressed(Key::Z),
                i.modifiers.command && i.key_pressed(Key::Y),
                i.modifiers.command && i.modifiers.shift && i.key_pressed(Key::Z),
            )
        });
        let (q, w, e, r, t, y, focus, frame_all, del, save, dupe, undo, redo, redo_alt) = input;
        if q {
            self.state.tool = Tool::Hand;
        }
        if w {
            self.state.tool = Tool::Move;
        }
        if e {
            self.state.tool = Tool::Rotate;
        }
        if r {
            self.state.tool = Tool::Scale;
        }
        if t {
            self.state.tool = Tool::Rect;
        }
        if y {
            self.state.tool = Tool::Transform;
        }
        if focus {
            self.focus_selection();
        }
        if frame_all {
            self.frame_all();
        }
        if del {
            self.despawn_selected();
        }
        if save {
            self.save_scene();
        }
        if dupe {
            self.duplicate_selected();
        }
        if undo {
            self.apply_undo();
        }
        if redo || redo_alt {
            self.apply_redo();
        }
    }

    /// Move the camera to the selection's world position (F).
    pub fn focus_selection(&mut self) {
        let Some(primary) = self.state.primary() else {
            return;
        };
        let world = &self.engine.scene.world;
        if !world.contains(primary) {
            return;
        }
        let pos = ecs::world_transform(world, primary).position;
        self.editor_cam.focus_on(pos);
    }

    /// Frame every entity (Home).
    pub fn frame_all(&mut self) {
        let world = &self.engine.scene.world;
        let mut center = Vec3::ZERO;
        let mut count = 0usize;
        let mut radius = 1.0f32;
        for (_, t) in world.query::<&Transform>().iter() {
            center += t.position;
            count += 1;
        }
        if count == 0 {
            self.editor_cam
                .frame(Vec3::ZERO, 1.0, self.engine.scene.dimension);
            return;
        }
        center /= count as f32;
        for (_, t) in world.query::<&Transform>().iter() {
            radius = radius.max((t.position - center).length());
        }
        self.editor_cam
            .frame(center, radius, self.engine.scene.dimension);
    }

    // -----------------------------------------------------------------------
    // Viewport overlay drawing
    // -----------------------------------------------------------------------

    fn draw_overlay(&mut self, ui: &mut egui::Ui, painter: &egui::Painter, ppp: f32) {
        let dimension = self.engine.scene.dimension;
        let viewport = self.state.viewport_rect_px();
        let aspect = viewport.2 as f32 / viewport.3.max(1) as f32;
        let vp = gizmo::view_proj(&self.editor_cam, dimension, aspect);

        // Grid + axes.
        match dimension {
            spark::scene::Dimension::D3 => {
                gizmo::draw_grid_and_axes(painter, vp, self.state.viewport_px, ppp);
            }
            spark::scene::Dimension::D2 => {
                gizmo::draw_grid_2d(painter, &self.editor_cam, vp, self.state.viewport_px, ppp);
            }
        }

        // Selection outlines.
        gizmo::draw_selection(
            painter,
            &self.engine.scene.world,
            &self.state.selected,
            vp,
            dimension,
            self.state.viewport_px,
            ppp,
        );

        // Gizmo for the current tool at the selection centroid.
        self.state.hovered = None;
        let tool = self.state.tool;
        if tool == Tool::Hand {
            return;
        }
        let selected: Vec<hecs::Entity> = self
            .state
            .selected
            .iter()
            .copied()
            .filter(|e| self.engine.scene.world.contains(*e))
            .collect();
        let Some(primary) = selected.last().copied() else {
            return;
        };
        let world = &self.engine.scene.world;
        let primary_wt = ecs::world_transform(world, primary);
        let origin = if selected.len() == 1 {
            primary_wt.position
        } else {
            let mut c = Vec3::ZERO;
            for e in &selected {
                c += ecs::world_transform(world, *e).position;
            }
            c / selected.len() as f32
        };
        let mouse = ui
            .input(|i| i.pointer.hover_pos())
            .unwrap_or(egui::pos2(-9999.0, -9999.0));
        let len = gizmo::gizmo_scale(&self.editor_cam, origin, dimension, viewport.3 as f32 / ppp);

        // Rect tool: sprite world rect.
        if tool == Tool::Rect {
            if let Ok(sp) = world.get::<&Sprite>(primary) {
                let half = Vec2::new(
                    sp.size.x * primary_wt.scale.x * 0.5,
                    sp.size.y * primary_wt.scale.y * 0.5,
                );
                let mw =
                    gizmo::mouse_world_2d(&self.editor_cam, mouse, self.state.viewport_px, ppp);
                self.state.hovered = gizmo::draw_rect_gizmo(
                    painter,
                    vp,
                    primary_wt.position,
                    half,
                    primary_wt.rotation.z,
                    self.state.viewport_px,
                    ppp,
                    mouse,
                    mw,
                );
            }
            return;
        }

        let (translate, rotate, scale) = gizmo::tool_parts(tool);
        let basis = gizmo::gizmo_basis(self.state.local_space, &primary_wt);
        match dimension {
            spark::scene::Dimension::D2 => {
                if translate {
                    self.state.hovered = gizmo::draw_2d_move_gizmo(
                        painter,
                        vp,
                        origin,
                        len,
                        self.state.viewport_px,
                        ppp,
                        mouse,
                    );
                }
                if rotate {
                    self.state.hovered = gizmo::draw_rotate_gizmo(
                        painter,
                        vp,
                        origin,
                        basis,
                        len,
                        self.state.viewport_px,
                        ppp,
                        mouse,
                    );
                }
                if scale {
                    self.state.hovered = gizmo::draw_scale_gizmo(
                        painter,
                        vp,
                        origin,
                        basis,
                        len,
                        self.state.viewport_px,
                        ppp,
                        mouse,
                    );
                }
            }
            spark::scene::Dimension::D3 => {
                if translate {
                    self.state.hovered = gizmo::draw_translate_gizmo(
                        painter,
                        vp,
                        origin,
                        basis,
                        len,
                        self.state.viewport_px,
                        ppp,
                        mouse,
                        true,
                    );
                }
                if rotate {
                    self.state.hovered = gizmo::draw_rotate_gizmo(
                        painter,
                        vp,
                        origin,
                        basis,
                        len,
                        self.state.viewport_px,
                        ppp,
                        mouse,
                    );
                }
                if scale {
                    self.state.hovered = gizmo::draw_scale_gizmo(
                        painter,
                        vp,
                        origin,
                        basis,
                        len,
                        self.state.viewport_px,
                        ppp,
                        mouse,
                    );
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Viewport interaction (mouse)
    // -----------------------------------------------------------------------

    fn viewport_interaction(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        // Only react to the pointer inside the scene rect (below the
        // toolbar) — clicks on toolbar buttons must not pick entities.
        if !ui.rect_contains_pointer(rect) {
            return;
        }
        let dimension = self.engine.scene.dimension;
        let viewport = self.state.viewport_rect_px();
        let aspect = viewport.2 as f32 / viewport.3.max(1) as f32;
        let ppp = ui.ctx().pixels_per_point();

        let (primary, middle, secondary, delta, pressed, released, mouse, ctrl) =
            ui.ctx().input(|i| {
                (
                    i.pointer.primary_down(),
                    i.pointer.middle_down(),
                    i.pointer.secondary_down(),
                    i.pointer.delta(),
                    i.pointer.button_pressed(egui::PointerButton::Primary),
                    i.pointer.button_released(egui::PointerButton::Primary),
                    i.pointer.hover_pos(),
                    i.modifiers.ctrl,
                )
            });
        let Some(mouse_pos) = mouse else { return };

        // A drag in progress takes priority; it ends on release.
        if self.state.drag.is_some() {
            if primary {
                self.update_drag(mouse_pos, ppp);
            }
            if released && !primary {
                self.commit_drag();
            }
            return;
        }

        // Camera navigation.
        if secondary && dimension == spark::scene::Dimension::D3 {
            self.editor_cam.look(Vec2::new(delta.x, delta.y));
            return;
        }
        if middle {
            self.editor_cam.pan(Vec2::new(delta.x, delta.y), dimension);
            return;
        }
        if self.state.tool == Tool::Hand && primary {
            self.editor_cam.pan(Vec2::new(delta.x, delta.y), dimension);
            return;
        }

        if pressed {
            // Gizmo part under the pointer (set while drawing this frame)?
            if let Some(hit) = self.state.hovered.take() {
                self.start_drag(hit, mouse_pos, ppp);
                return;
            }
            // Otherwise: entity picking via ray vs world AABBs.
            let (origin, dir) = gizmo::pick_ray(
                &self.editor_cam,
                dimension,
                aspect,
                mouse_pos,
                self.state.viewport_px,
                ppp,
            );
            if let Some((entity, _t)) = gizmo::pick_entity(&self.engine.scene.world, origin, dir) {
                if ctrl {
                    self.state.toggle_select(entity);
                } else {
                    self.state.select(entity);
                }
            } else if !ctrl {
                self.state.selected.clear();
            }
        }
    }

    fn start_drag(&mut self, hit: GizmoHit, mouse: egui::Pos2, ppp: f32) {
        let world = &self.engine.scene.world;
        let selected: Vec<hecs::Entity> = self
            .state
            .selected
            .iter()
            .copied()
            .filter(|e| world.contains(*e))
            .collect();
        let Some(primary) = selected.last().copied() else {
            return;
        };
        let dimension = self.engine.scene.dimension;
        let viewport = self.state.viewport_rect_px();
        let aspect = viewport.2 as f32 / viewport.3.max(1) as f32;

        let drag_kind = match hit {
            GizmoHit::Axis(i) => GizmoDrag::TranslateAxis { axis: i },
            GizmoHit::Plane(p) => GizmoDrag::TranslatePlane { plane: p },
            GizmoHit::Screen | GizmoHit::RectInside => GizmoDrag::TranslateScreen,
            GizmoHit::Ring(i) => GizmoDrag::RotateAxis { axis: i },
            GizmoHit::ScaleBox(i) => GizmoDrag::ScaleAxis { axis: i },
            GizmoHit::ScaleUniform => GizmoDrag::ScaleUniform,
            GizmoHit::RectCorner(i) => GizmoDrag::RectCorner { corner: i },
        };

        let primary_wt = ecs::world_transform(world, primary);
        let basis = gizmo::gizmo_basis(self.state.local_space, &primary_wt);
        let origin = if selected.len() == 1 {
            primary_wt.position
        } else {
            let mut c = Vec3::ZERO;
            for e in &selected {
                c += ecs::world_transform(world, *e).position;
            }
            c / selected.len() as f32
        };

        // Ray at the press position.
        let (ray_o, ray_d) = gizmo::pick_ray(
            &self.editor_cam,
            dimension,
            aspect,
            mouse,
            self.state.viewport_px,
            ppp,
        );

        // Axis / plane normal for the drag.
        let axis_dir = match drag_kind {
            GizmoDrag::TranslateAxis { axis } => basis[axis],
            GizmoDrag::TranslatePlane { plane } => match plane {
                0 => basis[2], // XY plane, normal Z
                1 => basis[1], // XZ plane, normal Y
                _ => basis[0], // YZ plane, normal X
            },
            GizmoDrag::RotateAxis { axis } | GizmoDrag::ScaleAxis { axis } => basis[axis],
            _ => Vec3::ZERO,
        };

        // Start hit / angle / t / px distance, depending on the kind.
        let (start_hit, start_angle, start_t, start_px_dist, rect_anchor) = match drag_kind {
            GizmoDrag::TranslateAxis { .. } => {
                let h = gizmo::ray_line_closest(ray_o, ray_d, origin, axis_dir);
                (h, 0.0, 0.0, 0.0, Vec3::ZERO)
            }
            GizmoDrag::TranslatePlane { .. } => {
                let h = gizmo::ray_plane(ray_o, ray_d, origin, axis_dir);
                (h, 0.0, 0.0, 0.0, Vec3::ZERO)
            }
            GizmoDrag::TranslateScreen => {
                let h = match dimension {
                    spark::scene::Dimension::D2 => Vec3::new(ray_o.x, ray_o.y, origin.z),
                    spark::scene::Dimension::D3 => {
                        let n = self.editor_cam.forward() * -1.0;
                        gizmo::ray_plane(ray_o, ray_d, origin, n)
                    }
                };
                (h, 0.0, 0.0, 0.0, Vec3::ZERO)
            }
            GizmoDrag::RotateAxis { .. } => {
                let u = if axis_dir.x.abs() < 0.9 {
                    Vec3::X
                } else {
                    Vec3::Y
                };
                let u = (u - axis_dir * u.dot(axis_dir)).normalize_or_zero();
                let w = axis_dir.cross(u);
                let hit = gizmo::ray_plane(ray_o, ray_d, origin, axis_dir);
                let angle = gizmo::angle_in_plane(hit - origin, u, w);
                (hit, angle, 0.0, 0.0, Vec3::ZERO)
            }
            GizmoDrag::ScaleAxis { .. } => {
                let h = gizmo::ray_line_closest(ray_o, ray_d, origin, axis_dir);
                (h, 0.0, (h - origin).dot(axis_dir), 0.0, Vec3::ZERO)
            }
            GizmoDrag::ScaleUniform => {
                let vp = gizmo::view_proj(&self.editor_cam, dimension, aspect);
                let c = gizmo::project(vp, origin, self.state.viewport_px, ppp).unwrap_or(mouse);
                (
                    Vec3::ZERO,
                    0.0,
                    0.0,
                    (mouse - c).length().max(1.0),
                    Vec3::ZERO,
                )
            }
            GizmoDrag::RectCorner { corner } => {
                // Anchor = the opposite corner of the sprite's world rect.
                let Ok(sp) = world.get::<&Sprite>(primary) else {
                    return;
                };
                let half = Vec2::new(
                    sp.size.x * primary_wt.scale.x * 0.5,
                    sp.size.y * primary_wt.scale.y * 0.5,
                );
                let q = Quat::from_rotation_z(primary_wt.rotation.z.to_radians());
                let corners = [
                    Vec2::new(-half.x, -half.y),
                    Vec2::new(half.x, -half.y),
                    Vec2::new(half.x, half.y),
                    Vec2::new(-half.x, half.y),
                ];
                let anchor = primary_wt.position
                    + q * Vec3::new(
                        corners[(corner + 2) % 4].x,
                        corners[(corner + 2) % 4].y,
                        0.0,
                    );
                (ray_o, 0.0, 0.0, 0.0, anchor)
            }
        };

        let rect_sprite_before = match drag_kind {
            GizmoDrag::RectCorner { .. } => self.snapshot_component(primary, "Sprite"),
            _ => None,
        };

        let entities: Vec<(hecs::Entity, Transform, Transform)> = selected
            .iter()
            .map(|&e| {
                let local = world.get::<&Transform>(e).map(|t| *t).unwrap_or_default();
                let wt = ecs::world_transform(world, e);
                (e, local, wt)
            })
            .collect();

        self.state.drag = Some(DragState {
            drag: drag_kind,
            start_mouse: mouse,
            start_world: origin,
            start_hit,
            axis_dir,
            start_angle,
            start_t,
            start_px_dist,
            rect_anchor,
            rect_sprite_before,
            entities,
        });
    }

    /// Apply the ongoing drag to the selection (per frame while held).
    fn update_drag(&mut self, mouse: egui::Pos2, ppp: f32) {
        let Some(drag) = self.state.drag.take() else {
            return;
        };
        let dimension = self.engine.scene.dimension;
        let viewport = self.state.viewport_rect_px();
        let aspect = viewport.2 as f32 / viewport.3.max(1) as f32;
        let snap = self.state.snap;

        match drag.drag {
            GizmoDrag::TranslateAxis { .. }
            | GizmoDrag::TranslatePlane { .. }
            | GizmoDrag::TranslateScreen => {
                let now_hit = gizmo::translate_now(
                    &self.editor_cam,
                    dimension,
                    &drag,
                    mouse,
                    self.state.viewport_px,
                    ppp,
                    aspect,
                );
                let delta = now_hit - drag.start_hit;
                let world = &mut self.engine.scene.world;
                for (e, _, start_wt) in &drag.entities {
                    let mut wt = *start_wt;
                    wt.position = gizmo::snap_translate(&snap, start_wt.position + delta);
                    ecs::set_world_transform(world, *e, wt);
                }
            }
            GizmoDrag::RotateAxis { .. } => {
                let now_deg = gizmo::rotate_now_deg(
                    &self.editor_cam,
                    dimension,
                    &drag,
                    mouse,
                    self.state.viewport_px,
                    ppp,
                    aspect,
                );
                let mut delta = drag.rotation_deg(now_deg);
                if snap.enabled {
                    delta = gizmo::snap_f32(&snap, delta, snap.rotate_deg);
                }
                let q = Quat::from_axis_angle(drag.axis_dir, delta.to_radians());
                let world = &mut self.engine.scene.world;
                for (e, _, start_wt) in &drag.entities {
                    let mut wt = *start_wt;
                    let eu = (q * start_wt.quat()).to_euler(EulerRot::XYZ);
                    wt.rotation =
                        Vec3::new(eu.0.to_degrees(), eu.1.to_degrees(), eu.2.to_degrees());
                    ecs::set_world_transform(world, *e, wt);
                }
            }
            GizmoDrag::ScaleAxis { axis } => {
                let vp = gizmo::view_proj(&self.editor_cam, dimension, aspect);
                let center = gizmo::project(vp, drag.start_world, self.state.viewport_px, ppp)
                    .unwrap_or(mouse);
                let f = gizmo::scale_factor_now(
                    &self.editor_cam,
                    &drag,
                    mouse,
                    self.state.viewport_px,
                    ppp,
                    aspect,
                    center,
                );
                let world = &mut self.engine.scene.world;
                for (e, _, start_wt) in &drag.entities {
                    let mut wt = *start_wt;
                    let s = start_wt.scale;
                    wt.scale = match axis {
                        0 => Vec3::new(gizmo::snap_scale_val(&snap, s.x * f), s.y, s.z),
                        1 => Vec3::new(s.x, gizmo::snap_scale_val(&snap, s.y * f), s.z),
                        _ => Vec3::new(s.x, s.y, gizmo::snap_scale_val(&snap, s.z * f)),
                    };
                    ecs::set_world_transform(world, *e, wt);
                }
            }
            GizmoDrag::ScaleUniform => {
                let vp = gizmo::view_proj(&self.editor_cam, dimension, aspect);
                let center = gizmo::project(vp, drag.start_world, self.state.viewport_px, ppp)
                    .unwrap_or(mouse);
                let f = gizmo::scale_factor_now(
                    &self.editor_cam,
                    &drag,
                    mouse,
                    self.state.viewport_px,
                    ppp,
                    aspect,
                    center,
                );
                let world = &mut self.engine.scene.world;
                for (e, _, start_wt) in &drag.entities {
                    let mut wt = *start_wt;
                    let s = start_wt.scale;
                    wt.scale = Vec3::new(
                        gizmo::snap_scale_val(&snap, s.x * f),
                        gizmo::snap_scale_val(&snap, s.y * f),
                        gizmo::snap_scale_val(&snap, s.z * f),
                    );
                    ecs::set_world_transform(world, *e, wt);
                }
            }
            GizmoDrag::RectCorner { .. } => {
                // Resize the primary entity's sprite, keeping the opposite
                // corner anchored.
                let Some((e, _, start_wt)) = drag.entities.last() else {
                    self.state.drag = Some(drag);
                    return;
                };
                let mouse_w =
                    gizmo::mouse_world_2d(&self.editor_cam, mouse, self.state.viewport_px, ppp);
                let anchor = drag.rect_anchor;
                let q = Quat::from_rotation_z(start_wt.rotation.z.to_radians());
                let inv_q = q.conjugate();
                let diag_world = Vec3::new(mouse_w.x - anchor.x, mouse_w.y - anchor.y, 0.0);
                let diag_local = inv_q * diag_world;
                let snap_size = |v: f32| {
                    let s = gizmo::snap_f32(&snap, v, snap.translate);
                    if s < 0.05 { 0.05 } else { s }
                };
                let world = &mut self.engine.scene.world;
                if let Ok(mut sp) = world.get::<&mut Sprite>(*e) {
                    sp.size.x = snap_size(diag_local.x.abs() / start_wt.scale.x.max(1e-4));
                    sp.size.y = snap_size(diag_local.y.abs() / start_wt.scale.y.max(1e-4));
                }
                // Keep the center between anchor and mouse.
                let center = Vec3::new(
                    (anchor.x + mouse_w.x) * 0.5,
                    (anchor.y + mouse_w.y) * 0.5,
                    start_wt.position.z,
                );
                let mut wt = *start_wt;
                wt.position = center;
                ecs::set_world_transform(world, *e, wt);
            }
        }
        self.state.drag = Some(drag);
    }

    /// End the drag: record undo commands for everything it changed.
    fn commit_drag(&mut self) {
        let Some(drag) = self.state.drag.take() else {
            return;
        };
        // Collect undo data while the world is immutably borrowed.
        let mut transform_cmds = Vec::new();
        let mut sprite_cmd = None;
        {
            let world = &self.engine.scene.world;
            for (e, local_before, _) in &drag.entities {
                if !world.contains(*e) {
                    continue;
                }
                if let Ok(after) = world.get::<&Transform>(*e).map(|t| *t) {
                    let before_text = ron::to_string(local_before).ok();
                    let after_text = ron::to_string(&after).ok();
                    if before_text != after_text {
                        transform_cmds.push((*e, before_text, after_text));
                    }
                }
            }
            // Rect resize also touched Sprite on the primary entity.
            if matches!(drag.drag, GizmoDrag::RectCorner { .. })
                && let Some((e, _, _)) = drag.entities.last()
                && world.contains(*e)
                && let Some(before) = drag.rect_sprite_before.clone()
                && let Some(entry) = self.engine.registry.get("Sprite")
                && let after = (entry.save)(world, *e)
                && after.as_deref() != Some(before.as_str())
            {
                sprite_cmd = Some((*e, before, after));
            }
        }
        for (e, before, after) in transform_cmds {
            self.push_component_cmd(e, "Transform", before, after, "Gizmo");
        }
        if let Some((e, before, after)) = sprite_cmd {
            self.push_component_cmd(e, "Sprite", Some(before), after, "Resize");
        }
        self.engine.physics.request_rebuild();
    }

    // -----------------------------------------------------------------------
    // Bottom: asset browser + console
    // -----------------------------------------------------------------------

    pub(crate) fn bottom_panel(&mut self, ui: &mut egui::Ui) {
        egui::TopBottomPanel::bottom("stats").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.weak(format!(
                    "entities: {}  |  fps: {:.0}  |  {}",
                    self.engine.scene.world.iter().count(),
                    self.engine.stats.fps,
                    self.project_dir
                        .as_ref()
                        .map(|d| d.display().to_string())
                        .unwrap_or_else(|| "no project".into())
                ));
            });
        });
        egui::SidePanel::left("assets")
            .resizable(true)
            .default_width(280.0)
            .show_inside(ui, |ui| {
                self.asset_browser(ui);
            });
        egui::CentralPanel::default().show_inside(ui, |ui| {
            self.console_panel(ui);
        });
    }

    fn asset_browser(&mut self, ui: &mut egui::Ui) {
        ui.heading("Assets");
        ui.separator();
        let kinds: Vec<(&str, AssetKind)> = vec![
            ("Textures", AssetKind::Texture),
            ("Models", AssetKind::Model),
            ("Sounds", AssetKind::Sound),
            ("Scenes", AssetKind::Scene),
            ("Prefabs", AssetKind::Prefab),
        ];
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (title, kind) in kinds {
                let list = self.engine.assets.list(kind);
                if list.is_empty() {
                    continue;
                }
                egui::CollapsingHeader::new(format!("{title} ({})", list.len()))
                    .default_open(true)
                    .show(ui, |ui| {
                        for path in list {
                            ui.horizontal(|ui| {
                                let selected =
                                    self.selected_asset.as_deref() == Some(path.as_str());
                                if ui.selectable_label(selected, &path).clicked() {
                                    self.selected_asset = Some(path.clone());
                                }
                                // Sound preview: play it through the engine's
                                // audio output (real playback when a device
                                // exists; a one-time warning otherwise).
                                if kind == AssetKind::Sound {
                                    let p = path.clone();
                                    if ui
                                        .small_button("\u{25b6}")
                                        .on_hover_text("preview sound")
                                        .clicked()
                                    {
                                        if let Some(bytes) =
                                            self.engine.assets.sound(&p).map(|b| b.to_vec())
                                        {
                                            let name = p.clone();
                                            self.engine.audio.play_bytes(&bytes, 0.8);
                                            self.log("info", &format!("previewing {name}"));
                                        } else {
                                            self.log("warn", &format!("cannot read {p}"));
                                        }
                                    }
                                }
                            });
                            if self.state.primary().is_some() {
                                let p = path.clone();
                                ui.menu_button("→ assign", |ui| {
                                    if ui.button("as Sprite image").clicked() {
                                        self.assign_asset(&p, "Sprite");
                                        ui.close();
                                    }
                                    if ui.button("as Mesh").clicked() {
                                        self.assign_asset(&p, "MeshRenderer");
                                        ui.close();
                                    }
                                    if ui.button("as Albedo texture").clicked() {
                                        self.assign_asset(&p, "Material");
                                        ui.close();
                                    }
                                    if ui.button("as Music").clicked() {
                                        self.assign_asset(&p, "Music");
                                        ui.close();
                                    }
                                });
                            }
                        }
                    });
            }
            if self.engine.assets.list(AssetKind::Texture).is_empty()
                && self.engine.assets.list(AssetKind::Model).is_empty()
            {
                ui.weak("drop files into the project's assets/ folder");
            }
        });
    }

    /// Assign an asset path to a component on the primary selection,
    /// undoably.
    pub fn assign_asset(&mut self, path: &str, target: &str) {
        let Some(e) = self.state.primary() else {
            return;
        };
        if !self.engine.scene.world.contains(e) {
            return;
        }
        // "Material" edits the material nested inside MeshRenderer.
        let comp: &'static str = match target {
            "Sprite" => "Sprite",
            "MeshRenderer" | "Material" => "MeshRenderer",
            "Music" => "Music",
            _ => return,
        };
        let before = self.snapshot_component(e, comp);
        let world = &mut self.engine.scene.world;
        match target {
            "Sprite" => {
                let _ = world.insert_one(e, Transform::default());
                let _ = world.insert_one(
                    e,
                    Sprite {
                        image: path.to_string(),
                        ..Default::default()
                    },
                );
            }
            "MeshRenderer" => {
                let _ = world.insert_one(e, Transform::default());
                let _ = world.insert_one(
                    e,
                    MeshRenderer {
                        mesh: path.to_string(),
                        ..Default::default()
                    },
                );
            }
            "Material" => {
                if let Ok(mut mr) = world.get::<&mut MeshRenderer>(e) {
                    mr.material.texture = Some(path.to_string());
                } else {
                    let _ = world.insert_one(e, Transform::default());
                    let _ = world.insert_one(
                        e,
                        MeshRenderer {
                            mesh: "cube".into(),
                            material: Material {
                                texture: Some(path.to_string()),
                                ..Default::default()
                            },
                        },
                    );
                }
            }
            "Music" => {
                let _ = world.insert_one(
                    e,
                    Music {
                        track: path.to_string(),
                        ..Default::default()
                    },
                );
            }
            _ => {}
        }
        let after = self.snapshot_component(e, comp);
        if before != after {
            self.push_component_cmd(e, comp, before, after, "Assign");
        }
        self.log("info", &format!("assigned {path} → {target}"));
    }

    fn console_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Console");
        ui.separator();
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for (level, msg) in &self.console {
                    let color = match level.as_str() {
                        "error" => egui::Color32::RED,
                        "warn" => egui::Color32::YELLOW,
                        _ => egui::Color32::GRAY,
                    };
                    ui.colored_label(color, format!("[{level}] {msg}"));
                }
            });
    }

    // -----------------------------------------------------------------------
    // Modals
    // -----------------------------------------------------------------------

    pub(crate) fn modals(&mut self, ctx: &egui::Context) {
        if self.state.show_new_project {
            let mut open = true;
            egui::Window::new("New Project")
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.label("Project name:");
                    ui.text_edit_singleline(&mut self.state.new_project_name);
                    ui.label("Dimension:");
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut self.state.new_project_dim, Dimension::D2, "2D");
                        ui.selectable_value(&mut self.state.new_project_dim, Dimension::D3, "3D");
                    });
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("Create").clicked() {
                            let (n, d) = (
                                self.state.new_project_name.clone(),
                                self.state.new_project_dim,
                            );
                            self.new_project(&n, d);
                            self.state.show_new_project = false;
                        }
                        if ui.button("Cancel").clicked() {
                            self.state.show_new_project = false;
                        }
                    });
                });
            self.state.show_new_project &= open;
        }
        if self.state.show_open_project {
            let mut open = true;
            egui::Window::new("Open Project")
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.label("Path to project directory (contains project.ron):");
                    ui.text_edit_singleline(&mut self.state.open_path);
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("Open").clicked() {
                            let p = std::path::PathBuf::from(&self.state.open_path);
                            self.open_project(&p);
                            self.state.show_open_project = false;
                        }
                        if ui.button("Cancel").clicked() {
                            self.state.show_open_project = false;
                        }
                    });
                });
            self.state.show_open_project &= open;
        }
    }
}

// ---------------------------------------------------------------------------
// Rule editing widgets (free functions to keep borrows simple)
// ---------------------------------------------------------------------------

fn rule_event_ui(ui: &mut egui::Ui, ev: &mut RuleEvent) {
    egui::Grid::new("rule_event").num_columns(2).show(ui, |ui| {
        ui.strong("On");
        egui::ComboBox::from_id_salt("event")
            .selected_text(ev.describe())
            .show_ui(ui, |ui| {
                for label in [
                    "Start",
                    "Update",
                    "Timer",
                    "Key pressed",
                    "Key held",
                    "Action",
                    "Collision enter",
                    "Collision exit",
                    "Message",
                    "Clicked",
                ] {
                    if ui.selectable_label(false, label).clicked() {
                        *ev = match label {
                            "Start" => RuleEvent::Start,
                            "Update" => RuleEvent::Update,
                            "Timer" => RuleEvent::Timer {
                                secs: 1.0,
                                repeat: true,
                            },
                            "Key pressed" => RuleEvent::KeyPressed("Space".into()),
                            "Key held" => RuleEvent::KeyHeld("KeyA".into()),
                            "Action" => RuleEvent::ActionPressed("jump".into()),
                            "Collision enter" => RuleEvent::CollisionEnter { other: None },
                            "Collision exit" => RuleEvent::CollisionExit { other: None },
                            "Message" => RuleEvent::Message("msg".into()),
                            _ => RuleEvent::Clicked,
                        };
                    }
                }
            });
        ui.end_row();
        match ev {
            RuleEvent::Timer { secs, repeat } => {
                ui.strong("Every (s)");
                ui.add(egui::DragValue::new(secs).range(0.01..=3600.0).speed(0.1));
                ui.end_row();
                ui.strong("Repeat");
                ui.checkbox(repeat, "");
                ui.end_row();
            }
            RuleEvent::KeyPressed(k) | RuleEvent::KeyHeld(k) | RuleEvent::KeyReleased(k) => {
                ui.strong("Key");
                ui.text_edit_singleline(k);
                ui.end_row();
            }
            RuleEvent::ActionPressed(a) => {
                ui.strong("Action");
                ui.text_edit_singleline(a);
                ui.end_row();
            }
            RuleEvent::CollisionEnter { other } | RuleEvent::CollisionExit { other } => {
                ui.strong("Other tag");
                let mut txt = other.clone().unwrap_or_default();
                if ui.text_edit_singleline(&mut txt).changed() {
                    *other = if txt.is_empty() { None } else { Some(txt) };
                }
                ui.end_row();
            }
            RuleEvent::Message(m) => {
                ui.strong("Message");
                ui.text_edit_singleline(m);
                ui.end_row();
            }
            _ => {}
        }
    });
}

fn rule_conditions_ui(ui: &mut egui::Ui, conds: &mut Vec<Cond>) {
    let mut remove = None;
    for (i, c) in conds.iter_mut().enumerate() {
        ui.push_id(i, |ui| {
            ui.horizontal(|ui| {
                match c {
                    Cond::Once => {
                        ui.label("once");
                    }
                    Cond::KeyHeld(k) => {
                        ui.label("key held");
                        ui.text_edit_singleline(k);
                    }
                    Cond::KeyNotHeld(k) => {
                        ui.label("key not held");
                        ui.text_edit_singleline(k);
                    }
                    Cond::Cooldown(t) => {
                        ui.label("cooldown");
                        ui.add(egui::DragValue::new(t).range(0.01..=60.0).speed(0.1));
                        ui.label("s");
                    }
                    Cond::Var {
                        scope,
                        name,
                        op,
                        value,
                    } => {
                        egui::ComboBox::from_id_salt("scope")
                            .selected_text(if *scope == VarScope::Entity {
                                "entity"
                            } else {
                                "global"
                            })
                            .show_ui(ui, |ui| {
                                if ui
                                    .selectable_label(*scope == VarScope::Entity, "entity")
                                    .clicked()
                                {
                                    *scope = VarScope::Entity;
                                }
                                if ui
                                    .selectable_label(*scope == VarScope::Global, "global")
                                    .clicked()
                                {
                                    *scope = VarScope::Global;
                                }
                            });
                        ui.text_edit_singleline(name).on_hover_text("variable");
                        let op_txt = match op {
                            CmpOp::Lt => "<",
                            CmpOp::Gt => ">",
                            CmpOp::Le => "<=",
                            CmpOp::Ge => ">=",
                            CmpOp::Eq => "==",
                            CmpOp::Ne => "!=",
                        };
                        egui::ComboBox::from_id_salt("op")
                            .selected_text(op_txt)
                            .show_ui(ui, |ui| {
                                for (txt, val) in [
                                    ("<", CmpOp::Lt),
                                    (">", CmpOp::Gt),
                                    ("<=", CmpOp::Le),
                                    (">=", CmpOp::Ge),
                                    ("==", CmpOp::Eq),
                                    ("!=", CmpOp::Ne),
                                ] {
                                    if ui.selectable_label(*op == val, txt).clicked() {
                                        *op = val;
                                    }
                                }
                            });
                        ui.add(egui::DragValue::new(value).speed(0.1));
                    }
                    Cond::Chance(p) => {
                        ui.label("chance");
                        ui.add(egui::DragValue::new(p).range(0.0..=1.0).speed(0.05));
                    }
                }
                if ui.small_button("×").clicked() {
                    remove = Some(i);
                }
            });
        });
    }
    if let Some(i) = remove {
        conds.remove(i);
    }
    if ui.small_button("+ condition").clicked() {
        conds.push(Cond::Var {
            scope: VarScope::Entity,
            name: "var".into(),
            op: CmpOp::Eq,
            value: 1.0,
        });
    }
}

fn rule_actions_ui(ui: &mut egui::Ui, actions: &mut Vec<Action>) {
    let mut remove = None;
    for (i, a) in actions.iter_mut().enumerate() {
        ui.push_id(i, |ui| {
            ui.horizontal(|ui| {
                match a {
                    Action::Log(msg) => {
                        ui.strong("log");
                        ui.text_edit_singleline(msg);
                    }
                    Action::SetVar { scope, name, value }
                    | Action::AddVar {
                        scope,
                        name,
                        delta: value,
                    } => {
                        egui::ComboBox::from_id_salt("scope")
                            .selected_text(if *scope == VarScope::Entity {
                                "entity"
                            } else {
                                "global"
                            })
                            .show_ui(ui, |ui| {
                                if ui
                                    .selectable_label(*scope == VarScope::Entity, "entity")
                                    .clicked()
                                {
                                    *scope = VarScope::Entity;
                                }
                                if ui
                                    .selectable_label(*scope == VarScope::Global, "global")
                                    .clicked()
                                {
                                    *scope = VarScope::Global;
                                }
                            });
                        ui.text_edit_singleline(name);
                        ui.add(egui::DragValue::new(value).speed(0.1));
                    }
                    Action::Translate { by }
                    | Action::SetVelocity { v: by, .. }
                    | Action::ApplyImpulse { v: by } => {
                        let mut arr = [by.x, by.y, by.z];
                        for v in &mut arr {
                            ui.add(egui::DragValue::new(v).speed(0.1));
                        }
                        *by = Vec3::new(arr[0], arr[1], arr[2]);
                    }
                    Action::PlaySound { sound, volume } => {
                        ui.strong("sfx");
                        ui.text_edit_singleline(sound);
                        ui.add(egui::DragValue::new(volume).range(0.0..=2.0).speed(0.05));
                    }
                    Action::PlayMusic { track, volume } => {
                        ui.strong("music");
                        ui.text_edit_singleline(track);
                        ui.add(egui::DragValue::new(volume).range(0.0..=2.0).speed(0.05));
                    }
                    Action::Spawn { prefab, .. } => {
                        ui.strong("spawn");
                        ui.text_edit_singleline(prefab);
                    }
                    Action::LoadScene(s) => {
                        ui.strong("load");
                        ui.text_edit_singleline(s);
                    }
                    Action::SendMessage(m) => {
                        ui.strong("send");
                        ui.text_edit_singleline(m);
                    }
                    Action::SetColor(c) => {
                        ui.strong("color");
                        let mut col = [c.r, c.g, c.b, c.a];
                        if ui.color_edit_button_rgba_unmultiplied(&mut col).changed() {
                            *c = Color::rgba(col[0], col[1], col[2], col[3]);
                        }
                    }
                    other => {
                        ui.strong(action_label(other));
                    }
                }
                if ui.small_button("×").clicked() {
                    remove = Some(i);
                }
            });
        });
    }
    if let Some(i) = remove {
        actions.remove(i);
    }
    add_action_button(ui, actions);
}

fn add_action_button(ui: &mut egui::Ui, actions: &mut Vec<Action>) {
    egui::ComboBox::from_id_salt("add_action")
        .selected_text("+ action")
        .show_ui(ui, |ui| {
            let catalogue: Vec<(&str, Action)> = vec![
                ("Log", Action::Log("hello".into())),
                ("DestroySelf", Action::DestroySelf),
                ("DestroyOther", Action::DestroyOther),
                (
                    "SetVelocity",
                    Action::SetVelocity {
                        v: Vec3::ZERO,
                        relative: false,
                    },
                ),
                (
                    "ApplyImpulse",
                    Action::ApplyImpulse {
                        v: Vec3::new(0.0, 5.0, 0.0),
                    },
                ),
                ("Translate", Action::Translate { by: Vec3::ZERO }),
                (
                    "SetVar",
                    Action::SetVar {
                        scope: VarScope::Entity,
                        name: "var".into(),
                        value: 0.0,
                    },
                ),
                (
                    "AddVar",
                    Action::AddVar {
                        scope: VarScope::Entity,
                        name: "var".into(),
                        delta: 1.0,
                    },
                ),
                (
                    "PlaySound",
                    Action::PlaySound {
                        sound: "assets/sfx.wav".into(),
                        volume: 0.8,
                    },
                ),
                (
                    "PlayMusic",
                    Action::PlayMusic {
                        track: "assets/music.ogg".into(),
                        volume: 0.6,
                    },
                ),
                ("StopMusic", Action::StopMusic),
                (
                    "SetGravity",
                    Action::SetGravity {
                        g: Vec3::new(0.0, -9.81, 0.0),
                    },
                ),
                ("CameraFollowMe", Action::CameraFollowMe { lerp: 0.1 }),
                ("LoadScene", Action::LoadScene("scenes/main.scene".into())),
                ("SendMessage", Action::SendMessage("msg".into())),
                ("ToggleVisible", Action::ToggleVisible),
                ("SetVisible", Action::SetVisible(true)),
                ("Quit", Action::Quit),
            ];
            for (name, tmpl) in catalogue {
                if ui.selectable_label(false, name).clicked() {
                    actions.push(tmpl);
                }
            }
        });
}

fn action_label(a: &Action) -> String {
    match a {
        Action::DestroySelf => "destroy self".into(),
        Action::DestroyOther => "destroy other".into(),
        Action::Rotate { by_deg } => format!("rotate {:?}", by_deg),
        Action::StopMusic => "stop music".into(),
        Action::SetGravity { g } => format!("gravity {:?}", g),
        Action::CameraFollowMe { lerp } => format!("camera follows ({lerp})"),
        Action::ToggleVisible => "toggle visible".into(),
        Action::SetVisible(v) => format!("visible = {v}"),
        Action::Quit => "quit".into(),
        _ => "…".into(),
    }
}
