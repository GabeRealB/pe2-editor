//! The editor's look: a dark or a light palette with a chosen accent - the dark one in the manner of Godot's editor -,
//! more room between widgets, and rounded frames. The panes ask for colors by role (`weak()`, `link()`, ...), so a
//! change of theme applies everywhere at once.

use eframe::egui::{self, Color32, CornerRadius, FontFamily, FontId, Margin, Stroke, TextStyle, Visuals};
use std::sync::RwLock;

use crate::settings::{Appearance, Theme};

#[derive(Clone, Copy)]
struct Palette {
    dark: bool,
    background: Color32,
    panel: Color32,
    raised: Color32,
    border: Color32,
    text: Color32,
    weak: Color32,
    accent: Color32,
    link: Color32,
    /// A layer shown alone, as a mixer lights a soloed channel.
    solo: Color32,
}

const DARK: Palette = Palette {
    dark: true,
    background: Color32::from_rgb(0x1d, 0x21, 0x28),
    panel: Color32::from_rgb(0x24, 0x29, 0x31),
    raised: Color32::from_rgb(0x2c, 0x32, 0x3c),
    border: Color32::from_rgb(0x15, 0x18, 0x1d),
    text: Color32::from_rgb(0xd6, 0xda, 0xe0),
    weak: Color32::from_rgb(0x8a, 0x93, 0xa0),
    accent: Color32::from_rgb(0x4d, 0x96, 0xd9),
    link: Color32::from_rgb(0x7f, 0xb8, 0xf0),
    solo: Color32::from_rgb(0xe0, 0xa8, 0x3a),
};

const LIGHT: Palette = Palette {
    dark: false,
    background: Color32::from_rgb(0xe4, 0xe7, 0xec),
    panel: Color32::from_rgb(0xf3, 0xf4, 0xf6),
    raised: Color32::from_rgb(0xff, 0xff, 0xff),
    border: Color32::from_rgb(0xc4, 0xc9, 0xd1),
    text: Color32::from_rgb(0x1f, 0x24, 0x2b),
    weak: Color32::from_rgb(0x66, 0x6f, 0x7b),
    accent: Color32::from_rgb(0x2f, 0x74, 0xc0),
    link: Color32::from_rgb(0x1d, 0x5f, 0xa8),
    solo: Color32::from_rgb(0xb8, 0x7a, 0x12),
};

static PALETTE: RwLock<Palette> = RwLock::new(DARK);

fn palette() -> Palette {
    PALETTE.read().map_or(DARK, |p| *p)
}

pub fn background() -> Color32 {
    palette().background
}
pub fn border() -> Color32 {
    palette().border
}
/// A surface raised over the panels: a card, a control.
pub fn raised() -> Color32 {
    palette().raised
}
pub fn text() -> Color32 {
    palette().text
}
pub fn weak() -> Color32 {
    palette().weak
}
pub fn accent() -> Color32 {
    palette().accent
}
pub fn link() -> Color32 {
    palette().link
}
pub fn solo() -> Color32 {
    palette().solo
}

/// Mix two colors: `t` of `b` into `a`.
fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a blend of two bytes stays a byte")]
    let channel = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color32::from_rgb(channel(a.r(), b.r()), channel(a.g(), b.g()), channel(a.b(), b.b()))
}

pub fn apply(ctx: &egui::Context, appearance: &Appearance) {
    let mut p = match appearance.theme {
        Theme::Dark => DARK,
        Theme::Light => LIGHT,
    };
    let [r, g, b] = appearance.accent;
    p.accent = Color32::from_rgb(r, g, b);
    p.link = if p.dark { mix(p.accent, Color32::WHITE, 0.35) } else { mix(p.accent, Color32::BLACK, 0.2) };
    if let Ok(mut current) = PALETTE.write() {
        *current = p;
    }

    let mut visuals = if p.dark { Visuals::dark() } else { Visuals::light() };
    visuals.panel_fill = p.panel;
    visuals.window_fill = p.panel;
    visuals.extreme_bg_color = p.background;
    visuals.faint_bg_color = p.raised;
    visuals.code_bg_color = p.background;
    visuals.hyperlink_color = p.link;
    visuals.override_text_color = Some(p.text);
    visuals.selection.bg_fill = mix(p.panel, p.accent, 0.55);
    visuals.selection.stroke = Stroke::new(1.0, p.accent);
    visuals.window_stroke = Stroke::new(1.0, p.border);
    visuals.window_corner_radius = CornerRadius::same(6);
    for widget in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widget.corner_radius = CornerRadius::same(4);
    }
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    visuals.widgets.inactive.weak_bg_fill = p.raised;
    visuals.widgets.inactive.bg_fill = p.raised;
    visuals.widgets.hovered.weak_bg_fill = mix(p.raised, p.accent, 0.15);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, mix(p.raised, p.accent, 0.7));
    ctx.set_visuals(visuals);

    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 5.0);
        style.spacing.button_padding = egui::vec2(8.0, 3.0);
        style.spacing.window_margin = Margin::same(10);
        style.spacing.menu_margin = Margin::same(6);
        style.spacing.indent = 16.0;
        style.text_styles = [
            (TextStyle::Heading, FontId::new(17.0, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(13.5, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(13.5, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(12.5, FontFamily::Monospace)),
            (TextStyle::Small, FontId::new(11.0, FontFamily::Proportional)),
        ]
        .into();
    });
}

/// Text on one line, cut short where it does not fit, the whole of it shown on hover: a pane never runs past its edge.
pub fn short(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>) -> egui::Response {
    ui.add(egui::Label::new(text).truncate())
}

/// A link on one line, cut short where it does not fit, the whole of it shown on hover.
pub fn short_link(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let label = egui::Label::new(egui::RichText::new(text).color(link())).truncate().sense(egui::Sense::click());
    let response = ui.add(label).on_hover_cursor(egui::CursorIcon::PointingHand);
    if response.hovered() {
        ui.painter().hline(response.rect.x_range(), response.rect.bottom(), Stroke::new(1.0, link()));
    }
    response
}

/// A row of a link on the left - cut short where the row is too narrow - and a detail on the right.
pub fn link_row(ui: &mut egui::Ui, text: &str, detail: impl Into<egui::WidgetText>) -> egui::Response {
    egui::containers::Sides::new().shrink_left().truncate().show(ui, |ui| short_link(ui, text), |ui| ui.label(detail)).0
}

/// A titled section of a pane: a small heading, then its contents on the background tone.
pub fn section(ui: &mut egui::Ui, title: &str, add_contents: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(6.0);
    ui.label(egui::RichText::new(title.to_uppercase()).small().strong().color(weak()));
    ui.add_space(2.0);
    egui::Frame::new().fill(background()).corner_radius(CornerRadius::same(4)).inner_margin(Margin::same(8)).show(
        ui,
        |ui| {
            ui.set_width(ui.available_width());
            add_contents(ui);
        },
    );
}

/// The steps of a path that fit a width with what separates them, from the end: the first step to show.
pub fn fitting_steps(ui: &egui::Ui, steps: &[&str], width: f32) -> usize {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let wide = |text: &str| ui.painter().layout_no_wrap(text.to_string(), font.clone(), egui::Color32::WHITE).size().x;
    let (gap, mut used) = (wide(" / ") + ui.spacing().item_spacing.x * 2.0, 0.0);
    let mut first = steps.len();
    while first > 0 {
        used += wide(steps[first - 1]) + gap;
        if used > width {
            break;
        }
        first -= 1;
    }
    first.min(steps.len().saturating_sub(1))
}
