//! wgpu renderer: device/surface setup + full-window clear. M1 adds glyph atlas + grid quads.
#![allow(dead_code)] // scaffold: Renderer pieces are wired up in M1
use wgpu::{
    DeviceDescriptor, InstanceDescriptor, PowerPreference, RequestAdapterOptions,
    SurfaceTargetUnsafe, SurfaceConfiguration
};
use winit::window::Window;

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: SurfaceConfiguration,
    // TODO(M1): glyph atlas (fontdb discovery + swash rasterize, per-OS font fallback),
    // pane quad pipeline, sidebar geometry, uniform buffer.
}

impl Renderer {
    pub fn new(window: &Window) -> anyhow::Result<Self> {
        let instance = wgpu::Instance::new(&InstanceDescriptor::default());

        // SAFETY(field lifetime): Surface<'static> must never outlive the Window. Both are
        // owned fields of the same app struct today; TODO(M1): enforce via one owner struct.
        let surface_target = unsafe { SurfaceTargetUnsafe::from_window(window)? };
        let surface = instance
            .create_surface_unsafe(surface_target)
            .expect("create surface");

        let adapter = pollster::block_on(instance.request_adapter(&RequestAdapterOptions {
            power_preference: PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            ..Default::default()
        }))?
        .ok_or_else(|| anyhow::anyhow!("no suitable GPU adapter found"))?;

        let (device, queue) = pollster::block_on(adapter.request_device(&DeviceDescriptor {
            label: Some("berth-device"),
            ..Default::default()
        }))?;

        let capabilities = surface.get_capabilities(&adapter);
        let size = window.inner_size();
        let config = SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: capabilities.formats[0],
            width: size.width.max(1),
            height: size.height.max(1),
            ..Default::default()
        };
        surface.configure(&device, &config);

        Ok(Self {
            surface,
            device,
            queue,
            config,
        })
    }

    pub fn draw(&mut self, _window: &Window) -> anyhow::Result<()> {
        let frame = match self.surface.get_current_texture() {
            Ok(frame) => frame,
            Err(_) => {
                // surface lost/resized: reconfigure happens on Resized; skip this frame
                return Ok(());
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("berth") });
        {
            let background = wgpu::Color {
                r: 0.05,
                g: 0.05,
                b: 0.06,
                a: 1.0,
            };
            let _ = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("berth-clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(background),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        }
        // TODO(M1): glyph + pane pipeline draws.
        self.queue.submit(Some(encoder.finish()));
        frame.present();
        Ok(())
    }
}
