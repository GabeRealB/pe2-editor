//! `pe2-editor`: the project editor. For now it browses: the project's assets as a tree, an asset's fields and slots,
//! references followed both ways, pictures previewed - in panes that can be moved, split, resized and closed.
//!
//!     pe2-editor [PROJECT] [--select ASSET] [--show PANE] [--screenshot FILE] [--agents-drive]
//!
//! `--select` opens an asset (`stages/akropolis/rooms/square/folder`), `--show` a pane (`settings`); `--screenshot`
//! saves the window as a PNG once it has settled and exits - what scripts and checks use to see it. `--agents-drive`
//! lets agents click, type and press keys in this session, as the Agents setting does for every one.
//!
//! Under `WSLg` the editor talks X11: `WSLg`'s Wayland compositor disconnects it when it resizes on a scaled display (the
//! event loop ends in "Broken pipe"). `PE2_EDITOR_BACKEND=wayland` or `x11` chooses either anywhere.
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
mod gpu_view;
mod graph_pane;
mod icons;
mod inspector;
mod layers;
mod panes;
mod player;
mod settings;
mod theme;
mod pictures;
mod runner;
mod takein;
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
fn wgpu_options() -> WgpuConfiguration {
    let mut config = WgpuConfiguration::default();
    if let WgpuSetup::CreateNew(setup) = &mut config.wgpu_setup {
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

/// The event loop: made here rather than by eframe, as it is what knows the monitors, and a window's place has to be judged by
/// them before there is a window. On Linux it talks X11 where Wayland is unsafe (`WSLg`) or asked against; elsewhere winit chooses.
fn event_loop() -> eframe::Result<winit::event_loop::EventLoop<eframe::UserEvent>> {
    let mut builder = winit::event_loop::EventLoop::<eframe::UserEvent>::with_user_event();
    #[cfg(target_os = "linux")]
    {
        let wslg = std::path::Path::new("/mnt/wslg").is_dir() || std::env::var_os("WSL_DISTRO_NAME").is_some();
        let x11 = match std::env::var("PE2_EDITOR_BACKEND").as_deref() {
            Ok("x11") => true,
            Ok("wayland") => false,
            _ => wslg,
        };
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

fn main() -> eframe::Result {
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
    let mut event_loop = event_loop()?;
    let monitors = monitors(&mut event_loop);
    if let Some(window) = settings::EditorSettings::load().0.main_window.filter(|window| window.place.is_reachable(&monitors)) {
        // Filling the screen comes after: made so, it would be on whichever monitor the platform likes.
        viewport = viewport.with_inner_size(window.place.size).with_position(window.place.position);
    }
    let options = eframe::NativeOptions {
        viewport,
        persist_window: false,
        wgpu_options: wgpu_options(),
        ..Default::default()
    };
    // eframe only logs what stops it making the window: here that would be a silent exit.
    let made = std::sync::atomic::AtomicBool::new(false);
    let flag = &made;
    let mut app = eframe::create_native(
        settings::APP_ID,
        options,
        Box::new(move |cc| {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
            if let Some(state) = &cc.wgpu_render_state {
                // wgpu panics on an error nobody handles; a surface that cannot be configured (a window larger than
                // the device allows) then ends the editor. Log instead: the frame is lost, not the program.
                state.device.on_uncaptured_error(Arc::new(|e| eprintln!("wgpu: {e}")));
                // The player's YCbCr frames are drawn by a pipeline of its own.
                yuv::install(state);
                gpu_view::install(state);
            }
            let max_surface = cc.wgpu_render_state.as_ref().map(|s| s.device.limits().max_texture_dimension_2d);
            let start = app::Start { project, select, show, screenshot, max_surface, agents_drive, monitors };
            Ok(Box::new(app::Editor::new(cc, start)) as Box<dyn eframe::App>)
        }),
        &event_loop,
    );
    event_loop.run_app_on_demand(&mut app)?;
    drop(app);
    if !made.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(eframe::Error::AppCreation("the editor's window could not be made".into()));
    }
    Ok(())
}
