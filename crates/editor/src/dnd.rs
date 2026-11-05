//! Dragging, one grammar for all of it: what is dragged is an address - an asset, a layer of a stack - and a place it can
//! go says what it would do before the drop, by the pointer, as a wire of the graph is refused with its reason.

use eframe::egui::{self, DragAndDrop, RichText};

use crate::theme;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Dragged {
    /// An asset, by its path.
    Asset(String),
    /// A layer of an asset's stack: the asset's path and its place.
    Layer(String, usize),
    /// A row of the contents: its asset's path, its id, its label and what its type says it is when dragged.
    Row { asset: String, id: String, label: String, what: String },
}

impl Dragged {
    fn name(&self) -> String {
        match self {
            Dragged::Asset(path) => path.rsplit('/').next().unwrap_or(path).to_string(),
            Dragged::Layer(_, at) => format!("Layer {at}"),
            Dragged::Row { label, .. } => label.clone(),
        }
    }
}

/// What a drop on the place under the pointer would do, or why it would not.
#[derive(Clone)]
struct Says(Result<String, String>);

impl Default for Says {
    fn default() -> Says {
        Says(Err(String::new()))
    }
}

fn said() -> egui::Id {
    egui::Id::unique("what a drop would do")
}

/// A place something can be dropped: lit while it is over it, by what `verdict` says of it. What was dropped, once it is.
pub fn target(
    ui: &egui::Ui,
    rect: egui::Rect,
    verdict: impl FnOnce(&Dragged) -> Result<String, String>,
) -> Option<Dragged> {
    let ctx = ui.ctx();
    let dragged = DragAndDrop::payload::<Dragged>(ctx)?;
    let pointer = ctx.input(|i| i.pointer.latest_pos())?;
    if !ui.clip_rect().intersect(rect).contains(pointer) {
        return None;
    }
    let says = Says(verdict(&dragged));
    let color = if says.0.is_ok() { theme::accent() } else { ui.visuals().error_fg_color };
    let over = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, said()));
    over.rect_stroke(rect, 3.0, egui::Stroke::new(1.5, color), egui::StrokeKind::Inside);
    let accepted = says.0.is_ok();
    ctx.data_mut(|d| d.insert_temp(said(), says));
    (accepted && ctx.input(|i| i.pointer.any_released())).then(|| {
        DragAndDrop::clear_payload(ctx);
        (*dragged).clone()
    })
}

/// What is dragged, by the pointer, and what dropping it would do.
pub fn ghost(ctx: &egui::Context) {
    let says: Option<Says> = ctx.data_mut(|d| d.remove_temp(said()));
    let (Some(dragged), Some(pointer)) = (DragAndDrop::payload::<Dragged>(ctx), ctx.input(|i| i.pointer.latest_pos()))
    else {
        return;
    };
    egui::Area::new(said().with("ghost"))
        .order(egui::Order::Tooltip)
        .fixed_pos(pointer + egui::vec2(14.0, 12.0))
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                ui.label(RichText::new(dragged.name()).strong());
                match says.map(|says| says.0) {
                    Some(Ok(what)) => drop(ui.label(RichText::new(what).small().color(theme::weak()))),
                    Some(Err(why)) => drop(ui.label(RichText::new(why).small().color(ui.visuals().error_fg_color))),
                    None => {}
                }
            });
        });
}
