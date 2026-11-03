//! Window/eventloop: winit 0.30 Application trait, one wgpu surface per window.
//! MVP layout: [sidebar | active tab panes] — sidebar from berth-core::config::Config.
use berth_core::{config::Config, layout::Layout};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    window::{Window, WindowId},
};

pub struct Berth {
    /// live config (hot-reloaded in M2; drives sidebar size/font).
    #[allow(dead_code)]
    config: Config,
    /// layout tree the renderer consumes; built empty in the scaffold.
    #[allow(dead_code)]
    layout: Layout,
    window: Option<Window>,
    renderer: Option<crate::render::Renderer>,
    // TODO(M1): pane registry: PaneId -> emulation::Pane + one reader thread each.
}

impl ApplicationHandler for Berth {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let attrs = Window::default_attributes()
            .with_title("berth")
            .with_min_inner_size(winit::dpi::LogicalSize::new(400, 200));
        self.window = Some(event_loop.create_window(attrs).expect("window creation"));
        if let Some(win) = &self.window {
            self.renderer = Some(crate::render::Renderer::new(win).expect("renderer init"));
        }
    }

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::Resized(_) => {
                // TODO(M1): recompute grid cell counts per pane, call Pane::resize, redraw.
            }
            WindowEvent::KeyboardInput { .. } => {
                // TODO(M1): build key sequences (incl. platform oddities: kitty protocol, IME, Fn keys)
                // and feed focused pane writer.
            }
            WindowEvent::RedrawRequested => {
                if let (Some(r), Some(w)) = (self.renderer.as_mut(), &self.window) {
                    r.draw(w).expect("draw");
                }
            }
            WindowEvent::CloseRequested => {
                // TODO(M1): confirm panes with live shells; then exit event loop.
            }
            _ => {}
        }
    }
}

pub fn run() -> anyhow::Result<()> {
    let config = Config::load()?;
    let layout = Layout::with_defaults("main");

    let event_loop = EventLoop::new()?;
    let mut app = Berth {
        config,
        layout,
        window: None,
        renderer: None,
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}
