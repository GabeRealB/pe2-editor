//! `pe2-editor`: the project editor. For now it browses: the project's assets as a tree, an asset's fields and slots,
//! references followed both ways, pictures previewed - in panes that can be moved, split, resized and closed.
//!
//! ```text
//! pe2-editor [PROJECT] [--select ASSET] [--show PANE] [--screenshot FILE] [--agents-drive] [--trust-everything]
//! ```
//!
//! `--select` opens an asset (`stages/akropolis/rooms/square/folder`), `--show` a pane (`settings`); `--screenshot`
//! saves the window as a PNG once it has settled and exits - what scripts and checks use to see it. `--agents-drive`
//! lets agents click, type and press keys in this session, as the Agents setting does for every one. `--trust-everything`
//! runs every unsandboxed extension in this session, asking nothing and keeping nothing (`extensions::trust`).
//!
//! Under `WSLg` the editor talks X11 unless told otherwise: maximizing or resizing a window on Wayland can crash `WSLg`'s
//! compositor, not every time, and every Linux window goes with it (the event loop ends in "Broken pipe"). It is the
//! slower way there: at 4K, Wayland on Mesa's Dozen shows a third more frames than X11 on OpenGL. The Startup settings
//! choose; `PE2_EDITOR_BACKEND=wayland` or `x11` overrides them for a run.
//!
//! Panes moved into windows of their own open as native windows; `PE2_EDITOR_EMBED_WINDOWS=1` draws those windows
//! inside the main one instead (and so in its screenshots).

// wgpu's handles are Send through a deep chain of types: proving it for the player's renderer needs a higher limit.
#![recursion_limit = "256"]

mod app;
mod commands;
mod contents;
mod dnd;
mod events_pane;
mod drive;
mod ext_ui;
mod extensions_pane;
mod external;
mod gpu_stats;
mod gpu_view;
mod graph_pane;
mod icons;
mod inspector;
mod layers;
mod onboarding;
mod panes;
mod perf;
mod player;
mod pointer_gate;
mod settings;
mod theme;
mod pictures;
mod runner;
#[cfg(any(test, feature = "tests"))]
#[allow(clippy::missing_panics_doc, reason = "what fails in a test panics")]
pub mod scenario;
mod takein;
mod tests_pane;
mod thumbs;
mod upscale;
mod viewer;
mod watch;
mod windows;
mod workspace;
mod yuv;

use winit::platform::run_on_demand::EventLoopExtRunOnDemand as _;
use eframe::egui;
use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetup, wgpu};
use std::path::PathBuf;
use std::sync::Arc;

/// eframe's wgpu setup, but with the adapter's own texture size limit: eframe asks for 8192, and a window surface
/// beyond the device's limit is a panic in `Surface::configure`.
fn wgpu_options(x11: bool) -> WgpuConfiguration {
    // A frame queued is a frame's more wait between the pointer moving and the window showing it.
    let mut config = WgpuConfiguration { surface: eframe::egui_wgpu::SurfaceConfig::LOW_LATENCY, ..WgpuConfiguration::default() };
    if let WgpuSetup::CreateNew(setup) = &mut config.wgpu_setup {
        // wgpu hides drivers that are not conformant, and Mesa's Dozen, the Vulkan of WSL, is not. It is the fastest way
        // to Wayland there, and the slowest to X11, where OpenGL is left to be chosen.
        if cfg!(target_os = "linux") && !x11 {
            setup.instance_descriptor.flags |= wgpu::InstanceFlags::ALLOW_UNDERLYING_NONCOMPLIANT_ADAPTER;
        }
        let default = setup.device_descriptor.clone();
        setup.device_descriptor = Arc::new(move |adapter| {
            let mut descriptor = default(adapter);
            // Extensions draw on this device: what the hardware has is theirs to use, the standard's optional features among it.
            descriptor.required_limits = adapter.limits();
            let offered = adapter.features();
            descriptor.required_features |= wgpu::Features { features_wgpu: wgpu::FeaturesWGPU::empty(), features_webgpu: offered.features_webgpu };
            descriptor
        });
    }
    config
}

fn is_wslg() -> bool {
    std::path::Path::new("/mnt/wslg").is_dir() || std::env::var_os("WSL_DISTRO_NAME").is_some()
}

/// Whether the window is reached through X11 rather than Wayland.
fn talks_x11(chosen: settings::DisplayServer) -> bool {
    use settings::DisplayServer;
    let wanted = match std::env::var("PE2_EDITOR_BACKEND").as_deref() {
        Ok("x11") => DisplayServer::X11,
        Ok("wayland") => DisplayServer::Wayland,
        _ => chosen,
    };
    cfg!(target_os = "linux") && (wanted == DisplayServer::X11 || (wanted == DisplayServer::Auto && is_wslg()))
}

/// The event loop: made here rather than by eframe, as it is what knows the monitors, and a window's place has to be judged by
/// them before there is a window. On Linux it talks X11 where Wayland is unsafe (`WSLg`) or asked against; elsewhere winit chooses.
fn event_loop(x11: bool) -> eframe::Result<winit::event_loop::EventLoop<eframe::UserEvent>> {
    let mut builder = winit::event_loop::EventLoop::<eframe::UserEvent>::with_user_event();
    #[cfg(not(target_os = "linux"))]
    let _ = x11;
    #[cfg(target_os = "linux")]
    {
        let wslg = is_wslg();
        // WSLg's X server states no cursor size, and Xcursor then takes a 48th of the display's shorter side: 45 on a 4K
        // screen, the theme's 48, twice the usual.
        if wslg && std::env::var_os("XCURSOR_SIZE").is_none() {
            // SAFETY: at the start of `main`, before anything that could read the environment is running.
            unsafe { std::env::set_var("XCURSOR_SIZE", "24") };
        }
        if x11 {
            use winit::platform::x11::EventLoopBuilderExtX11;
            builder.with_x11();
        }
    }
    Ok(builder.build()?)
}

/// Reads the monitors, which only a running event loop knows, and stops it.
#[derive(Default)]
struct Monitors(Vec<egui::Rect>);

impl winit::application::ApplicationHandler<eframe::UserEvent> for Monitors {
    fn resumed(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        #[expect(clippy::cast_precision_loss, reason = "a desktop's pixels are far below f32's exact integers")]
        let rect = |monitor: winit::monitor::MonitorHandle| {
            let (at, size) = (monitor.position(), monitor.size());
            egui::Rect::from_min_size(egui::pos2(at.x as f32, at.y as f32), egui::vec2(size.width as f32, size.height as f32))
        };
        self.0 = event_loop.available_monitors().map(rect).collect();
        event_loop.exit();
    }

    fn window_event(&mut self, _: &winit::event_loop::ActiveEventLoop, _: winit::window::WindowId, _: winit::event::WindowEvent) {}
}

/// The monitors, in pixels of the desktop: none where they cannot be told.
fn monitors(event_loop: &mut winit::event_loop::EventLoop<eframe::UserEvent>) -> Vec<egui::Rect> {
    let mut monitors = Monitors::default();
    match event_loop.run_app_on_demand(&mut monitors) {
        Ok(()) => monitors.0,
        Err(_) => Vec::new(),
    }
}

/// What the editor draws with, on its device.
fn install_gpu(state: &eframe::egui_wgpu::RenderState) {
    // wgpu panics on an error nobody handles; a surface that cannot be configured (a window larger than the device
    // allows) then ends the editor. Log instead: the frame is lost, not the program.
    state.device.on_uncaptured_error(Arc::new(|e| eprintln!("wgpu: {e}")));
    // The player's YCbCr frames are drawn by a pipeline of its own.
    yuv::install(state);
    gpu_view::install(state);
}

/// What running the editor comes to.
pub type Result = eframe::Result;

/// The editor.
///
/// # Errors
/// Its window could not be made.
pub fn run() -> Result {
    project::ext::component::link_gpu(pe2_webgpu::add_to_linker);
    let mut args = std::env::args_os().skip(1);
    let (mut project, mut select, mut show, mut screenshot) = (None, None, None, None);
    let mut agents_drive = false;
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--select") => select = args.next().map(|a| a.to_string_lossy().into_owned()),
            Some("--show") => show = args.next().map(|a| a.to_string_lossy().into_owned()),
            Some("--screenshot") => screenshot = args.next().map(PathBuf::from),
            Some("--agents-drive") => agents_drive = true,
            Some("--trust-everything") => extensions::trust::trust_everything(),
            _ => project = Some(PathBuf::from(arg)),
        }
    }
    let project = project.or_else(|| {
        let cwd = std::env::current_dir().ok()?;
        cwd.join(project::FILE).is_file().then_some(cwd)
    });
    // Kept here rather than by eframe: under WSLg it would read back a place a decoration off, and a window closed filling the
    // screen would come back that size.
    let mut viewport = egui::ViewportBuilder::default().with_title("Parasite Eve II editor").with_inner_size([1400.0, 900.0]);
    let saved = settings::EditorSettings::load().0;
    let x11 = talks_x11(saved.startup.display_server);
    let mut event_loop = event_loop(x11)?;
    let monitors = monitors(&mut event_loop);
    if let Some(window) = saved.main_window.filter(|window| window.place.is_reachable(&monitors)) {
        // Made filling the screen, at its place so that it fills the right monitor: asked to after it is shown, it would be
        // seen to arrive, at the wrong size for that monitor first, then at the right one, then filling it.
        viewport = viewport.with_inner_size(window.place.size).with_position(window.place.position);
        viewport = viewport.with_maximized(window.maximized).with_fullscreen(window.fullscreen);
    }
    let options = eframe::NativeOptions {
        viewport,
        persist_window: false,
        wgpu_options: wgpu_options(x11),
        ..Default::default()
    };
    // eframe only logs what stops it making the window: here that would be a silent exit.
    let made = std::sync::atomic::AtomicBool::new(false);
    let flag = &made;
    let context = Arc::new(std::sync::OnceLock::new());
    let its_context = context.clone();
    let app = eframe::create_native(
        settings::APP_ID,
        options,
        Box::new(move |cc| {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
            let _ = its_context.set(cc.egui_ctx.clone());
            if let Some(state) = &cc.wgpu_render_state {
                let adapter = state.adapter.get_info();
                perf::note_renderer(format!("{} ({:?})", adapter.name, adapter.backend), adapter.vendor);
                install_gpu(state);
            }
            let max_surface = cc.wgpu_render_state.as_ref().map(|s| s.device.limits().max_texture_dimension_2d);
            let start = app::Start { project, select, show, screenshot, max_surface, agents_drive, unattended: false, monitors };
            Ok(Box::new(app::Editor::new(cc, start)) as Box<dyn eframe::App>)
        }),
        &event_loop,
    );
    let mut app = pointer_gate::PointerGate::new(app, context);
    event_loop.run_app_on_demand(&mut app)?;
    drop(app);
    if !made.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(eframe::Error::AppCreation("the editor's window could not be made".into()));
    }
    Ok(())
}
