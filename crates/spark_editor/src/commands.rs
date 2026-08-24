//! Editor commands (undo/redo) + scene mutation helpers.
//!
//! Every user-visible mutation goes through [`CommandStack`] so undo works
//! uniformly. Commands capture the *data* needed to apply/revert, never
//! world references.

use spark::ecs::{self, Registry};
use spark::prelude::*;
use spark::reexport::hecs;

use crate::Editor;

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

/// Entity spawn/despawn with full subtree snapshots.
pub struct SpawnCommand {
    label: String,
    record: spark::scene::EntityRecord,
    entity: Option<hecs::Entity>,
    /// Despawn's revert data (records of children for respawn).
    despawn_revert: Option<(Vec<spark::scene::EntityRecord>, Option<hecs::Entity>)>,
}

impl Command for SpawnCommand {
    fn label(&self) -> String {
        self.label.clone()
    }
    fn apply(&mut self, ctx: &mut CommandCtx) {
        if let Some(_e) = self.entity {
            // Despawn branch: entity exists → remove it.
            if ctx.world.contains(_e) {
                ecs::despawn_recursive(ctx.world, _e);
            }
        } else {
            let e = spark::scene::spawn_record_world(ctx.world, &self.record, None, ctx.registry);
            self.entity = Some(e);
        }
    }
    fn revert(&mut self, ctx: &mut CommandCtx) {
        if let Some((records, parent)) = &self.despawn_revert {
            // Respawn despawned entities.
            for rec in records {
                spark::scene::spawn_record_world(ctx.world, rec, *parent, ctx.registry);
            }
            self.despawn_revert = None;
        } else if let Some(e) = self.entity
            && ctx.world.contains(e)
        {
            ecs::despawn_recursive(ctx.world, e);
        }
    }
}

impl Editor {
    pub(crate) fn add_entity(&mut self, name: &str) {
        let world = &mut self.engine.scene.world;
        let e = world.spawn((spark::ecs::Name(name.to_string()), Transform::default()));
        self.state.selected = Some(e);
        self.log("info", &format!("added entity \"{name}\""));
    }

    pub(crate) fn add_sprite(&mut self) {
        let world = &mut self.engine.scene.world;
        let e = world.spawn((
            spark::ecs::Name("Sprite".into()),
            Transform::default(),
            Sprite::default(),
        ));
        self.state.selected = Some(e);
    }

    pub(crate) fn add_mesh(&mut self, mesh: &str, _dim: Dimension) {
        let world = &mut self.engine.scene.world;
        let e = world.spawn((
            spark::ecs::Name(mesh.to_string()),
            Transform::default(),
            MeshRenderer {
                mesh: mesh.to_string(),
                ..Default::default()
            },
        ));
        self.state.selected = Some(e);
    }

    pub(crate) fn add_point_light(&mut self) {
        let world = &mut self.engine.scene.world;
        let e = world.spawn((
            spark::ecs::Name("Point Light".into()),
            Transform::default(),
            Light {
                kind: LightKind::Point { range: 10.0 },
                color: Color::WHITE,
                intensity: 2.0,
            },
        ));
        self.state.selected = Some(e);
    }

    /// Duplicate an entity with all its (registered) components.
    pub(crate) fn duplicate_entity(&mut self, e: hecs::Entity) {
        let registry = &self.engine.registry;
        let rec = self.record_of(e, registry);
        let world = &mut self.engine.scene.world;
        let new = spark::scene::spawn_record_world(world, &rec, None, registry);
        if let Some(name) = world
            .get::<&spark::ecs::Name>(new)
            .ok()
            .map(|n| format!("{} copy", n.0))
        {
            let _ = world.insert_one(new, spark::ecs::Name(name));
        }
        self.state.selected = Some(new);
    }

    fn record_of(&self, e: hecs::Entity, registry: &Registry) -> spark::scene::EntityRecord {
        // Snapshot the entity (with every registered component) as a record.
        let world = &self.engine.scene.world;
        let mut comps = Vec::new();
        for entry in &registry.entries {
            if !(entry.has)(world, e) {
                continue;
            }
            if let Some(text) = (entry.save)(world, e) {
                comps.push(spark::scene::ComponentData::Custom(
                    entry.name.to_string(),
                    text,
                ));
            }
        }
        spark::scene::EntityRecord {
            name: world.get::<&spark::ecs::Name>(e).ok().map(|n| n.0.clone()),
            tag: world.get::<&spark::ecs::Tag>(e).ok().map(|t| t.0.clone()),
            transform: world.get::<&Transform>(e).ok().map(|t| *t),
            components: comps,
            children: Vec::new(),
        }
    }

    /// Despawn via undoable command (with subtree snapshot).
    pub(crate) fn despawn_selected(&mut self) {
        let Some(target) = self.state.selected else {
            return;
        };
        if !self.engine.scene.world.contains(target) {
            return;
        }
        // Snapshot the subtree (immutable) before mutating.
        let records = self.snapshot_subtree(target);
        let parent = self
            .engine
            .scene
            .world
            .get::<&spark::ecs::Parent>(target)
            .ok()
            .map(|p| p.0);
        let world = &mut self.engine.scene.world;
        let mut cmd = SpawnCommand {
            label: "Delete Entity".into(),
            record: spark::scene::EntityRecord::default(),
            entity: Some(target),
            despawn_revert: Some((records, parent)),
        };
        let registry = &self.engine.registry;
        let mut w = std::mem::take(world);
        {
            let mut ctx = CommandCtx {
                world: &mut w,
                registry,
            };
            self.undo.push(&mut ctx, Box::new(cmd));
            cmd = SpawnCommand {
                label: "Delete Entity".into(),
                record: spark::scene::EntityRecord::default(),
                entity: None,
                despawn_revert: None,
            };
        }
        let _ = cmd;
        self.engine.scene.world = w;
        self.state.selected = None;
    }

    fn snapshot_subtree(&self, root: hecs::Entity) -> Vec<spark::scene::EntityRecord> {
        let registry = &self.engine.registry;
        let world = &self.engine.scene.world;
        let mut out = vec![self.record_of(root, registry)];
        for c in ecs::descendants(world, root) {
            out.push(self.record_of(c, registry));
        }
        out
    }

    /// Record an inspector edit for undo (before → after component text).
    pub(crate) fn push_component_cmd(
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
    pub(crate) fn snapshot_component(&self, entity: hecs::Entity, name: &str) -> Option<String> {
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
}
