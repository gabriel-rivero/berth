//! wgpu renderer: glyph atlas + grid pipeline for pane terminal content.
//!
//! Per frame: cell backgrounds -> selection tint -> cursor -> glyph quads, all
//! in one dynamic vertex buffer. Glyphs are rasterized with swash into a single
//! RGBA8 atlas (alpha glyphs stored as white-with-coverage, color glyphs raw).
use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Point};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Term, point_to_viewport, viewport_to_point};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};
use bytemuck::{Pod, Zeroable};
use fontdb::{Database, Family, Query, Stretch, Style, Weight};
use std::collections::HashMap;
use swash::scale::image::Content;
use swash::scale::{Render, ScaleContext, Source, StrikeWith};
use swash::{FontRef, zeno};
use wgpu::{
    CurrentSurfaceTexture, LoadOp, Operations, RenderPassColorAttachment, RenderPassDescriptor,
};
use winit::window::Window;

/// One-line stdout diagnostics, enabled with BERTH_DEBUG=1 (cross-platform font/render triage).
pub fn debug_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("BERTH_DEBUG").ok().as_deref() == Some("1"))
}

macro_rules! dlog {
    ($($arg:tt)*) => {
        if debug_enabled() { eprintln!($($arg)*); }
    };
}

/// BERTH_FRAME_DUMP=1: read back every presented frame to
/// /tmp/opencode/berth-frame-N.ppm (rootless render-truth check).
fn frame_dump_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("BERTH_FRAME_DUMP").ok().as_deref() == Some("1"))
}

fn frame_dump_index() -> u32 {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
}

const PANEL_BG: [f32; 4] = [0.05, 0.05, 0.06, 1.0];
const CLEAR_COLOR: wgpu::Color = wgpu::Color {
    r: 0.05,
    g: 0.05,
    b: 0.06,
    a: 1.0,
};
const CURSOR_BG: [f32; 4] = [0.85, 0.85, 0.82, 1.0];
const FG_DEFAULT: [f32; 4] = [0.847, 0.847, 0.847, 1.0];
const BG_DEFAULT: [f32; 4] = PANEL_BG;
const DIM_FACTOR: f32 = 0.62;
const ATLAS_W: u32 = 2048;
const ATLAS_H: u32 = 2048;

/// xterm-style 16 color palette (0-7 dim, 8-15 bright).
const PALETTE: [[f32; 4]; 16] = [
    [0.0, 0.0, 0.0, 1.0],
    [0.8, 0.0, 0.0, 1.0],
    [0.0, 0.8, 0.0, 1.0],
    [0.8, 0.8, 0.0, 1.0],
    [0.0, 0.0, 0.8, 1.0],
    [0.8, 0.0, 0.8, 1.0],
    [0.0, 0.8, 0.8, 1.0],
    [0.898, 0.898, 0.898, 1.0],
    [0.5, 0.5, 0.5, 1.0],
    [1.0, 0.0, 0.0, 1.0],
    [0.0, 1.0, 0.0, 1.0],
    [1.0, 1.0, 0.0, 1.0],
    [0.36, 0.36, 1.0, 1.0],
    [1.0, 0.0, 1.0, 1.0],
    [0.0, 1.0, 1.0, 1.0],
    [1.0, 1.0, 1.0, 1.0],
];

/// Device-pixel cell metrics (whole px for a stable grid).
#[derive(Debug, Clone, Copy)]
pub struct Metrics {
    pub cell_w: f32,
    pub cell_h: f32,
    /// Baseline distance from cell top (swash scale ascent, y-down).
    pub ascent: f32,
}

/// A rasterized glyph parked in the atlas.
#[derive(Debug, Clone, Copy)]
struct GlyphRef {
    /// (x, y, w, h) in atlas pixels.
    uv: [f32; 4],
    /// Ink offset from the pen/baseline (zeno Placement, y-down).
    left: i32,
    top: i32,
    /// false = alpha coverage (tint by fg), true = color glyph (emoji...).
    color: bool,
}

/// Shelf packer for the glyph atlas.
struct Atlas {
    cursor_x: u32,
    cursor_y: u32,
    row_h: u32,
}

impl Atlas {
    fn new() -> Self {
        Self {
            cursor_x: 1,
            cursor_y: 1,
            row_h: 1,
        }
    }

    fn reset(&mut self) {
        self.cursor_x = 1;
        self.cursor_y = 1;
        self.row_h = 1;
    }

    fn pack(&mut self, w: u32, h: u32) -> Option<[u32; 2]> {
        if self.cursor_y + h + 1 >= ATLAS_H {
            return None;
        }
        if self.cursor_x + w + 1 > ATLAS_W {
            self.cursor_y += self.row_h + 1;
            self.cursor_x = 1;
            self.row_h = 1;
            if self.cursor_y + h + 1 >= ATLAS_H {
                return None;
            }
        }
        let pos = [self.cursor_x, self.cursor_y];
        self.cursor_x += w + 1;
        self.row_h = self.row_h.max(h);
        Some(pos)
    }
}

/// Pending texture upload from a rasterized glyph.
struct Upload {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    /// RGBA bytes, w*h*4, rows top-down.
    bytes: Vec<u8>,
}

/// Per-font glyph cache + raster contexts.
pub struct FontCtx {
    /// Font data kept alive for the transmuted 'static FontRef.
    _data: Vec<u8>,
    font: FontRef<'static>,
    size_px: f32,
    ctx: ScaleContext,
    /// char -> raster (None = glyph unavailable in this font).
    glyphs: HashMap<char, Option<GlyphRef>>,
    pub metrics: Metrics,
}

impl FontCtx {
    /// Bundled fallback when no system font matches (OFL; keeps M1 usable on minimal installs).
    const FALLBACK_FONT: &'static [u8] = include_bytes!("../fonts/JetBrainsMono-Regular.ttf");

    /// Load `family` (fallback path: system monospace -> bundled font) at `size_px` device px.
    pub fn new(family: &str, size_px: f32) -> anyhow::Result<Self> {
        let mut db = Database::new();
        db.load_system_fonts();
        let query = Query {
            families: &[Family::Name(family), Family::Name("monospace")],
            weight: Weight::NORMAL,
            stretch: Stretch::Normal,
            style: Style::Normal,
        };
        let id = db.query(&query).or_else(|| {
            db.query(&Query {
                families: &[Family::Monospace],
                weight: Weight::NORMAL,
                stretch: Stretch::Normal,
                style: Style::Normal,
            })
        });
        let (data, face_index) = id
            .and_then(|id| db.with_face_data(id, |d, idx| (d.to_vec(), idx)))
            .unwrap_or_else(|| (Self::FALLBACK_FONT.to_vec(), 0));
        let face_index = face_index as usize;
        let font_ref = FontRef::from_index(&data, face_index)
            .ok_or_else(|| anyhow::anyhow!("invalid sfnt font"))?;
        // SAFETY: `data` is owned by this struct and outlives the FontRef.
        let font: FontRef<'static> = unsafe { std::mem::transmute(font_ref) };
        let source_desc = id
            .and_then(|id| db.face(id))
            .map(|fi| {
                format!(
                    "system '{}' (index {face_index}, ps {:?})",
                    fi.families
                        .first()
                        .map(|f| &f.0)
                        .unwrap_or(&"?".to_string()),
                    fi.post_script_name
                )
            })
            .unwrap_or_else(|| "bundled JetBrainsMono-Regular".to_string());
        dlog!("font: family={family:?} -> {source_desc}");

        Ok(Self {
            _data: data,
            font,
            size_px,
            ctx: swash::scale::ScaleContext::new(),
            glyphs: HashMap::new(),
            metrics: Self::compute_metrics(&font, size_px),
        })
    }

    fn compute_metrics(font: &FontRef<'_>, size_px: f32) -> Metrics {
        let m = font.metrics(&[]).scale(size_px);
        // swash: ascent = baseline->top (positive), descent = baseline->bottom (negative).
        let ascent = m.ascent;
        let cell_h = (m.ascent - m.descent).clamp(6.0, 128.0).round();
        let gid = font.charmap().map('M');
        let cell_w = font
            .glyph_metrics(&[])
            .scale(size_px)
            .advance_width(gid)
            .clamp(cell_h * 0.25, cell_h * 2.0)
            .round();
        Metrics {
            cell_w,
            cell_h,
            ascent,
        }
    }

    /// Probe a handful of representative chars; part of BERTH_DEBUG triage output.
    pub fn raster_probe(&mut self, chars: &[char]) {
        if !debug_enabled() {
            return;
        }
        let mut ok = 0usize;
        for &c in chars {
            let gid = self.font.charmap().map(c);
            if gid == 0 {
                dlog!("probe '{c}' (U+{:04X}): not in font", c as u32);
                continue;
            }
            let mut scaler = self.ctx.builder(self.font).size(self.size_px).build();
            let sources = [
                Source::ColorOutline(0),
                Source::ColorBitmap(StrikeWith::BestFit),
                Source::Outline,
            ];
            let mut render = Render::new(&sources);
            let render = render.format(zeno::Format::Alpha);
            match render.render(&mut scaler, gid) {
                Some(image) => {
                    ok += 1;
                    dlog!(
                        "probe '{c}' (U+{:04X}): gid={gid} {}x{} content={:?}",
                        c as u32,
                        image.placement.width,
                        image.placement.height,
                        image.content
                    );
                }
                None => dlog!("probe '{c}' (U+{:04X}): gid={gid} RENDER FAILED", c as u32),
            }
        }
        dlog!("probe done: {ok}/{} rasterized", chars.len());
    }

    /// Update rasters after DPI/size change; invalidates the glyph cache.
    pub fn set_size(&mut self, size_px: f32) {
        if (size_px - self.size_px).abs() > 0.01 {
            self.size_px = size_px;
            self.metrics = Self::compute_metrics(&self.font, size_px);
            self.glyphs.clear();
            self.ctx = swash::scale::ScaleContext::new();
        }
    }
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct Uniforms {
    /// Screen size in device px (y-down -> NDC flip in the shader).
    screen: [f32; 2],
    _pad: [f32; 2],
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct Vertex {
    pos: [f32; 2],
    color: [f32; 4],
    /// Normalized atlas coords; (0,0) for plain color quads.
    uv: [f32; 2],
    /// 0 = plain color, 1 = alpha coverage, 2 = color glyph.
    tex: u32,
}

/// Quad accumulator for one frame.
#[derive(Default)]
struct QuadSink {
    verts: Vec<Vertex>,
    uploads: Vec<Upload>,
}

const QUAD_LIMIT: usize = (16 << 20) / 4 / std::mem::size_of::<Vertex>();

impl QuadSink {
    fn push(
        &mut self,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        color: [f32; 4],
        glyph: Option<GlyphRef>,
    ) {
        if self.verts.len() + 6 > QUAD_LIMIT {
            return; // grid larger than M1 budget; drop excess quads
        }
        let [r, g, b, a] = color;
        let (u0, v0, u1, v1, tex) = match glyph {
            Some(gl) => (
                gl.uv[0] / ATLAS_W as f32,
                gl.uv[1] / ATLAS_H as f32,
                (gl.uv[0] + gl.uv[2]) / ATLAS_W as f32,
                (gl.uv[1] + gl.uv[3]) / ATLAS_H as f32,
                if gl.color { 2u32 } else { 1u32 },
            ),
            None => (0.0, 0.0, 0.0, 0.0, 0u32),
        };
        let (bl, br, tl, tr) = (
            Vertex {
                pos: [x0, y1],
                color: [r, g, b, a],
                uv: [u0, v1],
                tex,
            },
            Vertex {
                pos: [x1, y1],
                color: [r, g, b, a],
                uv: [u1, v1],
                tex,
            },
            Vertex {
                pos: [x0, y0],
                color: [r, g, b, a],
                uv: [u0, v0],
                tex,
            },
            Vertex {
                pos: [x1, y0],
                color: [r, g, b, a],
                uv: [u1, v0],
                tex,
            },
        );
        // Triangle-list quad: (bl, br, tl) + (br, tr, tl).
        self.verts.extend([bl, br, tl, br, tr, tl]);
    }
}

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniforms: wgpu::Buffer,
    atlas_tex: wgpu::Texture,
    atlas: Atlas,
    font: FontCtx,
    quad_buf: wgpu::Buffer,
}

impl Renderer {
    /// Create surface + pipeline; call once per window.
    pub fn new(window: &Window, family: &str, size_pt: f32) -> anyhow::Result<Self> {
        let scale = window.scale_factor();
        let size_px = size_pt * 1.333_333 * scale as f32;
        let mut font = FontCtx::new(family, size_px)?;
        let phys = window.inner_size();
        dlog!(
            "renderer: scale={scale} size_px={size_px} metrics={:?} surface={}x{}",
            font.metrics,
            phys.width,
            phys.height
        );
        font.raster_probe(&['M', 'p', '%']);

        // Env-overridable (WGPU_BACKEND etc. via with_env), display handled per-surface.
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        // SAFETY: the caller keeps `window` alive (app holds it in a field) while
        // the surface lives inside the Renderer returned here. Unlike from_window(),
        // from_display_and_window() actually carries the display handle (wgpu 30
        // from_window() deliberately sets it to None).
        let surface = unsafe {
            instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::from_display_and_window(
                window, window,
            )?)?
        };
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            ..Default::default()
        }))?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))?;
        // Surface backend validation errors loudly: glyphs/atlas issues show up here.
        device.on_uncaptured_error(std::sync::Arc::new(|err| {
            eprintln!("wgpu uncaptured error: {err}");
        }));

        let size = window.inner_size();
        let config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .ok_or_else(|| anyhow::anyhow!("no matching surface configuration"))?;
        // COPY_SRC enables BERTH_FRAME_DUMP readbacks.
        let mut config = config;
        config.usage |= wgpu::TextureUsages::COPY_SRC;

        let atlas_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("berth-atlas"),
            size: wgpu::Extent3d {
                width: ATLAS_W,
                height: ATLAS_H,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let atlas_view = atlas_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("berth-uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("berth-bind-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("berth-bind-group"),
            layout: &bind_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("berth-shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let quad_capacity = 16 << 20;
        let quad_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("berth-quads"),
            size: quad_capacity,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("berth-pipeline"),
            layout: Some(
                &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("berth-pipeline-layout"),
                    bind_group_layouts: &[Some(&bind_layout)],
                    immediate_size: 0,
                }),
            ),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 0,
                            shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 8,
                            shader_location: 1,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 24,
                            shader_location: 2,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Uint32,
                            offset: 32,
                            shader_location: 3,
                        },
                    ],
                })],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });

        Ok(Self {
            surface,
            device,
            queue,
            config,
            pipeline,
            bind_group,
            uniforms,
            atlas_tex,
            atlas: Atlas::new(),
            font,
            quad_buf,
        })
    }

    /// Font metrics in device px.
    pub fn metrics(&self) -> Metrics {
        self.font.metrics
    }

    pub fn resize_surface(&mut self, window: &Window) {
        let size = window.inner_size();
        if size.width > 0 && size.height > 0 {
            dlog!("resize_surface: {}x{}", size.width, size.height);
            self.config.width = size.width;
            self.config.height = size.height;
            self.surface.configure(&self.device, &self.config);
        }
    }

    /// Rasterize `c` (cached); queues the atlas upload on first use.
    fn rasterize(&mut self, c: char, sink: &mut QuadSink) -> Option<GlyphRef> {
        if let Some(cached) = self.font.glyphs.get(&c).copied() {
            return cached;
        }
        let font = &mut self.font;
        let mut scaler = font.ctx.builder(font.font).size(font.size_px).build();
        let gid = font.font.charmap().map(c);
        let mut found = None;
        if gid != 0 {
            let sources = [
                Source::ColorOutline(0),
                Source::ColorBitmap(StrikeWith::BestFit),
                Source::Outline,
            ];
            let mut render = Render::new(&sources);
            let render = render.format(zeno::Format::Alpha);
            if let Some(image) = render.render(&mut scaler, gid) {
                let is_color = matches!(image.content, Content::Color);
                let placement = image.placement;
                let bytes = if is_color {
                    image.data.clone()
                } else {
                    image
                        .data
                        .iter()
                        .flat_map(|&a| [255u8, 255, 255, a])
                        .collect::<Vec<u8>>()
                };
                let w = placement.width;
                let h = placement.height;
                if w > 0 && h > 0 {
                    match self.atlas.pack(w, h) {
                        Some([px, py]) => {
                            let g = GlyphRef {
                                uv: [px as f32, py as f32, w as f32, h as f32],
                                left: placement.left,
                                top: placement.top,
                                color: is_color,
                            };
                            sink.uploads.push(Upload {
                                x: px,
                                y: py,
                                w,
                                h,
                                bytes,
                            });
                            found = Some(g);
                        }
                        None => {
                            // Atlas exhausted: reset packer; glyph cache cleared by reset().
                            self.atlas.reset();
                            self.font.glyphs.clear();
                        }
                    }
                }
            }
        }
        self.font.glyphs.insert(c, found);
        found
    }

    /// Full frame from one visible pane grid: build quads, resolve, draw.
    pub fn draw(&mut self, term: &Term<VoidListener>, window: &Window) -> anyhow::Result<()> {
        let phys = window.inner_size();
        let screen_w = phys.width as f32;
        let screen_h = phys.height as f32;
        self.queue.write_buffer(
            &self.uniforms,
            0,
            bytemuck::bytes_of(&Uniforms {
                screen: [screen_w, screen_h],
                _pad: [0.0; 2],
            }),
        );

        let metrics = self.font.metrics;
        let pane = (0.0f32, 0.0f32); // pane covers the window (M1)
        let cols = ((screen_w - pane.0) / metrics.cell_w).floor().max(1.0) as usize;
        let rows = ((screen_h - pane.1) / metrics.cell_h).floor().max(1.0) as usize;
        dlog!(
            "draw: screen={screen_w}x{screen_h} cell={0:.1}x{1:.1} grid={cols}x{rows}",
            metrics.cell_w,
            metrics.cell_h,
        );
        let quad_trace = std::env::var("BERTH_TRACE").ok().as_deref() == Some("1");

        let content = term.renderable_content();
        dlog!(
            "term grid: {}x{} display_offset={}",
            term.grid().columns(),
            term.grid().screen_lines(),
            content.display_offset
        );
        let mut dump_rows: Vec<String> = Vec::new();
        let mut dump_cur: Option<(usize, String)> = None; // (row, chars)
        let mut sink = QuadSink::default();
        let cursor = point_to_viewport(content.display_offset, content.cursor.point);
        let (mut quads_bg, mut quads_glyph) = (0usize, 0usize);

        // Row-major iteration over the visible viewport cells.
        let mut cells = content.display_iter;
        for idx in 0..(rows * cols) {
            let Some(indexed) = cells.next() else { break };
            let cell = indexed.cell;
            let row = idx / cols;
            let col = idx % cols;
            let px = pane.0 + col as f32 * metrics.cell_w;
            let py = pane.1 + row as f32 * metrics.cell_h;

            // bg (respect selection tint).
            let mut is_default_bg = cell.bg == Color::Named(NamedColor::Background);
            let mut bg = color_of(cell.bg);
            let selected = content
                .selection
                .as_ref()
                .map(|sel| {
                    let pt =
                        viewport_to_point(content.display_offset, Point::new(row, Column(col)));
                    sel.contains(pt)
                })
                .unwrap_or(false);
            if selected {
                bg = blend(bg);
                is_default_bg = false;
            }
            if !is_default_bg {
                sink.push(px, py, px + metrics.cell_w, py + metrics.cell_h, bg, None);
                quads_bg += 1;
            }
            if std::env::var("BERTH_DUMP").ok().as_deref() == Some("1") {
                match &mut dump_cur {
                    Some((r, chars)) if *r == row => {}
                    _ => {
                        if let Some((r, chars)) = dump_cur.take() {
                            dump_rows.push(format!("[{r:02}] |{chars}|"));
                        }
                        dump_cur = Some((row, String::new()));
                    }
                }
                if let Some((_, chars)) = &mut dump_cur {
                    chars.push(cell.c);
                }
            }

            let c = cell.c;
            // cursor: block drawn under the glyph (shown here in inverse). Must be
            // handled before the empty-cell skip so the block shows on blank rows.
            let mut fg = color_of(cell.fg);
            if selected {
                fg = blend(fg);
            }
            let on_cursor = cursor
                .map(|vp| vp.line == row && vp.column.0 == col)
                .unwrap_or(false);
            if on_cursor {
                sink.push(
                    px,
                    py,
                    px + metrics.cell_w,
                    py + metrics.cell_h,
                    CURSOR_BG,
                    None,
                );
            }
            if c == ' ' || c == '\0' || cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            fg = if on_cursor { BG_DEFAULT } else { fg };

            if let Some(g) = self.rasterize(c, &mut sink) {
                // swash origin = (pen_x, baseline_y), y-up: the image's top edge
                // sits `top` px above the baseline, `left` px right of the pen.
                let gx = px + g.left as f32;
                let gy = py + metrics.ascent - g.top as f32;
                sink.push(gx, gy, gx + g.uv[2], gy + g.uv[3], fg, Some(g));
                quads_glyph += 1;
                if quad_trace && row < 4 {
                    dlog!(
                        "quad [{row},{col}] '{c}' gx={gx:.2} gy={gy:.2} gx2={:.2} gy2={:.2} tex={}",
                        gx + g.uv[2],
                        gy + g.uv[3],
                        if g.color { 2u32 } else { 1u32 }
                    );
                }
            } else if debug_enabled() {
                dlog!("char '{}' (U+{:04X}) has no glyph quad", c, c as u32);
            }
        }

        dlog!(
            "draw done: bg_quads={} glyph_quads={} uploads={} total_verts={}",
            quads_bg,
            quads_glyph,
            sink.uploads.len(),
            sink.verts.len()
        );
        if !dump_rows.is_empty() {
            if let Some((r, chars)) = dump_cur.take() {
                dump_rows.push(format!("[{r:02}] |{chars}|"));
            }
            for line in &dump_rows {
                dlog!("{line}");
            }
        }

        // Upload newly rastered glyphs.
        for u in &sink.uploads {
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.atlas_tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: u.x,
                        y: u.y,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                &u.bytes,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(u.w * 4),
                    rows_per_image: Some(u.h),
                },
                wgpu::Extent3d {
                    width: u.w,
                    height: u.h,
                    depth_or_array_layers: 1,
                },
            );
        }

        let frame = match self.surface.get_current_texture() {
            CurrentSurfaceTexture::Success(t) => t,
            CurrentSurfaceTexture::Suboptimal(t) => t,
            CurrentSurfaceTexture::Timeout
            | CurrentSurfaceTexture::Occluded
            | CurrentSurfaceTexture::Outdated => return Ok(()),
            _ => anyhow::bail!("surface texture lost/invalid"),
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("berth-frame"),
            });

        if !sink.verts.is_empty() {
            self.queue
                .write_buffer(&self.quad_buf, 0, bytemuck::cast_slice(&sink.verts));
        }

        {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("berth-pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: Operations {
                        load: LoadOp::Clear(CLEAR_COLOR),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, self.quad_buf.slice(..));
            let n = sink.verts.len() as u32;
            if n > 0 {
                pass.draw(0..n, 0..1);
            }
        }
        self.queue.submit(Some(encoder.finish()));

        if frame_dump_enabled() {
            let (w, h) = (self.config.width, self.config.height);
            let padded = (w * 4).div_ceil(256) * 256;
            let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("berth-frame-dump"),
                size: padded as u64 * h as u64,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut enc = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("berth-frame-dump-enc"),
                });
            enc.copy_texture_to_buffer(
                frame.texture.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &buf,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(padded),
                        rows_per_image: None,
                    },
                },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
            );
            self.queue.submit(Some(enc.finish()));

            // flush copy, map, read back (debug only)
            let slice = buf.slice(..);
            let (txm, rxm) = std::sync::mpsc::channel();
            slice.map_async(wgpu::MapMode::Read, move |_| {
                let _ = txm.send(());
            });
            let _ = self.device.poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(std::time::Duration::from_secs(2)),
            });
            let _ = rxm.recv_timeout(std::time::Duration::from_secs(2));
            let data = slice
                .get_mapped_range()
                .map_err(|e| anyhow::anyhow!("frame dump map failed: {e}"))?;
            let fx = format!("/tmp/opencode/berth-frame-{}.ppm", frame_dump_index());
            if let Ok(mut f) = std::fs::File::create(&fx) {
                use std::io::Write;
                let _ = write!(f, "P6\n{w} {h}\n255\n");
                for y in 0..h as usize {
                    let row = &data[y * padded as usize..][..(w * 4) as usize];
                    for px in row.as_chunks::<4>().0 {
                        // surface format (bgra) -> ppm rgb, expected srgb bytes
                        let _ = f.write_all(&[px[2], px[1], px[0]]);
                    }
                }
            }
            drop(data);
            buf.unmap();
            dlog!("frame dumped: {fx}");
        }
        self.queue.present(frame);
        Ok(())
    }
}

/// Map a term Color to linear RGBA32; handles dim and 256-color variants.
fn color_of(c: Color) -> [f32; 4] {
    match c {
        Color::Named(NamedColor::Foreground) => FG_DEFAULT,
        Color::Named(NamedColor::Background) => BG_DEFAULT,
        Color::Named(n) => match n {
            NamedColor::Black => PALETTE[0],
            NamedColor::Red => PALETTE[1],
            NamedColor::Green => PALETTE[2],
            NamedColor::Yellow => PALETTE[3],
            NamedColor::Blue => PALETTE[4],
            NamedColor::Magenta => PALETTE[5],
            NamedColor::Cyan => PALETTE[6],
            NamedColor::White => PALETTE[7],
            NamedColor::BrightBlack => PALETTE[8],
            NamedColor::BrightRed => PALETTE[9],
            NamedColor::BrightGreen => PALETTE[10],
            NamedColor::BrightYellow => PALETTE[11],
            NamedColor::BrightBlue => PALETTE[12],
            NamedColor::BrightMagenta => PALETTE[13],
            NamedColor::BrightCyan => PALETTE[14],
            NamedColor::BrightWhite => PALETTE[15],
            // Dim variants (SGR 2).
            NamedColor::DimBlack => dim(PALETTE[0]),
            NamedColor::DimRed => dim(PALETTE[1]),
            NamedColor::DimGreen => dim(PALETTE[2]),
            NamedColor::DimYellow => dim(PALETTE[3]),
            NamedColor::DimBlue => dim(PALETTE[4]),
            NamedColor::DimMagenta => dim(PALETTE[5]),
            NamedColor::DimCyan => dim(PALETTE[6]),
            NamedColor::DimWhite => dim(PALETTE[7]),
            NamedColor::DimForeground => dim(FG_DEFAULT),
            _ => PALETTE[7],
        },
        Color::Spec(Rgb { r, g, b }) => [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0],
        Color::Indexed(i) => indexed(i),
    }
}

/// xterm-256 palette into PALETTE slot range.
fn indexed(i: u8) -> [f32; 4] {
    let (r, g, b) = if i < 16 {
        let c = PALETTE[i as usize];
        (
            (c[0] * 255.0) as u8,
            (c[1] * 255.0) as u8,
            (c[2] * 255.0) as u8,
        )
    } else if i < 232 {
        let i = i as u32 - 16;
        let lvl = |v: u32| if v == 0 { 0 } else { 55 + v * 40 };
        (lvl(i / 36) as u8, lvl((i % 36) / 6) as u8, lvl(i % 6) as u8)
    } else {
        let v = ((i as u32 - 232) * 10 + 8) as u8;
        (v, v, v)
    };
    [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0]
}

fn dim(c: [f32; 4]) -> [f32; 4] {
    [
        c[0] * DIM_FACTOR,
        c[1] * DIM_FACTOR,
        c[2] * DIM_FACTOR,
        c[3],
    ]
}

fn blend(c: [f32; 4]) -> [f32; 4] {
    [
        c[0] * 0.5 + FG_DEFAULT[0] * 0.5,
        c[1] * 0.5 + FG_DEFAULT[1] * 0.5,
        c[2] * 0.5 + FG_DEFAULT[2] * 0.5,
        1.0,
    ]
}

/// Shader source exposed for offline WGSL validation tests.
pub fn shader_source() -> &'static str {
    SHADER
}

const SHADER: &str = r#"struct Uniforms {
    screen: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var atlas: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

struct VertexInput {
    @location(0) pos: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) tex: u32,
};

struct VertexOutput {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) @interpolate(flat) tex: u32,
    @location(2) uv: vec2<f32>,
};

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    // CPU coords are device px, y-down; flip into NDC.
    let x = 2.0 * in.pos.x / u.screen.x - 1.0;
    let y = 1.0 - 2.0 * in.pos.y / u.screen.y;
    let clip = vec4<f32>(x, y, 0.0, 1.0);
    // naga: struct construction is parenthesized (positional, declaration order).
    return VertexOutput(clip, in.color, in.tex, in.uv);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    if (in.tex == 1u) {
        let cov = textureSample(atlas, samp, in.uv).a;
        return vec4<f32>(in.color.rgb, in.color.a * cov);
    } else if (in.tex == 2u) {
        let t = textureSample(atlas, samp, in.uv);
        return vec4<f32>(t.rgb, t.a * in.color.a);
    } else {
        return in.color;
    }
}
"#;
