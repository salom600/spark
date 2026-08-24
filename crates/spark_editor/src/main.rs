//! spark editor binary — window + event loop around the `spark_editor` lib.

use spark_editor::Editor;

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // CLI: `spark` opens the editor; `spark --game <dir>` runs a game.
    let args: Vec<String> = std::env::args().collect();
    if let Some(dir) = args
        .iter()
        .position(|a| a == "--game")
        .and_then(|i| args.get(i + 1))
    {
        return spark::app::run_game(std::path::PathBuf::from(dir).as_path());
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
                    editor.on_window_event_pub(&event);
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
