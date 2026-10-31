//! What extensions rendered, in the window: a published texture, registered with egui once, so it is drawn as it is, with no copy.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock, PoisonError};

use eframe::egui;
use eframe::egui_wgpu::{self, wgpu};

static STATE: OnceLock<egui_wgpu::RenderState> = OnceLock::new();
static SHOWN: Mutex<Option<HashMap<u64, (wgpu::Texture, egui::TextureId)>>> = Mutex::new(None);

/// Share the window's device with extensions.
pub fn install(state: &egui_wgpu::RenderState) {
    let _ = STATE.set(state.clone());
    pe2_webgpu::install(&state.instance, state.adapter.clone(), state.device.clone(), state.queue.clone());
}

/// The id egui knows a published texture by; none where it is not published.
pub fn texture_id(id: u64) -> Option<egui::TextureId> {
    let (state, texture) = (STATE.get()?, pe2_webgpu::published(id)?);
    let mut shown = SHOWN.lock().unwrap_or_else(PoisonError::into_inner);
    let shown = shown.get_or_insert_with(HashMap::new);
    if let Some((known, native)) = shown.get(&id)
        && *known == texture
    {
        return Some(*native);
    }
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let native = state.renderer.write().register_native_texture(&state.device, &view, wgpu::FilterMode::Linear);
    shown.insert(id, (texture, native));
    Some(native)
}
