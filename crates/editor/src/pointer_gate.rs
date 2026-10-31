//! Keeping from the window the moves of the pointer that cannot change it. Each one egui is told of is a pass and a frame
//! shown, and showing one can cost the whole window, copied, however little of it changed: a slow move along a row would
//! pay that many times a second for a highlight already drawn. Between widgets, and over anything tall enough to care
//! where in it the pointer is, every move still gets through.

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use eframe::egui::{self, ViewportId};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalPosition;
use winit::event::{DeviceEvent, DeviceId, ElementState, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::window::WindowId;

/// How long the pointer rests before egui hears where it is: what starts a tooltip's wait.
const REST: Duration = Duration::from_millis(120);
/// The tallest widget kept from: a row or a tile has one look for a pointer anywhere on it, a view or a canvas need not.
const TALLEST: f32 = 128.0;

struct Held {
    window: WindowId,
    device_id: DeviceId,
    position: PhysicalPosition<f64>,
    at: Instant,
}

pub struct PointerGate<'a> {
    app: eframe::EframeWinitApplication<'a>,
    context: Arc<OnceLock<egui::Context>>,
    main: Option<WindowId>,
    held: Option<Held>,
    buttons: usize,
}

impl<'a> PointerGate<'a> {
    pub fn new(app: eframe::EframeWinitApplication<'a>, context: Arc<OnceLock<egui::Context>>) -> Self {
        PointerGate { app, context, main: None, held: None, buttons: 0 }
    }

    /// Whether what is under the pointer at `position` is what egui last found under it.
    fn same_widget(&self, window: WindowId, position: PhysicalPosition<f64>) -> bool {
        let Some(context) = self.context.get().filter(|_| self.main == Some(window) && self.buttons == 0) else {
            return false;
        };
        #[expect(clippy::cast_possible_truncation, reason = "a pixel of a window")]
        let point = egui::pos2(position.x as f32, position.y as f32) / context.pixels_per_point();
        let Some(layer) = context.layer_id_at(point) else { return false };
        let reach = context.global_style().interaction.interact_radius;
        context.viewport_for(ViewportId::ROOT, |viewport| {
            // What would answer it: the containers around that come after it, in what contains the pointer.
            let here = viewport.hits.click.as_ref().or(viewport.hits.drag.as_ref());
            // As egui finds it: the topmost one the pointer is on, or else the nearest within reach.
            let (mut there, mut nearest) = (None, reach);
            for widget in viewport.this_pass.widgets.get_layer(layer).filter(|w| w.sense.interactive()) {
                let distance = widget.interact_rect.distance_to_pos(point);
                if distance == 0.0 || distance < nearest {
                    there = Some(widget);
                    nearest = distance.min(nearest);
                }
            }
            match (here, there) {
                (None, None) => true,
                (Some(here), Some(there)) => there.id == here.id && there.rect.height() <= TALLEST,
                _ => false,
            }
        })
    }

    fn release(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(held) = self.held.take() {
            let moved = WindowEvent::CursorMoved { device_id: held.device_id, position: held.position };
            self.app.window_event(event_loop, held.window, moved);
        }
    }
}

impl ApplicationHandler<eframe::UserEvent> for PointerGate<'_> {
    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        self.app.new_events(event_loop, cause);
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.app.resumed(event_loop);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: eframe::UserEvent) {
        self.app.user_event(event_loop, event);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, window: WindowId, event: WindowEvent) {
        // The first window there is is the main one.
        self.main.get_or_insert(window);
        match event {
            WindowEvent::CursorMoved { device_id, position } if self.same_widget(window, position) => {
                self.held = Some(Held { window, device_id, position, at: Instant::now() });
                return;
            }
            WindowEvent::CursorMoved { .. } => self.held = None,
            WindowEvent::MouseInput { state, .. } => {
                self.release(event_loop);
                self.buttons = if state == ElementState::Pressed { self.buttons + 1 } else { self.buttons.saturating_sub(1) };
            }
            // A frame would only ask for another: the widget under the pointer is the same, wherever on it egui thinks it is.
            WindowEvent::RedrawRequested => {}
            _ => self.release(event_loop),
        }
        self.app.window_event(event_loop, window, event);
    }

    fn device_event(&mut self, event_loop: &ActiveEventLoop, device: DeviceId, event: DeviceEvent) {
        // The same move again, raw, and asking for a frame of its own: only what is dragged has a use for it.
        if self.buttons == 0 && matches!(event, DeviceEvent::MouseMotion { .. }) {
            return;
        }
        self.app.device_event(event_loop, device, event);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.held.as_ref().is_some_and(|held| held.at.elapsed() >= REST) {
            self.release(event_loop);
        }
        self.app.about_to_wait(event_loop);
        if let Some(held) = &self.held {
            let wake = held.at + REST;
            match event_loop.control_flow() {
                ControlFlow::Poll => {}
                ControlFlow::WaitUntil(sooner) if sooner <= wake => {}
                _ => event_loop.set_control_flow(ControlFlow::WaitUntil(wake)),
            }
        }
    }

    fn suspended(&mut self, event_loop: &ActiveEventLoop) {
        self.app.suspended(event_loop);
    }

    fn exiting(&mut self, event_loop: &ActiveEventLoop) {
        self.app.exiting(event_loop);
    }

    fn memory_warning(&mut self, event_loop: &ActiveEventLoop) {
        self.app.memory_warning(event_loop);
    }
}
