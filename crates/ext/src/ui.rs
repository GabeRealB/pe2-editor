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

/// A polygon over a canvas, in its units: a face where a texture has it.
#[derive(Clone, Debug, PartialEq)]
pub struct Traced {
    pub corners: Vec<(f32, f32)>,
    /// Filled and drawn stronger: what is selected.
    pub strong: bool,
    /// What hovering it says.
    pub label: Option<String>,
    /// What clicking it opens: a path, or `#` and a part.
    pub link: Option<String>,
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
    /// Polygons over the pictures.
    pub traces: Vec<Traced>,
}

/// The keys held with what else is done.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

/// What the pointer and the keys did over a viewport this frame. Positions are in points from its corner.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ViewportInput {
    pub hovered: bool,
    pub pointer: Option<(f32, f32)>,
    /// What it moved by while held.
    pub drag: (f32, f32),
    /// The primary, the secondary and the middle button.
    pub held: (bool, bool, bool),
    pub pressed: (bool, bool, bool),
    pub released: (bool, bool, bool),
    /// A press and release of the primary button without moving.
    pub clicked: bool,
    pub scroll: f32,
    pub modifiers: Modifiers,
    /// The keys pressed this frame while over it, by name (`A`, `Num1`, `Home`, `Period`).
    pub keys: Vec<String>,
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
    /// A word lit while it is on, a toolbar's; whether it was clicked.
    fn toggle(&mut self, text: &str, on: bool) -> bool;
    /// An image at a size, framed: a palette's swatch.
    fn image(&mut self, texture: &Texture, width: f32, height: f32);
    /// How much room is left here, in points, and how many pixels a point is.
    fn available(&mut self) -> (f32, f32, f32) {
        (0.0, 0.0, 1.0)
    }
    /// A texture published through `gpu`, filling this many points, taking what the pointer and the keys do over it.
    fn gpu_viewport(&mut self, id: u64, width: f32, height: f32) -> ViewportInput {
        let _ = (id, width, height);
        ViewportInput::default()
    }
    /// Draw again next frame, nothing having happened.
    fn request_repaint(&mut self) {}
    /// The host's chip for a reference: its asset's icon and name, opening it on a click.
    fn reference(&mut self, reference: &Value);
    /// The host's chip for a reference, taking an asset of `kind` dropped on it: the reference that makes, its other keys
    /// kept, in the frame it is dropped. With no reference it is a place to drop one, and answers with the dropped asset's
    /// uid. `label` says what is being set.
    fn reference_taking(&mut self, label: &str, kind: &str, reference: Option<&Value>) -> Option<Value>;
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
    /// As `set`, a step of its own: what a gesture ended with, where `set` keeps a value being adjusted in one.
    fn set_step(&mut self, key: &str, value: Option<&Value>);
    /// What a gesture has made so far of fields of the part selected, nothing being written yet: the inspector shows
    /// these over what the asset holds until the asset next changes, or this is called with none.
    fn live(&mut self, fields: &[(&str, Value)]);
    /// A field of the part selected the inspector is being dragged or typed in, with what it holds so far, nothing being
    /// written yet.
    fn editing(&self) -> Option<(String, Value)>;
    /// A row: what is drawn on it is laid out left to right, until it is dropped.
    fn horizontal(&mut self) -> Box<dyn UiHost + '_>;
    /// A row that wraps where the pane is narrow.
    fn wrapped(&mut self) -> Box<dyn UiHost + '_>;
    /// What is drawn on it one under the other, beside what its row holds.
    fn vertical(&mut self) -> Box<dyn UiHost + '_>;
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
    fn picture(&mut self, path: &str, look: Option<&str>) -> Option<Result<Texture, String>>;
    /// Select the asset at a path, as clicking it in the tree would.
    fn navigate(&mut self, path: &str);
    /// The part selected of the asset shown: its fragment (`cells/cursor`), wherever it was selected.
    fn selected(&self) -> Option<String>;
    /// What a view of the asset last left for itself: a view is made again whenever what it shows changes.
    fn note(&self) -> Option<String>;
    fn leave_note(&mut self, text: &str);
    /// Select a part of the asset shown, as clicking it anywhere would.
    fn select(&mut self, part: &str);
    /// Look at the asset shown in a member of a group of variants: everything showing it follows, nothing is written.
    fn choose(&mut self, group: &str, member: &str);
    /// In a row, go on from this many points from its start, for rows that line up: past it already, go on as it is.
    fn tab(&mut self, at: f32);
    /// The asset the view shows, lent for the frame; none for a panel.
    fn asset(&self) -> Option<Asset<'_>>;
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

    /// A word lit while it is on, a toolbar's; whether it was clicked.
    pub fn toggle(&mut self, text: &str, on: bool) -> bool {
        self.host.toggle(text, on)
    }

    /// How much room is left here, in points, and how many pixels a point is.
    pub fn available(&mut self) -> (f32, f32, f32) {
        self.host.available()
    }

    /// A texture published through `gpu`, filling this many points, taking what the pointer and the keys do over it.
    pub fn gpu_viewport(&mut self, id: u64, width: f32, height: f32) -> ViewportInput {
        self.host.gpu_viewport(id, width, height)
    }

    /// Draw again next frame, nothing having happened.
    pub fn request_repaint(&mut self) {
        self.host.request_repaint();
    }

    pub fn image(&mut self, texture: &Texture, width: f32, height: f32) {
        self.host.image(texture, width, height);
    }

    pub fn reference(&mut self, reference: &Value) {
        self.host.reference(reference);
    }

    /// The host's chip for a reference, taking an asset of `kind` dropped on it: the reference that makes, its other keys
    /// kept, in the frame it is dropped. With no reference it is a place to drop one, and answers with the dropped asset's
    /// uid. `label` says what is being set.
    pub fn reference_taking(&mut self, label: &str, kind: &str, reference: Option<&Value>) -> Option<Value> {
        self.host.reference_taking(label, kind, reference)
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

    /// As `set`, a step of its own: what a gesture ended with, where `set` keeps a value being adjusted in one.
    pub fn set_step(&mut self, key: &str, value: Option<&Value>) {
        self.host.set_step(key, value);
    }

    /// What a gesture has made so far of fields of the part selected, nothing being written yet: the inspector shows
    /// these over what the asset holds until the asset next changes, or this is called with none.
    pub fn live(&mut self, fields: &[(&str, Value)]) {
        self.host.live(fields);
    }

    /// A field of the part selected the inspector is being dragged or typed in, with what it holds so far, nothing being
    /// written yet.
    #[must_use]
    pub fn editing(&self) -> Option<(String, Value)> {
        self.host.editing()
    }

    /// What `body` draws, in a row.
    pub fn horizontal(&mut self, body: impl FnOnce(&mut Ui)) {
        let mut row = self.host.horizontal();
        body(&mut Ui { host: &mut *row });
    }

    /// What `body` draws, one under the other, beside what its row holds.
    pub fn vertical(&mut self, body: impl FnOnce(&mut Ui)) {
        let mut column = self.host.vertical();
        body(&mut Ui { host: &mut *column });
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
    pub fn picture(&mut self, path: &str, look: Option<&str>) -> Option<Result<Texture, String>> {
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

    /// What a view of the asset last left for itself: a view is made again whenever what it shows changes.
    #[must_use]
    pub fn note(&self) -> Option<String> {
        self.host.note()
    }

    pub fn leave_note(&mut self, text: &str) {
        self.host.leave_note(text);
    }

    /// Select a part of the asset shown: in the contents, the inspector and every view of it.
    pub fn select(&mut self, part: &str) {
        self.host.select(part);
    }

    /// Look at the asset shown in a member of a group of variants: everything showing it follows, nothing is written.
    pub fn choose(&mut self, group: &str, member: &str) {
        self.host.choose(group, member);
    }

    /// In a row, go on from this many points from its start, for rows that line up: past it already, go on as it is.
    pub fn tab(&mut self, at: f32) {
        self.host.tab(at);
    }

    /// The asset shown; none for a panel.
    #[must_use]
    pub fn asset(&self) -> Option<Asset<'_>> {
        self.host.asset()
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
