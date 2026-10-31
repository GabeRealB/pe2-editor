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
    let mut renderer = state.renderer.write();
    let native = renderer.register_native_texture(&state.device, &view, wgpu::FilterMode::Linear);
    // A viewport that resized has let go of what it was showing.
    shown.retain(|known, (_, native)| {
        let kept = pe2_webgpu::published(*known).is_some();
        if !kept {
            renderer.free_texture(native);
        }
        kept
    });
    shown.insert(id, (texture, native));
    Some(native)
}

/// What the pointer and the keys did over a viewport this frame. Keys are only its while it is hovered, so that typing elsewhere reaches what it was typed in.
pub fn input(ui: &egui::Ui, response: &egui::Response, rect: egui::Rect) -> pe2_ext::ViewportInput {
    use egui::PointerButton::{Middle, Primary, Secondary};
    // All of it asked before reading the input: that holds the context, and each of these can take it too.
    let over = response.hovered();
    let hovered = over || response.dragged();
    let pointer = response.hover_pos().or_else(|| response.interact_pointer_pos());
    let keyboard_is_theirs = hovered && !ui.ctx().egui_wants_keyboard_input();
    let clicked = response.clicked();
    ui.input(|input| {
        let button = |test: &dyn Fn(egui::PointerButton) -> bool| (test(Primary), test(Secondary), test(Middle));
        let held = button(&|b| input.pointer.button_down(b));
        let pressed = if over { button(&|b| input.pointer.button_pressed(b)) } else { (false, false, false) };
        let mut keys = Vec::new();
        if keyboard_is_theirs {
            for event in &input.events {
                if let egui::Event::Key { key, pressed: true, repeat: false, .. } = event {
                    keys.push(format!("{key:?}"));
                }
            }
        }
        pe2_ext::ViewportInput {
            hovered,
            pointer: pointer.map(|at| (at.x - rect.min.x, at.y - rect.min.y)),
            drag: if hovered { (input.pointer.delta().x, input.pointer.delta().y) } else { (0.0, 0.0) },
            held,
            pressed,
            released: button(&|b| input.pointer.button_released(b)),
            clicked,
            scroll: if over { input.smooth_scroll_delta.y } else { 0.0 },
            modifiers: pe2_ext::Modifiers { shift: input.modifiers.shift, ctrl: input.modifiers.command, alt: input.modifiers.alt },
            keys,
        }
    })
}
