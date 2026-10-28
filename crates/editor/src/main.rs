//! `pe2-editor`: the project editor. For now it browses: the project's assets as a tree, an asset's fields and slots,
//! references followed both ways, pictures previewed - in panes that can be moved, split, resized and closed.
//!
//!     pe2-editor [PROJECT] [--select ASSET] [--show PANE] [--screenshot FILE]
//!
//! `--select` opens an asset (`stages/akropolis/rooms/square/folder`), `--show` a pane (`settings`); `--screenshot`
//! saves the window as a PNG once it has settled and exits - what scripts and checks use to see it.
//!
//! Under `WSLg` the editor talks X11: `WSLg`'s Wayland compositor disconnects it when it resizes on a scaled display (the
//! event loop ends in "Broken pipe"). `PE2_EDITOR_BACKEND=wayland` or `x11` chooses either anywhere.
//!
//! Panes moved into windows of their own open as native windows; `PE2_EDITOR_EMBED_WINDOWS=1` draws those windows
//! inside the main one instead (and so in its screenshots).

mod app;
mod external;
mod icons;
mod inspector;
mod layers;
mod panes;
mod settings;
mod theme;
mod thumbs;
mod upscale;
mod viewer;
mod watch;
mod windows;
mod workspace;

use eframe::egui;
use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetup};
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
            descriptor.required_limits.max_texture_dimension_2d = adapter.limits().max_texture_dimension_2d;
            descriptor
        });
    }
    config
}

/// On Linux, the event loop talks X11 where Wayland is unsafe (`WSLg`) or asked against; elsewhere winit chooses.
#[cfg(target_os = "linux")]
fn event_loop_builder() -> Option<eframe::EventLoopBuilderHook> {
    let x11 = match std::env::var("PE2_EDITOR_BACKEND").as_deref() {
        Ok("x11") => true,
        Ok("wayland") => false,
        _ => std::path::Path::new("/mnt/wslg").is_dir() || std::env::var_os("WSL_DISTRO_NAME").is_some(),
    };
    x11.then(|| {
        Box::new(|builder: &mut eframe::EventLoopBuilder<eframe::UserEvent>| {
            use winit::platform::x11::EventLoopBuilderExtX11;
            builder.with_x11();
        }) as eframe::EventLoopBuilderHook
    })
}

#[cfg(not(target_os = "linux"))]
fn event_loop_builder() -> Option<eframe::EventLoopBuilderHook> {
    None
}

fn main() -> eframe::Result {
    let mut args = std::env::args_os().skip(1);
    let (mut project, mut select, mut show, mut screenshot) = (None, None, None, None);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--select") => select = args.next().map(|a| a.to_string_lossy().into_owned()),
            Some("--show") => show = args.next().map(|a| a.to_string_lossy().into_owned()),
            Some("--screenshot") => screenshot = args.next().map(PathBuf::from),
            _ => project = Some(PathBuf::from(arg)),
        }
    }
    let project = project.or_else(|| {
        let cwd = std::env::current_dir().ok()?;
        cwd.join(project::FILE).is_file().then_some(cwd)
    });
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Parasite Eve II editor")
            .with_inner_size([1400.0, 900.0]),
        wgpu_options: wgpu_options(),
        event_loop_builder: event_loop_builder(),
        ..Default::default()
    };
    eframe::run_native(
        settings::APP_ID,
        options,
        Box::new(move |cc| {
            if let Some(state) = &cc.wgpu_render_state {
                // wgpu panics on an error nobody handles; a surface that cannot be configured (a window larger than
                // the device allows) then ends the editor. Log instead: the frame is lost, not the program.
                state.device.on_uncaptured_error(Arc::new(|e| eprintln!("wgpu: {e}")));
            }
            let max_surface = cc.wgpu_render_state.as_ref().map(|s| s.device.limits().max_texture_dimension_2d);
            let start = app::Start { project, select, show, screenshot, max_surface };
            Ok(Box::new(app::Editor::new(cc, start)) as Box<dyn eframe::App>)
        }),
    )
}
