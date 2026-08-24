//! spark editor — the engine's own tooling, as a library + thin binary.
//!
//! The editor is an *overlay* on the same engine that runs games — one
//! binary, one code path, WYSIWYG by construction (DECISIONS.md §4.4).
//! Being a library lets integration tests drive the real editor
//! (commands, selection, gizmo math) without a window or GPU.

pub mod commands;
pub mod gizmo;
pub mod panels;
pub mod state;

use std::path::PathBuf;

use spark::prelude::*;

use state::{EditorCamera, EditorState, PlaySnapshot};

/// Play-mode state machine: Stopped (editing) \u{2192} Playing \u{2194} Paused \u{2192} Stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PlayState {
    #[default]
    Stopped,
    Playing,
    Paused,
}

/// The editor application: engine + editor-only state.
///
/// Layout (egui panels):
/// ```text
/// ┌──────────┬───────────────────────────┬─────────────┐
/// │Hierarchy │        Viewport           │ Inspector   │
/// │ (tree)   │  (scene render + gizmos   │ (components │
/// │          │   + camera controls)      │  + rules)   │
/// ├──────────┴───────────────────────────┤             │
/// │ Asset browser / console / stats      │             │
/// └──────────────────────────────────────┴─────────────┘
/// ```
pub struct Editor {
    pub engine: Engine<'static>,
    pub state: EditorState,
    pub project_dir: Option<PathBuf>,
    pub scene_path: String,
    pub undo: CommandStack,
    pub editor_cam: EditorCamera,
    pub play_state: PlayState,
    pub playing: Option<PlaySnapshot>,
    pub console: Vec<(String, String)>,
    pub selected_asset: Option<String>,
    /// Texture thumbnails for the asset browser, cached by (path, version).
    pub tex_previews: std::collections::HashMap<String, (u32, egui::TextureHandle)>,
    /// Asset being dragged from the browser (for viewport drop-to-spawn).
    pub drag_asset: Option<String>,
}

impl Editor {
    /// Editor bound to a window surface.
    pub fn new(window: &'static winit::window::Window) -> anyhow::Result<Self> {
        let engine = Engine::editor(window)?;
        let (w, h) = engine.renderer.as_ref().unwrap().size();
        let state = EditorState {
            viewport_px: [0, 0, w, h],
            ..Default::default()
        };
        let editor_cam = EditorCamera::default();
        Ok(Self {
            engine,
            state,
            project_dir: None,
            scene_path: "scenes/main.scene".into(),
            undo: CommandStack::default(),
            editor_cam,
            play_state: PlayState::Stopped,
            playing: None,
            console: vec![(
                "info".into(),
                "spark editor ready — File → New/Open Project".into(),
            )],
            selected_asset: None,
            tex_previews: std::collections::HashMap::new(),
            drag_asset: None,
        })
    }

    /// Headless editor (no window, no GPU) for integration tests: same
    /// engine, same commands, same scene pipeline.
    pub fn headless() -> Self {
        let engine = Engine::headless_empty();
        Self {
            engine,
            state: EditorState::default(),
            project_dir: None,
            scene_path: "scenes/main.scene".into(),
            undo: CommandStack::default(),
            editor_cam: EditorCamera::default(),
            play_state: PlayState::Stopped,
            playing: None,
            console: Vec::new(),
            selected_asset: None,
            tex_previews: std::collections::HashMap::new(),
            drag_asset: None,
        }
    }

    fn on_window_event(&mut self, event: &winit::event::WindowEvent) {
        use winit::event::{ElementState, WindowEvent};
        match event {
            WindowEvent::KeyboardInput { event: key, .. } => {
                if let winit::keyboard::PhysicalKey::Code(code) = key.physical_key {
                    self.engine.input.on_key(code, key.state);
                    let _ = ElementState::Pressed;
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.engine
                    .input
                    .on_mouse_move(Vec2::new(position.x as f32, position.y as f32));
            }
            WindowEvent::MouseInput { state, button, .. } => {
                self.engine.input.on_mouse_button(*button, *state);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.engine.input.on_wheel(*delta);
            }
            WindowEvent::Focused(false) => self.engine.input.blur(),
            _ => {}
        }
    }

    /// Public wrapper for the binary's event loop (raw input forwarding).
    pub fn on_window_event_pub(&mut self, event: &winit::event::WindowEvent) {
        self.on_window_event(event);
    }

    pub fn on_resized(&mut self, w: u32, h: u32) {
        if let Some(r) = self.engine.renderer.as_mut() {
            r.resize(w, h);
        }
        self.state.viewport_px = [0, 0, w, h];
    }

    // -----------------------------------------------------------------------
    // Project / scene lifecycle
    // -----------------------------------------------------------------------

    pub fn new_project(&mut self, name: &str, dimension: Dimension) {
        let dir = std::env::current_dir()
            .unwrap_or_default()
            .join("projects")
            .join(name);
        match Project::create_from_template(&dir, name, dimension) {
            Ok(_) => self.log("info", &format!("created project at {}", dir.display())),
            Err(e) => self.log("error", &format!("create failed: {e}")),
        }
        self.open_project(&dir);
    }

    /// Copy a file into the project's assets/ and index it immediately.
    /// The practical import path (no native dialog dependency); works on
    /// every platform CI covers.
    pub fn import_asset(&mut self, src: &str) {
        let Some(dir) = self.project_dir.clone() else {
            self.log("warn", "open a project before importing");
            return;
        };
        let src_path = std::path::PathBuf::from(src);
        let Some(name) = src_path.file_name().and_then(|n| n.to_str()) else {
            self.log("error", "import: no file name");
            return;
        };
        if !src_path.is_file() {
            self.log("error", &format!("import: {src} is not a file"));
            return;
        }
        let dst = dir.join("assets").join(name);
        if let Some(parent) = dst.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::copy(&src_path, &dst) {
            Ok(bytes) => {
                self.engine.assets.rescan();
                self.log(
                    "info",
                    &format!("imported {name} ({bytes} bytes) into assets/"),
                );
            }
            Err(e) => self.log("error", &format!("import failed: {e}")),
        }
    }

    pub fn open_project(&mut self, dir: &std::path::Path) {
        match self.engine.open_project(dir) {
            Ok(_) => {
                self.project_dir = Some(dir.to_path_buf());
                self.undo.clear();
                self.state.selected.clear();
                self.editor_cam.fit_dimension(self.engine.scene.dimension);
                self.log("info", &format!("opened project {}", dir.display()));
                self.guess_scene_path();
            }
            Err(e) => self.log("error", &format!("open failed: {e}")),
        }
    }

    /// `project.ron`'s main_scene is authoritative; mirror it in the UI.
    fn guess_scene_path(&mut self) {
        if let Some(p) = &self.engine.project {
            self.scene_path = p.main_scene.clone();
        }
    }

    pub fn save_scene(&mut self) {
        let Some(dir) = self.project_dir.clone() else {
            self.log("warn", "no project open");
            return;
        };
        let path = dir.join(&self.scene_path);
        let text = self.engine.scene.save(&self.engine.registry);
        match write_scene_atomic(&path, &text) {
            Ok(()) => self.log("info", &format!("saved {}", path.display())),
            Err(e) => self.log("error", &format!("save failed: {e}")),
        }
    }

    pub fn load_scene(&mut self) {
        let Some(dir) = self.project_dir.clone() else {
            self.log("warn", "no project open");
            return;
        };
        let path = dir.join(&self.scene_path);
        match spark::scene::load_scene_file(&path, &self.engine.registry) {
            Ok(scene) => {
                self.engine.scene = scene;
                self.engine.rules.clear();
                self.engine.playing_track = None;
                self.state.selected.clear();
                self.undo.clear();
                self.engine.physics.request_rebuild();
                self.log("info", &format!("loaded {}", path.display()));
            }
            Err(e) => self.log("error", &format!("load failed: {e}")),
        }
    }

    /// Toggle the scene's dimension (2D ↔ 3D), keeping scene data.
    pub fn set_dimension(&mut self, d: Dimension) {
        self.engine.set_dimension(d);
        self.editor_cam.fit_dimension(d);
        self.log(
            "info",
            &format!(
                "scene switched to {}",
                match d {
                    Dimension::D2 => "2D",
                    Dimension::D3 => "3D",
                }
            ),
        );
    }

    // -----------------------------------------------------------------------
    // Play mode (snapshot / restore) — Play · Pause · Stop · Restart · Step
    // -----------------------------------------------------------------------

    /// Start playing (snapshot the edit state first).
    pub fn play(&mut self) {
        if self.play_state != PlayState::Stopped {
            return;
        }
        let snapshot = self.engine.scene.save(&self.engine.registry);
        self.playing = Some(PlaySnapshot {
            scene_text: snapshot,
        });
        self.play_state = PlayState::Playing;
        self.engine.rules.clear();
        self.engine.playing_track = None;
        self.state.mark_all_fresh(&mut self.engine);
        self.state.drag = None;
        self.log(
            "info",
            "play (F5 stop \u{00b7} F6 pause \u{00b7} F7 step \u{00b7} F8 restart)",
        );
    }

    /// Pause the simulation (music pauses with it, position kept).
    pub fn pause_play(&mut self) {
        if self.play_state == PlayState::Playing {
            self.play_state = PlayState::Paused;
            self.engine.audio.pause_music();
            self.log("info", "paused");
        }
    }

    /// Resume a paused simulation.
    pub fn resume_play(&mut self) {
        if self.play_state == PlayState::Paused {
            self.play_state = PlayState::Playing;
            self.engine.audio.resume_music();
            self.log("info", "resumed");
        }
    }

    /// Pause/resume toggle.
    pub fn toggle_pause(&mut self) {
        match self.play_state {
            PlayState::Playing => self.pause_play(),
            PlayState::Paused => self.resume_play(),
            PlayState::Stopped => {}
        }
    }

    /// Stop playing and restore the edit-state snapshot.
    pub fn stop_play(&mut self) {
        if self.play_state == PlayState::Stopped {
            return;
        }
        self.play_state = PlayState::Stopped;
        if let Some(snap) = self.playing.take() {
            match spark::scene::Scene::load(&snap.scene_text, &self.engine.registry) {
                Ok(scene) => {
                    self.engine.scene = scene;
                    self.engine.rules.clear();
                    self.engine.playing_track = None;
                    self.engine.audio.stop_music();
                    self.engine.physics.request_rebuild();
                    self.state.retain_existing(&self.engine.scene.world);
                }
                Err(e) => self.log("error", &format!("restore failed: {e}")),
            }
            self.log("info", "stopped \u{2014} scene restored");
        }
    }

    /// Restart: restore the snapshot and keep playing from the top.
    pub fn restart_play(&mut self) {
        if self.play_state == PlayState::Stopped {
            return;
        }
        let snapshot = self.playing.take();
        self.play_state = PlayState::Stopped;
        if let Some(snap) = snapshot {
            match spark::scene::Scene::load(&snap.scene_text, &self.engine.registry) {
                Ok(scene) => {
                    self.engine.scene = scene;
                    self.engine.rules.clear();
                    self.engine.playing_track = None;
                    self.engine.audio.stop_music();
                    self.engine.physics.request_rebuild();
                    self.state.retain_existing(&self.engine.scene.world);
                }
                Err(e) => self.log("error", &format!("restart restore failed: {e}")),
            }
        }
        self.play();
    }

    /// Advance exactly one simulation frame (from pause).
    pub fn step_frame(&mut self) {
        if self.play_state != PlayState::Paused {
            return;
        }
        self.engine.tick(1.0 / 60.0);
        if self.engine.rules.quit_requested {
            self.stop_play();
        }
    }

    /// Legacy toggle kept for F5: stopped \u{2192} play, otherwise stop.
    pub fn toggle_play(&mut self) {
        if self.play_state == PlayState::Stopped {
            self.play();
        } else {
            self.stop_play();
        }
    }

    pub fn log(&mut self, level: &str, msg: &str) {
        self.console.push((level.to_string(), msg.to_string()));
        if self.console.len() > 500 {
            self.console.remove(0);
        }
    }

    // -----------------------------------------------------------------------
    // Frame
    // -----------------------------------------------------------------------

    pub fn frame(
        &mut self,
        window: &winit::window::Window,
        ctx: &egui::Context,
        egui_state: &mut egui_winit::State,
        pixels_per_point: f32,
    ) {
        let dt = self.engine.take_dt();
        let in_play = self.play_state != PlayState::Stopped;

        // Simulation: run while playing (paused = frozen but still
        // rendered; stepping ticks on demand from `step_frame`).
        if self.play_state == PlayState::Playing {
            self.engine.tick(dt);
            if self.engine.rules.quit_requested {
                self.stop_play();
            }
        } else if self.play_state == PlayState::Paused {
            // Keep hot-reload + music loop bookkeeping alive while frozen.
            self.engine.assets.update();
        } else {
            // Keep asset hot-reload alive while editing.
            self.engine.assets.update();
            self.engine.audio.update();
        }

        // ---- egui UI pass -------------------------------------------------
        let raw = egui_state.take_egui_input(window);
        let output = ctx.run(raw, |ctx| {
            self.play_shortcuts(ctx);
            self.ui(ctx)
        });
        egui_state.handle_platform_output(window, output.platform_output);

        let size = window.inner_size();
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [size.width, size.height],
            pixels_per_point,
        };
        let jobs = ctx.tessellate(output.shapes, pixels_per_point);

        // ---- Scene draw (before borrowing the renderer) --------------------
        // Embedded game view: the scene always renders into the central
        // viewport rect; edit mode additionally overrides the camera.
        let viewport = self.state.viewport_rect_px();
        let cam_override =
            (!in_play).then(|| self.editor_cam.as_override(self.engine.scene.dimension));
        let aspect = viewport.2 as f32 / viewport.3.max(1) as f32;
        self.engine.viewport_px = Vec2::new(viewport.2 as f32, viewport.3 as f32);
        self.engine.viewport_origin_px = Vec2::new(viewport.0 as f32, viewport.1 as f32);
        let draw = spark::render::build_frame_draw(
            &self.engine.scene,
            &mut self.engine.assets,
            aspect,
            cam_override,
        );

        // ---- GPU submit ---------------------------------------------------
        if let Some(r) = self.engine.renderer.as_mut() {
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

            if let Err(e) = r.render(
                &mut self.engine.assets,
                &draw,
                Some((jobs.as_slice(), &screen)),
                Some(viewport),
                pre,
            ) {
                self.log("error", &format!("render: {e}"));
            }
        }

        // Per-frame input edges clear at the very end of the frame.
        self.engine.input.end_frame();
    }

    // -----------------------------------------------------------------------
    // UI layout
    // -----------------------------------------------------------------------

    fn ui(&mut self, ctx: &egui::Context) {
        self.menu_bar(ctx);
        // Maximize-on-play: while playing, the viewport becomes the whole
        // window (a real game view at the window's resolution) unless the
        // user keeps the editor panels visible.
        let panels_hidden = self.play_state != PlayState::Stopped && self.state.maximize_on_play;
        if !panels_hidden {
            egui::TopBottomPanel::bottom("console")
                .resizable(true)
                .show(ctx, |ui| {
                    self.bottom_panel(ui);
                });
            egui::SidePanel::left("hierarchy")
                .resizable(true)
                .default_width(240.0)
                .show(ctx, |ui| {
                    self.hierarchy_panel(ui);
                });
            egui::SidePanel::right("inspector")
                .resizable(true)
                .default_width(320.0)
                .show(ctx, |ui| {
                    self.inspector_panel(ui);
                });
        }
        egui::CentralPanel::default().show(ctx, |ui| {
            self.viewport_panel(ui, ctx);
        });
    }

    fn menu_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("menu").show(ctx, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("New Project…").clicked() {
                        self.state.show_new_project = true;
                        ui.close();
                    }
                    if ui.button("Open Project…").clicked() {
                        self.state.show_open_project = true;
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Save Scene (Ctrl+S)").clicked() {
                        self.save_scene();
                        ui.close();
                    }
                    if ui.button("Import Asset…").clicked() {
                        self.state.show_import = true;
                        ui.close();
                    }
                    if ui.button("Reload Scene").clicked() {
                        self.load_scene();
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Exit").clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
                ui.menu_button("Edit", |ui| {
                    let undo_lbl = self
                        .undo
                        .peek_undo()
                        .map(|l| format!("Undo {l}"))
                        .unwrap_or("Undo".into());
                    let redo_lbl = self
                        .undo
                        .peek_redo()
                        .map(|l| format!("Redo {l}"))
                        .unwrap_or("Redo".into());
                    if ui
                        .add_enabled(self.undo.can_undo(), egui::Button::new(undo_lbl))
                        .clicked()
                    {
                        self.apply_undo();
                        ui.close();
                    }
                    if ui
                        .add_enabled(self.undo.can_redo(), egui::Button::new(redo_lbl))
                        .clicked()
                    {
                        self.apply_redo();
                        ui.close();
                    }
                });
                ui.menu_button("Scene", |ui| {
                    let playing = self.play_state != PlayState::Stopped;
                    let label = if playing { "Stop (F5)" } else { "Play (F5)" };
                    if ui.button(label).clicked() {
                        self.toggle_play();
                        ui.close();
                    }
                    ui.separator();
                    let dim = self.engine.scene.dimension;
                    if dim == spark::scene::Dimension::D2 {
                        if ui.button("Switch to 3D").clicked() {
                            self.set_dimension(Dimension::D3);
                            ui.close();
                        }
                    } else if ui.button("Switch to 2D").clicked() {
                        self.set_dimension(Dimension::D2);
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Add Entity").clicked() {
                        self.add_entity("Entity", None);
                        ui.close();
                    }
                    if ui.button("Add Sprite (2D)").clicked() {
                        self.set_dimension_if(Dimension::D2);
                        self.add_sprite();
                        ui.close();
                    }
                    if ui.button("Add Cube (3D)").clicked() {
                        self.set_dimension_if(Dimension::D3);
                        self.add_mesh("cube");
                        ui.close();
                    }
                    if ui.button("Add Ground (physics floor)").clicked() {
                        self.set_dimension_if(Dimension::D3);
                        self.add_ground();
                        ui.close();
                    }
                    if ui.button("Add Plane (visual)").clicked() {
                        self.set_dimension_if(Dimension::D3);
                        self.add_mesh("plane");
                        ui.close();
                    }
                    if ui.button("Add Sphere (3D)").clicked() {
                        self.set_dimension_if(Dimension::D3);
                        self.add_mesh("sphere");
                        ui.close();
                    }
                    if ui.button("Add Camera").clicked() {
                        self.add_camera();
                        ui.close();
                    }
                    if ui.button("Add Point Light").clicked() {
                        self.add_point_light();
                        ui.close();
                    }
                    if ui.button("Add Spot Light").clicked() {
                        self.add_spot_light();
                        ui.close();
                    }
                    if ui.button("Add Sun (directional)").clicked() {
                        self.add_sun();
                        ui.close();
                    }
                });
                ui.menu_button("Project", |ui| {
                    if let Some(dir) = self.project_dir.clone() {
                        if ui.button("Export Game…").clicked() {
                            self.export_game(&dir);
                            ui.close();
                        }
                        ui.label(format!("dir: {}", dir.display()));
                    } else {
                        ui.weak("no project open");
                    }
                });
                self.status_area(ui);
            });
        });
        self.modals(ctx);
    }

    /// Switch the scene dimension only if it differs (menu-item helper).
    fn set_dimension_if(&mut self, d: Dimension) {
        if self.engine.scene.dimension != d {
            self.set_dimension(d);
        }
    }

    pub fn apply_undo(&mut self) {
        let mut world = std::mem::take(&mut self.engine.scene.world);
        let registry = &self.engine.registry;
        {
            let mut cmd_ctx = CommandCtx {
                world: &mut world,
                registry,
            };
            if let Some(label) = self.undo.undo(&mut cmd_ctx) {
                self.log("info", &format!("undo: {label}"));
            }
        }
        self.engine.scene.world = world;
        self.engine.physics.request_rebuild();
        self.state.retain_existing(&self.engine.scene.world);
    }

    pub fn apply_redo(&mut self) {
        let mut world = std::mem::take(&mut self.engine.scene.world);
        let registry = &self.engine.registry;
        {
            let mut cmd_ctx = CommandCtx {
                world: &mut world,
                registry,
            };
            if let Some(label) = self.undo.redo(&mut cmd_ctx) {
                self.log("info", &format!("redo: {label}"));
            }
        }
        self.engine.scene.world = world;
        self.engine.physics.request_rebuild();
        self.state.retain_existing(&self.engine.scene.world);
    }

    /// Play-mode keyboard shortcuts (work in every mode; F5 toggles).
    fn play_shortcuts(&mut self, ctx: &egui::Context) {
        use egui::Key;
        if ctx.wants_keyboard_input() {
            return;
        }
        let (f5, f6, f7, f8) = ctx.input(|i| {
            (
                i.key_pressed(Key::F5),
                i.key_pressed(Key::F6),
                i.key_pressed(Key::F7),
                i.key_pressed(Key::F8),
            )
        });
        if f5 {
            self.toggle_play();
        }
        if f6 {
            self.toggle_pause();
        }
        if f7 {
            self.step_frame();
        }
        if f8 {
            self.restart_play();
        }
    }

    fn status_area(&mut self, ui: &mut egui::Ui) {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let fps = self.engine.stats.fps;
            ui.weak(format!("{fps:.0} FPS"));
            match self.play_state {
                PlayState::Playing => {
                    ui.colored_label(egui::Color32::GREEN, "▶ PLAYING");
                }
                PlayState::Paused => {
                    ui.colored_label(egui::Color32::YELLOW, "⏸ PAUSED");
                }
                PlayState::Stopped => {
                    ui.weak("edit");
                }
            }
            let dim = match self.engine.scene.dimension {
                Dimension::D2 => "2D",
                Dimension::D3 => "3D",
            };
            ui.weak(dim);
            if self.state.selected.len() > 1 {
                ui.weak(format!("{} selected", self.state.selected.len()));
            }
        });
    }
}

/// Write a scene file atomically: temp file + rename, so a crash mid-write
/// can never truncate the previous scene.
fn write_scene_atomic(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("scene.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}
