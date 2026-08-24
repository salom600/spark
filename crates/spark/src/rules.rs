//! Rules: spark's data-driven behavior system (the "scripting" layer).
//!
//! A rule is `on: Event, when: [conditions], run: [actions]`, stored in the
//! scene file on the entity it belongs to. Events arrive from the engine
//! (keys, collisions, timers, messages, clicks); conditions filter them
//! (cooldowns, once-flags, variable comparisons, chance); actions mutate the
//! world (spawn, destroy, move, play sounds, change variables, load scenes).
//!
//! Why not an interpreter? See `DECISIONS.md §4.3` — rules cover the
//! declarative 90% of game behavior with zero engine-language surface area,
//! hot-reload for free (they live in the scene file), and full determinism
//! (tests below). When a game outgrows rules, it graduates to Rust systems
//! against the same `World`.

use std::collections::HashMap;

use hecs::{Entity, World};
use serde::{Deserialize, Serialize};

use crate::assets::Assets;
use crate::audio::Audio;
use crate::components::Vars;
use crate::math::{Color, Vec3};
use crate::physics::Physics;

// ---------------------------------------------------------------------------
// Events / conditions / actions
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum RuleEvent {
    /// Fires once, shortly after the entity spawns.
    Start,
    /// Fires every simulation tick.
    Update,
    /// Fires every `secs` seconds (or once when `repeat` is false).
    Timer {
        secs: f32,
        #[serde(default = "yes")]
        repeat: bool,
    },
    /// Physical key name (`"Space"`, `"KeyA"`, `"ArrowLeft"`).
    KeyPressed(String),
    KeyHeld(String),
    KeyReleased(String),
    /// Named action from the project's input bindings.
    ActionPressed(String),
    /// Collisions with an optional partner tag filter.
    CollisionEnter {
        #[serde(default)]
        other: Option<String>,
    },
    CollisionExit {
        #[serde(default)]
        other: Option<String>,
    },
    /// Global broadcast (arrives the frame after it is sent).
    Message(String),
    /// Mouse clicked inside the entity's 2D sprite bounds.
    Clicked,
}

fn yes() -> bool {
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum VarScope {
    Entity,
    Global,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum CmpOp {
    Lt,
    Gt,
    Le,
    Ge,
    Eq,
    Ne,
}

impl CmpOp {
    fn check(&self, a: f64, b: f64) -> bool {
        match self {
            CmpOp::Lt => a < b,
            CmpOp::Gt => a > b,
            CmpOp::Le => a <= b,
            CmpOp::Ge => a >= b,
            CmpOp::Eq => (a - b).abs() < 1e-9,
            CmpOp::Ne => (a - b).abs() >= 1e-9,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Cond {
    /// Fire at most once per entity lifetime.
    Once,
    /// True while the named physical key is held.
    KeyHeld(String),
    /// True while the named physical key is NOT held.
    KeyNotHeld(String),
    /// At most once per `f32` seconds.
    Cooldown(f32),
    /// Compare a variable (entity-local or global).
    Var {
        scope: VarScope,
        name: String,
        op: CmpOp,
        value: f64,
    },
    /// Fire with probability `p` (deterministic per entity/frame).
    Chance(f32),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Action {
    Log(String),
    DestroySelf,
    /// Destroy the collision partner (only valid in collision rules).
    DestroyOther,
    /// Destroy every entity carrying this tag (useful for resets).
    DestroyTagged(String),
    /// Spawn a prefab (`.ron` entity record) relative to this entity.
    Spawn {
        prefab: String,
        #[serde(default)]
        offset: Vec3,
    },
    SetVelocity {
        v: Vec3,
        #[serde(default)]
        relative: bool,
    },
    /// Set only the horizontal component of velocity (keeps fall speed).
    SetVelX {
        x: f32,
    },
    /// Set only the vertical component of velocity (jumping).
    SetVelY {
        y: f32,
    },
    /// Teleport (absolute position, wakes the body).
    Teleport {
        to: Vec3,
    },
    ApplyImpulse {
        v: Vec3,
    },
    Translate {
        by: Vec3,
    },
    Rotate {
        by_deg: Vec3,
    },
    SetColor(Color),
    SetVar {
        scope: VarScope,
        name: String,
        value: f64,
    },
    AddVar {
        scope: VarScope,
        name: String,
        delta: f64,
    },
    PlaySound {
        sound: String,
        #[serde(default = "default_vol")]
        volume: f32,
    },
    PlayMusic {
        track: String,
        #[serde(default = "default_vol")]
        volume: f32,
    },
    StopMusic,
    SetGravity {
        g: Vec3,
    },
    CameraFollowMe {
        #[serde(default = "default_lerp")]
        lerp: f32,
    },
    /// Load another scene (project-relative path) at the end of the tick.
    LoadScene(String),
    /// Broadcast a message; other entities receive it next tick.
    SendMessage(String),
    ToggleVisible,
    SetVisible(#[serde(default = "yes")] bool),
    /// Stop play mode / exit the game.
    Quit,
}

fn default_vol() -> f32 {
    0.8
}
fn default_lerp() -> f32 {
    0.1
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Rule {
    pub on: RuleEvent,
    #[serde(default)]
    pub when: Vec<Cond>,
    pub run: Vec<Action>,
    #[serde(default = "yes")]
    pub enabled: bool,
}

impl Rule {
    /// One-line description for editor lists and logs.
    pub fn summary(&self) -> String {
        format!("on {} → {} action(s)", self.on.describe(), self.run.len())
    }
}

impl RuleEvent {
    pub fn describe(&self) -> String {
        match self {
            RuleEvent::Start => "Start".into(),
            RuleEvent::Update => "Update".into(),
            RuleEvent::Timer { secs, repeat } => {
                format!("Timer {secs}s{}", if *repeat { " ∞" } else { "" })
            }
            RuleEvent::KeyPressed(k) => format!("Key {k} pressed"),
            RuleEvent::KeyHeld(k) => format!("Key {k} held"),
            RuleEvent::KeyReleased(k) => format!("Key {k} released"),
            RuleEvent::ActionPressed(a) => format!("Action {a}"),
            RuleEvent::CollisionEnter { other: Some(t) } => format!("Hit {t}"),
            RuleEvent::CollisionEnter { other: None } => "Hit any".into(),
            RuleEvent::CollisionExit { other: Some(t) } => format!("Left {t}"),
            RuleEvent::CollisionExit { other: None } => "Left any".into(),
            RuleEvent::Message(m) => format!("Message \"{m}\""),
            RuleEvent::Clicked => "Clicked".into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Collision pairs (produced by the physics system each tick)
// ---------------------------------------------------------------------------

/// One physics contact/intersection transition.
#[derive(Clone, Copy, Debug)]
pub struct CollisionPair {
    pub a: Entity,
    pub b: Entity,
    pub started: bool,
}

// ---------------------------------------------------------------------------
// Runtime + executor
// ---------------------------------------------------------------------------

/// Per-entity rule bookkeeping and cross-tick requests.
#[derive(Default)]
pub struct RuleRuntime {
    fired_once: HashMap<(Entity, u32), bool>,
    cooldowns: HashMap<(Entity, u32), f32>,
    timers: HashMap<(Entity, u32), f32>,
    timer_done: HashMap<(Entity, u32), bool>,
    /// Entities spawned since the last pass (fire `Start` next).
    fresh: Vec<Entity>,
    pub(crate) incoming: Vec<String>,
    pub camera_follow: Option<(Entity, f32)>,
    pub load_scene_request: Option<String>,
    pub quit_requested: bool,
}

impl RuleRuntime {
    pub fn mark_fresh(&mut self, e: Entity) {
        self.fresh.push(e);
    }

    /// Queue a message for delivery next tick (also used by Rust game code).
    pub fn send_message(&mut self, msg: impl Into<String>) {
        self.incoming.push(msg.into());
    }

    /// Reset everything (scene reload / play-mode restart).
    pub fn clear(&mut self) {
        self.fired_once.clear();
        self.cooldowns.clear();
        self.timers.clear();
        self.timer_done.clear();
        self.fresh.clear();
        self.incoming.clear();
        self.camera_follow = None;
        self.load_scene_request = None;
        self.quit_requested = false;
    }
}

/// Everything an action may touch.
pub struct ActionCtx<'a> {
    pub world: &'a mut World,
    pub globals: &'a mut HashMap<String, f64>,
    pub entity: Entity,
    pub other: Option<Entity>,
    pub assets: &'a mut Assets,
    pub audio: &'a mut Audio,
    pub physics: &'a mut Physics,
    pub messages_out: &'a mut Vec<String>,
    pub spawned: &'a mut Vec<Entity>,
    pub destroy_queue: &'a mut Vec<Entity>,
    pub camera_follow: &'a mut Option<(Entity, f32)>,
    pub load_scene: &'a mut Option<String>,
    pub quit: &'a mut bool,
    /// Mouse position in world space (2D scenes), if a camera exists.
    pub mouse_world: Option<crate::math::Vec2>,
}

/// Deterministic pseudo-random (xorshift) — stable across platforms, good
/// enough for game chance gates and reproducible tests.
fn chance(seed: u64) -> f32 {
    let mut x = seed | 1;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    (x % 10_000) as f32 / 10_000.0
}

/// Execute one entity's rules for this tick.
///
/// `collisions` are this tick's transitions; `keys`/`mouse` come from input.
pub fn run_rules(
    rt: &mut RuleRuntime,
    ctx: &mut ActionCtx,
    rules: &[Rule],
    collisions: &[CollisionPair],
    input: &crate::input::Input,
    dt: f32,
) {
    let entity = ctx.entity;
    let mut fired: Vec<u32> = Vec::new();

    for (idx, rule) in rules.iter().enumerate() {
        if !rule.enabled {
            continue;
        }
        // Event match.
        let event_hit = match &rule.on {
            RuleEvent::Start => rt.fresh.contains(&entity),
            RuleEvent::Update => true,
            RuleEvent::Timer { secs, repeat } => {
                if rt
                    .timer_done
                    .get(&(entity, idx as u32))
                    .copied()
                    .unwrap_or(false)
                {
                    false
                } else {
                    let t = rt.timers.entry((entity, idx as u32)).or_insert(0.0);
                    *t += dt;
                    if *t >= *secs {
                        if *repeat {
                            *t = 0.0;
                        } else {
                            rt.timer_done.insert((entity, idx as u32), true);
                        }
                        true
                    } else {
                        false
                    }
                }
            }
            RuleEvent::KeyPressed(k) => input.key_pressed(k),
            RuleEvent::KeyHeld(k) => input.key_held(k),
            RuleEvent::KeyReleased(k) => input.key_released(k),
            RuleEvent::ActionPressed(a) => input.action_pressed(a),
            RuleEvent::CollisionEnter { other } => {
                hit_partner(ctx.world, collisions, entity, other, true)
            }
            RuleEvent::CollisionExit { other } => {
                hit_partner(ctx.world, collisions, entity, other, false)
            }
            RuleEvent::Message(m) => rt.incoming.iter().any(|msg| msg == m),
            RuleEvent::Clicked => {
                let Some(m) = ctx.mouse_world else { continue };
                clicked_entity(ctx.world, entity, m)
            }
        };
        if !event_hit {
            continue;
        }

        // Conditions.
        let mut ok = true;
        for cond in &rule.when {
            ok &= match cond {
                Cond::Once => !rt
                    .fired_once
                    .get(&(entity, idx as u32))
                    .copied()
                    .unwrap_or(false),
                Cond::KeyHeld(k) => input.key_held(k),
                Cond::KeyNotHeld(k) => !input.key_held(k),
                Cond::Cooldown(secs) => {
                    let c = rt.cooldowns.entry((entity, idx as u32)).or_insert(0.0);
                    *c <= 0.0 && {
                        *c = *secs;
                        true
                    }
                }
                Cond::Var {
                    scope,
                    name,
                    op,
                    value,
                } => {
                    let v = read_var(ctx.world, ctx.globals, entity, *scope, name);
                    op.check(v, *value)
                }
                Cond::Chance(p) => {
                    chance(entity.to_bits().get() ^ idx as u64 ^ frame_seed(ctx)) < *p
                }
            };
            if !ok {
                break;
            }
        }
        if !ok {
            continue;
        }

        fired.push(idx as u32);
        for action in &rule.run {
            run_action(ctx, action);
        }
    }

    // Bookkeeping for fired rules.
    for idx in fired {
        rt.fired_once.insert((entity, idx), true);
    }
}

fn frame_seed(ctx: &ActionCtx) -> u64 {
    ctx.globals.get("_frame").copied().unwrap_or(0.0) as u64
}

/// Tag-filtered collision partner lookup (sets `ctx.other` as a side effect
/// for `DestroyOther`).
fn hit_partner(
    world: &World,
    collisions: &[CollisionPair],
    entity: Entity,
    tag_filter: &Option<String>,
    started: bool,
) -> bool {
    for pair in collisions {
        if pair.started != started {
            continue;
        }
        let partner = if pair.a == entity {
            pair.b
        } else if pair.b == entity {
            pair.a
        } else {
            continue;
        };
        let tag_matches = match tag_filter {
            Some(t) => world
                .get::<&crate::ecs::Tag>(partner)
                .map(|tag| tag.0 == *t)
                .unwrap_or(false),
            None => true,
        };
        if tag_matches {
            return true;
        }
    }
    false
}

fn partner_of(collisions: &[CollisionPair], entity: Entity) -> Option<Entity> {
    collisions.iter().find_map(|p| {
        if p.a == entity {
            Some(p.b)
        } else if p.b == entity {
            Some(p.a)
        } else {
            None
        }
    })
}

fn clicked_entity(world: &World, entity: Entity, mouse: crate::math::Vec2) -> bool {
    let (Ok(tr), Ok(sp)) = (
        world.get::<&crate::components::Transform>(entity),
        world.get::<&crate::components::Sprite>(entity),
    ) else {
        return false;
    };
    let half = sp.size * tr.scale.truncate() * 0.5;
    let p = tr.position;
    mouse.x >= p.x - half.x
        && mouse.x <= p.x + half.x
        && mouse.y >= p.y - half.y
        && mouse.y <= p.y + half.y
}

fn read_var(
    world: &World,
    globals: &HashMap<String, f64>,
    e: Entity,
    scope: VarScope,
    name: &str,
) -> f64 {
    match scope {
        VarScope::Global => globals.get(name).copied().unwrap_or(0.0),
        VarScope::Entity => world
            .get::<&Vars>(e)
            .map(|v| v.0.get(name).copied().unwrap_or(0.0))
            .unwrap_or(0.0),
    }
}

fn write_var(
    world: &mut World,
    globals: &mut HashMap<String, f64>,
    e: Entity,
    scope: VarScope,
    name: &str,
    f: impl Fn(f64) -> f64,
) {
    match scope {
        VarScope::Global => {
            let v = globals.entry(name.to_string()).or_insert(0.0);
            *v = f(*v);
        }
        VarScope::Entity => {
            if let Ok(mut vars) = world.get::<&mut Vars>(e) {
                let v = vars.0.entry(name.to_string()).or_insert(0.0);
                *v = f(*v);
            }
        }
    }
}

fn run_action(ctx: &mut ActionCtx, action: &Action) {
    let entity = ctx.entity;
    match action {
        Action::Log(msg) => log::info!("[rules] {msg}"),
        Action::DestroySelf => {
            if !ctx.destroy_queue.contains(&entity) {
                ctx.destroy_queue.push(entity);
            }
        }
        Action::DestroyOther => {
            if let Some(o) = ctx.other
                && !ctx.destroy_queue.contains(&o)
            {
                ctx.destroy_queue.push(o);
            }
        }
        Action::DestroyTagged(tag) => {
            let tagged: Vec<Entity> = ctx
                .world
                .query::<&crate::ecs::Tag>()
                .iter()
                .filter(|(_, t)| t.0 == *tag)
                .map(|(e, _)| e)
                .collect();
            for t in tagged {
                if !ctx.destroy_queue.contains(&t) {
                    ctx.destroy_queue.push(t);
                }
            }
        }
        Action::Spawn { prefab, offset } => {
            if let Some(e) =
                crate::scene::spawn_prefab(ctx.world, ctx.assets, prefab, Some(entity), *offset)
            {
                ctx.spawned.push(e);
            } else {
                log::warn!("[rules] prefab not found: {prefab}");
            }
        }
        Action::SetVelocity { v, relative } => ctx.physics.set_velocity(entity, *v, *relative),
        Action::SetVelX { x } => ctx.physics.set_vel_x(entity, *x),
        Action::SetVelY { y } => ctx.physics.set_vel_y(entity, *y),
        Action::Teleport { to } => {
            if let Ok(mut t) = ctx.world.get::<&mut crate::components::Transform>(entity) {
                t.position = *to;
            }
            ctx.physics.teleport(entity, *to);
        }
        Action::ApplyImpulse { v } => ctx.physics.apply_impulse(entity, *v),
        Action::Translate { by } => {
            if let Ok(mut t) = ctx.world.get::<&mut crate::components::Transform>(entity) {
                t.position += *by;
                ctx.physics.teleport(entity, t.position);
            }
        }
        Action::Rotate { by_deg } => {
            if let Ok(mut t) = ctx.world.get::<&mut crate::components::Transform>(entity) {
                t.rotation += *by_deg;
            }
        }
        Action::SetColor(c) => {
            if let Ok(mut s) = ctx.world.get::<&mut crate::components::Sprite>(entity) {
                s.color = *c;
            }
        }
        Action::SetVar { scope, name, value } => {
            write_var(ctx.world, ctx.globals, entity, *scope, name, |_| *value);
        }
        Action::AddVar { scope, name, delta } => {
            write_var(ctx.world, ctx.globals, entity, *scope, name, |v| v + delta);
        }
        Action::PlaySound { sound, volume } => {
            if let Some(bytes) = ctx.assets.sound(sound) {
                ctx.audio.play_bytes(&bytes, *volume);
            } else {
                log::warn!("[rules] sound not found: {sound}");
            }
        }
        Action::PlayMusic { track, volume } => {
            if let Some(bytes) = ctx.assets.sound(track) {
                ctx.audio.play_music(&bytes, *volume);
            }
        }
        Action::StopMusic => ctx.audio.stop_music(),
        Action::SetGravity { g } => ctx.physics.set_gravity(*g),
        Action::CameraFollowMe { lerp } => *ctx.camera_follow = Some((entity, *lerp)),
        Action::LoadScene(path) => *ctx.load_scene = Some(path.clone()),
        Action::SendMessage(m) => ctx.messages_out.push(m.clone()),
        Action::ToggleVisible => {
            if let Ok(mut v) = ctx.world.get::<&mut crate::components::Visible>(entity) {
                v.0 = !v.0;
            } else {
                ctx.world
                    .insert_one(entity, crate::components::Visible(false))
                    .ok();
            }
        }
        Action::SetVisible(vis) => {
            if let Ok(mut v) = ctx.world.get::<&mut crate::components::Visible>(entity) {
                v.0 = *vis;
            } else {
                ctx.world
                    .insert_one(entity, crate::components::Visible(*vis))
                    .ok();
            }
        }
        Action::Quit => *ctx.quit = true,
    }
}

/// Resolve the collision partner for `DestroyOther` before running actions.
/// Called by the engine between event matching and action execution via
/// [`run_rules`] (which sets `ctx.other` for collision events).
pub fn set_partner(ctx: &mut ActionCtx, collisions: &[CollisionPair]) {
    if ctx.other.is_none() {
        ctx.other = partner_of(collisions, ctx.entity);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{RulesComp, Transform, Vars};

    /// Test harness: keeps all ActionCtx borrows alive in one scope.
    struct Harness {
        globals: HashMap<String, f64>,
        assets: Assets,
        audio: Audio,
        physics: Physics,
        rt: RuleRuntime,
        spawned: Vec<Entity>,
        destroy_queue: Vec<Entity>,
        messages_out: Vec<String>,
        camera_follow: Option<(Entity, f32)>,
        load_scene: Option<String>,
        quit: bool,
    }

    impl Harness {
        fn new() -> Self {
            let tmp = std::env::temp_dir().join("spark_rules_test");
            Self {
                globals: HashMap::new(),
                assets: Assets::new(&tmp),
                audio: Audio::new(),
                physics: Physics::default(),
                rt: RuleRuntime::default(),
                spawned: Vec::new(),
                destroy_queue: Vec::new(),
                messages_out: Vec::new(),
                camera_follow: None,
                load_scene: None,
                quit: false,
            }
        }

        fn run(
            &mut self,
            world: &mut World,
            e: Entity,
            collisions: &[CollisionPair],
            input: &crate::input::Input,
            dt: f32,
        ) {
            let rules: Vec<Rule> = world
                .get::<&RulesComp>(e)
                .map(|r| r.rules.clone())
                .unwrap_or_default();
            let mut ctx = ActionCtx {
                world,
                globals: &mut self.globals,
                entity: e,
                other: None,
                assets: &mut self.assets,
                audio: &mut self.audio,
                physics: &mut self.physics,
                messages_out: &mut self.messages_out,
                spawned: &mut self.spawned,
                destroy_queue: &mut self.destroy_queue,
                camera_follow: &mut self.camera_follow,
                load_scene: &mut self.load_scene,
                quit: &mut self.quit,
                mouse_world: None,
            };
            set_partner(&mut ctx, collisions);
            run_rules(&mut self.rt, &mut ctx, &rules, collisions, input, dt);
        }
    }

    #[test]
    fn parse_and_run() {
        let rule: Rule = ron::from_str(
            r#"(on: KeyPressed("Space"), when: [Var(scope: Entity, name: "grounded", op: Eq, value: 1)], run: [SetVar(scope: Entity, name: "grounded", value: 0), Log("jump!")])"#,
        )
        .unwrap();
        assert!(matches!(rule.on, RuleEvent::KeyPressed(_)));

        let mut world = World::default();
        let e = world.spawn((
            Transform::default(),
            Vars(HashMap::from([("grounded".to_string(), 1.0)])),
            RulesComp { rules: vec![rule] },
        ));

        let mut input = crate::input::Input::new();
        input.on_key(
            winit::keyboard::KeyCode::Space,
            winit::event::ElementState::Pressed,
        );

        let mut h = Harness::new();
        h.run(&mut world, e, &[], &input, 0.016);
        assert_eq!(world.get::<&Vars>(e).unwrap().0["grounded"], 0.0);
    }

    #[test]
    fn collision_destroy_other() {
        let mut world = World::default();
        let coin = world.spawn((crate::ecs::Tag("coin".into()), Transform::default()));
        let player = world.spawn((
            Transform::default(),
            RulesComp {
                rules: vec![Rule {
                    on: RuleEvent::CollisionEnter {
                        other: Some("coin".to_string()),
                    },
                    when: vec![],
                    run: vec![
                        Action::DestroyOther,
                        Action::AddVar {
                            scope: VarScope::Global,
                            name: "coins".to_string(),
                            delta: 1.0,
                        },
                    ],
                    enabled: true,
                }],
            },
        ));

        let pairs = vec![CollisionPair {
            a: player,
            b: coin,
            started: true,
        }];
        let mut h = Harness::new();
        h.run(
            &mut world,
            player,
            &pairs,
            &crate::input::Input::new(),
            0.016,
        );
        for d in h.destroy_queue.clone() {
            let _ = world.despawn(d);
        }
        assert!(world.get::<&Transform>(coin).is_err());
        assert_eq!(h.globals["coins"], 1.0);
    }

    #[test]
    fn timer_and_message() {
        let mut world = World::default();
        let e = world.spawn((
            Transform::default(),
            RulesComp {
                rules: vec![
                    Rule {
                        on: RuleEvent::Timer {
                            secs: 0.5,
                            repeat: false,
                        },
                        when: vec![],
                        run: vec![Action::SendMessage("tick".into())],
                        enabled: true,
                    },
                    Rule {
                        on: RuleEvent::Message("tick".to_string()),
                        when: vec![Cond::Once],
                        run: vec![Action::SetVar {
                            scope: VarScope::Global,
                            name: "done".to_string(),
                            value: 1.0,
                        }],
                        enabled: true,
                    },
                ],
            },
        ));

        let mut h = Harness::new();
        let empty = crate::input::Input::new();
        for frame in 0..35 {
            h.globals.insert("_frame".to_string(), frame as f64);
            h.run(&mut world, e, &[], &empty, 0.016);
            // deliver messages next tick
            for m in h.messages_out.drain(..) {
                h.rt.incoming.push(m);
            }
        }
        assert_eq!(h.globals.get("done"), Some(&1.0));
    }
}
