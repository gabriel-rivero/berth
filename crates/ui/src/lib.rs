//! berth-ui: winit 0.30 app + wgpu renderer. Re-exports run() as the platform entrypoint.
pub mod app;
pub mod render;

/// Open the first window and enter the event loop. Returns only when the user quits.
pub fn run() -> anyhow::Result<()> {
    app::run()
}

/// Human-readable list of every wgpu adapter visible to this build, without
/// opening a window (`berth --gpu-info`). Use it to confirm hardware accel.
pub fn gpu_report() -> String {
    render::gpu_report()
}
