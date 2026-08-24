//! Physics: rapier-backed rigid bodies and colliders, one component set for
//! both 2D and 3D. The scene's [`Dimension`] selects the backend:
//!
//! * D2 — x/y plane, rotation about Z, `Transform.z` is untouched draw order.
//! * D3 — full translation + rotation.
//!
//! Sync model per tick: create/remove bodies for component changes → push
//! `Transform` into static/kinematic bodies → step → pull transforms back out
//! of dynamic bodies → drain contact events into [`CollisionPair`]s for the
//! rules system.
//!
//! Editor edits to physics components set a dirty flag; the next tick
//! rebuilds the affected bodies (cheap at editor scale).

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender};

use hecs::{Entity, World};
use rapier2d::prelude as r2;
use rapier3d::prelude as r3;

use crate::components::{BodyKind, Collider, ColliderShape, RigidBody, Transform};
use crate::math::Vec3;
use crate::rules::CollisionPair;
use crate::scene::Dimension;

const GRAVITY_Y: f32 = -9.81;

/// Collects `CollisionEvent`s from the pipeline into a channel.
struct Collector(Sender<r2::CollisionEvent>);

impl r2::EventHandler for Collector {
    fn handle_collision_event(
        &self,
        _bodies: &r2::RigidBodySet,
        _colliders: &r2::ColliderSet,
        event: r2::CollisionEvent,
        _pair: Option<&r2::ContactPair>,
    ) {
        let _ = self.0.send(event);
    }
    fn handle_contact_force_event(
        &self,
        _dt: r2::Real,
        _bodies: &r2::RigidBodySet,
        _colliders: &r2::ColliderSet,
        _pair: &r2::ContactPair,
        _total_force: r2::Real,
    ) {
    }
}

struct Collector3(Sender<r3::CollisionEvent>);

impl r3::EventHandler for Collector3 {
    fn handle_collision_event(
        &self,
        _bodies: &r3::RigidBodySet,
        _colliders: &r3::ColliderSet,
        event: r3::CollisionEvent,
        _pair: Option<&r3::ContactPair>,
    ) {
        let _ = self.0.send(event);
    }
    fn handle_contact_force_event(
        &self,
        _dt: r3::Real,
        _bodies: &r3::RigidBodySet,
        _colliders: &r3::ColliderSet,
        _pair: &r3::ContactPair,
        _total_force: r3::Real,
    ) {
    }
}

pub struct Physics {
    pub gravity: Vec3,
    pub enabled: bool,
    pub dimension: Dimension,
    dirty: bool,
    p2: Physics2,
    p3: Physics3,
    map2: HashMap<Entity, r2::RigidBodyHandle>,
    map3: HashMap<Entity, r3::RigidBodyHandle>,
    col2ent: HashMap<r2::ColliderHandle, Entity>,
    col3ent: HashMap<r3::ColliderHandle, Entity>,
    ev_tx2: Sender<r2::CollisionEvent>,
    ev_rx2: Receiver<r2::CollisionEvent>,
    ev_tx3: Sender<r3::CollisionEvent>,
    ev_rx3: Receiver<r3::CollisionEvent>,
}

struct Physics2 {
    params: r2::IntegrationParameters,
    islands: r2::IslandManager,
    broad: r2::BroadPhaseMultiSap,
    narrow: r2::NarrowPhase,
    bodies: r2::RigidBodySet,
    colliders: r2::ColliderSet,
    joints: r2::ImpulseJointSet,
    mjoints: r2::MultibodyJointSet,
    ccd: r2::CCDSolver,
    query: r2::QueryPipeline,
    pipeline: r2::PhysicsPipeline,
}

struct Physics3 {
    params: r3::IntegrationParameters,
    islands: r3::IslandManager,
    broad: r3::BroadPhaseMultiSap,
    narrow: r3::NarrowPhase,
    bodies: r3::RigidBodySet,
    colliders: r3::ColliderSet,
    joints: r3::ImpulseJointSet,
    mjoints: r3::MultibodyJointSet,
    ccd: r3::CCDSolver,
    query: r3::QueryPipeline,
    pipeline: r3::PhysicsPipeline,
}

impl Default for Physics {
    fn default() -> Self {
        Self::new(Dimension::D2)
    }
}

impl Physics {
    pub fn new(dimension: Dimension) -> Self {
        let (tx2, rx2) = std::sync::mpsc::channel();
        let (tx3, rx3) = std::sync::mpsc::channel();
        Self {
            gravity: Vec3::new(0.0, GRAVITY_Y, 0.0),
            enabled: true,
            dimension,
            dirty: true,
            p2: Physics2::new(),
            p3: Physics3::new(),
            map2: HashMap::new(),
            map3: HashMap::new(),
            col2ent: HashMap::new(),
            col3ent: HashMap::new(),
            ev_tx2: tx2,
            ev_rx2: rx2,
            ev_tx3: tx3,
            ev_rx3: rx3,
        }
    }

    pub fn set_dimension(&mut self, d: Dimension) {
        if d != self.dimension {
            self.dimension = d;
            self.request_rebuild();
        }
    }

    /// Editor signal: physics components changed, rebuild bodies.
    pub fn request_rebuild(&mut self) {
        self.dirty = true;
    }

    pub fn set_gravity(&mut self, g: Vec3) {
        self.gravity = g;
    }

    /// Run one physics tick; returns collision transitions for the rules.
    pub fn update(&mut self, world: &mut World, dt: f32) -> Vec<CollisionPair> {
        if self.dirty {
            self.clear_backends();
            self.dirty = false;
        }
        if !self.enabled {
            return Vec::new();
        }
        self.sync_bodies(world);
        self.push_transforms(world);
        self.step(dt);
        self.pull_transforms(world);
        self.drain_events()
    }

    fn clear_backends(&mut self) {
        self.p2 = Physics2::new();
        self.p3 = Physics3::new();
        self.map2.clear();
        self.map3.clear();
        self.col2ent.clear();
        self.col3ent.clear();
        while self.ev_rx2.try_recv().is_ok() {}
        while self.ev_rx3.try_recv().is_ok() {}
    }

    /// Create bodies for entities that gained physics components; remove
    /// bodies for entities that lost them (or were despawned).
    fn sync_bodies(&mut self, world: &mut World) {
        let mut desired: Vec<Entity> = Vec::new();
        for (e, (_rb, _c, _t)) in world
            .query::<(&RigidBody, Option<&Collider>, &Transform)>()
            .iter()
        {
            desired.push(e);
        }
        // Colliders without a rigid body act as static sensors/bodies.
        for (e, (_c, _t)) in world.query::<(&Collider, &Transform)>().iter() {
            if world.get::<&RigidBody>(e).is_err() {
                desired.push(e);
            }
        }

        let (existing, present): (Vec<Entity>, Vec<Entity>) = if self.dimension == Dimension::D2 {
            (
                self.map2.keys().copied().collect(),
                desired
                    .iter()
                    .copied()
                    .filter(|e| self.map2.contains_key(e))
                    .collect(),
            )
        } else {
            (
                self.map3.keys().copied().collect(),
                desired
                    .iter()
                    .copied()
                    .filter(|e| self.map3.contains_key(e))
                    .collect(),
            )
        };
        for e in existing {
            if !desired.contains(&e) {
                self.remove_body(e);
            }
        }
        for e in &desired {
            if !present.contains(e) {
                self.create_body(world, *e);
            }
        }
    }

    fn remove_body(&mut self, e: Entity) {
        if self.dimension == Dimension::D2 {
            if let Some(bh) = self.map2.remove(&e) {
                self.p2.bodies.remove(
                    bh,
                    &mut self.p2.islands,
                    &mut self.p2.colliders,
                    &mut self.p2.joints,
                    &mut self.p2.mjoints,
                    true,
                );
            }
            self.col2ent.retain(|_, ent| *ent != e);
        } else if let Some(bh) = self.map3.remove(&e) {
            self.p3.bodies.remove(
                bh,
                &mut self.p3.islands,
                &mut self.p3.colliders,
                &mut self.p3.joints,
                &mut self.p3.mjoints,
                true,
            );
            self.col3ent.retain(|_, ent| *ent != e);
        }
    }

    fn create_body(&mut self, world: &mut World, e: Entity) {
        let transform = world.get::<&Transform>(e).map(|t| *t).unwrap_or_default();
        let rb = world
            .get::<&RigidBody>(e)
            .ok()
            .map(|r| (*r).clone())
            .unwrap_or_default();
        let Some(col) = world.get::<&Collider>(e).ok().map(|c| (*c).clone()) else {
            return;
        };
        let position = transform.position;

        if self.dimension == Dimension::D2 {
            let builder = match rb.kind {
                BodyKind::Static => r2::RigidBodyBuilder::fixed(),
                BodyKind::Kinematic => r2::RigidBodyBuilder::kinematic_position_based(),
                BodyKind::Dynamic => r2::RigidBodyBuilder::dynamic(),
            }
            .translation(r2::Vector::new(position.x, position.y))
            .rotation(transform.rotation.z.to_radians())
            .linear_damping(rb.linear_damping)
            .angular_damping(rb.angular_damping);
            let body = self.p2.bodies.insert(builder);
            if let Some(b) = self.p2.bodies.get_mut(body) {
                if rb.kind == BodyKind::Dynamic {
                    b.set_gravity_scale(rb.gravity_scale, true);
                    b.lock_rotations(rb.lock_rotation, true);
                }
                b.wake_up(true);
            }
            for cb in collider_builders_2d(&col, rb.restitution, rb.friction) {
                let h = self
                    .p2
                    .colliders
                    .insert_with_parent(cb, body, &mut self.p2.bodies);
                self.col2ent.insert(h, e);
            }
            self.map2.insert(e, body);
        } else {
            let quat = transform.quat();
            let na_quat = r3::nalgebra::Quaternion::new(quat.w, quat.x, quat.y, quat.z);
            let builder = match rb.kind {
                BodyKind::Static => r3::RigidBodyBuilder::fixed(),
                BodyKind::Kinematic => r3::RigidBodyBuilder::kinematic_position_based(),
                BodyKind::Dynamic => r3::RigidBodyBuilder::dynamic(),
            }
            .translation(r3::Vector::new(position.x, position.y, position.z))
            .rotation(r3::nalgebra::Vector3::new(
                transform.rotation.x.to_radians(),
                transform.rotation.y.to_radians(),
                transform.rotation.z.to_radians(),
            ))
            .linear_damping(rb.linear_damping)
            .angular_damping(rb.angular_damping);
            let body = self.p3.bodies.insert(builder);
            if let Some(b) = self.p3.bodies.get_mut(body) {
                b.set_rotation(r3::nalgebra::Unit::new_normalize(na_quat), false);
                if rb.kind == BodyKind::Dynamic {
                    b.set_gravity_scale(rb.gravity_scale, true);
                    b.lock_rotations(rb.lock_rotation, true);
                }
                b.wake_up(true);
            }
            for cb in collider_builders_3d(&col, rb.restitution, rb.friction) {
                let h = self
                    .p3
                    .colliders
                    .insert_with_parent(cb, body, &mut self.p3.bodies);
                self.col3ent.insert(h, e);
            }
            self.map3.insert(e, body);
        }
    }

    // -----------------------------------------------------------------------
    // Per-tick sync
    // -----------------------------------------------------------------------

    fn push_transforms(&mut self, world: &World) {
        // Static and kinematic bodies follow their Transform (editor moves).
        // Dynamic bodies are only repositioned via `teleport`.
        if self.dimension == Dimension::D2 {
            for (e, bh) in self.map2.clone() {
                let Ok(rb) = world.get::<&RigidBody>(e) else {
                    continue;
                };
                if rb.kind == BodyKind::Dynamic {
                    continue;
                }
                let Ok(t) = world.get::<&Transform>(e) else {
                    continue;
                };
                if let Some(b) = self.p2.bodies.get_mut(bh) {
                    b.set_translation(r2::Vector::new(t.position.x, t.position.y), false);
                    b.set_rotation(
                        r2::nalgebra::UnitComplex::new(t.rotation.z.to_radians()),
                        false,
                    );
                }
            }
        } else {
            for (e, bh) in self.map3.clone() {
                let Ok(rb) = world.get::<&RigidBody>(e) else {
                    continue;
                };
                if rb.kind == BodyKind::Dynamic {
                    continue;
                }
                let Ok(t) = world.get::<&Transform>(e) else {
                    continue;
                };
                let quat = t.quat();
                if let Some(b) = self.p3.bodies.get_mut(bh) {
                    b.set_translation(
                        r3::Vector::new(t.position.x, t.position.y, t.position.z),
                        false,
                    );
                    b.set_rotation(
                        r3::nalgebra::Unit::new_normalize(r3::nalgebra::Quaternion::new(
                            quat.w, quat.x, quat.y, quat.z,
                        )),
                        false,
                    );
                }
            }
        }
    }

    fn step(&mut self, dt: f32) {
        let g = self.gravity;
        if self.dimension == Dimension::D2 {
            self.p2.params.dt = dt.min(0.05);
            let Physics2 {
                params,
                islands,
                broad,
                narrow,
                bodies,
                colliders,
                joints,
                mjoints,
                ccd,
                query,
                pipeline,
            } = &mut self.p2;
            let collector = Collector(self.ev_tx2.clone());
            pipeline.step(
                &r2::Vector::new(g.x, g.y),
                params,
                islands,
                broad,
                narrow,
                bodies,
                colliders,
                joints,
                mjoints,
                ccd,
                Some(query),
                &(),
                &collector,
            );
        } else {
            self.p3.params.dt = dt.min(0.05);
            let Physics3 {
                params,
                islands,
                broad,
                narrow,
                bodies,
                colliders,
                joints,
                mjoints,
                ccd,
                query,
                pipeline,
            } = &mut self.p3;
            let collector = Collector3(self.ev_tx3.clone());
            pipeline.step(
                &r3::Vector::new(g.x, g.y, g.z),
                params,
                islands,
                broad,
                narrow,
                bodies,
                colliders,
                joints,
                mjoints,
                ccd,
                Some(query),
                &(),
                &collector,
            );
        }
    }

    fn pull_transforms(&mut self, world: &mut World) {
        if self.dimension == Dimension::D2 {
            for (e, bh) in self.map2.clone() {
                let Some(b) = self.p2.bodies.get(bh) else {
                    continue;
                };
                let pos = b.translation();
                let rot = b.rotation();
                if let Ok(mut t) = world.get::<&mut Transform>(e) {
                    t.position.x = pos.x;
                    t.position.y = pos.y;
                    t.rotation.z = rot.angle().to_degrees();
                }
            }
        } else {
            for (e, bh) in self.map3.clone() {
                let Some(b) = self.p3.bodies.get(bh) else {
                    continue;
                };
                let pos = b.translation();
                let q = b.rotation().quaternion();
                if let Ok(mut t) = world.get::<&mut Transform>(e) {
                    t.position.x = pos.x;
                    t.position.y = pos.y;
                    t.position.z = pos.z;
                    let eu =
                        glam::Quat::from_xyzw(q.i, q.j, q.k, q.w).to_euler(glam::EulerRot::XYZ);
                    t.rotation = Vec3::new(eu.0.to_degrees(), eu.1.to_degrees(), eu.2.to_degrees());
                }
            }
        }
    }

    fn drain_events(&mut self) -> Vec<CollisionPair> {
        let mut out = Vec::new();
        if self.dimension == Dimension::D2 {
            while let Ok(ev) = self.ev_rx2.try_recv() {
                match ev {
                    r2::CollisionEvent::Started(c1, c2, _) => {
                        if let Some(pair) = self.pair2(c1, c2, true) {
                            out.push(pair);
                        }
                    }
                    r2::CollisionEvent::Stopped(c1, c2, _) => {
                        if let Some(pair) = self.pair2(c1, c2, false) {
                            out.push(pair);
                        }
                    }
                }
            }
        } else {
            while let Ok(ev) = self.ev_rx3.try_recv() {
                match ev {
                    r3::CollisionEvent::Started(c1, c2, _) => {
                        if let Some(pair) = self.pair3(c1, c2, true) {
                            out.push(pair);
                        }
                    }
                    r3::CollisionEvent::Stopped(c1, c2, _) => {
                        if let Some(pair) = self.pair3(c1, c2, false) {
                            out.push(pair);
                        }
                    }
                }
            }
        }
        out
    }

    fn pair2(
        &self,
        c1: r2::ColliderHandle,
        c2: r2::ColliderHandle,
        started: bool,
    ) -> Option<CollisionPair> {
        let a = *self.col2ent.get(&c1)?;
        let b = *self.col2ent.get(&c2)?;
        Some(CollisionPair { a, b, started })
    }

    fn pair3(
        &self,
        c1: r3::ColliderHandle,
        c2: r3::ColliderHandle,
        started: bool,
    ) -> Option<CollisionPair> {
        let a = *self.col3ent.get(&c1)?;
        let b = *self.col3ent.get(&c2)?;
        Some(CollisionPair { a, b, started })
    }

    // -----------------------------------------------------------------------
    // Rules-facing operations
    // -----------------------------------------------------------------------

    pub fn set_velocity(&mut self, e: Entity, v: Vec3, relative: bool) {
        if self.dimension == Dimension::D2 {
            if let Some(bh) = self.map2.get(&e)
                && let Some(b) = self.p2.bodies.get_mut(*bh)
            {
                let target = r2::Vector::new(v.x, v.y);
                let new_v = if relative {
                    b.linvel() + target
                } else {
                    target
                };
                b.set_linvel(new_v, true);
            }
        } else if let Some(bh) = self.map3.get(&e)
            && let Some(b) = self.p3.bodies.get_mut(*bh)
        {
            let target = r3::Vector::new(v.x, v.y, v.z);
            let new_v = if relative {
                b.linvel() + target
            } else {
                target
            };
            b.set_linvel(new_v, true);
        }
    }

    /// Set only the X component of linear velocity.
    pub fn set_vel_x(&mut self, e: Entity, x: f32) {
        if self.dimension == Dimension::D2 {
            if let Some(bh) = self.map2.get(&e)
                && let Some(b) = self.p2.bodies.get_mut(*bh)
            {
                let v = b.linvel();
                b.set_linvel(r2::Vector::new(x, v.y), true);
            }
        } else if let Some(bh) = self.map3.get(&e)
            && let Some(b) = self.p3.bodies.get_mut(*bh)
        {
            let v = *b.linvel();
            b.set_linvel(r3::Vector::new(x, v.y, v.z), true);
        }
    }

    /// Set only the Y component of linear velocity.
    pub fn set_vel_y(&mut self, e: Entity, y: f32) {
        if self.dimension == Dimension::D2 {
            if let Some(bh) = self.map2.get(&e)
                && let Some(b) = self.p2.bodies.get_mut(*bh)
            {
                let v = b.linvel();
                b.set_linvel(r2::Vector::new(v.x, y), true);
            }
        } else if let Some(bh) = self.map3.get(&e)
            && let Some(b) = self.p3.bodies.get_mut(*bh)
        {
            let v = *b.linvel();
            b.set_linvel(r3::Vector::new(v.x, y, v.z), true);
        }
    }

    pub fn apply_impulse(&mut self, e: Entity, v: Vec3) {
        if self.dimension == Dimension::D2 {
            if let Some(bh) = self.map2.get(&e)
                && let Some(b) = self.p2.bodies.get_mut(*bh)
            {
                b.apply_impulse(r2::Vector::new(v.x, v.y), true);
            }
        } else if let Some(bh) = self.map3.get(&e)
            && let Some(b) = self.p3.bodies.get_mut(*bh)
        {
            b.apply_impulse(r3::Vector::new(v.x, v.y, v.z), true);
        }
    }

    /// Teleport a body (rules `Translate`); wakes it.
    pub fn teleport(&mut self, e: Entity, pos: Vec3) {
        if self.dimension == Dimension::D2 {
            if let Some(bh) = self.map2.get(&e)
                && let Some(b) = self.p2.bodies.get_mut(*bh)
            {
                b.set_translation(r2::Vector::new(pos.x, pos.y), true);
            }
        } else if let Some(bh) = self.map3.get(&e)
            && let Some(b) = self.p3.bodies.get_mut(*bh)
        {
            b.set_translation(r3::Vector::new(pos.x, pos.y, pos.z), true);
        }
    }

    /// Editor: Transform edited directly — conservative rebuild.
    pub fn on_transform_edited(&mut self) {
        self.dirty = true;
    }
}

impl Physics2 {
    fn new() -> Self {
        Self {
            params: r2::IntegrationParameters::default(),
            islands: r2::IslandManager::new(),
            broad: r2::BroadPhaseMultiSap::new(),
            narrow: r2::NarrowPhase::new(),
            bodies: r2::RigidBodySet::new(),
            colliders: r2::ColliderSet::new(),
            joints: r2::ImpulseJointSet::new(),
            mjoints: r2::MultibodyJointSet::new(),
            ccd: r2::CCDSolver::new(),
            query: r2::QueryPipeline::new(),
            pipeline: r2::PhysicsPipeline::new(),
        }
    }
}

impl Physics3 {
    fn new() -> Self {
        Self {
            params: r3::IntegrationParameters::default(),
            islands: r3::IslandManager::new(),
            broad: r3::BroadPhaseMultiSap::new(),
            narrow: r3::NarrowPhase::new(),
            bodies: r3::RigidBodySet::new(),
            colliders: r3::ColliderSet::new(),
            joints: r3::ImpulseJointSet::new(),
            mjoints: r3::MultibodyJointSet::new(),
            ccd: r3::CCDSolver::new(),
            query: r3::QueryPipeline::new(),
            pipeline: r3::PhysicsPipeline::new(),
        }
    }
}

fn collider_builders_2d(c: &Collider, restitution: f32, friction: f32) -> Vec<r2::ColliderBuilder> {
    vec![
        match &c.shape {
            ColliderShape::Box { half } => r2::ColliderBuilder::cuboid(half.x.abs(), half.y.abs()),
            ColliderShape::Ball { r } => r2::ColliderBuilder::ball(r.abs()),
            ColliderShape::Capsule { half_height, r } => {
                r2::ColliderBuilder::capsule_y(half_height.abs(), r.abs())
            }
        }
        .sensor(c.sensor)
        .restitution(restitution)
        .friction(friction)
        .active_events(r2::ActiveEvents::COLLISION_EVENTS),
    ]
}

fn collider_builders_3d(c: &Collider, restitution: f32, friction: f32) -> Vec<r3::ColliderBuilder> {
    vec![
        match &c.shape {
            ColliderShape::Box { half } => {
                r3::ColliderBuilder::cuboid(half.x.abs(), half.y.abs(), half.z.abs())
            }
            ColliderShape::Ball { r } => r3::ColliderBuilder::ball(r.abs()),
            ColliderShape::Capsule { half_height, r } => {
                r3::ColliderBuilder::capsule_y(half_height.abs(), r.abs())
            }
        }
        .sensor(c.sensor)
        .restitution(restitution)
        .friction(friction)
        .active_events(r3::ActiveEvents::COLLISION_EVENTS),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{BodyKind, RigidBody};
    use crate::ecs::Tag;

    #[test]
    fn gravity_and_events_2d() {
        let mut world = World::default();
        let _floor = world.spawn((
            Transform {
                position: Vec3::new(0.0, -5.0, 0.0),
                ..Default::default()
            },
            RigidBody {
                kind: BodyKind::Static,
                ..Default::default()
            },
            Collider {
                shape: ColliderShape::Box {
                    half: Vec3::new(10.0, 0.5, 1.0),
                },
                sensor: false,
            },
            Tag("floor".into()),
        ));
        let ball = world.spawn((
            Transform {
                position: Vec3::new(0.0, 5.0, 0.0),
                ..Default::default()
            },
            RigidBody {
                kind: BodyKind::Dynamic,
                ..Default::default()
            },
            Collider {
                shape: ColliderShape::Ball { r: 0.5 },
                sensor: false,
            },
        ));

        let mut physics = Physics::new(Dimension::D2);
        let mut saw_start = false;
        let mut fell = false;
        for _ in 0..240 {
            let pairs = physics.update(&mut world, 1.0 / 60.0);
            if pairs.iter().any(|p| p.started) {
                saw_start = true;
            }
            let y = world.get::<&Transform>(ball).unwrap().position.y;
            if y < 4.0 {
                fell = true;
            }
        }
        assert!(fell, "ball should fall under gravity");
        assert!(saw_start, "collision events should fire on impact");
        let y = world.get::<&Transform>(ball).unwrap().position.y;
        assert!(
            (-4.2..=-3.4).contains(&y),
            "ball should rest on the floor, got {y}"
        );
    }

    #[test]
    fn set_velocity_2d() {
        let mut world = World::default();
        let ball = world.spawn((
            Transform::default(),
            RigidBody {
                kind: BodyKind::Dynamic,
                gravity_scale: 0.0,
                ..Default::default()
            },
            Collider {
                shape: ColliderShape::Ball { r: 0.5 },
                sensor: false,
            },
        ));
        let mut physics = Physics::new(Dimension::D2);
        physics.update(&mut world, 1.0 / 60.0);
        physics.set_velocity(ball, Vec3::new(3.0, 0.0, 0.0), false);
        physics.update(&mut world, 1.0 / 60.0);
        let x = world.get::<&Transform>(ball).unwrap().position.x;
        assert!(x > 0.01, "ball should move right, got {x}");
    }
}
