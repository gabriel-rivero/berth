//! wgpu renderer: device/surface setup + full-window clear. M1 adds glyph atlas + grid quads.
#![allow(dead_code)] // scaffold: Renderer pieces are wired up in M1
use wgpu::{
    DeviceDescriptor, InstanceDescriptor, PowerPreference, RequestAdapterOptions,
    SurfaceTargetUnsafe,
};
use winit::window::Window;

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    // TODO(M1): glyph atlas (fontdb discovery + swash rasterize, per-OS font fallback),
    // pane quad pipeline, sidebar geometry, uniform buffer.
}

impl Renderer {
    pub fn new(window: &Window) -> anyhow::Result<Self> {
        // NOTE(wgpu-30): display handle can be attached here; without it some Wayland/GL
        // paths degrade. TODO(M1): pass winit's OwnedDisplayHandle.
        let instance = wgpu::Instance::new(InstanceDescriptor::new_without_display_handle());

        // SAFETY(field lifetime): Surface<'static> must never outlive the Window. Both are
        // owned fields of the same app struct today; TODO(M1): enforce via one owner struct.
        let surface_target = unsafe { SurfaceTargetUnsafe::from_window(window)? };
        // SAFETY(lifetime): surface borrows nothing; `surface_target` bundles raw handles from
        // `window`, which outlives Renderer (same owner struct). Enforced by TODO above.
        let surface = unsafe {
            instance
                .create_surface_unsafe(surface_target)
                .expect("create surface")
        };

        let adapter = pollster::block_on(instance.request_adapter(&RequestAdapterOptions {
            power_preference: PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            ..Default::default()
        }))?;

        let (device, queue) = pollster::block_on(adapter.request_device(&DeviceDescriptor {
            label: Some("berth-device"),
            ..Default::default()
        }))?;

        let size = window.inner_size();
        let config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .ok_or_else(|| anyhow::anyhow!("no compatible surface configuration"))?;
        surface.configure(&device, &config);

        Ok(Self {
            surface,
            device,
            queue,
            config,
        })
    }

    pub fn draw(&mut self, _window: &Window) -> anyhow::Result<()> {
        match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => self.draw_frame(frame),
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => {
                // TODO(M1): reconfigure surface when we track ResizeRequested events.
                self.draw_frame(frame)
            }
            wgpu::CurrentSurfaceTexture::Timeout
            | wgpu::CurrentSurfaceTexture::Occluded
            | wgpu::CurrentSurfaceTexture::Outdated => {
                // skip frame; reconfigure/recapture next time
                Ok(())
            }
            wgpu::CurrentSurfaceTexture::Lost => anyhow::bail!("surface lost; recreate renderer"),
            wgpu::CurrentSurfaceTexture::Validation => {
                anyhow::bail!("surface validation error (error scope)")
            }
        }
    }

    fn draw_frame(&mut self, frame: wgpu::SurfaceTexture) -> anyhow::Result<()> {
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("berth"),
            });
        {
            let _ = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("berth-clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(PANEL_BG),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }
        // TODO(M1): glyph + pane pipeline draws.
        self.queue.submit(Some(encoder.finish()));
        self.queue.present(frame);
        Ok(())
    }
}

const PANEL_BG: wgpu::Color = wgpu::Color {
    r: 0.05,
    g: 0.05,
    b: 0.06,
    a: 1.0,
};
