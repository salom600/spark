//! Editor panels: hierarchy tree, inspector, viewport, asset browser, console.

use spark::ecs;
use spark::prelude::*;
use spark::reexport::hecs;
use spark::rules::{Action, CmpOp, Cond, Rule, RuleEvent};

use crate::Editor;

impl Editor {
    // -----------------------------------------------------------------------
    // Hierarchy (left)
    // -----------------------------------------------------------------------

    pub(crate) fn hierarchy_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Hierarchy");
        ui.separator();
        if self.playing.is_some() {
            ui.weak("(snapshot — edits disabled while playing)");
        }
        egui::ScrollArea::vertical().show(ui, |ui| {
            let roots = ecs::roots(&self.engine.scene.world);
            let empty = roots.is_empty();
            for root in roots {
                self.entity_row(ui, root, 0);
            }
            if empty {
                ui.weak("(empty scene — Scene → Add Entity)");
            }
        });
    }

    fn entity_row(&mut self, ui: &mut egui::Ui, e: hecs::Entity, depth: usize) {
        // Snapshot what we need before the recursive closure borrows self.
        let (label, children) = {
            let world = &self.engine.scene.world;
            if !world.contains(e) {
                return;
            }
            (ecs::entity_label(world, e), ecs::children(world, e))
        };
        let selected = self.state.selected == Some(e);
        let marker = if selected { "▸ " } else { "  " };
        let id = egui::Id::new(("ent", e.to_bits()));
        let resp = egui::CollapsingHeader::new(format!("{marker}{label}"))
            .id_salt(id)
            .default_open(depth < 1)
            .show(ui, |ui| {
                for child in children {
                    self.entity_row(ui, child, depth + 1);
                }
            });
        if resp.header_response.clicked() {
            self.state.selected = Some(e);
        }
        resp.header_response.context_menu(|ui| {
            self.entity_context_menu(ui, e);
        });
    }

    fn entity_context_menu(&mut self, ui: &mut egui::Ui, e: hecs::Entity) {
        if ui.button("Duplicate").clicked() {
            self.duplicate_entity(e);
            ui.close();
        }
        if ui.button("Delete").clicked() {
            self.state.selected = Some(e);
            self.despawn_selected();
            ui.close();
        }
    }

    // -----------------------------------------------------------------------
    // Inspector (right)
    // -----------------------------------------------------------------------

    pub(crate) fn inspector_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Inspector");
        ui.separator();

        let Some(e) = self.state.selected else {
            ui.weak("Select an entity in the Hierarchy");
            return;
        };
        if !self.engine.scene.world.contains(e) {
            self.state.selected = None;
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
                    let entry = self.engine.registry.get(name).unwrap();
                    (entry.add_default)(&mut self.engine.scene.world, e);
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
    // Rules editor (bespoke UI over Rule lists)
    // -----------------------------------------------------------------------

    fn rules_editor(&mut self, ui: &mut egui::Ui, e: hecs::Entity) {
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
                            });
                    });
                }
            });
    }

    // -----------------------------------------------------------------------
    // Viewport (center): camera controls + 3D gizmo + picking
    // -----------------------------------------------------------------------

    pub(crate) fn viewport_panel(&mut self, ui: &mut egui::Ui, _ctx: &egui::Context) {
        let ppp = ui.ctx().pixels_per_point();

        // ---- Toolbar (Move / Rotate / Scale) — shown only in edit mode + 3D
        if self.playing.is_none()
            && matches!(self.engine.scene.dimension, spark::scene::Dimension::D3)
        {
            ui.horizontal(|ui| {
                use crate::state::GizmoMode;
                let mode = self.state.gizmo_mode;
                let mut mk = |label: &str, m: GizmoMode, ui: &mut egui::Ui| {
                    if ui.selectable_label(mode == m, label).clicked() {
                        self.state.gizmo_mode = m;
                    }
                };
                mk("Move", GizmoMode::Translate, ui);
                mk("Rotate", GizmoMode::Rotate, ui);
                mk("Scale", GizmoMode::Scale, ui);
                ui.separator();
                ui.weak("RMB: orbit  |  MMB: pan  |  Wheel: zoom  |  LMB: pick/drag");
            });
        }

        // Compute the viewport rect (below the toolbar if any) in physical
        // pixels — this is what the GPU scissor + 3D overlay use.
        let rect = ui.available_rect_before_wrap();
        let x = (rect.min.x * ppp) as u32;
        let y = (rect.min.y * ppp) as u32;
        let w = (rect.width() * ppp) as u32;
        let h = (rect.height() * ppp) as u32;
        self.state.viewport_px = [x, y, w.max(1), h.max(1)];

        // The 3D overlay (grid + axes + gizmo) is drawn via egui's Painter,
        // so we don't need a new wgpu pass. We project 3D points to screen
        // pixels using the same view_proj the GPU uses.
        let painter = ui.painter().clone();
        if self.playing.is_none() {
            self.draw_3d_overlay(ui, &painter, ppp);
            self.viewport_interaction(ui);
        }
    }

    fn draw_3d_overlay(&mut self, ui: &mut egui::Ui, painter: &egui::Painter, ppp: f32) {
        let dimension = self.engine.scene.dimension;
        let viewport = self.state.viewport_rect_px();
        let aspect = viewport.2 as f32 / viewport.3.max(1) as f32;
        let vp = crate::gizmo::view_proj(&self.editor_cam, dimension, aspect);

        if dimension == spark::scene::Dimension::D3 {
            crate::gizmo::draw_grid_and_axes(painter, vp, self.state.viewport_px, ppp);
        }

        // Translate gizmo at the selected entity's position.
        let mode = self.state.gizmo_mode;
        if mode == crate::state::GizmoMode::Translate
            && let Some(e) = self.state.selected
            && self.engine.scene.world.contains(e)
            && let Ok(t) = self.engine.scene.world.get::<&Transform>(e)
        {
            let origin = t.position;
            drop(t);
            let mouse = ui
                .input(|i| i.pointer.hover_pos())
                .unwrap_or(egui::pos2(0.0, 0.0));
            let _ = crate::gizmo::draw_translate_gizmo(
                painter,
                vp,
                origin,
                self.state.viewport_px,
                ppp,
                mouse,
            );
        }
    }

    fn viewport_interaction(&mut self, ui: &mut egui::Ui) {
        if !ui.rect_contains_pointer(ui.max_rect()) {
            return;
        }
        let dimension = self.engine.scene.dimension;
        let viewport = self.state.viewport_rect_px();
        let aspect = viewport.2 as f32 / viewport.3.max(1) as f32;

        let (primary, middle, secondary, delta, pressed, released, mouse) = ui.ctx().input(|i| {
            (
                i.pointer.primary_down(),
                i.pointer.middle_down(),
                i.pointer.secondary_down(),
                i.pointer.delta(),
                i.pointer.button_pressed(egui::PointerButton::Primary),
                i.pointer.button_released(egui::PointerButton::Primary),
                i.pointer.hover_pos(),
            )
        });

        // Orbit (RMB drag).
        if secondary {
            self.editor_cam.look(Vec2::new(delta.x, delta.y));
            return;
        }
        // Pan (MMB drag).
        if middle {
            self.editor_cam.pan(Vec2::new(delta.x, delta.y), dimension);
            return;
        }

        // Gizmo drag (LMB): if a drag is in progress, continue it; otherwise
        // check whether the click landed on a gizmo axis.
        let Some(mouse_pos) = mouse else { return };

        if primary && self.state.gizmo_drag_axis.is_some() {
            // Continue dragging.
            self.apply_gizmo_drag(aspect, mouse_pos, ui.ctx().pixels_per_point());
            return;
        }
        if pressed && dimension == spark::scene::Dimension::D3 {
            // Check gizmo axis hit first (only Translate mode + an entity
            // selected). If hit, start a drag.
            if self.state.gizmo_mode == crate::state::GizmoMode::Translate
                && let Some(e) = self.state.selected
                && self.engine.scene.world.contains(e)
            {
                let origin = self
                    .engine
                    .scene
                    .world
                    .get::<&Transform>(e)
                    .map(|t| t.position)
                    .unwrap_or_default();
                let vp = crate::gizmo::view_proj(&self.editor_cam, dimension, aspect);
                if let Some(axis) = crate::gizmo::draw_translate_gizmo(
                    ui.painter(),
                    vp,
                    origin,
                    self.state.viewport_px,
                    ui.ctx().pixels_per_point(),
                    mouse_pos,
                ) {
                    self.state.gizmo_drag_axis = Some(axis);
                    self.state.gizmo_drag_start_mouse = Some(mouse_pos);
                    self.state.gizmo_drag_start_transform = self
                        .engine
                        .scene
                        .world
                        .get::<&Transform>(e)
                        .ok()
                        .map(|t| *t);
                    return;
                }
            }
            // Otherwise: entity picking via ray vs AABB.
            let (origin, dir) = crate::gizmo::pick_ray(
                &self.editor_cam,
                dimension,
                aspect,
                mouse_pos,
                self.state.viewport_px,
                ui.ctx().pixels_per_point(),
            );
            if let Some((entity, _t)) =
                crate::gizmo::pick_entity(&self.engine.scene.world, origin, dir)
            {
                self.state.selected = Some(entity);
            }
        }
        if released && self.state.gizmo_drag_axis.is_some() {
            // Commit the drag: capture undo state.
            self.state.gizmo_drag_axis = None;
            self.state.gizmo_drag_start_mouse = None;
            self.state.gizmo_drag_start_transform = None;
            self.engine.physics.request_rebuild();
        }
    }

    fn apply_gizmo_drag(&mut self, aspect: f32, mouse_pos: egui::Pos2, ppp: f32) {
        let Some(axis) = self.state.gizmo_drag_axis else {
            return;
        };
        let Some(start_mouse) = self.state.gizmo_drag_start_mouse else {
            return;
        };
        let Some(start_t) = self.state.gizmo_drag_start_transform else {
            return;
        };
        let Some(e) = self.state.selected else { return };
        if !self.engine.scene.world.contains(e) {
            return;
        }
        let delta = crate::gizmo::axis_drag_delta(
            &self.editor_cam,
            aspect,
            start_t.position,
            axis,
            mouse_pos,
            self.state.viewport_px,
            ppp,
            start_mouse,
        );
        let _ = self.engine.scene.world.insert_one(
            e,
            Transform {
                position: start_t.position + delta,
                ..start_t
            },
        );
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
                            let selected = self.selected_asset.as_deref() == Some(path.as_str());
                            if ui.selectable_label(selected, &path).clicked() {
                                self.selected_asset = Some(path.clone());
                            }
                            if self.state.selected.is_some() {
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

    fn assign_asset(&mut self, path: &str, target: &str) {
        let Some(e) = self.state.selected else { return };
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
