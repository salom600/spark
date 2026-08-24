//! # spark
//!
//! A lightweight data-driven game engine: unified 2D/3D renderer (wgpu),
//! integrated editor, ECS scenes in human-readable RON, rapier physics,
//! rodio audio, and rule-based behavior — in a core of well under 10k lines.
//!
//! Start with [`app::Engine`] (own everything, tick it) or
//! [`app::run_game`] (run a project as a game). The editor binary in
//! `crates/spark_editor` shows every subsystem in action.
//!
//! ## Component authoring in three lines
//!
//! ```ignore
//! #[derive(spark_macros::ComponentDef, Clone, Default, Serialize, Deserialize)]
//! struct Health { value: f32, max: f32 }
//! // then: registry.register::<Health>();
//! ```
//!
//! That single derive gives the component scene serialization, an egui
//! inspector, cloning, and editor "add component" support.

extern crate self as spark;

pub mod reexport {
    //! Crates the engine re-exports so games don't pin their own versions.
    pub use egui;
    pub use hecs;
}

pub mod app;
pub mod assets;
pub mod audio;
pub mod cmd;
pub mod components;
pub mod ecs;
pub mod input;
pub mod math;
pub mod physics;
pub mod project;
pub mod render;
pub mod rules;
pub mod scene;

pub mod prelude {
    //! Everything a game crate usually wants.
    pub use crate::app::{Engine, FrameStats, HudFn};
    pub use crate::assets::{AssetKind, AssetRef, Assets};
    pub use crate::audio::Audio;
    pub use crate::cmd::{Command, CommandCtx, CommandStack};
    pub use crate::components::*;
    pub use crate::ecs::{self, ComponentDef, Registry};
    pub use crate::input::{Binding, Input};
    pub use crate::math::{Color, EulerRot, Mat4, Quat, Vec2, Vec3, Vec4};
    pub use crate::physics::Physics;
    pub use crate::project::Project;
    pub use crate::rules::{Action, Cond, Rule, RuleEvent, RuleRuntime, VarScope};
    pub use crate::scene::{Dimension, EntityRecord, Scene};
    pub use spark_macros::ComponentDef;
}
