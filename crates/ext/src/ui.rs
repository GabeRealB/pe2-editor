//! Views: an extension's own UI, in immediate mode as the editor's is. A view is one instance a pane, keeping its
//! state between frames; every frame the host calls `show` with a `Ui` it lends for the call, and each call on it
//! draws a widget and returns what the user did with it this frame. A view shows an asset (a kind's view, in the
//! preview or the inspector), or none (a panel).

use crate::{Asset, Icon, Project, Rect, Texture, Value};

/// A table's cell (`Ui::table`).
#[derive(Clone, Debug)]
pub enum Cell {
    /// Nothing, its column kept.
    Empty,
    Text(String, Style),
    /// The host's chip for a reference: its asset's icon and name, opening it on a click.
    Reference(Value),
    /// An icon button, what it does said on hovering it.
    Button(Icon, String),
}

/// How a label is set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Normal,
    /// Secondary: a value's unit, a name beside a swatch.
    Weak,
    /// Small and secondary: a count, a note.
    Note,
    Strong,
    Error,
}

/// A picture on a canvas: where it is drawn, what hovering it says, what clicking it opens.
#[derive(Clone)]
pub struct Placed {
    pub texture: Texture,
    pub rect: Rect,
    pub label: Option<String>,
    /// An asset's path.
    pub link: Option<String>,
}

/// A named rectangle over a canvas.
#[derive(Clone)]
pub struct Marked {
    pub name: String,
    pub rect: Rect,
    /// Drawn stronger: the selected one.
    pub highlight: bool,
}

/// Images laid out on a canvas the host zooms and pans, with named boxes over them: an atlas's cells, an image's
/// pages, VRAM's slots. Sizes and rectangles are in the canvas's own units (an image's pixels, VRAM's halfwords).
#[derive(Clone, Default)]
pub struct Canvas {
    pub width: f32,
    pub height: f32,
    pub pictures: Vec<Placed>,
    pub boxes: Vec<Marked>,
    /// What the boxes are, beside their toggle: "12 cells". Without a toggle the boxes are always drawn.
    pub summary: String,
    /// The toggle showing the boxes: "Cells".
    pub toggle: String,
    /// Boxes each in a color of its own, named in a corner and in a legend; else plain.
    pub legend: bool,
    /// Lines every so many units across and down: VRAM's texture pages.
    pub grid: Option<(f32, f32)>,
}

/// What the host gives a view to draw with: its widgets, and what it knows. The host implements it over its own UI; a
/// component reaches the same through the host's `ui` and `project` resources.
pub trait UiHost {
    fn label(&mut self, text: &str, style: Style);
    /// A button; whether it was clicked.
    fn button(&mut self, text: &str) -> bool;
    /// A button showing an icon of the host's set, what it does said on hovering it; whether it was clicked.
    fn icon_button(&mut self, icon: Icon, hint: &str) -> bool;
    /// A checkbox over `value`; whether it was toggled.
    fn checkbox(&mut self, text: &str, value: &mut bool) -> bool;
    /// A word in a pill: a type, a flag.
    fn badge(&mut self, text: &str);
    /// A small framed name, marked where `selected`: a cell, an item; whether it was clicked.
    fn chip(&mut self, text: &str, selected: bool) -> bool;
    /// An image at a size, framed: a palette's swatch.
    fn image(&mut self, texture: &Texture, width: f32, height: f32);
    /// A texture published through `gpu`, at this size.
    fn gpu_image(&mut self, id: u64, width: f32, height: f32) {
        let _ = (id, width, height);
    }
    /// The host's chip for a reference: its asset's icon and name, opening it on a click.
    fn reference(&mut self, reference: &Value);
    /// A field within a row, the host's way: its label and its reading (a reference as a chip, a flag as a badge).
    fn field(&mut self, key: &str, value: &Value);
    /// What hovering the last widget says.
    fn tooltip(&mut self, text: &str);
    /// A one-line text field over `text`, `id` keeping it apart; whether it was committed this frame (Enter, or the
    /// keyboard taken elsewhere).
    fn text_edit(&mut self, id: &str, text: &mut String) -> bool;
    /// A dropdown showing `selected`; the option chosen this frame.
    fn combo(&mut self, id: &str, selected: &str, options: &[String]) -> Option<usize>;
    /// Whether the asset shown can be edited: its layer is not locked, nor outside a solo.
    fn editable(&self) -> bool;
    /// Set a field of the asset shown - its whole value, or none to remove it - in one undoable step with the frame's
    /// other changes. A table of parts keeps a line each.
    fn set(&mut self, key: &str, value: Option<&Value>);
    /// A row: what is drawn on it is laid out left to right, until it is dropped.
    fn horizontal(&mut self) -> Box<dyn UiHost + '_>;
    /// A row that wraps where the pane is narrow.
    fn wrapped(&mut self) -> Box<dyn UiHost + '_>;
    /// What is drawn on it indented.
    fn indented(&mut self) -> Box<dyn UiHost + '_>;
    /// One of the inspector's sections, titled.
    fn section(&mut self, title: &str) -> Box<dyn UiHost + '_>;
    /// A collapsing header, `id` keeping it open or not; its body while open.
    fn collapsing(&mut self, id: &str, title: &str, note: &str) -> Option<Box<dyn UiHost + '_>>;
    fn canvas(&mut self, canvas: &Canvas);
    /// Rows of cells in aligned columns, `id` keeping its widths: the cell of a button clicked this frame (row, column).
    fn table(&mut self, id: &str, rows: &[Vec<Cell>]) -> Option<(usize, usize)>;
    /// An asset's picture in a look as the host draws it - once, off the window's thread, shared by all that show it:
    /// none while it is being drawn (the view is drawn again when it is), or why it cannot be.
    fn picture(&mut self, path: &str, look: usize) -> Option<Result<Texture, String>>;
    /// Select the asset at a path, as clicking it in the tree would.
    fn navigate(&mut self, path: &str);
    /// The part selected of the asset shown: its fragment (`cells/cursor`), wherever it was selected.
    fn selected(&self) -> Option<String>;
    /// Select a part of the asset shown, as clicking it anywhere would.
    fn select(&mut self, part: &str);
    /// The asset the view shows, lent for the frame; none for a panel.
    fn asset(&self) -> Option<Asset<'_>>;
    /// The look the asset is shown in (an index into its picture's looks), as the host's look picker chose it.
    fn look(&self) -> usize;
    fn project(&self) -> &dyn Project;
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

    pub fn icon_button(&mut self, icon: Icon, hint: &str) -> bool {
        self.host.icon_button(icon, hint)
    }

    pub fn checkbox(&mut self, text: &str, value: &mut bool) -> bool {
        self.host.checkbox(text, value)
    }

    pub fn badge(&mut self, text: &str) {
        self.host.badge(text);
    }

    pub fn chip(&mut self, text: &str, selected: bool) -> bool {
        self.host.chip(text, selected)
    }

    /// A texture published through `gpu`, at this size.
    pub fn gpu_image(&mut self, id: u64, width: f32, height: f32) {
        self.host.gpu_image(id, width, height);
    }

    pub fn image(&mut self, texture: &Texture, width: f32, height: f32) {
        self.host.image(texture, width, height);
    }

    pub fn reference(&mut self, reference: &Value) {
        self.host.reference(reference);
    }

    pub fn field(&mut self, key: &str, value: &Value) {
        self.host.field(key, value);
    }

    pub fn tooltip(&mut self, text: &str) {
        self.host.tooltip(text);
    }

    /// A one-line text field; whether it was committed this frame.
    pub fn text_edit(&mut self, id: &str, text: &mut String) -> bool {
        self.host.text_edit(id, text)
    }

    /// A dropdown; the option chosen this frame.
    pub fn combo(&mut self, id: &str, selected: &str, options: &[String]) -> Option<usize> {
        self.host.combo(id, selected, options)
    }

    /// Whether the asset shown can be edited.
    #[must_use]
    pub fn editable(&self) -> bool {
        self.host.editable()
    }

    /// Set a field of the asset shown, or remove it.
    pub fn set(&mut self, key: &str, value: Option<&Value>) {
        self.host.set(key, value);
    }

    /// What `body` draws, in a row.
    pub fn horizontal(&mut self, body: impl FnOnce(&mut Ui)) {
        let mut row = self.host.horizontal();
        body(&mut Ui { host: &mut *row });
    }

    /// What `body` draws, in a row wrapping where the pane is narrow.
    pub fn wrapped(&mut self, body: impl FnOnce(&mut Ui)) {
        let mut row = self.host.wrapped();
        body(&mut Ui { host: &mut *row });
    }

    /// What `body` draws, indented.
    pub fn indented(&mut self, body: impl FnOnce(&mut Ui)) {
        let mut inner = self.host.indented();
        body(&mut Ui { host: &mut *inner });
    }

    /// What `body` draws, as one of the inspector's sections.
    pub fn section(&mut self, title: &str, body: impl FnOnce(&mut Ui)) {
        let mut section = self.host.section(title);
        body(&mut Ui { host: &mut *section });
    }

    /// A collapsing header; `body` draws what it holds while it is open.
    pub fn collapsing(&mut self, id: &str, title: &str, note: &str, body: impl FnOnce(&mut Ui)) {
        if let Some(mut inner) = self.host.collapsing(id, title, note) {
            body(&mut Ui { host: &mut *inner });
        }
    }

    pub fn canvas(&mut self, canvas: &Canvas) {
        self.host.canvas(canvas);
    }

    /// Rows of cells in aligned columns - a list's entries, a table's records - `id` keeping its widths: the cell of a
    /// button clicked this frame (row, column).
    pub fn table(&mut self, id: &str, rows: &[Vec<Cell>]) -> Option<(usize, usize)> {
        self.host.table(id, rows)
    }

    /// An asset's picture in a look as the host draws it: none while it is being drawn, or why it cannot be.
    pub fn picture(&mut self, path: &str, look: usize) -> Option<Result<Texture, String>> {
        self.host.picture(path, look)
    }

    pub fn navigate(&mut self, path: &str) {
        self.host.navigate(path);
    }

    /// The part selected of the asset shown, wherever it was selected.
    #[must_use]
    pub fn selected(&self) -> Option<String> {
        self.host.selected()
    }

    /// Select a part of the asset shown: in the contents, the inspector and every view of it.
    pub fn select(&mut self, part: &str) {
        self.host.select(part);
    }

    /// The asset shown; none for a panel.
    #[must_use]
    pub fn asset(&self) -> Option<Asset<'_>> {
        self.host.asset()
    }

    /// The look it is shown in.
    #[must_use]
    pub fn look(&self) -> usize {
        self.host.look()
    }

    /// The project, to ask about other assets.
    #[must_use]
    pub fn project(&self) -> &dyn Project {
        self.host.project()
    }

    /// The host's widgets, for a host passing the frame on (to a component).
    pub fn host(&mut self) -> &mut dyn UiHost {
        self.host
    }
}

/// A view: of an asset, made when a pane first shows it, kept while it does, made again when it changes; or a panel's,
/// kept while the panel is open.
pub trait View {
    fn show(&mut self, ui: &mut Ui);
}
