//! pe2.viewport: seeing in three dimensions, rendered by the extension on the editor's device through `wasi:webgpu`.

use pe2_ext::{Icon, Registry, Result, Ui, View};

pe2_ext::extension!("pe2.viewport", "0.1.0", register);

#[cfg(target_family = "wasm")]
mod render;

#[expect(clippy::unnecessary_wraps, reason = "an entry point may fail; this one does not")]
fn register(r: &mut Registry) -> Result<()> {
    r.panel("viewport", "Viewport", Icon::Image, || Box::new(Viewport::default()));
    Ok(())
}

/// What the panel shows.
#[derive(Default)]
struct Viewport {
    #[cfg(target_family = "wasm")]
    drawn: Option<render::Scene>,
}

impl View for Viewport {
    #[cfg(target_family = "wasm")]
    fn show(&mut self, ui: &mut Ui) {
        if self.drawn.is_none() {
            self.drawn = render::Scene::new(512);
        }
        match &self.drawn {
            Some(scene) => {
                scene.draw();
                ui.gpu_viewport(scene.id(), 512.0, 512.0);
            }
            None => ui.label("Nothing here draws"),
        }
    }

    #[cfg(not(target_family = "wasm"))]
    fn show(&mut self, ui: &mut Ui) {
        ui.label("Only a component reaches the GPU");
    }
}
