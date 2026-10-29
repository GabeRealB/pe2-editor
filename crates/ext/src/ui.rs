//! Views: an extension's own UI, in immediate mode as the editor's is. A view is one instance a pane, keeping its
//! state between frames; every frame the host calls `show` with a `Ui` it lends for the call, and each call on it
//! draws a widget and returns what the user did with it this frame.

use crate::{Asset, Rect, Texture};

/// How a label is set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Normal,
    /// Secondary: a note, a count.
    Weak,
    Strong,
    Error,
}

/// Images laid out on a canvas the host zooms and pans, with named boxes over them and a legend: an atlas's cells, an
/// image's pages. Sizes and rectangles are in the canvas's own units (an image's pixels).
pub struct Canvas {
    pub width: f32,
    pub height: f32,
    pub pictures: Vec<(Texture, Rect)>,
    pub boxes: Vec<(String, Rect)>,
    /// What the boxes are, beside their toggle: "12 cells".
    pub summary: String,
    /// The toggle showing the boxes: "Cells".
    pub toggle: String,
}

/// What the host gives a view to draw with: its widgets. The host implements it over its own UI; a component reaches
/// the same through the host's `ui` resource.
pub trait UiHost {
    fn label(&mut self, text: &str, style: Style);
    /// A button; whether it was clicked.
    fn button(&mut self, text: &str) -> bool;
    /// A checkbox over `value`; whether it was toggled.
    fn checkbox(&mut self, text: &str, value: &mut bool) -> bool;
    /// What `body` draws, in a row.
    fn horizontal(&mut self, body: &mut dyn FnMut(&mut dyn UiHost));
    fn canvas(&mut self, canvas: &Canvas);
    /// The asset the view shows, lent for the frame.
    fn asset(&self) -> Asset<'_>;
    /// The look the asset is shown in (an index into its picture's looks), as the host's look picker chose it.
    fn look(&self) -> usize;
}

/// A view's handle on the frame being drawn.
pub struct Ui<'a> {
    host: &'a mut dyn UiHost,
}

impl<'a> Ui<'a> {
    /// The host's handle, lent to a view.
    pub fn new(host: &'a mut dyn UiHost) -> Ui<'a> {
        Ui { host }
    }

    pub fn label(&mut self, text: &str) {
        self.host.label(text, Style::Normal);
    }

    pub fn styled(&mut self, text: &str, style: Style) {
        self.host.label(text, style);
    }

    pub fn error(&mut self, text: &str) {
        self.host.label(text, Style::Error);
    }

    pub fn button(&mut self, text: &str) -> bool {
        self.host.button(text)
    }

    pub fn checkbox(&mut self, text: &str, value: &mut bool) -> bool {
        self.host.checkbox(text, value)
    }

    /// What `body` draws, in a row.
    pub fn horizontal(&mut self, mut body: impl FnMut(&mut Ui)) {
        self.host.horizontal(&mut |host| body(&mut Ui { host }));
    }

    pub fn canvas(&mut self, canvas: &Canvas) {
        self.host.canvas(canvas);
    }

    /// The asset shown.
    #[must_use]
    pub fn asset(&self) -> Asset<'_> {
        self.host.asset()
    }

    /// The look it is shown in.
    #[must_use]
    pub fn look(&self) -> usize {
        self.host.look()
    }
}

/// A view of an asset: made when a pane first shows the asset, kept while it does, made again when the asset
/// changes.
pub trait View {
    fn show(&mut self, ui: &mut Ui);
}
