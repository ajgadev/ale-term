//! GPU rendering for ale-term.
//!
//! Everything on screen is one of two things: a solid rectangle or a glyph.
//! The app describes a frame by calling [`Renderer::rect`] and
//! [`Renderer::cell`] / [`Renderer::text`]; each call appends an *instance*
//! to a list, and [`Renderer::present`] uploads the list and draws it in a
//! single draw call.
//!
//! All coordinates are physical pixels with the origin at the top-left.
//!
//! This crate knows nothing about terminals or windows: it is handed a
//! surface to draw on and draws what it is told.

mod atlas;
mod font;
mod upload;

use std::collections::HashMap;

use atlas::ShelfPacker;
use font::FontStack;
use unicode_width::UnicodeWidthChar;

pub use font::CellMetrics;

/// Straight (non-premultiplied) RGBA, 8 bits per channel.
pub type Color = [u8; 4];

/// Makes an opaque color from RGB.
pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
    [r, g, b, 255]
}

const KIND_RECT: u32 = 0;
const KIND_MASK: u32 = 1;
const KIND_COLOR: u32 = 2;
const KIND_IMAGE: u32 = 3;

/// One rectangle sent to the GPU. Must match `Instance` in `shader.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Instance {
    pos: [f32; 2],
    size: [f32; 2],
    uv: [f32; 4],
    color: Color,
    kind: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Globals {
    screen: [f32; 2],
    atlas: [f32; 2],
}

/// A glyph bitmap stored in the atlas.
#[derive(Clone, Copy)]
struct Sprite {
    /// Position in the atlas, in pixels.
    atlas_x: f32,
    atlas_y: f32,
    width: f32,
    height: f32,
    /// Offset from the cell's top-left corner.
    left: f32,
    top: f32,
    color: bool,
}

/// A run of sprites in `Renderer::sprites`: the glyphs for one cell.
#[derive(Clone, Copy)]
struct SpriteRun {
    start: u32,
    len: u32,
}

/// An inline image uploaded to the GPU.
struct ImageTexture {
    /// Binds this image in place of the glyph atlas.
    bind_group: wgpu::BindGroup,
    /// GPU memory used, for the image budget.
    bytes: usize,
}

/// A run of instances drawn with the same texture bound: the glyph atlas
/// (`image` is `None`) or one image. Batches are drawn in order, so things
/// queued later appear on top.
struct Batch {
    image: Option<u64>,
    start: u32,
    count: u32,
}

/// Bold and italic packed into one small cache key.
fn style_bits(bold: bool, italic: bool) -> u8 {
    bold as u8 | (italic as u8) << 1
}

const INITIAL_ATLAS_SIZE: u32 = 1024;
const MAX_ATLAS_SIZE: u32 = 4096;

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,

    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    bind_group: wgpu::BindGroup,
    globals_buffer: wgpu::Buffer,
    sampler: wgpu::Sampler,
    atlas_texture: wgpu::Texture,

    /// Gets each frame's instances to the GPU; see the `upload` module.
    uploader: upload::InstanceUploader,
    instances: Vec<Instance>,
    batches: Vec<Batch>,
    clear: Color,

    images: HashMap<u64, ImageTexture>,
    /// Image keys, oldest first, for evicting when over budget.
    image_order: std::collections::VecDeque<u64>,
    image_bytes: usize,
    max_texture_side: u32,

    font: FontStack,
    packer: ShelfPacker,
    sprites: Vec<Sprite>,
    /// Glyphs for cells holding a single character (nearly all of them).
    char_cache: HashMap<(char, u8), SpriteRun>,
    /// Glyphs for cells holding a character plus combining marks.
    cluster_cache: HashMap<(Box<str>, u8), SpriteRun>,
    /// Set when the atlas was rebuilt mid-frame, which invalidates glyphs
    /// already queued. The frame is drawn again to fix it.
    atlas_rebuilt: bool,
}

/// Font settings in physical pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct FontSpec {
    pub family: String,
    pub size_px: f32,
    pub line_height: f32,
}

impl Renderer {
    /// Sets up the GPU for drawing to `target` (a window).
    pub async fn new(
        target: impl Into<wgpu::SurfaceTarget<'static>>,
        width: u32,
        height: u32,
        font: &FontSpec,
    ) -> Result<Renderer, String> {
        let started = std::time::Instant::now();
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let surface = instance.create_surface(target).map_err(|e| e.to_string())?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                // The integrated GPU is plenty for text and uses less power.
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: Some(&surface),
                ..Default::default()
            })
            .await
            .map_err(|e| e.to_string())?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("ale-term"),
                // Modest limits, except textures as large as the GPU allows
                // so big windows and the glyph atlas fit.
                required_limits: wgpu::Limits::downlevel_defaults()
                    .using_resolution(adapter.limits()),
                // Lets frame data be written straight into GPU-visible
                // memory (see `upload`); optional.
                required_features: adapter.features() & upload::InstanceUploader::FEATURE,
                memory_hints: wgpu::MemoryHints::MemoryUsage,
                ..Default::default()
            })
            .await
            .map_err(|e| e.to_string())?;

        let max_texture_side = device.limits().max_texture_dimension_2d;
        let caps = surface.get_capabilities(&adapter);
        let mut config = surface
            .get_default_config(&adapter, width.max(1), height.max(1))
            .ok_or("the GPU cannot draw to this window")?;
        // Prefer a non-sRGB format: blending text in gamma space is what
        // other terminals do and keeps light-on-dark text from looking thin.
        if let Some(format) = caps.formats.iter().copied().find(|f| !f.is_srgb()) {
            config.format = format;
        }
        config.present_mode = wgpu::PresentMode::Fifo;
        config.desired_maximum_frame_latency = 1;
        surface.configure(&device, &config);

        let shader = device.create_shader_module(wgpu::include_wgsl!("shader.wgsl"));
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("globals+atlas"),
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
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("main"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("main"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: size_of::<Instance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x2, 1 => Float32x2, 2 => Float32x4, 3 => Unorm8x4, 4 => Uint32
                    ],
                })],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let globals_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globals"),
            size: size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("atlas"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let atlas_texture = create_atlas_texture(&device, INITIAL_ATLAS_SIZE);
        let bind_group =
            create_bind_group(&device, &bind_group_layout, &globals_buffer, &atlas_texture, &sampler);

        log::info!("gpu ready in {:?}", started.elapsed());
        let uploader = upload::InstanceUploader::new(&device);

        let started = std::time::Instant::now();
        let font = FontStack::new(&font.family, font.size_px, font.line_height);
        log::info!("fonts ready in {:?}", started.elapsed());
        let renderer = Renderer {
            surface,
            device,
            queue,
            config,
            pipeline,
            bind_group_layout,
            bind_group,
            globals_buffer,
            sampler,
            atlas_texture,
            uploader,
            instances: Vec::with_capacity(4096),
            batches: Vec::new(),
            clear: [0, 0, 0, 255],
            images: HashMap::new(),
            image_order: Default::default(),
            image_bytes: 0,
            max_texture_side,
            font,
            packer: ShelfPacker::new(INITIAL_ATLAS_SIZE),
            sprites: Vec::new(),
            char_cache: HashMap::new(),
            cluster_cache: HashMap::new(),
            atlas_rebuilt: false,
        };
        renderer.write_globals();
        Ok(renderer)
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        let (width, height) = (width.max(1), height.max(1));
        if (width, height) == (self.config.width, self.config.height) {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        self.write_globals();
    }

    pub fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    /// Switches font. Every cached glyph is thrown away.
    pub fn set_font(&mut self, font: &FontSpec) {
        self.font.set_font(&font.family, font.size_px, font.line_height);
        self.reset_atlas(self.packer.size());
    }

    pub fn metrics(&self) -> CellMetrics {
        self.font.metrics()
    }

    // ---- Describing a frame -------------------------------------------

    /// Starts a new frame filled with `clear`.
    pub fn begin(&mut self, clear: Color) {
        self.instances.clear();
        self.batches.clear();
        self.clear = clear;
        self.atlas_rebuilt = false;
    }

    /// A solid rectangle.
    pub fn rect(&mut self, x: f32, y: f32, width: f32, height: f32, color: Color) {
        if width <= 0.0 || height <= 0.0 || color[3] == 0 {
            return;
        }
        self.push(Instance { pos: [x, y], size: [width, height], uv: [0.0; 4], color, kind: KIND_RECT }, None);
    }

    /// Queues an instance, extending the current batch when it uses the
    /// same texture and starting a new one otherwise.
    fn push(&mut self, instance: Instance, image: Option<u64>) {
        let index = self.instances.len() as u32;
        self.instances.push(instance);
        match self.batches.last_mut() {
            Some(batch) if batch.image == image => batch.count += 1,
            _ => self.batches.push(Batch { image, start: index, count: 1 }),
        }
    }

    // ---- Images -------------------------------------------------------

    /// Uploads an image under `key`, replacing any image with that key.
    /// Images too large for the GPU are ignored.
    pub fn upload_image(&mut self, key: u64, width: u32, height: u32, rgba: &[u8]) {
        let fits = width > 0 && height > 0 && width.max(height) <= self.max_texture_side;
        if !fits || rgba.len() < (width * height * 4) as usize {
            return;
        }
        self.remove_image(key);
        let size = wgpu::Extent3d { width, height, depth_or_array_layers: 1 };
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("image"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            texture.as_image_copy(),
            &rgba[..(width * height * 4) as usize],
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(width * 4), rows_per_image: None },
            size,
        );
        let bind_group = create_bind_group(
            &self.device,
            &self.bind_group_layout,
            &self.globals_buffer,
            &texture,
            &self.sampler,
        );
        let bytes = (width * height * 4) as usize;
        self.images.insert(key, ImageTexture { bind_group, bytes });
        self.image_order.push_back(key);
        self.image_bytes += bytes;
    }

    pub fn remove_image(&mut self, key: u64) {
        if let Some(image) = self.images.remove(&key) {
            self.image_bytes -= image.bytes;
            self.image_order.retain(|k| *k != key);
        }
    }

    /// Removes every image whose key fails `keep`.
    pub fn retain_images(&mut self, keep: impl Fn(u64) -> bool) {
        let doomed: Vec<u64> = self.images.keys().copied().filter(|key| !keep(*key)).collect();
        for key in doomed {
            self.remove_image(key);
        }
    }

    /// Frees the oldest images until at most `budget` bytes of image
    /// memory are in use.
    pub fn limit_image_memory(&mut self, budget: usize) {
        while self.image_bytes > budget {
            let Some(oldest) = self.image_order.front().copied() else { break };
            self.remove_image(oldest);
        }
    }

    pub fn image_memory(&self) -> usize {
        self.image_bytes
    }

    /// Draws part of an image. `uv` is the part to show as x, y, width and
    /// height in the 0..1 range. Does nothing if the image is not loaded.
    pub fn image(&mut self, key: u64, x: f32, y: f32, width: f32, height: f32, uv: [f32; 4]) {
        if width > 0.0 && height > 0.0 && self.images.contains_key(&key) {
            let instance =
                Instance { pos: [x, y], size: [width, height], uv, color: [255; 4], kind: KIND_IMAGE };
            self.push(instance, Some(key));
        }
    }

    /// Draws the text of one terminal cell whose top-left corner is at
    /// (`x`, `y`). `text` is a single character, optionally followed by
    /// combining marks.
    pub fn cell(&mut self, text: &str, bold: bool, italic: bool, x: f32, y: f32, color: Color) {
        let style = style_bits(bold, italic);
        let mut chars = text.chars();
        let run = match (chars.next(), chars.next()) {
            (None, _) => return,
            (Some(c), None) => match self.char_cache.get(&(c, style)) {
                Some(run) => *run,
                None => {
                    let run = self.rasterize(text, bold, italic);
                    self.char_cache.insert((c, style), run);
                    run
                }
            },
            _ => match self.cluster_cache.get(&(Box::from(text), style)) {
                Some(run) => *run,
                None => {
                    let run = self.rasterize(text, bold, italic);
                    self.cluster_cache.insert((Box::from(text), style), run);
                    run
                }
            },
        };
        for index in run.start as usize..(run.start + run.len) as usize {
            let sprite = self.sprites[index];
            let instance = Instance {
                pos: [x + sprite.left, y + sprite.top],
                size: [sprite.width, sprite.height],
                uv: [sprite.atlas_x, sprite.atlas_y, sprite.width, sprite.height],
                color,
                kind: if sprite.color { KIND_COLOR } else { KIND_MASK },
            };
            self.push(instance, None);
        }
    }

    /// Draws a line of text on the cell grid starting at (`x`, `y`), cut off
    /// at `max_width` pixels. Returns the width drawn. Used for UI labels.
    pub fn text(&mut self, text: &str, bold: bool, x: f32, y: f32, max_width: f32, color: Color) -> f32 {
        let cell_width = self.font.metrics().width;
        let mut advance = 0.0;
        let mut utf8 = [0u8; 4];
        for c in text.chars() {
            let width = c.width().unwrap_or(0) as f32 * cell_width;
            if width == 0.0 {
                continue;
            }
            if advance + width > max_width {
                break;
            }
            if c != ' ' {
                self.cell(c.encode_utf8(&mut utf8), bold, false, x + advance, y, color);
            }
            advance += width;
        }
        advance
    }

    /// Width in pixels that [`Renderer::text`] would need for `text`.
    pub fn text_width(&self, text: &str) -> f32 {
        let cells: usize = text.chars().map(|c| c.width().unwrap_or(0)).sum();
        cells as f32 * self.font.metrics().width
    }

    // ---- Glyph cache --------------------------------------------------

    /// Rasterizes a cell's glyphs into the atlas, growing the atlas when it
    /// is full.
    fn rasterize(&mut self, text: &str, bold: bool, italic: bool) -> SpriteRun {
        let glyphs = self.font.rasterize(text, bold, italic);
        let start = self.sprites.len() as u32;
        for glyph in &glyphs {
            let slot = match self.packer.allocate(glyph.width, glyph.height) {
                Some(slot) => slot,
                None => {
                    // Atlas full: start over with a bigger one (or the same
                    // size once at the maximum). Everything cached so far is
                    // gone, including this cell's earlier glyphs, so retry.
                    let size = (self.packer.size() * 2).min(MAX_ATLAS_SIZE);
                    if glyph.width + 2 > size || glyph.height + 2 > size {
                        continue;
                    }
                    self.reset_atlas(size);
                    self.atlas_rebuilt = true;
                    return self.rasterize(text, bold, italic);
                }
            };
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.atlas_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x: slot.x, y: slot.y, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                &glyph.rgba,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(glyph.width * 4),
                    rows_per_image: None,
                },
                wgpu::Extent3d { width: glyph.width, height: glyph.height, depth_or_array_layers: 1 },
            );
            self.sprites.push(Sprite {
                atlas_x: slot.x as f32,
                atlas_y: slot.y as f32,
                width: glyph.width as f32,
                height: glyph.height as f32,
                left: glyph.left as f32,
                top: glyph.top as f32,
                color: glyph.color,
            });
        }
        SpriteRun { start, len: self.sprites.len() as u32 - start }
    }

    /// Empties the glyph cache and (re)creates the atlas at `size`.
    fn reset_atlas(&mut self, size: u32) {
        self.sprites.clear();
        self.char_cache.clear();
        self.cluster_cache.clear();
        if size != self.packer.size() {
            self.atlas_texture = create_atlas_texture(&self.device, size);
            self.bind_group = create_bind_group(
                &self.device,
                &self.bind_group_layout,
                &self.globals_buffer,
                &self.atlas_texture,
                &self.sampler,
            );
        }
        self.packer = ShelfPacker::new(size);
        self.write_globals();
    }

    fn write_globals(&self) {
        let globals = Globals {
            screen: [self.config.width as f32, self.config.height as f32],
            atlas: [self.packer.size() as f32; 2],
        };
        self.queue.write_buffer(&self.globals_buffer, 0, bytemuck::bytes_of(&globals));
    }

    // ---- Drawing ------------------------------------------------------

    /// Draws the frame to the window.
    ///
    /// Returns `true` when the frame should be described and presented again
    /// (the glyph atlas was rebuilt part-way through, or the surface had to
    /// be recreated).
    pub fn present(&mut self) -> bool {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return true;
            }
            // Window hidden or the GPU is busy: skip this frame.
            _ => return false,
        };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.draw(&view);
        self.queue.present(frame);
        self.atlas_rebuilt
    }

    /// Draws the frame off-screen and returns it as RGBA pixels with its
    /// width and height. Used for screenshots in tests.
    pub fn capture(&mut self) -> (u32, u32, Vec<u8>) {
        let (width, height) = (self.config.width, self.config.height);
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("capture"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.config.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.draw(&view);

        // Rows in the copy must be padded to a multiple of 256 bytes.
        let row_bytes = width * 4;
        let padded_row = row_bytes.div_ceil(256) * 256;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("capture"),
            size: (padded_row * height) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        self.queue.submit([encoder.finish()]);
        buffer.map_async(wgpu::MapMode::Read, .., |_| {});
        let _ = self.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None });

        let mut rgba = Vec::with_capacity((row_bytes * height) as usize);
        if let Ok(mapped) = buffer.get_mapped_range(..) {
            let bgra = matches!(
                self.config.format,
                wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
            );
            for row in mapped.chunks(padded_row as usize) {
                for pixel in row[..row_bytes as usize].chunks(4) {
                    if bgra {
                        rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
                    } else {
                        rgba.extend_from_slice(pixel);
                    }
                }
            }
        }
        (width, height, rgba)
    }

    fn draw(&mut self, view: &wgpu::TextureView) {
        let instance_buffer =
            self.uploader.upload(&self.device, &self.queue, bytemuck::cast_slice(&self.instances));

        let channel = |v: u8| v as f64 / 255.0;
        let clear = wgpu::Color {
            r: channel(self.clear[0]),
            g: channel(self.clear[1]),
            b: channel(self.clear[2]),
            a: 1.0,
        };
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("frame"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_vertex_buffer(0, instance_buffer.slice(..));
            for batch in &self.batches {
                let bind_group = match batch.image {
                    None => &self.bind_group,
                    // Removed since it was queued: skip.
                    Some(key) => match self.images.get(&key) {
                        Some(image) => &image.bind_group,
                        None => continue,
                    },
                };
                pass.set_bind_group(0, bind_group, &[]);
                pass.draw(0..4, batch.start..batch.start + batch.count);
            }
        }
        self.queue.submit([encoder.finish()]);
        self.uploader.submitted();
    }
}

fn create_atlas_texture(device: &wgpu::Device, size: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("glyph atlas"),
        size: wgpu::Extent3d { width: size, height: size, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

fn create_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    globals: &wgpu::Buffer,
    atlas: &wgpu::Texture,
    sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    let view = atlas.create_view(&wgpu::TextureViewDescriptor::default());
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("globals+atlas"),
        layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: globals.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view) },
            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(sampler) },
        ],
    })
}
