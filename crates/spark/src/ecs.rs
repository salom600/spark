//! Entity storage and the component registry.
//!
//! spark uses [`hecs`] as the archetypal ECS store. On top of it sit three
//! small abstractions that the whole engine shares:
//!
//! * [`ComponentDef`] — implemented by the `#[derive(ComponentDef)]` macro:
//!   type name + egui inspector. Combined with serde derives this is all a
//!   component needs to be saveable, inspectable and cloneable in the editor.
//! * [`Registry`] — type-erased operations (save/load/remove/inspect/duplicate)
//!   for every registered component, in ~40 generic lines total.
//! * hierarchy helpers — `Parent`/`Children` components maintained through
//!   [`set_parent`] / [`detach`] / [`despawn_recursive`].

use std::collections::HashMap;

use hecs::{Entity, World};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::math::{Color, Vec2, Vec3, Vec4};

/// A component definition: named, inspectable, serializable, clonable.
///
/// Derive it with `#[derive(ComponentDef)]` from `spark_macros`; you will also
/// want `Clone, Default, Serialize, Deserialize` on the same type.
pub trait ComponentDef:
    Clone + Default + Serialize + DeserializeOwned + Send + Sync + 'static
{
    /// Wire name used in scene files and editor menus (e.g. `"Sprite"`).
    const NAME: &'static str;

    /// Render the inspector UI for this component; return `true` when mutated
    /// (so the editor can mark the scene dirty / record undo).
    fn inspect(&mut self, ui: &mut crate::reexport::egui::Ui) -> bool;

    /// Name of the active enum variant, or the type name for structs.
    fn variant_name(&self) -> &'static str {
        Self::NAME
    }
}

/// Field-level editing used by the generated inspectors.
///
/// Implemented for engine value types; user components containing custom types
/// can implement `Inspect` for them the same way.
pub trait Inspect {
    fn inspect(&mut self, ui: &mut crate::reexport::egui::Ui) -> bool;
}

fn drag_f32(v: &mut f32, ui: &mut egui::Ui) -> bool {
    ui.add(egui::DragValue::new(v).speed(0.05)).changed()
}

impl Inspect for f32 {
    fn inspect(&mut self, ui: &mut egui::Ui) -> bool {
        drag_f32(self, ui)
    }
}
impl Inspect for f64 {
    fn inspect(&mut self, ui: &mut egui::Ui) -> bool {
        ui.add(egui::DragValue::new(self).speed(0.05)).changed()
    }
}
impl Inspect for u32 {
    fn inspect(&mut self, ui: &mut egui::Ui) -> bool {
        ui.add(egui::DragValue::new(self).range(0..=u32::MAX))
            .changed()
    }
}
impl Inspect for usize {
    fn inspect(&mut self, ui: &mut egui::Ui) -> bool {
        ui.add(egui::DragValue::new(self).range(0..=usize::MAX))
            .changed()
    }
}
impl Inspect for bool {
    fn inspect(&mut self, ui: &mut egui::Ui) -> bool {
        ui.checkbox(self, "").changed()
    }
}
impl Inspect for String {
    fn inspect(&mut self, ui: &mut egui::Ui) -> bool {
        ui.text_edit_singleline(self).changed()
    }
}
impl Inspect for Vec2 {
    fn inspect(&mut self, ui: &mut egui::Ui) -> bool {
        let mut a = [self.x, self.y];
        let mut c = false;
        ui.horizontal(|ui| {
            for v in &mut a {
                c |= drag_f32(v, ui);
            }
        });
        self.x = a[0];
        self.y = a[1];
        c
    }
}
impl Inspect for Vec3 {
    fn inspect(&mut self, ui: &mut egui::Ui) -> bool {
        let mut a = [self.x, self.y, self.z];
        let mut c = false;
        ui.horizontal(|ui| {
            for v in &mut a {
                c |= drag_f32(v, ui);
            }
        });
        self.x = a[0];
        self.y = a[1];
        self.z = a[2];
        c
    }
}
impl Inspect for Vec4 {
    fn inspect(&mut self, ui: &mut egui::Ui) -> bool {
        let mut a = [self.x, self.y, self.z, self.w];
        let mut c = false;
        ui.horizontal(|ui| {
            for v in &mut a {
                c |= drag_f32(v, ui);
            }
        });
        self.x = a[0];
        self.y = a[1];
        self.z = a[2];
        self.w = a[3];
        c
    }
}
impl Inspect for Color {
    fn inspect(&mut self, ui: &mut egui::Ui) -> bool {
        let mut c = [self.r, self.g, self.b, self.a];
        let changed = ui.color_edit_button_rgba_unmultiplied(&mut c).changed();
        if changed {
            *self = Color {
                r: c[0],
                g: c[1],
                b: c[2],
                a: c[3],
            };
        }
        changed
    }
}
impl<T: Inspect + Clone + Default> Inspect for Option<T> {
    fn inspect(&mut self, ui: &mut egui::Ui) -> bool {
        let mut c = false;
        ui.horizontal(|ui| {
            let mut on = self.is_some();
            if ui.checkbox(&mut on, "").changed() {
                let owned = self.clone();
                *self = if on {
                    Some(owned.unwrap_or_default())
                } else {
                    None
                };
                c = true;
            }
            if let Some(v) = self {
                c |= v.inspect(ui);
            }
        });
        c
    }
}

/// Type-erased operations over one registered component type.
pub struct ComponentEntry {
    pub name: &'static str,
    pub has: fn(&World, Entity) -> bool,
    pub save: fn(&World, Entity) -> Option<String>,
    pub load: fn(&mut World, Entity, &str) -> anyhow::Result<()>,
    pub remove: fn(&mut World, Entity),
    pub add_default: fn(&mut World, Entity),
    pub inspect: fn(&mut World, Entity, &mut egui::Ui) -> bool,
    pub duplicate: fn(&World, Entity, &mut World, Entity),
}

/// All component types known to the editor and the scene serializer.
#[derive(Default)]
pub struct Registry {
    pub entries: Vec<ComponentEntry>,
    by_name: HashMap<&'static str, usize>,
}

impl Registry {
    /// Register one component type `T` (idempotent).
    pub fn register<T: ComponentDef>(&mut self) -> &mut Self {
        if self.by_name.contains_key(T::NAME) {
            return self;
        }
        let entry = ComponentEntry {
            name: T::NAME,
            has: |w, e| w.get::<&T>(e).is_ok(),
            save: |w, e| {
                w.get::<&T>(e)
                    .ok()
                    .map(|c| ron::to_string(&*c).unwrap_or_default())
            },
            load: |w, e, s| {
                let c: T = ron::from_str(s)?;
                w.insert_one(e, c)?;
                Ok(())
            },
            remove: |w, e| {
                w.remove_one::<T>(e).ok();
            },
            add_default: |w, e| {
                w.insert_one(e, T::default()).ok();
            },
            inspect: |w, e, ui| match w.get::<&mut T>(e) {
                Ok(mut c) => c.inspect(ui),
                Err(_) => false,
            },
            duplicate: |src_w, src, dst_w, dst| {
                if let Ok(c) = src_w.get::<&T>(src) {
                    let owned = (*c).clone();
                    dst_w.insert_one(dst, owned).ok();
                }
            },
        };
        self.by_name.insert(entry.name, self.entries.len());
        self.entries.push(entry);
        self
    }

    pub fn get(&self, name: &str) -> Option<&ComponentEntry> {
        self.by_name.get(name).map(|&i| &self.entries[i])
    }

    /// Every registered component name, in registration order.
    pub fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.entries.iter().map(|e| e.name)
    }
}

// ---------------------------------------------------------------------------
// Hierarchy
// ---------------------------------------------------------------------------

/// Structural components (serialized specially by `scene`, not via Registry).
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Name(pub String);

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Tag(pub String);

/// Parent link, serialized as the entity's raw bits (stable within a save).
#[derive(Clone, Copy, Debug)]
pub struct Parent(pub Entity);

impl serde::Serialize for Parent {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(self.0.to_bits().get())
    }
}

impl<'de> serde::Deserialize<'de> for Parent {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let bits = u64::deserialize(d)?;
        Entity::from_bits(bits)
            .map(Parent)
            .ok_or_else(|| serde::de::Error::custom("invalid entity id"))
    }
}

/// Child links (runtime only — reconstructed from scene records on load).
#[derive(Clone, Debug, Default)]
pub struct Children(pub Vec<Entity>);

/// Attach `child` under `parent`, maintaining both sides of the link.
pub fn set_parent(world: &mut World, child: Entity, parent: Option<Entity>) {
    // Unlink from the old parent first (no-op when already correct).
    if let Ok(old_ref) = world.get::<&Parent>(child) {
        let old = old_ref.0;
        if parent == Some(old) {
            return; // already correct
        }
        drop(old_ref);
        if let Ok(mut siblings) = world.get::<&mut Children>(old) {
            siblings.0.retain(|&e| e != child);
        }
    }
    match parent {
        Some(p) => {
            world.insert_one(child, Parent(p)).ok();
            let has = world.get::<&Children>(p).is_ok();
            if has {
                if let Ok(mut c) = world.get::<&mut Children>(p) {
                    c.0.push(child);
                }
            } else {
                world.insert_one(p, Children(vec![child])).ok();
            }
        }
        None => {
            world.remove_one::<Parent>(child).ok();
        }
    };
}

/// All direct children of `e`.
pub fn children(world: &World, e: Entity) -> Vec<Entity> {
    world
        .get::<&Children>(e)
        .map(|c| c.0.clone())
        .unwrap_or_default()
}

/// Depth-first descendant list (excluding `e`).
pub fn descendants(world: &World, e: Entity) -> Vec<Entity> {
    let mut out = Vec::new();
    let mut stack = children(world, e);
    while let Some(c) = stack.pop() {
        out.push(c);
        stack.extend(children(world, c));
    }
    out
}

/// Remove `e` and its whole subtree.
pub fn despawn_recursive(world: &mut World, e: Entity) {
    for d in descendants(world, e) {
        let _ = world.despawn(d);
    }
    let _ = world.despawn(e);
}

/// First entity whose `Name` matches exactly.
pub fn find_by_name(world: &World, name: &str) -> Option<Entity> {
    world
        .query::<&Name>()
        .iter()
        .find(|(_, n)| n.0 == name)
        .map(|(e, _)| e)
}

/// First entity whose `Tag` matches exactly.
pub fn find_by_tag(world: &World, tag: &str) -> Option<Entity> {
    world
        .query::<&Tag>()
        .iter()
        .find(|(_, t)| t.0 == tag)
        .map(|(e, _)| e)
}

/// Entities that have no `Parent` (hierarchy roots), in stable spawn order.
pub fn roots(world: &World) -> Vec<Entity> {
    let mut with_parent: Vec<Entity> = world.query::<&Parent>().iter().map(|(e, _)| e).collect();
    with_parent.sort_unstable_by_key(|e| e.to_bits());
    let mut all: Vec<Entity> = world.iter().map(|er| er.entity()).collect();
    all.sort_unstable_by_key(|e| e.to_bits());
    all.retain(|e| !with_parent.contains(e));
    all
}

// ---------------------------------------------------------------------------
// World transforms (parent chain composition)
// ---------------------------------------------------------------------------

/// Compose a child's local transform onto a parent's transform (SRT order:
/// `world = T_p · R_p · S_p · T_c · R_c · S_c`).
pub fn compose_transforms(
    parent: &crate::components::Transform,
    child: &crate::components::Transform,
) -> crate::components::Transform {
    use crate::components::Transform;
    let rot = parent.quat() * child.quat();
    let scale = Vec3::new(
        parent.scale.x * child.scale.x,
        parent.scale.y * child.scale.y,
        parent.scale.z * child.scale.z,
    );
    let position = parent.quat() * (child.position * parent.scale) + parent.position;
    let e = rot.to_euler(glam::EulerRot::XYZ);
    Transform {
        position,
        rotation: Vec3::new(e.0.to_degrees(), e.1.to_degrees(), e.2.to_degrees()),
        scale,
    }
}

/// The entity's transform in world space (local transforms composed up the
/// `Parent` chain). Rendering, physics and picking all use this so children
/// actually follow their parents.
pub fn world_transform(world: &World, e: Entity) -> crate::components::Transform {
    use crate::components::Transform;
    let local = world.get::<&Transform>(e).map(|t| *t).unwrap_or_default();
    let parent = world.get::<&Parent>(e).ok().map(|p| p.0);
    match parent {
        Some(p) => compose_transforms(&world_transform(world, p), &local),
        None => local,
    }
}

/// Inverse of [`compose_transforms`]: the local transform that makes `world_t`
/// true under `parent`. Degenerate (zero) parent scales pass the world value
/// through untouched instead of dividing by zero.
fn decompose_transforms(
    parent: &crate::components::Transform,
    world_t: &crate::components::Transform,
) -> crate::components::Transform {
    use crate::components::Transform;
    let inv_q = parent.quat().conjugate();
    let rel = inv_q * (world_t.position - parent.position);
    let div = |a: f32, b: f32| if b.abs() < 1e-8 { a } else { a / b };
    let position = Vec3::new(
        div(rel.x, parent.scale.x),
        div(rel.y, parent.scale.y),
        div(rel.z, parent.scale.z),
    );
    let scale = Vec3::new(
        div(world_t.scale.x, parent.scale.x),
        div(world_t.scale.y, parent.scale.y),
        div(world_t.scale.z, parent.scale.z),
    );
    let e = (inv_q * world_t.quat()).to_euler(glam::EulerRot::XYZ);
    Transform {
        position,
        rotation: Vec3::new(e.0.to_degrees(), e.1.to_degrees(), e.2.to_degrees()),
        scale,
    }
}

/// Set an entity's *world* transform: decomposes into local space under the
/// current parent chain and writes the local `Transform`.
pub fn set_world_transform(world: &mut World, e: Entity, world_t: crate::components::Transform) {
    let parent = world.get::<&Parent>(e).ok().map(|p| p.0);
    let local = match parent {
        Some(p) => {
            let pw = world_transform(world, p);
            decompose_transforms(&pw, &world_t)
        }
        None => world_t,
    };
    world.insert_one(e, local).ok();
}

/// Human label for an entity used by the editor tree.
pub fn entity_label(world: &World, e: Entity) -> String {
    world
        .get::<&Name>(e)
        .map(|n| n.0.clone())
        .unwrap_or_else(|_| format!("Entity {}", e.to_bits().get()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hierarchy_ops() {
        let mut w = World::default();
        let a = w.spawn((Name("a".into()),));
        let b = w.spawn((Name("b".into()),));
        set_parent(&mut w, b, Some(a));
        assert_eq!(children(&w, a), vec![b]);
        assert_eq!(descendants(&w, a), vec![b]);
        set_parent(&mut w, b, None);
        assert!(children(&w, a).is_empty());
        set_parent(&mut w, b, Some(a));
        despawn_recursive(&mut w, a);
        assert!(w.get::<&Name>(b).is_err());
    }

    #[test]
    fn registry_roundtrip() {
        let mut reg = Registry::default();
        reg.register::<crate::components::Sprite>();
        let mut w = World::default();
        let e = w.spawn((crate::components::Sprite::default(),));
        let entry = reg.get("Sprite").unwrap();
        assert!((entry.has)(&w, e));
        let saved = (entry.save)(&w, e).unwrap();
        let e2 = w.spawn(());
        (entry.load)(&mut w, e2, &saved).unwrap();
        assert!((entry.has)(&w, e2));
        (entry.remove)(&mut w, e);
        assert!(!(entry.has)(&w, e));
    }

    #[test]
    fn world_transform_composes_chain() {
        use crate::components::Transform;
        let mut w = World::default();
        let a = w.spawn((
            Name("a".into()),
            Transform {
                position: Vec3::new(10.0, 0.0, 0.0),
                rotation: Vec3::new(0.0, 90.0, 0.0), // yaw 90°: +X → -Z
                scale: Vec3::new(2.0, 2.0, 2.0),
            },
        ));
        let b = w.spawn((
            Name("b".into()),
            Transform {
                position: Vec3::new(1.0, 0.0, 0.0),
                ..Default::default()
            },
        ));
        set_parent(&mut w, b, Some(a));
        let wt = world_transform(&w, b);
        // Parent yaw 90° maps local +X to world -Z, scaled by 2, plus +10 X.
        assert!((wt.position.x - 10.0).abs() < 1e-4, "x = {}", wt.position.x);
        assert!((wt.position.z + 2.0).abs() < 1e-4, "z = {}", wt.position.z);
        assert!((wt.position.y).abs() < 1e-4);
        assert_eq!(wt.scale, Vec3::new(2.0, 2.0, 2.0));

        // set_world_transform on the child decomposes back correctly.
        set_world_transform(
            &mut w,
            b,
            Transform {
                position: Vec3::new(10.0, 5.0, 0.0),
                ..Default::default()
            },
        );
        let wt = world_transform(&w, b);
        assert!((wt.position.x - 10.0).abs() < 1e-4);
        assert!((wt.position.y - 5.0).abs() < 1e-4);
        assert!((wt.position.z).abs() < 1e-4);
    }

    #[test]
    fn world_transform_deep_chain_and_rotation() {
        use crate::components::Transform;
        let mut w = World::default();
        let a = w.spawn((
            Name("a".into()),
            Transform {
                position: Vec3::new(0.0, 0.0, 0.0),
                rotation: Vec3::new(0.0, 0.0, 90.0), // roll 90°: +X → +Y
                ..Default::default()
            },
        ));
        let b = w.spawn((
            Name("b".into()),
            Transform {
                position: Vec3::new(1.0, 0.0, 0.0),
                ..Default::default()
            },
        ));
        let c = w.spawn((
            Name("c".into()),
            Transform {
                position: Vec3::new(1.0, 0.0, 0.0),
                ..Default::default()
            },
        ));
        set_parent(&mut w, b, Some(a));
        set_parent(&mut w, c, Some(b));
        let wt = world_transform(&w, c);
        // a's roll maps b's local +X to world +Y; c's local +X then adds
        // another world +Y (b's world rotation is also 90° roll).
        assert!((wt.position.y - 2.0).abs() < 1e-4, "y = {}", wt.position.y);
        assert!((wt.position.x).abs() < 1e-4);
        assert!((wt.position.z).abs() < 1e-4);
    }
}
