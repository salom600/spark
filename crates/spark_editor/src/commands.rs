//! Editor commands (undo/redo) + scene mutation helpers.
//!
//! Every user-visible mutation goes through [`CommandStack`] so undo works
//! uniformly. Commands capture the *data* needed to apply/revert, never
//! world references. Entity creation, deletion (subtree-preserving),
//! renaming, reparenting and component edits are all covered — if a user
//! action mutates the scene, it must go through here.

use spark::cmd::{Command, CommandCtx};
use spark::ecs::{self, Registry};
use spark::prelude::*;
use spark::reexport::hecs;
use spark::scene::EntityRecord;

use crate::Editor;

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Snapshot-based command: applies by swapping component data wholesale.
/// Simple, generic and always correct — the editor scale is small.
pub struct SwapCommand {
    label: String,
    entity: hecs::Entity,
    before: Option<String>,
    after: Option<String>,
    component: &'static str,
}

impl Command for SwapCommand {
    fn label(&self) -> String {
        self.label.clone()
    }
    fn apply(&mut self, ctx: &mut CommandCtx) {
        if let Some(text) = &self.after
            && let Some(entry) = ctx.registry.get(self.component)
        {
            let _ = (entry.load)(ctx.world, self.entity, text);
        }
    }
    fn revert(&mut self, ctx: &mut CommandCtx) {
        match &self.before {
            Some(text) => {
                if let Some(entry) = ctx.registry.get(self.component) {
                    let _ = (entry.load)(ctx.world, self.entity, text);
                }
            }
            None => {
                if let Some(entry) = ctx.registry.get(self.component) {
                    (entry.remove)(ctx.world, self.entity);
                }
            }
        }
    }
}

/// Create entities (spawned eagerly by the editor; the command re-spawns
/// them on redo with fresh ids and despawns on undo).
pub struct CreateEntitiesCommand {
    label: String,
    records: Vec<EntityRecord>,
    entities: Vec<Option<hecs::Entity>>,
}

impl Command for CreateEntitiesCommand {
    fn label(&self) -> String {
        self.label.clone()
    }
    fn apply(&mut self, ctx: &mut CommandCtx) {
        for (rec, slot) in self.records.iter().zip(self.entities.iter_mut()) {
            let alive = slot.map(|e| ctx.world.contains(e)).unwrap_or(false);
            if !alive {
                *slot = Some(spark::scene::spawn_record_world(
                    ctx.world,
                    rec,
                    None,
                    ctx.registry,
                ));
            }
        }
    }
    fn revert(&mut self, ctx: &mut CommandCtx) {
        for slot in self.entities.iter_mut() {
            if let Some(e) = *slot
                && ctx.world.contains(e)
            {
                ecs::despawn_recursive(ctx.world, e);
            }
            *slot = None;
        }
    }
}

/// Delete entities. Undo respawns each subtree record under its original
/// parent (hierarchy preserved via nested records); redo despawns again.
pub struct DeleteEntitiesCommand {
    label: String,
    /// (nested record incl. children, original parent)
    items: Vec<(EntityRecord, Option<hecs::Entity>)>,
    entities: Vec<Option<hecs::Entity>>,
}

impl Command for DeleteEntitiesCommand {
    fn label(&self) -> String {
        self.label.clone()
    }
    fn apply(&mut self, ctx: &mut CommandCtx) {
        for slot in self.entities.iter_mut() {
            if let Some(e) = *slot
                && ctx.world.contains(e)
            {
                ecs::despawn_recursive(ctx.world, e);
            }
            *slot = None;
        }
    }
    fn revert(&mut self, ctx: &mut CommandCtx) {
        for ((rec, parent), slot) in self.items.iter().zip(self.entities.iter_mut()) {
            *slot = Some(spark::scene::spawn_record_world(
                ctx.world,
                rec,
                *parent,
                ctx.registry,
            ));
        }
    }
}

/// Rename an entity (`Name` is structural, not a registered component).
pub struct SetNameCommand {
    entity: hecs::Entity,
    before: String,
    after: String,
}

impl Command for SetNameCommand {
    fn label(&self) -> String {
        format!("Rename → {}", self.after)
    }
    fn apply(&mut self, ctx: &mut CommandCtx) {
        let _ = ctx
            .world
            .insert_one(self.entity, ecs::Name(self.after.clone()));
    }
    fn revert(&mut self, ctx: &mut CommandCtx) {
        let _ = ctx
            .world
            .insert_one(self.entity, ecs::Name(self.before.clone()));
    }
}

/// Reparent an entity, preserving its world transform in both directions.
pub struct ReparentCommand {
    child: hecs::Entity,
    old_parent: Option<hecs::Entity>,
    new_parent: Option<hecs::Entity>,
    local_before: Transform,
    local_after: Transform,
}

impl Command for ReparentCommand {
    fn label(&self) -> String {
        "Reparent".into()
    }
    fn apply(&mut self, ctx: &mut CommandCtx) {
        ecs::set_parent(ctx.world, self.child, self.new_parent);
        let _ = ctx.world.insert_one(self.child, self.local_after);
    }
    fn revert(&mut self, ctx: &mut CommandCtx) {
        ecs::set_parent(ctx.world, self.child, self.old_parent);
        let _ = ctx.world.insert_one(self.child, self.local_before);
    }
}

// ---------------------------------------------------------------------------
// Editor mutation helpers
// ---------------------------------------------------------------------------

impl Editor {
    /// Push a command that has not been applied yet.
    pub(crate) fn push_command(&mut self, cmd: Box<dyn Command>) {
        let mut world = std::mem::take(&mut self.engine.scene.world);
        let registry = &self.engine.registry;
        {
            let mut ctx = CommandCtx {
                world: &mut world,
                registry,
            };
            self.undo.push(&mut ctx, cmd);
        }
        self.engine.scene.world = world;
        self.engine.physics.request_rebuild();
    }

    /// Push a command that has *already* been applied (interactive drags,
    /// eagerly-spawned entities).
    pub(crate) fn push_prepared_command(&mut self, cmd: Box<dyn Command>) {
        self.undo.push_prepared(cmd);
        self.engine.physics.request_rebuild();
    }

    /// Spawn an entity bundle eagerly, then register an undoable create.
    /// Returns the new entity.
    fn spawn_with_undo(
        &mut self,
        label: &str,
        bundle: Vec<(&'static str, String)>,
    ) -> hecs::Entity {
        let world = &mut self.engine.scene.world;
        let e = world.spawn((ecs::Name(label.to_string()), Transform::default()));
        for (comp, text) in &bundle {
            if let Some(entry) = self.engine.registry.get(comp) {
                let _ = (entry.load)(world, e, text);
            }
        }
        let record = self.engine.scene.record_of(e, &self.engine.registry);
        self.push_prepared_command(Box::new(CreateEntitiesCommand {
            label: format!("Add {label}"),
            records: vec![record],
            entities: vec![Some(e)],
        }));
        self.state.select(e);
        self.log("info", &format!("added \"{label}\""));
        e
    }

    pub fn add_entity(&mut self, name: &str, parent: Option<hecs::Entity>) {
        let e = self.spawn_with_undo(name, Vec::new());
        if let Some(p) = parent
            && self.engine.scene.world.contains(p)
        {
            self.reparent(e, Some(p));
        }
    }

    pub fn add_sprite(&mut self) {
        self.spawn_with_undo(
            "Sprite",
            vec![(
                "Sprite",
                ron::to_string(&Sprite::default()).unwrap_or_default(),
            )],
        );
    }

    pub fn add_mesh(&mut self, mesh: &str) {
        self.spawn_with_undo(
            mesh,
            vec![(
                "MeshRenderer",
                ron::to_string(&MeshRenderer {
                    mesh: mesh.to_string(),
                    ..Default::default()
                })
                .unwrap_or_default(),
            )],
        );
    }

    pub fn add_point_light(&mut self) {
        self.spawn_with_undo(
            "Point Light",
            vec![(
                "Light",
                ron::to_string(&Light {
                    kind: LightKind::Point { range: 10.0 },
                    color: Color::WHITE,
                    intensity: 2.0,
                })
                .unwrap_or_default(),
            )],
        );
    }

    pub fn add_camera(&mut self) {
        let persp = matches!(self.engine.scene.dimension, spark::scene::Dimension::D3);
        let cam = if persp {
            Camera {
                kind: CameraKind::Perspective { fov_deg: 60.0 },
                ..Default::default()
            }
        } else {
            Camera::default()
        };
        self.spawn_with_undo(
            "Camera",
            vec![("Camera", ron::to_string(&cam).unwrap_or_default())],
        );
    }

    /// Add a Sun (directional light), the shadow caster.
    pub fn add_sun(&mut self) {
        self.spawn_with_undo(
            "Sun",
            vec![(
                "Light",
                ron::to_string(&Light::default()).unwrap_or_default(),
            )],
        );
    }

    /// Duplicate every selected entity with its whole subtree.
    pub fn duplicate_selected(&mut self) {
        let selected: Vec<hecs::Entity> = self
            .state
            .selected
            .iter()
            .copied()
            .filter(|e| self.engine.scene.world.contains(*e))
            .collect();
        if selected.is_empty() {
            return;
        }
        let mut records = Vec::new();
        let mut entities = Vec::new();
        let mut new_selection = Vec::new();
        for e in selected {
            let rec = self.engine.scene.record_of(e, &self.engine.registry);
            let world = &mut self.engine.scene.world;
            let new = spark::scene::spawn_record_world(world, &rec, None, &self.engine.registry);
            let name = world.get::<&ecs::Name>(e).map(|n| n.0.clone()).ok();
            if let Some(name) = name {
                let _ = world.insert_one(new, ecs::Name(format!("{name} copy")));
            }
            records.push(rec);
            entities.push(Some(new));
            new_selection.push(new);
        }
        self.push_prepared_command(Box::new(CreateEntitiesCommand {
            label: "Duplicate".into(),
            records,
            entities,
        }));
        self.state.selected = new_selection;
        self.log("info", "duplicated selection");
    }

    /// Delete the whole (multi-)selection, undoably, preserving subtrees.
    pub fn despawn_selected(&mut self) {
        let selected: Vec<hecs::Entity> = self
            .state
            .selected
            .iter()
            .copied()
            .filter(|e| self.engine.scene.world.contains(*e))
            .collect();
        if selected.is_empty() {
            return;
        }
        let items: Vec<(EntityRecord, Option<hecs::Entity>)> = selected
            .iter()
            .map(|&e| {
                let rec = self.engine.scene.record_of(e, &self.engine.registry);
                let parent = self
                    .engine
                    .scene
                    .world
                    .get::<&ecs::Parent>(e)
                    .ok()
                    .map(|p| p.0);
                (rec, parent)
            })
            .collect();
        self.push_command(Box::new(DeleteEntitiesCommand {
            label: format!(
                "Delete {} entit{}",
                items.len(),
                if items.len() == 1 { "y" } else { "ies" }
            ),
            items,
            entities: selected.iter().map(|e| Some(*e)).collect(),
        }));
        self.state.selected.clear();
    }

    /// Undoable rename.
    pub fn rename_entity(&mut self, e: hecs::Entity, new_name: &str) {
        if !self.engine.scene.world.contains(e) {
            return;
        }
        let before = self
            .engine
            .scene
            .world
            .get::<&ecs::Name>(e)
            .map(|n| n.0.clone())
            .unwrap_or_default();
        if before == new_name {
            return;
        }
        self.push_command(Box::new(SetNameCommand {
            entity: e,
            before,
            after: new_name.to_string(),
        }));
    }

    /// Undoable reparent that preserves the entity's world transform.
    /// Refuses cycles (reparenting under own descendant).
    pub fn reparent(&mut self, child: hecs::Entity, new_parent: Option<hecs::Entity>) {
        let world = &self.engine.scene.world;
        if !world.contains(child) {
            return;
        }
        if let Some(p) = new_parent
            && (p == child || ecs::descendants(world, child).contains(&p))
        {
            self.log("warn", "cannot parent an entity under its own descendant");
            return;
        }
        let old_parent = world.get::<&ecs::Parent>(child).ok().map(|p| p.0);
        if old_parent == new_parent {
            return;
        }
        let local_before = world
            .get::<&Transform>(child)
            .map(|t| *t)
            .unwrap_or_default();
        let world_t = ecs::world_transform(world, child);
        let mut w = std::mem::take(&mut self.engine.scene.world);
        ecs::set_parent(&mut w, child, new_parent);
        ecs::set_world_transform(&mut w, child, world_t);
        let local_after = w.get::<&Transform>(child).map(|t| *t).unwrap_or_default();
        self.engine.scene.world = w;
        self.push_prepared_command(Box::new(ReparentCommand {
            child,
            old_parent,
            new_parent,
            local_before,
            local_after,
        }));
        self.engine.physics.request_rebuild();
    }

    /// Record an inspector edit for undo (before → after component text).
    pub fn push_component_cmd(
        &mut self,
        entity: hecs::Entity,
        component: &'static str,
        before: Option<String>,
        after: Option<String>,
        label: &str,
    ) {
        let cmd = SwapCommand {
            label: format!("{label} {component}"),
            entity,
            before,
            after,
            component,
        };
        let registry = &self.engine.registry;
        let mut world = std::mem::take(&mut self.engine.scene.world);
        {
            let mut ctx = CommandCtx {
                world: &mut world,
                registry,
            };
            self.undo.push(&mut ctx, Box::new(cmd));
        }
        self.engine.scene.world = world;
    }

    /// Serialized component snapshot for undo baselines.
    pub fn snapshot_component(&self, entity: hecs::Entity, name: &str) -> Option<String> {
        let entry = self.engine.registry.get(name)?;
        (entry.save)(&self.engine.scene.world, entity)
    }

    pub(crate) fn export_game(&mut self, dir: &std::path::Path) {
        let exe = std::env::current_exe().ok();
        let Some(exe) = exe else {
            self.log("error", "cannot locate editor binary");
            return;
        };
        let out = dir.join("export");
        match Project::export(dir, &exe, &out) {
            Ok(p) => self.log(
                "info",
                &format!("exported to {} (copy this folder to share)", p.display()),
            ),
            Err(e) => self.log("error", &format!("export failed: {e}")),
        }
    }

    /// The registry, for tests.
    pub fn registry(&self) -> &Registry {
        &self.engine.registry
    }
}
