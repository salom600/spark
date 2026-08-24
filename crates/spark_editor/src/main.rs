//! spark editor — the engine's own tooling in one binary.
//!
//! Layout (egui dock-lite via panels):
//! ```text
//! ┌──────────┬───────────────────────────┬─────────────┐
//! │Hierarchy │        Viewport           │ Inspector   │
//! │ (tree)   │  (scene render + gizmo    │ (components │
//! │          │   + camera controls)      │  + material │
//! ├──────────┴───────────────────────────┤  + rules)   │
//! │ Asset browser / console / stats      │             │
//! └──────────────────────────────────────┴─────────────┘
//! ```
//!
//! Design: the editor is an *overlay* on the same engine that runs games —
//! one binary, one code path, WYSIWYG by construction (DECISIONS.md §4.4).

mod commands;
mod panels;
mod state;

use std::path::PathBuf;

use spark::prelude::*;

use state::{EditorCamera, EditorState, PlaySnapshot};

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // CLI: `spark` opens the editor; `spark --game <dir>` runs a game.
    let args: Vec<String> = std::env::args().collect();
    if let Some(dir) = args
        .iter()
        .position(|a| a == "--game")
        .and_then(|i| args.get(i + 1))
    {
        return spark::app::run_game(PathBuf::from(dir).as_path());
    }

    let event_loop = winit::event_loop::EventLoop::new()?;
    let attrs = winit::window::Window::default_attributes()
        .with_title("spark editor")
        .with_inner_size(winit::dpi::PhysicalSize::new(1600, 900));
    #[allow(deprecated)] // EventLoop::create_window; the run_app port is roadmap
    let window: &'static winit::window::Window =
        Box::leak(Box::new(event_loop.create_window(attrs)?));

    let ctx = egui::Context::default();
    let pixels_per_point = window.scale_factor() as f32;
    let mut egui_state = egui_winit::State::new(
        ctx.clone(),
        egui::ViewportId::ROOT,
        window,
        Some(pixels_per_point),
        None,
        None,
    );

    let mut editor = Editor::new(window)?;

    #[allow(deprecated)] // EventLoop::run; the run_app port is roadmap
    event_loop.run(move |event, elwt| {
        use winit::event::{Event, WindowEvent};
        match event {
            Event::WindowEvent { window_id, event } if window_id == window.id() => {
                let egui_res = egui_state.on_window_event(window, &event);
                if !egui_res.consumed {
                    editor.on_window_event(&event);
                }
                match event {
                    WindowEvent::Resized(size) => {
                        editor.on_resized(size.width, size.height);
                    }
                    WindowEvent::CloseRequested => elwt.exit(),
                    WindowEvent::RedrawRequested => {
                        editor.frame(window, &ctx, &mut egui_state, pixels_per_point);
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

/// The editor application: engine + editor-only state.
struct Editor {
    engine: Engine<'static>,
    state: EditorState,
    project_dir: Option<PathBuf>,
    scene_path: String,
    undo: CommandStack,
    editor_cam: EditorCamera,
    playing: Option<PlaySnapshot>,
    console: Vec<(String, String)>,
    selected_asset: Option<String>,
}

impl Editor {
    fn new(window: &'static winit::window::Window) -> anyhow::Result<Self> {
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
            playing: None,
            console: vec![(
                "info".into(),
                "spark editor ready — File → New/Open Project".into(),
            )],
            selected_asset: None,
        })
    }

    fn on_window_event(&mut self, event: &winit::event::WindowEvent) {
        use winit::event::{ElementState, MouseScrollDelta, WindowEvent};
        match event {
            WindowEvent::KeyboardInput { event: key, .. } => {
                if let winit::keyboard::PhysicalKey::Code(code) = key.physical_key {
                    self.engine.input.on_key(code, key.state);
                    // Editor shortcuts (with modifiers) live here so they work
                    // even while a text field has focus is NOT desired — egui
                    // consumes those first; these are the raw fallback.
                    if key.state == ElementState::Pressed {
                        self.on_shortcut(code);
                    }
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
                if let MouseScrollDelta::LineDelta(_, y) = delta {
                    self.editor_cam.zoom(*y);
                }
            }
            WindowEvent::Focused(false) => self.engine.input.blur(),
            _ => {}
        }
    }

    fn on_shortcut(&mut self, code: winit::keyboard::KeyCode) {
        if code == winit::keyboard::KeyCode::F5 {
            self.toggle_play();
        }
    }

    fn on_resized(&mut self, w: u32, h: u32) {
        if let Some(r) = self.engine.renderer.as_mut() {
            r.resize(w, h);
        }
        self.state.viewport_px = [0, 0, w, h];
    }

    // -----------------------------------------------------------------------
    // Project / scene lifecycle
    // -----------------------------------------------------------------------

    fn new_project(&mut self, name: &str, dimension: Dimension) {
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

    fn open_project(&mut self, dir: &std::path::Path) {
        match self.engine.open_project(dir) {
            Ok(_) => {
                self.project_dir = Some(dir.to_path_buf());
                self.undo.clear();
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

    fn save_scene(&mut self) {
        let Some(dir) = self.project_dir.clone() else {
            self.log("warn", "no project open");
            return;
        };
        let path = dir.join(&self.scene_path);
        let text = self.engine.scene.save(&self.engine.registry);
        match std::fs::write(&path, text) {
            Ok(_) => self.log("info", &format!("saved {}", path.display())),
            Err(e) => self.log("error", &format!("save failed: {e}")),
        }
    }

    fn load_scene(&mut self) {
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
                self.state.selected = None;
                self.undo.clear();
                self.log("info", &format!("loaded {}", path.display()));
            }
            Err(e) => self.log("error", &format!("load failed: {e}")),
        }
    }

    // -----------------------------------------------------------------------
    // Play mode (snapshot / restore)
    // -----------------------------------------------------------------------

    fn toggle_play(&mut self) {
        if self.playing.is_some() {
            self.stop_play();
        } else {
            self.start_play();
        }
    }

    fn start_play(&mut self) {
        let snapshot = self.engine.scene.save(&self.engine.registry);
        self.playing = Some(PlaySnapshot {
            scene_text: snapshot,
        });
        self.engine.rules.clear();
        self.engine.playing_track = None;
        self.state.mark_all_fresh(&mut self.engine);
        self.log("info", "play mode started (F5 to stop)");
    }

    fn stop_play(&mut self) {
        if let Some(snap) = self.playing.take() {
            match spark::scene::Scene::load(&snap.scene_text, &self.engine.registry) {
                Ok(scene) => {
                    self.engine.scene = scene;
                    self.engine.rules.clear();
                    self.engine.playing_track = None;
                    self.engine.audio.stop_music();
                }
                Err(e) => self.log("error", &format!("restore failed: {e}")),
            }
            self.log("info", "play mode stopped — scene restored");
        }
    }

    pub(crate) fn log(&mut self, level: &str, msg: &str) {
        self.console.push((level.to_string(), msg.to_string()));
        if self.console.len() > 500 {
            self.console.remove(0);
        }
    }

    // -----------------------------------------------------------------------
    // Frame
    // -----------------------------------------------------------------------

    fn frame(
        &mut self,
        window: &winit::window::Window,
        ctx: &egui::Context,
        egui_state: &mut egui_winit::State,
        pixels_per_point: f32,
    ) {
        let dt = self.engine.take_dt();
        let in_play = self.playing.is_some();

        // Simulation: run in play mode only (edit mode is static by design).
        if in_play {
            self.engine.tick(dt);
            if self.engine.rules.quit_requested {
                self.stop_play();
            }
        } else {
            // Keep asset hot-reload alive while editing.
            self.engine.assets.update();
            self.engine.audio.update();
        }

        // ---- egui UI pass -------------------------------------------------
        let raw = egui_state.take_egui_input(window);
        let output = ctx.run(raw, |ctx| self.ui(ctx));
        egui_state.handle_platform_output(window, output.platform_output);

        let size = window.inner_size();
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [size.width, size.height],
            pixels_per_point,
        };
        let jobs = ctx.tessellate(output.shapes, pixels_per_point);

        // ---- Scene draw (before borrowing the renderer) --------------------
        let (cam_override, viewport) = if in_play {
            (None, self.state.full_viewport())
        } else {
            (
                Some(self.editor_cam.as_override(self.engine.scene.dimension)),
                self.state.viewport_rect_px(),
            )
        };
        let aspect = viewport.2 as f32 / viewport.3.max(1) as f32;
        self.engine.viewport_px = Vec2::new(viewport.2 as f32, viewport.3 as f32);
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
                    let playing = self.playing.is_some();
                    let label = if playing { "Stop (F5)" } else { "Play (F5)" };
                    if ui.button(label).clicked() {
                        self.toggle_play();
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Add Entity").clicked() {
                        self.add_entity("Entity");
                        ui.close();
                    }
                    if ui.button("Add 2D Sprite").clicked() {
                        self.add_sprite();
                        ui.close();
                    }
                    if ui.button("Add Cube (3D)").clicked() {
                        self.add_mesh("cube", Dimension::D3);
                        ui.close();
                    }
                    if ui.button("Add Point Light").clicked() {
                        self.add_point_light();
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

    fn apply_undo(&mut self) {
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
    }

    fn apply_redo(&mut self) {
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
    }

    fn status_area(&mut self, ui: &mut egui::Ui) {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let fps = self.engine.stats.fps;
            ui.weak(format!("{fps:.0} FPS"));
            if self.playing.is_some() {
                ui.colored_label(egui::Color32::GREEN, "▶ PLAYING");
            } else {
                ui.weak("edit");
            }
            let dim = match self.engine.scene.dimension {
                Dimension::D2 => "2D",
                Dimension::D3 => "3D",
            };
            ui.weak(dim);
        });
    }
}
