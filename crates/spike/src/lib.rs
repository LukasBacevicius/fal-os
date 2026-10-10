//! Canvas engine spike: one winit app that runs natively (Metal) and in the browser (WebGPU).
//!
//! Controls: drag empty space to pan, drag a node to move it, trackpad scroll pans,
//! pinch / ctrl+scroll / mouse wheel zooms, `f` fits everything, `b` runs the benchmark.
//! Options (native args / web query string): `nodes=10000 videos=50 bench novsync`.

use falos_engine::{Budgets, Camera, FrameStats, Renderer, demo_scene};
use std::sync::Arc;
use web_time::Instant;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState};
use winit::window::{Window, WindowId};

#[derive(Clone, Debug)]
struct Options {
    nodes: usize,
    videos: usize,
    bench: bool,
    vsync: bool,
    budgets: Budgets,
}

impl Options {
    fn parse(args: &[String]) -> Self {
        let mut o = Options { nodes: 10_000, videos: 50, bench: false, vsync: true, budgets: Budgets::default() };
        for a in args {
            let (k, v) = a.trim_start_matches('-').split_once('=').unwrap_or((a.trim_start_matches('-'), ""));
            match k {
                "nodes" => o.nodes = v.parse().unwrap_or(o.nodes),
                "videos" => o.videos = v.parse().unwrap_or(o.videos),
                "bench" => o.bench = true,
                "novsync" => o.vsync = false,
                "detail" => o.budgets.detail_slots = v.parse().unwrap_or(o.budgets.detail_slots),
                "uploads" => o.budgets.detail_uploads_per_frame = v.parse().unwrap_or(o.budgets.detail_uploads_per_frame),
                "active_videos" => o.budgets.max_active_videos = v.parse().unwrap_or(o.budgets.max_active_videos),
                _ => {}
            }
        }
        o
    }
}

struct Gpu {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    renderer: Renderer,
    backend: String,
}

enum UserEvent {
    Ready(Box<Gpu>),
}

#[derive(Clone, Copy)]
enum Drag {
    Pan,
    Node(usize),
}

struct Bench {
    start: Instant,
    frame_ms: Vec<f32>,
    cpu_ms: Vec<f32>,
    uploads: u64,
}

const BENCH_SECS: f32 = 12.0;

struct App {
    opts: Options,
    proxy: Option<EventLoopProxy<UserEvent>>,
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    camera: Camera,
    fitted: bool,
    cursor: [f32; 2],
    drag: Option<Drag>,
    mods: ModifiersState,
    started: Instant,
    last_frame: Instant,
    window_ms: Vec<f32>,
    last_title: Instant,
    last_stats: FrameStats,
    bench: Option<Bench>,
    bench_center: [f32; 4],
}

impl App {
    fn new(opts: Options, proxy: EventLoopProxy<UserEvent>) -> Self {
        let now = Instant::now();
        Self {
            opts,
            proxy: Some(proxy),
            window: None,
            gpu: None,
            camera: Camera { zoom: 1.0, ..Default::default() },
            fitted: false,
            cursor: [0.0; 2],
            drag: None,
            mods: ModifiersState::empty(),
            started: now,
            last_frame: now,
            window_ms: Vec::new(),
            last_title: now,
            last_stats: FrameStats::default(),
            bench: None,
            bench_center: [0.0; 4],
        }
    }

    fn start_bench(&mut self) {
        if let Some(gpu) = &self.gpu {
            self.fitted = true;
            self.bench_center = gpu.renderer.bounds();
            self.bench = Some(Bench { start: Instant::now(), frame_ms: Vec::new(), cpu_ms: Vec::new(), uploads: 0 });
        }
    }

    /// Scripted camera: overview orbit → dive in → fast pan at 1:1 → zoom back out.
    fn bench_camera(&mut self, t: f32) {
        let [x, y, w, h] = self.bench_center;
        let mut cam = self.camera;
        cam.fit([x, y, w, h]);
        let fit_zoom = cam.zoom;
        let center = |cam: &mut Camera, cx: f32, cy: f32, zoom: f32| {
            cam.zoom = zoom;
            cam.offset = [cx - cam.viewport[0] * 0.5 / zoom, cy - cam.viewport[1] * 0.5 / zoom];
        };
        let (cx, cy) = (x + w * 0.5, y + h * 0.5);
        match t {
            t if t < 3.0 => {
                let a = t * 2.0;
                center(&mut cam, cx + a.cos() * w * 0.05, cy + a.sin() * h * 0.05, fit_zoom * 1.1);
            }
            t if t < 6.0 => {
                let k = (t - 3.0) / 3.0;
                center(&mut cam, cx, cy, fit_zoom * (1.0 / fit_zoom).powf(k));
            }
            t if t < 9.0 => {
                let k = (t - 6.0) / 3.0;
                center(&mut cam, cx + (k - 0.5) * w * 0.3, cy + (k * 12.0).sin() * 400.0, 1.0);
            }
            t => {
                let k = ((t - 9.0) / 3.0).min(1.0);
                center(&mut cam, cx + w * 0.15 * (1.0 - k), cy, (1.0 / fit_zoom).powf(1.0 - k) * fit_zoom);
            }
        }
        self.camera = cam;
    }

    fn finish_bench(&mut self) {
        let Some(b) = self.bench.take() else { return };
        let mut f = b.frame_ms.clone();
        f.sort_by(f32::total_cmp);
        let mut c = b.cpu_ms.clone();
        c.sort_by(f32::total_cmp);
        let pct = |v: &[f32], p: f32| v.get(((v.len() as f32 - 1.0) * p) as usize).copied().unwrap_or(0.0);
        let avg = f.iter().sum::<f32>() / f.len().max(1) as f32;
        let backend = self.gpu.as_ref().map(|g| g.backend.clone()).unwrap_or_default();
        let json = format!(
            "{{\"platform\":\"{}\",\"backend\":\"{}\",\"nodes\":{},\"videos\":{},\"vsync\":{},\"frames\":{},\"fps_avg\":{:.1},\"frame_ms_p50\":{:.2},\"frame_ms_p99\":{:.2},\"frame_ms_max\":{:.2},\"cpu_ms_p50\":{:.2},\"cpu_ms_p99\":{:.2},\"detail_uploads\":{},\"viewport\":[{},{}]}}",
            if cfg!(target_arch = "wasm32") { "web" } else { "native" },
            backend,
            self.opts.nodes,
            self.opts.videos,
            self.opts.vsync,
            f.len(),
            1000.0 / avg,
            pct(&f, 0.5),
            pct(&f, 0.99),
            pct(&f, 1.0),
            pct(&c, 0.5),
            pct(&c, 0.99),
            b.uploads,
            self.camera.viewport[0],
            self.camera.viewport[1],
        );
        report_bench(&json);
        if self.opts.bench && !cfg!(target_arch = "wasm32") {
            std::process::exit(0);
        }
    }

    fn redraw(&mut self) {
        let now = Instant::now();
        let dt = (now - self.last_frame).as_secs_f32() * 1000.0;
        self.last_frame = now;

        if let Some(b) = &self.bench {
            let t = b.start.elapsed().as_secs_f32();
            if t > BENCH_SECS {
                self.finish_bench();
            } else {
                self.bench_camera(t);
            }
        }

        let Some(gpu) = &mut self.gpu else { return };
        let frame = match gpu.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            _ => {
                gpu.surface.configure(gpu.renderer.device(), &gpu.config);
                return;
            }
        };
        let view = frame.texture.create_view(&Default::default());
        let cpu_start = Instant::now();
        let stats = gpu.renderer.render(&view, &self.camera, self.started.elapsed().as_secs_f32());
        let cpu = cpu_start.elapsed().as_secs_f32() * 1000.0;
        if let Some(w) = &self.window {
            w.pre_present_notify();
        }
        gpu.renderer.present(frame);

        if let Some(b) = &mut self.bench {
            if b.start.elapsed().as_secs_f32() > 0.25 {
                b.frame_ms.push(dt);
                b.cpu_ms.push(cpu);
                b.uploads += stats.detail_uploads as u64;
            }
        }
        self.window_ms.push(dt);
        self.last_stats = stats;
        if self.last_title.elapsed().as_secs_f32() > 0.5 {
            let n = self.window_ms.len().max(1) as f32;
            let avg = self.window_ms.iter().sum::<f32>() / n;
            let worst = self.window_ms.iter().cloned().fold(0.0, f32::max);
            let s = stats;
            let title = format!(
                "fal-os spike · {} · {:.0} fps (worst {:.1} ms, cpu {:.2} ms) · {} nodes, {} visible · detail {} · videos {} · thumbs {}/{}{}",
                gpu.backend,
                1000.0 / avg,
                worst,
                cpu,
                s.nodes,
                s.visible,
                s.detail_resident,
                s.active_videos,
                s.low_ready,
                s.nodes,
                if self.bench.is_some() { " · BENCH" } else { "" }
            );
            if let Some(w) = &self.window {
                w.set_title(&title);
            }
            set_overlay(&title);
            self.window_ms.clear();
            self.last_title = Instant::now();
        }
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        #[allow(unused_mut)]
        let mut attrs = Window::default_attributes()
            .with_title("fal-os spike")
            .with_inner_size(winit::dpi::LogicalSize::new(std::env::var("FALOS_W").ok().and_then(|v| v.parse().ok()).unwrap_or(1440.0), std::env::var("FALOS_H").ok().and_then(|v| v.parse().ok()).unwrap_or(900.0)));
        #[cfg(target_arch = "wasm32")]
        {
            use winit::platform::web::WindowAttributesExtWebSys;
            attrs = attrs.with_append(true);
        }
        let window = Arc::new(event_loop.create_window(attrs).expect("window"));
        self.window = Some(window.clone());
        let proxy = self.proxy.take().unwrap();
        let opts = self.opts.clone();
        let fut = async move {
            let gpu = init_gpu(window, &opts).await;
            let _ = proxy.send_event(UserEvent::Ready(Box::new(gpu)));
        };
        #[cfg(target_arch = "wasm32")]
        wasm_bindgen_futures::spawn_local(fut);
        #[cfg(not(target_arch = "wasm32"))]
        pollster::block_on(fut);
    }

    fn user_event(&mut self, _: &ActiveEventLoop, event: UserEvent) {
        let UserEvent::Ready(gpu) = event;
        self.camera.viewport = [gpu.config.width as f32, gpu.config.height as f32];
        self.gpu = Some(*gpu);
        if self.opts.bench {
            self.start_bench();
        }
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(gpu) = &mut self.gpu {
                    let max = gpu.renderer.device().limits().max_texture_dimension_2d;
                    gpu.config.width = size.width.clamp(1, max);
                    gpu.config.height = size.height.clamp(1, max);
                    gpu.surface.configure(gpu.renderer.device(), &gpu.config);
                    self.camera.viewport = [gpu.config.width as f32, gpu.config.height as f32];
                    // Keep framing everything until the user takes the camera.
                    if !self.fitted && size.width > 1 {
                        self.camera.fit(gpu.renderer.bounds());
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                if !self.fitted {
                    if let Some(gpu) = &self.gpu {
                        self.camera.fit(gpu.renderer.bounds());
                    }
                }
                self.redraw();
            }
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            WindowEvent::CursorMoved { position, .. } => {
                let p = [position.x as f32, position.y as f32];
                let (dx, dy) = (p[0] - self.cursor[0], p[1] - self.cursor[1]);
                self.cursor = p;
                match (self.drag, &mut self.gpu) {
                    (Some(Drag::Pan), _) => self.camera.pan(dx, dy),
                    (Some(Drag::Node(i)), Some(gpu)) => gpu.renderer.move_node(i, dx / self.camera.zoom, dy / self.camera.zoom),
                    _ => {}
                }
            }
            WindowEvent::MouseInput { .. } | WindowEvent::MouseWheel { .. } | WindowEvent::PinchGesture { .. } if !self.fitted => {
                self.fitted = true;
                self.window_event(event_loop, id, event);
            }
            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => match state {
                ElementState::Pressed => {
                    let Some(gpu) = &mut self.gpu else { return };
                    let hit = gpu.renderer.hit_test(self.camera.screen_to_world(self.cursor));
                    gpu.renderer.set_selected(hit);
                    self.drag = Some(hit.map_or(Drag::Pan, Drag::Node));
                }
                ElementState::Released => self.drag = None,
            },
            WindowEvent::MouseWheel { delta, .. } => match delta {
                MouseScrollDelta::PixelDelta(d) => {
                    if self.mods.control_key() || self.mods.super_key() {
                        self.camera.zoom_at(self.cursor, (-(d.y as f32) * 0.01).exp());
                    } else {
                        self.camera.pan(d.x as f32, d.y as f32);
                    }
                }
                MouseScrollDelta::LineDelta(_, y) => self.camera.zoom_at(self.cursor, (y * 0.15).exp()),
            },
            WindowEvent::PinchGesture { delta, .. } => self.camera.zoom_at(self.cursor, 1.0 + delta as f32),
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => match event.logical_key {
                Key::Character(c) if c == "f" => {
                    if let Some(gpu) = &self.gpu {
                        self.camera.fit(gpu.renderer.bounds());
                    }
                }
                Key::Character(c) if c == "b" => self.start_bench(),
                _ => {}
            },
            _ => {}
        }
    }
}

async fn init_gpu(window: Arc<Window>, opts: &Options) -> Gpu {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let surface = instance.create_surface(window.clone()).expect("surface");
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            ..Default::default()
        })
        .await
        .expect("adapter");
    let info = adapter.get_info();
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: None,
            required_limits: if std::env::var("FALOS_MAX_LIMITS").is_ok() { adapter.limits() } else { wgpu::Limits::default().using_resolution(adapter.limits()) },
            ..Default::default()
        })
        .await
        .expect("device");
    let size = window.inner_size();
    let caps = surface.get_capabilities(&adapter);
    let format = caps.formats.iter().copied().find(|f| !f.is_srgb()).unwrap_or(caps.formats[0]);
    let mut config = surface.get_default_config(&adapter, size.width.max(1), size.height.max(1)).expect("surface config");
    config.format = format;
    config.present_mode = if opts.vsync { wgpu::PresentMode::AutoVsync } else { wgpu::PresentMode::AutoNoVsync };
    config.desired_maximum_frame_latency = 2;
    surface.configure(&device, &config);
    let nodes = demo_scene(opts.nodes, opts.videos);
    let renderer = Renderer::new(device, queue, format, nodes, opts.budgets.clone());
    Gpu { surface, config, renderer, backend: format!("{:?} · {}", info.backend, info.name) }
}

fn report_bench(json: &str) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        println!("BENCH {json}");
        if let Ok(out) = std::process::Command::new("footprint").args(["-p", &std::process::id().to_string()]).output() {
            let text = String::from_utf8_lossy(&out.stdout);
            if let Some(line) = text.lines().find(|l| l.contains("phys_footprint:")) {
                println!("MEMORY {}", line.trim());
            }
        }
        if let Ok(out) = std::process::Command::new("ps").args(["-o", "rss=", "-p", &std::process::id().to_string()]).output() {
            println!("RSS_KB {}", String::from_utf8_lossy(&out.stdout).trim());
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        web_sys::console::log_1(&format!("BENCH {json}").into());
        if let Some(w) = web_sys::window() {
            let _ = js_sys::Reflect::set(&w, &"__falosBench".into(), &json.into());
            let init = web_sys::RequestInit::new();
            init.set_method("POST");
            init.set_body(&json.into());
            let _ = w.fetch_with_str_and_init("/bench", &init);
        }
    }
}

fn set_overlay(_text: &str) {
    #[cfg(target_arch = "wasm32")]
    if let Some(el) = web_sys::window().and_then(|w| w.document()).and_then(|d| d.get_element_by_id("stats")) {
        el.set_text_content(Some(_text));
    }
}

pub fn run_with(args: Vec<String>) {
    let opts = Options::parse(&args);
    let event_loop = EventLoop::<UserEvent>::with_user_event().build().expect("event loop");
    let app = App::new(opts, event_loop.create_proxy());
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut app = app;
        event_loop.run_app(&mut app).expect("run");
    }
    #[cfg(target_arch = "wasm32")]
    {
        use winit::platform::web::EventLoopExtWebSys;
        event_loop.spawn_app(app);
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn run() {
    env_logger::init();
    run_with(std::env::args().skip(1).collect());
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    let _ = console_log::init_with_level(log::Level::Warn);
    let search = web_sys::window().and_then(|w| w.location().search().ok()).unwrap_or_default();
    let args = search.trim_start_matches('?').split('&').filter(|s| !s.is_empty()).map(String::from).collect();
    run_with(args);
}

#[cfg(target_arch = "wasm32")]
pub fn run() {}
