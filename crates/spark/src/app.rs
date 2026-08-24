//! The engine: one struct that owns every subsystem and one `tick` that runs
//! them in a fixed order — the same order in editor play mode, exported
//! games, and headless CI tests.
//!
//! ```text
//! assets hot-reload → audio housekeeping → physics step (+collision events)
//! → rules pass → deferred destroy/spawn → camera follow → music autoplay
//! → scene-swap / quit requests
//! ```

use std::path::{Path, PathBuf};
use std::time::Instant;

use hecs::Entity;

use crate::assets::Assets;
use crate::audio::Audio;
use crate::components::{Camera, Music, Transform};
use crate::ecs::{self, Registry};
use crate::input::Input;
use crate::math::{Vec2, Vec3};
use crate::physics::Physics;
use crate::project::Project;
use crate::render::{FrameDraw, Renderer, build_frame_draw};
use crate::rules::{ActionCtx, RuleRuntime, run_rules, set_partner};
use crate::scene::{Scene, default_registry, load_scene_file};

/// Game HUD hook: receives the egui context and engine each frame.
pub type HudFn = Box<dyn FnMut(&egui::Context, &Engine) + Send>;

pub struct Engine<'window> {
    pub scene: Scene,
    pub assets: Assets,
    pub audio: Audio,
    pub input: Input,
    pub physics: Physics,
    pub rules: RuleRuntime,
    pub registry: Registry,
    pub renderer: Option<Renderer<'window>>,
    pub project: Option<Project>,
    pub frame: u64,
    /// Size of the active viewport in physical pixels (for mouse→world).
    pub viewport_px: Vec2,
    pub hud: Option<HudFn>,
    /// Track currently playing (music autoplay bookkeeping).
    pub playing_track: Option<String>,
    pub(crate) last_instant: Instant,
    pub stats: FrameStats,
}

#[derive(Clone, Copy, Default)]
pub struct FrameStats {
    pub fps: f32,
    pub dt: f32,
}

impl Engine<'static> {
    /// Headless engine (no window, no GPU) — CI tests and deterministic
    /// simulation.
    pub fn headless(project_dir: &Path) -> anyhow::Result<Self> {
        Self::core(Some(project_dir), None)
    }

    /// A headless engine with no project (empty scene) — unit tests.
    pub fn headless_empty() -> Self {
        Self::core(None, None).expect("headless engine construction cannot fail")
    }
}

impl<'window> Engine<'window> {
    fn core(
        project_dir: Option<&Path>,
        renderer: Option<Renderer<'window>>,
    ) -> anyhow::Result<Self> {
        let root = project_dir
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let project = match project_dir {
            Some(_) => Some(Project::load_dir(&root)?),
            None => None,
        };

        let assets = Assets::new(&root);
        let registry = default_registry();

        let mut engine = Engine {
            scene: Scene::default(),
            assets,
            audio: Audio::new(),
            input: Input::new(),
            physics: Physics::new(project.as_ref().map(|p| p.dimension).unwrap_or_default()),
            rules: RuleRuntime::default(),
            registry,
            renderer,
            project: project.clone(),
            frame: 0,
            viewport_px: Vec2::new(1280.0, 720.0),
            hud: None,
            playing_track: None,
            last_instant: Instant::now(),
            stats: FrameStats::default(),
        };

        if let Some(p) = &project {
            engine.input.set_actions(p.input.clone());
            engine.load_scene(&p.main_scene)?;
        }
        Ok(engine)
    }

    /// Windowed engine bound to a window surface, with a loaded project.
    pub fn windowed(
        project_dir: &Path,
        window: &'window winit::window::Window,
    ) -> anyhow::Result<Self> {
        let renderer = Renderer::new(window)?;
        let mut engine = Self::core(Some(project_dir), Some(renderer))?;
        let (w, h) = engine.renderer.as_ref().unwrap().size();
        engine.viewport_px = Vec2::new(w as f32, h as f32);
        Ok(engine)
    }

    /// Editor entry: engine with a renderer but no project yet.
    pub fn editor(window: &'window winit::window::Window) -> anyhow::Result<Self> {
        let renderer = Renderer::new(window)?;
        Self::core(None, Some(renderer))
    }

    /// Open (or swap) the loaded project + main scene at runtime.
    pub fn open_project(&mut self, dir: &Path) -> anyhow::Result<()> {
        let project = Project::load_dir(dir)?;
        self.project = Some(project.clone());
        self.assets = Assets::new(dir);
        self.input.set_actions(project.input.clone());
        self.physics = Physics::new(project.dimension);
        self.load_scene(&project.main_scene)?;
        Ok(())
    }

    /// Load a scene (project-relative path), resetting runtime state.
    pub fn load_scene(&mut self, path: &str) -> anyhow::Result<()> {
        let fs = self.assets.root().join(path);
        let scene = load_scene_file(&fs, &self.registry)?;
        self.scene = scene;
        self.rules.clear();
        self.playing_track = None;
        self.mark_all_fresh();
        Ok(())
    }

    fn mark_all_fresh(&mut self) {
        let entities: Vec<Entity> = self.scene.world.iter().map(|er| er.entity()).collect();
        for e in entities {
            self.rules.mark_fresh(e);
        }
    }

    /// Delta time since the last call (clamped to avoid the spiral of death).
    pub fn take_dt(&mut self) -> f32 {
        let now = Instant::now();
        let dt = now.duration_since(self.last_instant).as_secs_f32();
        self.last_instant = now;
        dt.clamp(0.0, 0.1)
    }

    /// One full simulation tick (order documented at module top).
    pub fn tick(&mut self, dt: f32) {
        self.frame += 1;
        self.scene
            .globals
            .insert("_frame".into(), self.frame as f64);
        self.stats.dt = dt;
        if dt > 0.0 {
            self.stats.fps = self.stats.fps * 0.9 + (1.0 / dt) * 0.1;
        }

        self.assets.update();
        self.audio.update();
        let collisions = self.physics.update(&mut self.scene.world, dt);

        // Rules pass over a snapshot (actions mutate the world).
        let mouse_world = self.mouse_world();
        let mut messages_out = Vec::new();
        let mut spawned = Vec::new();
        let mut destroy_queue = Vec::new();
        let mut camera_follow = self.rules.camera_follow;
        let mut load_scene = self.rules.load_scene_request.take();
        let mut quit = self.rules.quit_requested;

        let entities: Vec<(Entity, Vec<crate::rules::Rule>)> = self
            .scene
            .world
            .query::<&crate::components::RulesComp>()
            .iter()
            .map(|(e, r)| (e, r.rules.clone()))
            .collect();

        for (e, rules) in entities {
            if destroy_queue.contains(&e) {
                continue;
            }
            let mut ctx = ActionCtx {
                world: &mut self.scene.world,
                globals: &mut self.scene.globals,
                entity: e,
                other: None,
                assets: &mut self.assets,
                audio: &mut self.audio,
                physics: &mut self.physics,
                messages_out: &mut messages_out,
                spawned: &mut spawned,
                destroy_queue: &mut destroy_queue,
                camera_follow: &mut camera_follow,
                load_scene: &mut load_scene,
                quit: &mut quit,
                mouse_world,
            };
            set_partner(&mut ctx, &collisions);
            run_rules(
                &mut self.rules,
                &mut ctx,
                &rules,
                &collisions,
                &self.input,
                dt,
            );
        }

        self.rules.camera_follow = camera_follow;
        if let Some(path) = load_scene {
            let _ = self.load_scene(&path);
            return;
        }
        self.rules.quit_requested = quit;

        // Deferred destruction (whole subtrees).
        for e in destroy_queue {
            if self.scene.world.contains(e) {
                ecs::despawn_recursive(&mut self.scene.world, e);
            }
        }

        // Messages arrive next tick.
        self.rules.incoming.clear();
        self.rules.incoming.extend(messages_out);

        // Newly spawned entities fire Start on their first tick.
        for e in spawned {
            if self.scene.world.contains(e) {
                self.rules.mark_fresh(e);
            }
        }

        self.camera_follow(dt);
        self.music_autoplay();

        // Input edges (pressed/released) are consumed by this tick.
        self.input.end_frame();
    }

    /// Mouse position mapped to world space (orthographic cameras only).
    pub fn mouse_world(&self) -> Option<Vec2> {
        let (cam, cam_tr) = self.primary_camera()?;
        let vp = self.viewport_px;
        if vp.x <= 0.0 || vp.y <= 0.0 {
            return None;
        }
        let ndc_x = (self.input.mouse_pos.x / vp.x) * 2.0 - 1.0;
        let ndc_y = 1.0 - (self.input.mouse_pos.y / vp.y) * 2.0;
        match &cam.kind {
            crate::components::CameraKind::Ortho2D { height } => {
                let w = height * (vp.x / vp.y);
                Some(Vec2::new(
                    cam_tr.position.x + ndc_x * w * 0.5,
                    cam_tr.position.y + ndc_y * height * 0.5,
                ))
            }
            crate::components::CameraKind::Perspective { .. } => None,
        }
    }

    pub fn primary_camera(&self) -> Option<(Camera, Transform)> {
        self.scene
            .world
            .query::<(&Camera, &Transform)>()
            .iter()
            .next()
            .map(|(_, (c, t))| (*c, *t))
    }

    fn camera_follow(&mut self, dt: f32) {
        let Some((target, lerp)) = self.rules.camera_follow else {
            return;
        };
        let cam_e = self
            .scene
            .world
            .query::<&Camera>()
            .iter()
            .next()
            .map(|(e, _)| e);
        let Some(cam_e) = cam_e else { return };
        let Ok(t_tr) = self.scene.world.get::<&Transform>(target) else {
            return;
        };
        let t_pos = t_tr.position;
        let k = 1.0 - (1.0 - lerp.clamp(0.0, 1.0)).powf((dt * 60.0).max(0.0));
        if let Ok(mut t) = self.scene.world.get::<&mut Transform>(cam_e) {
            t.position = t
                .position
                .lerp(Vec3::new(t_pos.x, t_pos.y, t.position.z), k);
        }
    }

    fn music_autoplay(&mut self) {
        let wanted: Option<Music> = self
            .scene
            .world
            .query::<&Music>()
            .iter()
            .next()
            .map(|(_, m)| m.clone());
        match wanted {
            Some(m) if self.playing_track.as_deref() != Some(m.track.as_str()) => {
                if let Some(bytes) = self.assets.sound(&m.track) {
                    self.audio.play_music(&bytes, m.volume);
                    self.playing_track = Some(m.track);
                }
            }
            None if self.playing_track.is_some() => {
                self.audio.stop_music();
                self.playing_track = None;
            }
            _ => {}
        }
    }

    /// Build the frame's draw data. `camera_override` is the editor camera.
    pub fn build_draw(&mut self, camera_override: Option<(Transform, Camera)>) -> FrameDraw {
        let aspect = self.viewport_px.x / self.viewport_px.y.max(1.0);
        build_frame_draw(&self.scene, &mut self.assets, aspect, camera_override)
    }
}

// ---------------------------------------------------------------------------
// Windowed game runner
// ---------------------------------------------------------------------------

/// Run a project as a standalone game (window + loop + built-in HUD).
pub fn run_game(project_dir: &Path) -> anyhow::Result<()> {
    run_game_with(project_dir, Some(Box::new(builtin_hud)))
}

/// Minimal built-in HUD: project name and scene globals (score counters etc.)
/// drawn top-left — gives every data-driven game a scoreboard for free.
pub fn builtin_hud(ctx: &egui::Context, engine: &Engine) {
    egui::Area::new(egui::Id::new("spark.hud"))
        .anchor(egui::Align2::LEFT_TOP, egui::vec2(12.0, 8.0))
        .show(ctx, |ui| {
            let title = engine
                .project
                .as_ref()
                .map(|p| p.name.clone())
                .unwrap_or_else(|| "spark".into());
            ui.heading(title);
            if !engine.scene.globals.is_empty() {
                let mut vars: Vec<(&String, &f64)> = engine.scene.globals.iter().collect();
                vars.sort_by(|a, b| a.0.cmp(b.0));
                ui.monospace(
                    vars.iter()
                        .map(|(k, v)| format!("{k}: {}", format_f64(**v)))
                        .collect::<Vec<_>>()
                        .join("   "),
                );
            }
        });
}

fn format_f64(v: f64) -> String {
    if v.fract() == 0.0 {
        format!("{}", v as i64)
    } else {
        format!("{v:.2}")
    }
}

pub fn run_game_with(project_dir: &Path, hud: Option<HudFn>) -> anyhow::Result<()> {
    let project = Project::load_dir(project_dir)?;
    let event_loop = winit::event_loop::EventLoop::new()?;
    let attrs = winit::window::Window::default_attributes()
        .with_title(format!("{} — spark", project.name))
        .with_inner_size(winit::dpi::PhysicalSize::new(1280, 720));
    // Single-window application: the window lives for the process lifetime,
    // which gives the wgpu surface a 'static borrow (winit 0.30 has no
    // Arc<Window>, see DECISIONS.md §6).
    #[allow(deprecated)] // EventLoop::create_window; the run_app port is roadmap
    let window: &'static winit::window::Window =
        Box::leak(Box::new(event_loop.create_window(attrs)?));

    let mut engine = Engine::windowed(project_dir, window)?;
    engine.hud = hud;

    let ctx = egui::Context::default();
    let pixels_per_point = window.scale_factor() as f32;
    let mut egui_state = egui_winit::State::new(
        ctx.clone(),
        egui::ViewportId::ROOT,
        &window,
        Some(pixels_per_point),
        None,
        None,
    );

    #[allow(deprecated)] // EventLoop::run; the run_app port is roadmap
    event_loop.run(move |event, elwt| {
        use winit::event::{Event, WindowEvent};
        match event {
            Event::WindowEvent { window_id, event } if window_id == window.id() => {
                let egui_res = egui_state.on_window_event(window, &event);
                if !egui_res.consumed {
                    forward_input(&mut engine, &event);
                }
                match event {
                    WindowEvent::Resized(size) => {
                        if let Some(r) = engine.renderer.as_mut() {
                            r.resize(size.width, size.height);
                        }
                        engine.viewport_px = Vec2::new(size.width as f32, size.height as f32);
                    }
                    WindowEvent::CloseRequested => elwt.exit(),
                    WindowEvent::RedrawRequested => {
                        game_frame(&mut engine, window, &ctx, &mut egui_state, pixels_per_point);
                        // End of frame: clear per-frame input edges.
                        engine.input.end_frame();
                    }
                    _ => {}
                }
            }
            Event::AboutToWait => window.request_redraw(),
            _ => {}
        }
    })?;
    Ok(())
}

/// One windowed frame: simulate, run the HUD, tessellate, render.
fn game_frame(
    engine: &mut Engine<'_>,
    window: &winit::window::Window,
    ctx: &egui::Context,
    egui_state: &mut egui_winit::State,
    pixels_per_point: f32,
) {
    let dt = engine.take_dt();
    engine.tick(dt);

    let raw = egui_state.take_egui_input(window);
    let mut hud = engine.hud.take();
    let output = ctx.run(raw, |ctx| {
        if let Some(hud) = hud.as_mut() {
            hud(ctx, engine);
        }
    });
    engine.hud = hud;
    egui_state.handle_platform_output(window, output.platform_output);

    let size = window.inner_size();
    let screen = egui_wgpu::ScreenDescriptor {
        size_in_pixels: [size.width, size.height],
        pixels_per_point,
    };
    let jobs = ctx.tessellate(output.shapes, pixels_per_point);

    // Split borrows: renderer and assets are disjoint Engine fields.
    let Engine {
        renderer, assets, ..
    } = engine;
    if let Some(r) = renderer.as_mut() {
        let dev = r.device.clone();
        let que = r.queue.clone();
        let mut enc = dev.create_command_encoder(&Default::default());
        for (id, delta) in &output.textures_delta.set {
            r.egui_renderer.update_texture(&dev, &que, *id, delta);
        }
        let pre = r
            .egui_renderer
            .update_buffers(&dev, &que, &mut enc, &jobs, &screen);
        let pre = [enc.finish()].into_iter().chain(pre).collect::<Vec<_>>();

        // build_draw needs &mut Engine; use the borrowed pieces directly.
        let aspect = engine.viewport_px.x / engine.viewport_px.y.max(1.0);
        let draw = crate::render::build_frame_draw(&engine.scene, assets, aspect, None);
        if let Err(e) = r.render(assets, &draw, Some((jobs.as_slice(), &screen)), None, pre) {
            log::error!("render: {e}");
        }
    }
}

fn forward_input(engine: &mut Engine, event: &winit::event::WindowEvent) {
    use winit::event::WindowEvent;
    match event {
        WindowEvent::KeyboardInput { event: key, .. } => {
            if let winit::keyboard::PhysicalKey::Code(code) = key.physical_key {
                engine.input.on_key(code, key.state);
            }
        }
        WindowEvent::CursorMoved { position, .. } => {
            engine
                .input
                .on_mouse_move(Vec2::new(position.x as f32, position.y as f32));
        }
        WindowEvent::MouseInput { state, button, .. } => {
            engine.input.on_mouse_button(*button, *state);
        }
        WindowEvent::MouseWheel { delta, .. } => engine.input.on_wheel(*delta),
        WindowEvent::Focused(false) => engine.input.blur(),
        _ => {}
    }
}
