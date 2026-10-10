//! fal-os canvas engine (spike).
//!
//! Platform-independent wgpu renderer: Metal natively, WebGPU in wasm. Techniques borrowed
//! from the Brilliant teardown (docs/research/brilliant-renderer.md):
//! - camera-only frames write one uniform; the instance buffer is rewritten only when dirty
//! - one instanced draw for every node, SDF corners with zoom-independent AA
//! - sub-pixel nodes collapse to zero area instead of changing the draw list
//! - two texture tiers: an always-resident 32px mipmapped tier for every node, and a
//!   256px detail tier with a fixed slot budget and LRU eviction
//! - per-frame upload budgets, progressive cold open, and a cap on active video decoders
//! - pluggable real video decode ([`media::VideoSource`]); new frames are GPU-blitted into detail slots

pub mod media;

use media::*;
use std::collections::{HashMap, HashSet};
use wgpu::util::DeviceExt;

const PAGE: u32 = 2048;
const LOW_PER_ROW: u32 = PAGE / LOW_TILE; // 64
const LOW_PER_PAGE: u32 = LOW_PER_ROW * LOW_PER_ROW; // 4096
const DETAIL_PER_ROW: u32 = PAGE / DETAIL_TILE; // 8
const DETAIL_PER_LAYER: u32 = DETAIL_PER_ROW * DETAIL_PER_ROW; // 64

pub const FLAG_SELECTED: u32 = 1;
pub const FLAG_VIDEO: u32 = 2;
pub const FLAG_LOW_READY: u32 = 4;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Image,
    Video,
}

#[derive(Clone, Debug)]
pub struct Node {
    pub pos: [f32; 2],
    pub size: [f32; 2],
    pub kind: Kind,
    pub seed: u32,
    pub selected: bool,
}

/// Tunables. Every optimization stays switchable so we can measure it.
#[derive(Clone, Debug)]
pub struct Budgets {
    /// Detail tier capacity in 256px slots (multiple of 64). 384 slots ≈ 126 MB of GPU memory with mips.
    pub detail_slots: u32,
    /// Max detail tiles decoded and uploaded per frame.
    pub detail_uploads_per_frame: u32,
    /// Low-tier tile rows (64 tiles each) generated per frame during cold open.
    pub low_rows_per_frame: u32,
    /// Max videos decoding at once; the rest show their last frame or poster.
    pub max_active_videos: usize,
    /// Minimum on-screen size (px) before a node gets a detail tile.
    pub detail_min_px: f32,
    /// Fraction of the viewport added on every side when deciding what to stream.
    pub overscan: f32,
}

impl Default for Budgets {
    fn default() -> Self {
        Self {
            detail_slots: 384,
            detail_uploads_per_frame: 12,
            low_rows_per_frame: 16,
            max_active_videos: 16,
            detail_min_px: 48.0,
            overscan: 0.25,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Camera {
    /// World position of the viewport's top-left corner.
    pub offset: [f32; 2],
    /// Physical pixels per world unit.
    pub zoom: f32,
    /// Viewport size in physical pixels.
    pub viewport: [f32; 2],
}

impl Camera {
    pub fn screen_to_world(&self, p: [f32; 2]) -> [f32; 2] {
        [self.offset[0] + p[0] / self.zoom, self.offset[1] + p[1] / self.zoom]
    }

    pub fn pan(&mut self, dx: f32, dy: f32) {
        self.offset[0] -= dx / self.zoom;
        self.offset[1] -= dy / self.zoom;
    }

    pub fn zoom_at(&mut self, screen: [f32; 2], factor: f32) {
        let before = self.screen_to_world(screen);
        self.zoom = (self.zoom * factor).clamp(0.002, 64.0);
        let after = self.screen_to_world(screen);
        self.offset[0] += before[0] - after[0];
        self.offset[1] += before[1] - after[1];
    }

    /// Frames the world rect `[x, y, w, h]`.
    pub fn fit(&mut self, rect: [f32; 4]) {
        self.zoom = (self.viewport[0] / rect[2]).min(self.viewport[1] / rect[3]) * 0.95;
        self.offset = [
            rect[0] + rect[2] * 0.5 - self.viewport[0] * 0.5 / self.zoom,
            rect[1] + rect[3] * 0.5 - self.viewport[1] * 0.5 / self.zoom,
        ];
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CameraUniform {
    offset: [f32; 2],
    viewport: [f32; 2],
    zoom: f32,
    time: f32,
    _pad: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Instance {
    pos: [f32; 2],
    size: [f32; 2],
    low_uv: [f32; 4],
    low_layer: u32,
    detail_slot: i32,
    color: u32,
    flags: u32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FrameStats {
    pub nodes: usize,
    pub visible: usize,
    pub detail_resident: usize,
    pub detail_uploads: u32,
    pub active_videos: usize,
    /// Real decoded video frames blitted into the detail tier this frame (VideoSource path).
    pub video_frames: u32,
    /// CPU time spent on video this frame (synth+upload, or source calls + blit encoding).
    pub video_ms: f32,
    pub low_ready: usize,
    pub instance_upload: bool,
}

struct DetailCache {
    free: Vec<u32>,
    slot_node: Vec<Option<usize>>,
    last_used: Vec<u64>,
    node_slot: HashMap<usize, u32>,
}

impl DetailCache {
    fn new(capacity: u32) -> Self {
        Self {
            free: (0..capacity).rev().collect(),
            slot_node: vec![None; capacity as usize],
            last_used: vec![0; capacity as usize],
            node_slot: HashMap::new(),
        }
    }

    /// Returns a slot for `node`, evicting the least-recently-used slot not touched this frame.
    fn alloc(&mut self, node: usize, frame: u64) -> Option<(u32, Option<usize>)> {
        let slot = match self.free.pop() {
            Some(s) => s,
            None => {
                let (s, &t) = self.last_used.iter().enumerate().min_by_key(|(_, t)| **t)?;
                if t >= frame {
                    return None;
                }
                s as u32
            }
        };
        let evicted = self.slot_node[slot as usize].take();
        if let Some(e) = evicted {
            self.node_slot.remove(&e);
        }
        self.slot_node[slot as usize] = Some(node);
        self.node_slot.insert(node, slot);
        self.last_used[slot as usize] = frame;
        Some((slot, evicted))
    }
}

pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    camera_buf: wgpu::Buffer,
    instance_buf: wgpu::Buffer,
    low_tex: wgpu::Texture,
    detail_tex: wgpu::Texture,
    budgets: Budgets,

    pub nodes: Vec<Node>,
    instances: Vec<Instance>,
    instances_dirty: bool,
    low_rows_done: u32,
    detail: DetailCache,
    frame: u64,
    scratch: Vec<u8>,

    video: Option<Box<dyn VideoSource>>,
    /// Node index → video id (index among video nodes), `usize::MAX` for images.
    video_id: Vec<usize>,
    playing: HashSet<usize>,
    /// Video nodes holding a slot that still waits for its first real frame.
    video_pending: HashSet<usize>,
    blit_pipeline: wgpu::RenderPipeline,
    blit_layout: wgpu::BindGroupLayout,
    blit_sampler: wgpu::Sampler,
    /// Render-target views of the detail tier, indexed `layer * DETAIL_MIPS + mip`.
    detail_targets: Vec<wgpu::TextureView>,
    blits: Vec<(u32, wgpu::BindGroup)>,
}

impl Renderer {
    pub fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        format: wgpu::TextureFormat,
        nodes: Vec<Node>,
        budgets: Budgets,
    ) -> Self {
        let low_pages = (nodes.len() as u32).div_ceil(LOW_PER_PAGE).max(1);
        let detail_layers = budgets.detail_slots.div_ceil(DETAIL_PER_LAYER).max(1);
        let tex = |label, layers, mips| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d { width: PAGE, height: PAGE, depth_or_array_layers: layers },
                mip_level_count: mips,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
        };
        let low_tex = tex("low tier", low_pages, LOW_MIPS);
        let detail_tex = tex("detail tier", detail_layers, DETAIL_MIPS);
        let array_view = |t: &wgpu::Texture| {
            t.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::D2Array),
                ..Default::default()
            })
        };

        let camera_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("camera"),
            size: std::mem::size_of::<CameraUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });

        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                tex_entry(1),
                tex_entry(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: camera_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&array_view(&low_tex)) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&array_view(&detail_tex)) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&sampler) },
            ],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("canvas"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("canvas"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Instance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x2, 1 => Float32x2, 2 => Float32x4,
                        3 => Uint32, 4 => Sint32, 5 => Uint32, 6 => Uint32
                    ],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });

        let (blit_pipeline, blit_layout) = blit_pipeline(&device);
        let blit_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let detail_targets = (0..detail_layers * DETAIL_MIPS)
            .map(|i| {
                detail_tex.create_view(&wgpu::TextureViewDescriptor {
                    dimension: Some(wgpu::TextureViewDimension::D2),
                    base_mip_level: i % DETAIL_MIPS,
                    mip_level_count: Some(1),
                    base_array_layer: i / DETAIL_MIPS,
                    array_layer_count: Some(1),
                    ..Default::default()
                })
            })
            .collect();
        let mut videos = 0;
        let video_id = nodes
            .iter()
            .map(|n| match n.kind {
                Kind::Video => {
                    videos += 1;
                    videos - 1
                }
                Kind::Image => usize::MAX,
            })
            .collect();

        let instances: Vec<Instance> = nodes.iter().enumerate().map(|(i, n)| instance_for(i, n)).collect();
        let instance_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("instances"),
            contents: bytemuck::cast_slice(&instances),
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        });

        Self {
            detail: DetailCache::new(budgets.detail_slots),
            device,
            queue,
            pipeline,
            bind_group,
            camera_buf,
            instance_buf,
            low_tex,
            detail_tex,
            budgets,
            nodes,
            instances,
            instances_dirty: false,
            low_rows_done: 0,
            frame: 0,
            scratch: Vec::new(),
            video: None,
            video_id,
            playing: HashSet::new(),
            video_pending: HashSet::new(),
            blit_pipeline,
            blit_layout,
            blit_sampler,
            detail_targets,
            blits: Vec::new(),
        }
    }

    /// Number of video nodes; video ids passed to a [`VideoSource`] are `0..video_count()`.
    pub fn video_count(&self) -> usize {
        self.nodes.iter().filter(|n| n.kind == Kind::Video).count()
    }

    /// Plugs in a platform decoder; replaces synthesized video frames.
    pub fn set_video_source(&mut self, source: Box<dyn VideoSource>) {
        self.video = Some(source);
        self.playing.clear();
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn present(&self, frame: wgpu::SurfaceTexture) {
        self.queue.present(frame);
    }

    pub fn budgets(&self) -> &Budgets {
        &self.budgets
    }

    /// Topmost node under a world point.
    pub fn hit_test(&self, p: [f32; 2]) -> Option<usize> {
        self.nodes.iter().rposition(|n| {
            p[0] >= n.pos[0] && p[1] >= n.pos[1] && p[0] <= n.pos[0] + n.size[0] && p[1] <= n.pos[1] + n.size[1]
        })
    }

    pub fn set_selected(&mut self, idx: Option<usize>) {
        for (i, n) in self.nodes.iter_mut().enumerate() {
            let sel = Some(i) == idx;
            if n.selected != sel {
                n.selected = sel;
                set_flag(&mut self.instances[i].flags, FLAG_SELECTED, sel);
                self.instances_dirty = true;
            }
        }
    }

    pub fn move_node(&mut self, idx: usize, dx: f32, dy: f32) {
        let n = &mut self.nodes[idx];
        n.pos[0] += dx;
        n.pos[1] += dy;
        self.instances[idx].pos = n.pos;
        self.instances_dirty = true;
    }

    pub fn bounds(&self) -> [f32; 4] {
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for n in &self.nodes {
            x0 = x0.min(n.pos[0]);
            y0 = y0.min(n.pos[1]);
            x1 = x1.max(n.pos[0] + n.size[0]);
            y1 = y1.max(n.pos[1] + n.size[1]);
        }
        [x0, y0, x1 - x0, y1 - y0]
    }

    /// Streams media within budget, then draws one frame into `target`.
    pub fn render(&mut self, target: &wgpu::TextureView, camera: &Camera, time: f32) -> FrameStats {
        self.frame += 1;
        let mut stats = FrameStats { nodes: self.nodes.len(), ..Default::default() };
        self.stream_low_tier(&mut stats);
        self.stream_detail_tier(camera, time, &mut stats);

        if self.instances_dirty {
            self.queue.write_buffer(&self.instance_buf, 0, bytemuck::cast_slice(&self.instances));
            self.instances_dirty = false;
            stats.instance_upload = true;
        }
        let cam = CameraUniform {
            offset: camera.offset,
            viewport: camera.viewport,
            zoom: camera.zoom,
            time,
            _pad: [0.0; 2],
        };
        self.queue.write_buffer(&self.camera_buf, 0, bytemuck::bytes_of(&cam));

        let mut encoder = self.device.create_command_encoder(&Default::default());
        let blit_start = web_time::Instant::now();
        self.encode_video_blits(&mut encoder);
        stats.video_ms += blit_start.elapsed().as_secs_f32() * 1000.0;
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("canvas"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.07, g: 0.07, b: 0.08, a: 1.0 }),
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
            pass.set_vertex_buffer(0, self.instance_buf.slice(..));
            pass.draw(0..4, 0..self.instances.len() as u32);
        }
        self.queue.submit([encoder.finish()]);
        stats
    }

    /// Progressive cold open: a few rows of 64 thumbnails per frame, uploaded as strips with mips.
    fn stream_low_tier(&mut self, stats: &mut FrameStats) {
        let total_rows = (self.nodes.len() as u32).div_ceil(LOW_PER_ROW);
        let end = (self.low_rows_done + self.budgets.low_rows_per_frame).min(total_rows);
        for row in self.low_rows_done..end {
            let first = (row * LOW_PER_ROW) as usize;
            let last = (first + LOW_PER_ROW as usize).min(self.nodes.len());
            let (w, h) = (PAGE, LOW_TILE);
            let mut strip = vec![0u8; (w * h * 4) as usize];
            for i in first..last {
                let col = (i as u32 % LOW_PER_PAGE) % LOW_PER_ROW;
                let off = (col * LOW_TILE * 4) as usize;
                synth(self.nodes[i].seed, 0.0, LOW_TILE, &mut strip[off..], w as usize);
            }
            let page = row / LOW_PER_ROW;
            let page_row = row % LOW_PER_ROW;
            let (mut mw, mut mh, mut data) = (w, h, strip);
            for mip in 0..LOW_MIPS {
                self.write_region(&self.low_tex, mip, [0, page_row * mh, page], [mw, mh], &data);
                if mip + 1 < LOW_MIPS {
                    data = downsample(&data, mw, mh);
                    mw /= 2;
                    mh /= 2;
                }
            }
            for i in first..last {
                set_flag(&mut self.instances[i].flags, FLAG_LOW_READY, true);
            }
            self.instances_dirty = true;
        }
        self.low_rows_done = end;
        stats.low_ready = (self.low_rows_done * LOW_PER_ROW).min(self.nodes.len() as u32) as usize;
    }

    fn stream_detail_tier(&mut self, camera: &Camera, time: f32, stats: &mut FrameStats) {
        let b = &self.budgets;
        let pad = [camera.viewport[0] * b.overscan / camera.zoom, camera.viewport[1] * b.overscan / camera.zoom];
        let view = [
            camera.offset[0] - pad[0],
            camera.offset[1] - pad[1],
            camera.offset[0] + camera.viewport[0] / camera.zoom + pad[0],
            camera.offset[1] + camera.viewport[1] / camera.zoom + pad[1],
        ];
        let mut wanted: Vec<(usize, f32)> = Vec::new();
        let mut videos: Vec<(usize, f32)> = Vec::new();
        for (i, n) in self.nodes.iter().enumerate() {
            if n.pos[0] > view[2] || n.pos[1] > view[3] || n.pos[0] + n.size[0] < view[0] || n.pos[1] + n.size[1] < view[1] {
                continue;
            }
            stats.visible += 1;
            let px = n.size[0].max(n.size[1]) * camera.zoom;
            if px < b.detail_min_px {
                continue;
            }
            match n.kind {
                Kind::Image => wanted.push((i, px)),
                Kind::Video => videos.push((i, px)),
            }
        }
        // Biggest on screen first.
        videos.sort_by(|a, b| b.1.total_cmp(&a.1));
        videos.truncate(b.max_active_videos);
        wanted.sort_by(|a, b| b.1.total_cmp(&a.1));

        let frame = self.frame;
        for &(i, _) in videos.iter().chain(wanted.iter()) {
            if let Some(&s) = self.detail.node_slot.get(&i) {
                self.detail.last_used[s as usize] = frame;
            }
        }

        let video_start = web_time::Instant::now();
        if self.video.is_some() {
            self.update_real_videos(&videos, stats);
        } else {
            // Simulated decode: synthesize and upload a new frame every frame.
            for &(i, _) in &videos {
                let slot = match self.detail.node_slot.get(&i) {
                    Some(&s) => s,
                    None => match self.assign_slot(i) {
                        Some(s) => s,
                        None => continue,
                    },
                };
                self.upload_detail(slot, self.nodes[i].seed, time);
                stats.detail_uploads += 1;
            }
        }
        stats.video_ms += video_start.elapsed().as_secs_f32() * 1000.0;
        stats.active_videos = videos.len();

        let mut uploads = 0;
        for &(i, _) in &wanted {
            if uploads >= self.budgets.detail_uploads_per_frame {
                break;
            }
            if self.detail.node_slot.contains_key(&i) {
                continue;
            }
            let Some(slot) = self.assign_slot(i) else { break };
            self.upload_detail(slot, self.nodes[i].seed, 0.0);
            uploads += 1;
        }
        stats.detail_uploads += uploads;
        stats.detail_resident = self.detail.node_slot.len();
    }

    /// Real decode path: play exactly the chosen videos, pause the rest (they keep their
    /// last frame), and queue a GPU blit for every new decoded frame.
    fn update_real_videos(&mut self, videos: &[(usize, f32)], stats: &mut FrameStats) {
        let Some(src) = self.video.as_mut() else { return };
        let now: HashSet<usize> = videos.iter().map(|v| v.0).collect();
        for &i in self.playing.difference(&now) {
            src.set_active(self.video_id[i], false);
        }
        for &i in now.difference(&self.playing) {
            src.set_active(self.video_id[i], true);
        }
        self.playing = now;

        for &(i, _) in videos {
            let (slot, fresh) = match self.detail.node_slot.get(&i) {
                Some(&s) => (s, false),
                None => match self.assign_slot(i) {
                    Some(s) => (s, true),
                    None => continue,
                },
            };
            let force = fresh || self.video_pending.contains(&i);
            let Some(src) = self.video.as_mut() else { return };
            let idx = slot % DETAIL_PER_LAYER;
            let target = SlotTarget {
                texture: &self.detail_tex,
                layer: slot / DETAIL_PER_LAYER,
                origin: [(idx % DETAIL_PER_ROW) * DETAIL_TILE, (idx / DETAIL_PER_ROW) * DETAIL_TILE],
                size: DETAIL_TILE,
                mips: DETAIL_MIPS,
            };
            let written = match src.update(self.video_id[i], force, &target, &self.device, &self.queue) {
                FrameUpdate::Texture(view) => {
                    let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("video frame"),
                        layout: &self.blit_layout,
                        entries: &[
                            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(view) },
                            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.blit_sampler) },
                        ],
                    });
                    self.blits.push((slot, bind_group));
                    true
                }
                FrameUpdate::Written => true,
                FrameUpdate::None => false,
            };
            if written {
                if self.video_pending.remove(&i) {
                    self.instances[i].detail_slot = slot as i32;
                    self.instances_dirty = true;
                }
                stats.video_frames += 1;
                stats.detail_uploads += 1;
            } else if force {
                // No frame yet: keep the slot but show the low tier until one arrives.
                self.video_pending.insert(i);
                self.instances[i].detail_slot = -1;
                self.instances_dirty = true;
            }
        }
    }

    /// One render pass per (layer, mip) touched; each blit draws into its slot's viewport.
    fn encode_video_blits(&mut self, encoder: &mut wgpu::CommandEncoder) {
        if self.blits.is_empty() {
            return;
        }
        let mut blits = std::mem::take(&mut self.blits);
        blits.sort_by_key(|b| b.0);
        for layer_blits in blits.chunk_by(|a, b| a.0 / DETAIL_PER_LAYER == b.0 / DETAIL_PER_LAYER) {
            let layer = layer_blits[0].0 / DETAIL_PER_LAYER;
            for mip in 0..DETAIL_MIPS {
                let size = (DETAIL_TILE >> mip) as f32;
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("video blit"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &self.detail_targets[(layer * DETAIL_MIPS + mip) as usize],
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(&self.blit_pipeline);
                for (slot, bind_group) in layer_blits {
                    let idx = slot % DETAIL_PER_LAYER;
                    let (cx, cy) = ((idx % DETAIL_PER_ROW) as f32, (idx / DETAIL_PER_ROW) as f32);
                    pass.set_viewport(cx * size, cy * size, size, size, 0.0, 1.0);
                    pass.set_bind_group(0, bind_group, &[]);
                    pass.draw(0..4, 0..1);
                }
            }
        }
        blits.clear();
        self.blits = blits;
    }

    fn assign_slot(&mut self, node: usize) -> Option<u32> {
        let (slot, evicted) = self.detail.alloc(node, self.frame)?;
        if let Some(e) = evicted {
            self.instances[e].detail_slot = -1;
        }
        self.instances[node].detail_slot = slot as i32;
        self.instances_dirty = true;
        Some(slot)
    }

    fn upload_detail(&mut self, slot: u32, seed: u32, t: f32) {
        let layer = slot / DETAIL_PER_LAYER;
        let idx = slot % DETAIL_PER_LAYER;
        let (cx, cy) = (idx % DETAIL_PER_ROW, idx / DETAIL_PER_ROW);
        let n = (DETAIL_TILE * DETAIL_TILE * 4) as usize;
        let mut data = std::mem::take(&mut self.scratch);
        data.resize(n, 0);
        synth(seed, t, DETAIL_TILE, &mut data, DETAIL_TILE as usize);
        let mut size = DETAIL_TILE;
        for mip in 0..DETAIL_MIPS {
            self.write_region(&self.detail_tex, mip, [cx * size, cy * size, layer], [size, size], &data[..(size * size * 4) as usize]);
            if mip + 1 < DETAIL_MIPS {
                let next = downsample(&data[..(size * size * 4) as usize], size, size);
                data[..next.len()].copy_from_slice(&next);
                size /= 2;
            }
        }
        self.scratch = data;
    }

    fn write_region(&self, tex: &wgpu::Texture, mip: u32, origin: [u32; 3], size: [u32; 2], data: &[u8]) {
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: tex,
                mip_level: mip,
                origin: wgpu::Origin3d { x: origin[0], y: origin[1], z: origin[2] },
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(size[0] * 4), rows_per_image: Some(size[1]) },
            wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
        );
    }
}

fn tex_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2Array,
            multisampled: false,
        },
        count: None,
    }
}

fn blit_pipeline(device: &wgpu::Device) -> (wgpu::RenderPipeline, wgpu::BindGroupLayout) {
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("video blit"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("video blit"),
        source: wgpu::ShaderSource::Wgsl(include_str!("blit.wgsl").into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None,
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("video blit"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba8Unorm,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleStrip, ..Default::default() },
        depth_stencil: None,
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    });
    (pipeline, layout)
}

fn set_flag(flags: &mut u32, flag: u32, on: bool) {
    if on { *flags |= flag } else { *flags &= !flag }
}

fn instance_for(i: usize, n: &Node) -> Instance {
    let page = i as u32 / LOW_PER_PAGE;
    let idx = i as u32 % LOW_PER_PAGE;
    let (col, row) = ((idx % LOW_PER_ROW) as f32, (idx / LOW_PER_ROW) as f32);
    let t = LOW_TILE as f32;
    let p = PAGE as f32;
    let mut flags = 0;
    set_flag(&mut flags, FLAG_VIDEO, n.kind == Kind::Video);
    set_flag(&mut flags, FLAG_SELECTED, n.selected);
    Instance {
        pos: n.pos,
        size: n.size,
        low_uv: [(col * t + 0.5) / p, (row * t + 0.5) / p, ((col + 1.0) * t - 0.5) / p, ((row + 1.0) * t - 0.5) / p],
        low_layer: page,
        detail_slot: -1,
        color: base_color(n.seed),
        flags,
    }
}

/// A moodboard-like scene: `count` nodes with varied aspect ratios packed in shelves,
/// `videos` of them videos.
pub fn demo_scene(count: usize, videos: usize) -> Vec<Node> {
    let mut rng = 0x9e37_79b9u32;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 17;
        rng ^= rng << 5;
        rng
    };
    let row_width = (count as f32).sqrt() * 300.0;
    let (mut x, mut y, mut row_h) = (0.0f32, 0.0f32, 0.0f32);
    let video_every = if videos == 0 { usize::MAX } else { (count / videos).max(1) };
    (0..count)
        .map(|i| {
            let aspect = [1.0, 1.5, 0.75, 1.777, 0.5625, 1.0][(next() % 6) as usize];
            let h = 160.0 + (next() % 160) as f32;
            let w = h * aspect;
            if x + w > row_width {
                x = 0.0;
                y += row_h + 24.0;
                row_h = 0.0;
            }
            let node = Node {
                pos: [x, y],
                size: [w, h],
                kind: if i % video_every == video_every / 2 && i / video_every < videos { Kind::Video } else { Kind::Image },
                seed: next(),
                selected: false,
            };
            x += w + 24.0;
            row_h = row_h.max(h);
            node
        })
        .collect()
}
