//! Scenes: the saved/restorable state of a [`hecs::World`] plus scene-level
//! settings. Scene files are human-editable RON.
//!
//! Core components serialize through a tagged enum for pretty, struct-style
//! output; anything else registered in the [`Registry`] (including user game
//! components) round-trips through the `Custom` variant, so scenes stay fully
//! generic without the engine knowing every type.
//!
//! Prefabs are single entity records (`.ron`), spawnable by the editor and by
//! the rules `Spawn` action — the same code path.

use std::collections::HashMap;
use std::path::Path;

use hecs::World;
use serde::{Deserialize, Serialize};

use crate::assets::Assets;
use crate::components::{Camera, Collider, Light, MeshRenderer, Music, RulesComp, RigidBody, Sprite, Transform, Vars, Visible};
use crate::ecs::{self, Registry};
use crate::math::Color;

/// Which physics backend and editor camera conventions a scene uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Dimension {
    #[default]
    D2,
    D3,
}

/// Tagged serialization for built-in components (pretty RON output).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ComponentData {
    Sprite(Sprite),
    MeshRenderer(MeshRenderer),
    Camera(Camera),
    Light(Light),
    RigidBody(RigidBody),
    Collider(Collider),
    Music(Music),
    Rules(RulesComp),
    Vars(Vars),
    Visible(Visible),
    /// Any other registered component, kept as raw RON text (type name, body).
    /// Raw text avoids Value round-tripping quirks with struct syntax.
    Custom(String, String),
}

/// One entity (with subtree) as stored in scene and prefab files.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EntityRecord {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub transform: Option<Transform>,
    #[serde(default)]
    pub components: Vec<ComponentData>,
    #[serde(default)]
    pub children: Vec<EntityRecord>,
}

/// The whole scene file.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SceneData {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub dimension: Dimension,
    #[serde(default = "default_ambient")]
    pub ambient: f32,
    #[serde(default)]
    pub sky: Color,
    #[serde(default)]
    pub globals: HashMap<String, f64>,
    #[serde(default)]
    pub entities: Vec<EntityRecord>,
}

fn default_ambient() -> f32 {
    0.35
}

/// Runtime scene state.
pub struct Scene {
    pub world: World,
    pub dimension: Dimension,
    pub ambient: f32,
    pub sky: Color,
    pub globals: HashMap<String, f64>,
    pub name: String,
}

impl Default for Scene {
    fn default() -> Self {
        Self {
            world: World::default(),
            dimension: Dimension::D2,
            ambient: default_ambient(),
            sky: Color::ENGINE_BG,
            globals: HashMap::new(),
            name: "Scene".into(),
        }
    }
}

impl Scene {
    /// Serialize the whole scene to a pretty RON string.
    pub fn save(&self, registry: &Registry) -> String {
        let data = SceneData {
            name: self.name.clone(),
            dimension: self.dimension,
            ambient: self.ambient,
            sky: self.sky,
            globals: self.globals.clone(),
            entities: self.records(registry),
        };
        ron::ser::to_string_pretty(&data, ron::ser::PrettyConfig::default().struct_names(true)).unwrap_or_default()
    }

    fn records(&self, registry: &Registry) -> Vec<EntityRecord> {
        ecs::roots(&self.world)
            .into_iter()
            .map(|e| self.record_of(e, registry))
            .collect()
    }

    fn record_of(&self, e: hecs::Entity, registry: &Registry) -> EntityRecord {
        let world = &self.world;
        let transform = world.get::<&Transform>(e).ok().map(|t| (*t).clone());
        let mut components = Vec::new();
        for entry in &registry.entries {
            if entry.name == "Transform" || !(entry.has)(world, e) {
                continue;
            }
            let Some(text) = (entry.save)(world, e) else { continue };
            let value = match entry.name {
                "Sprite" => ComponentData::Sprite(ron::from_str(&text).unwrap_or_default()),
                "MeshRenderer" => ComponentData::MeshRenderer(ron::from_str(&text).unwrap_or_default()),
                "Camera" => ComponentData::Camera(ron::from_str(&text).unwrap_or_default()),
                "Light" => ComponentData::Light(ron::from_str(&text).unwrap_or_default()),
                "RigidBody" => ComponentData::RigidBody(ron::from_str(&text).unwrap_or_default()),
                "Collider" => ComponentData::Collider(ron::from_str(&text).unwrap_or_default()),
                "Music" => ComponentData::Music(ron::from_str(&text).unwrap_or_default()),
                "Rules" => ComponentData::Rules(ron::from_str(&text).unwrap_or_default()),
                "Vars" => ComponentData::Vars(ron::from_str(&text).unwrap_or_default()),
                "Visible" => ComponentData::Visible(ron::from_str(&text).unwrap_or_default()),
                other => ComponentData::Custom(other.to_string(), text),
            };
            components.push(value);
        }
        EntityRecord {
            name: world.get::<&ecs::Name>(e).ok().map(|n| n.0.clone()),
            tag: world.get::<&ecs::Tag>(e).ok().map(|t| t.0.clone()),
            transform,
            components,
            children: ecs::children(world, e)
                .into_iter()
                .map(|c| self.record_of(c, registry))
                .collect(),
        }
    }

    /// Load a scene from RON text (clears the world).
    pub fn load(text: &str, registry: &Registry) -> anyhow::Result<Scene> {
        let data: SceneData = ron::from_str(text)?;
        let mut scene = Scene {
            world: World::default(),
            dimension: data.dimension,
            ambient: data.ambient,
            sky: data.sky,
            globals: data.globals,
            name: data.name.clone(),
        };
        for rec in &data.entities {
            scene.spawn_record(rec, None, registry);
        }
        Ok(scene)
    }

    /// Spawn one entity record (with children) and return its entity id.
    pub fn spawn_record(&mut self, rec: &EntityRecord, parent: Option<hecs::Entity>, registry: &Registry) -> hecs::Entity {
        spawn_record_world(&mut self.world, rec, parent, registry)
    }
}

/// Spawn an entity record (with children) into any world.
pub fn spawn_record_world(world: &mut World, rec: &EntityRecord, parent: Option<hecs::Entity>, registry: &Registry) -> hecs::Entity {
    let e = world.spawn((ecs::Name(rec.name.clone().unwrap_or_default()),));
    if let Some(tag) = &rec.tag {
        world.insert_one(e, ecs::Tag(tag.clone())).ok();
    }
    if let Some(t) = &rec.transform {
        world.insert_one(e, t.clone()).ok();
    }
    for comp in &rec.components {
        apply_component(world, e, comp, registry);
    }
    if let Some(p) = parent {
        ecs::set_parent(world, e, Some(p));
    }
    for child in &rec.children {
        spawn_record_world(world, child, Some(e), registry);
    }
    e
}

fn apply_component(world: &mut World, e: hecs::Entity, comp: &ComponentData, registry: &Registry) {
    match comp {
        ComponentData::Sprite(c) => {
            world.insert_one(e, c.clone()).ok();
        }
        ComponentData::MeshRenderer(c) => {
            world.insert_one(e, c.clone()).ok();
        }
        ComponentData::Camera(c) => {
            world.insert_one(e, c.clone()).ok();
        }
        ComponentData::Light(c) => {
            world.insert_one(e, c.clone()).ok();
        }
        ComponentData::RigidBody(c) => {
            world.insert_one(e, c.clone()).ok();
        }
        ComponentData::Collider(c) => {
            world.insert_one(e, c.clone()).ok();
        }
        ComponentData::Music(c) => {
            world.insert_one(e, c.clone()).ok();
        }
        ComponentData::Rules(c) => {
            world.insert_one(e, c.clone()).ok();
        }
        ComponentData::Vars(c) => {
            world.insert_one(e, c.clone()).ok();
        }
        ComponentData::Visible(c) => {
            world.insert_one(e, *c).ok();
        }
        ComponentData::Custom(name, text) => {
            if let Some(entry) = registry.get(name) {
                let _ = (entry.load)(world, e, text);
            } else {
                log::warn!("scene: unknown component type \"{name}\" skipped");
            }
        }
    }
}

/// Spawn a prefab file (project-relative path) into `world`, optionally under
/// `parent`, offset by `offset`. Returns the new entity.
pub fn spawn_prefab(
    world: &mut World,
    assets: &Assets,
    path: &str,
    parent: Option<hecs::Entity>,
    offset: crate::math::Vec3,
) -> Option<hecs::Entity> {
    let fs = assets.root().join(path);
    let text = std::fs::read_to_string(fs).ok()?;
    let mut rec: EntityRecord = ron::from_str(&text).ok()?;
    if offset != crate::math::Vec3::ZERO {
        if let Some(t) = &mut rec.transform {
            t.position += offset;
        }
    }
    let registry = default_registry();
    Some(spawn_record_world(world, &rec, parent, &registry))
}

/// The standard registry (core components) — used by prefabs and available to
/// games that want the default set.
pub fn default_registry() -> Registry {
    let mut r = Registry::default();
    crate::components::register_core(&mut r);
    r
}

/// Load a scene file from disk (project-relative path via `assets` root).
pub fn load_scene_file(path: &Path, registry: &Registry) -> anyhow::Result<Scene> {
    let text = std::fs::read_to_string(path)?;
    Scene::load(&text, registry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::Sprite;
    use crate::math::Vec2;

    #[test]
    fn roundtrip() {
        let registry = default_registry();
        let mut scene = Scene::default();
        let parent = scene.world.spawn((
            ecs::Name("Player".into()),
            ecs::Tag("player".into()),
            Transform { position: crate::math::Vec3::new(1.0, 2.0, 0.0), ..Default::default() },
            Sprite { image: "assets/player.png".into(), color: Color::RED, size: Vec2::new(2.0, 2.0) },
            Vars(HashMap::from([("hp".to_string(), 5.0)])),
        ));
        let child = scene.world.spawn((ecs::Name("Hat".into()), Transform::default()));
        ecs::set_parent(&mut scene.world, child, Some(parent));
        scene.globals.insert("score".to_string(), 42.0);

        let text = scene.save(&registry);
        let loaded = Scene::load(&text, &registry).unwrap();
        assert_eq!(loaded.globals["score"], 42.0);
        let player = ecs::find_by_name(&loaded.world, "Player").unwrap();
        assert!(loaded.world.get::<&Sprite>(player).is_ok());
        assert_eq!(loaded.world.get::<&Vars>(player).unwrap().0["hp"], 5.0);
        let hat = ecs::find_by_name(&loaded.world, "Hat").unwrap();
        assert_eq!(ecs::find_by_name(&loaded.world, "Player").unwrap(), loaded.world.get::<&ecs::Parent>(hat).unwrap().0);
    }

    #[test]
    fn custom_components_roundtrip() {
        // Register a "user" component through the registry and verify the
        // Custom path preserves it.
        use serde::{Deserialize, Serialize};
        #[derive(Clone, Debug, Default, Serialize, Deserialize, spark_macros::ComponentDef)]
        struct UserScore { pub score: f32 }

        let mut registry = default_registry();
        registry.register::<UserScore>();
        let mut scene = Scene::default();
        let e = scene.world.spawn((ecs::Name("Thing".into()), UserScore { score: 7.5 }));
        let _ = e;
        let text = scene.save(&registry);
        let loaded = Scene::load(&text, &registry).unwrap();
        let thing = ecs::find_by_name(&loaded.world, "Thing").unwrap();
        assert_eq!(loaded.world.get::<&UserScore>(thing).unwrap().score, 7.5);
    }
}
