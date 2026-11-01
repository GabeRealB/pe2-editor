//! Registration: an extension's entry point and what it registers.

use std::fmt;
use std::sync::Arc;

use std::marker::PhantomData;

use crate::data::{Call, OpDef, Port, Provides, Retain, Type};
use crate::ty::ValueType;
use crate::import::{Recognized, Signature, TypeDef};
use crate::{Asset, Project, Result, Table, Value, View};

/// An extension: its id, its version, how it depends on others and its entry point. `extension!` makes it.
pub struct Extension {
    pub id: &'static str,
    /// Its version, as semver: `0.1.0`.
    pub version: &'static str,
    /// The extensions it needs, by id and the versions it accepts: `pe2.images@^0.1` (a version alone is a caret
    /// requirement; an id alone takes any). It may extend only types these register. Without one, or with one
    /// refused, left off by the project or of another version, it is refused.
    pub requires: &'static [&'static str],
    /// What it uses where a compatible one is enabled, and otherwise does without, as `optional`. Linked, turning it off
    /// turns this extension off too. What it adds through one not linked is not there (`Registry::add_output`).
    pub weak: &'static [&'static str],
    /// What it uses where a compatible one is enabled, and otherwise does without. Linked, turning it off leaves this
    /// extension on.
    pub optional: &'static [&'static str],
    /// The services it offers, by name and version: `c-compiler@1.0.0`. What requires one takes whichever provides it,
    /// not knowing what is behind it, its operations named by the service (`c-compiler@compile`).
    pub provides: &'static [&'static str],
    /// The platforms it runs on, as `<os>-<arch>` (`linux-x86_64`); none, any.
    pub platforms: &'static [&'static str],
    /// It runs outside the sandbox: all of WASI, and programs (`pe2_ext::process`). The user enables it first, and
    /// until then none of it runs.
    pub unsandboxed: bool,
    pub register: fn(&mut Registry) -> Result<()>,
}

/// Declares an extension: its id, its version, its entry point and, optionally, what it requires. Defines `ID`, the
/// id, and `extension()`, which a host linking it in loads it by; built as a component, it exports it.
///
/// ```ignore
/// pe2_ext::extension!("acme.fonts", "0.1.0", register, requires = ["pe2.images@^0.1"]);
/// ```
#[macro_export]
macro_rules! extension {
    ($id:literal, $version:literal, $register:path
        $(, requires = [$($requires:literal),* $(,)?])?
        $(, weak = [$($weak:literal),* $(,)?])?
        $(, optional = [$($optional:literal),* $(,)?])?
        $(, provides = [$($provides:literal),* $(,)?])?
        $(, platforms = [$($platforms:literal),* $(,)?])?
        $(, unsandboxed = $unsandboxed:literal)?
        $(,)?) => {
        /// This extension's id.
        pub const ID: &str = $id;

        /// This extension, for the host to load.
        #[must_use]
        pub fn extension() -> $crate::Extension {
            $crate::Extension {
                id: $id,
                version: $version,
                requires: &[$($($requires),*)?],
                weak: &[$($($weak),*)?],
                optional: &[$($($optional),*)?],
                provides: &[$($($provides),*)?],
                platforms: &[$($($platforms),*)?],
                unsandboxed: false $(|| $unsandboxed)?,
                register: $register,
            }
        }

        /// This extension, for the SDK's component exports.
        #[cfg(target_family = "wasm")]
        #[unsafe(no_mangle)]
        fn __pe2_extension() -> $crate::Extension {
            extension()
        }

        /// Who it is, as its component carries it in the section the host reads without running it
        /// (`pe2-extension`): as TOML.
        #[cfg(target_family = "wasm")]
        const __PE2_ABOUT: &str = concat!(
            "id = \"", $id, "\"\nversion = \"", $version, "\"\nrequires = [", $($("\"", $requires, "\", ",)*)? "]\nweak = [",
            $($("\"", $weak, "\", ",)*)? "]\noptional = [", $($("\"", $optional, "\", ",)*)? "]\nprovides = [",
            $($("\"", $provides, "\", ",)*)? "]\nplatforms = [", $($("\"", $platforms, "\", ",)*)? "]\n"
            $(, "unsandboxed = ", $unsandboxed, "\n")?
        );
        #[cfg(target_family = "wasm")]
        #[unsafe(link_section = "pe2-extension")]
        #[used]
        static __PE2_ABOUT_SECTION: [u8; __PE2_ABOUT.len()] = $crate::section(__PE2_ABOUT);

        #[cfg(target_family = "wasm")]
        use $crate::guest::Component as __Pe2Component;
        #[cfg(target_family = "wasm")]
        $crate::guest::bindings::export_component!(__Pe2Component with_types_in $crate::guest::bindings);
    };
}

/// A text as a custom section's bytes, for `extension!`.
#[doc(hidden)]
#[must_use]
pub const fn section<const N: usize>(text: &str) -> [u8; N] {
    let (bytes, mut out, mut i) = (text.as_bytes(), [0; N], 0);
    while i < N {
        out[i] = bytes[i];
        i += 1;
    }
    out
}

/// A name an extension registers, qualified by its id: `pe2.images@image`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Name(String);

impl Name {
    /// `name`, as the extension `id` registers it.
    #[must_use]
    pub fn of(id: &str, name: &str) -> Name {
        Name(format!("{id}@{name}"))
    }

    /// A qualified name, as written: `pe2.images@image`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Name> {
        let (id, name) = text.split_once('@')?;
        (!id.is_empty() && !name.is_empty()).then(|| Name(text.to_string()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The id of the extension registering it.
    #[must_use]
    pub fn extension(&self) -> &str {
        self.0.split_once('@').map_or(&self.0, |(id, _)| id)
    }

    /// The name within its extension.
    #[must_use]
    pub fn local(&self) -> &str {
        self.0.split_once('@').map_or(&self.0, |(_, name)| name)
    }
}

impl fmt::Display for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// An icon of the host's set, for a type in the tree and the inspector.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Disc,
    Stack,
    Image,
    Palette,
    Grid,
    Film,
    Waveform,
    Speaker,
    Music,
    Package,
    Cube,
    Bone,
    Polygon,
    Terminal,
    Text,
    Binary,
    File,
    Folder,
    /// Actions, for icon buttons.
    CaretUp,
    CaretDown,
    Cross,
    Plus,
}

/// The family an icon is tinted by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tint {
    Structure,
    Picture,
    Motion,
    Sound,
    Code,
    Plain,
}

/// Formats a field's value for reading; none where the value is not of the form it formats.
pub type Format = dyn Fn(&Value) -> Option<String> + Send + Sync;

/// A field of a type, as the inspector shows it: its label and how its value reads.
#[derive(Clone)]
pub struct Field {
    pub key: String,
    pub label: Option<String>,
    /// Its value as a property.
    pub format: Option<Arc<Format>>,
    /// Its value within a row of a list, briefly.
    pub brief: Option<Arc<Format>>,
    /// The values it takes, each with its label: the inspector offers them as a list to choose from, and reads a
    /// value by its label.
    pub choices: Vec<(Value, String)>,
    /// Not edited in place: it describes stored data, which an edit would not change.
    pub read_only: bool,
    /// Drawn by its type's inspector view, not by the host's property sheet.
    pub custom: bool,
    /// How it reads when left out, which a choice can go back to; a field's value is then empty.
    pub absent: Option<String>,
    /// It has no value of its own to fall back on: left out, what it is set on is wrong.
    pub required: bool,
}

impl Field {
    #[must_use]
    pub fn new(key: &str) -> Field {
        Field {
            key: key.to_string(),
            label: None,
            format: None,
            brief: None,
            choices: vec![],
            read_only: false,
            custom: false,
            absent: None,
            required: false,
        }
    }

    #[must_use]
    pub fn label(mut self, label: &str) -> Field {
        self.label = Some(label.to_string());
        self
    }

    #[must_use]
    pub fn format(mut self, format: impl Fn(&Value) -> Option<String> + Send + Sync + 'static) -> Field {
        self.format = Some(Arc::new(format));
        self
    }

    #[must_use]
    pub fn brief(mut self, brief: impl Fn(&Value) -> Option<String> + Send + Sync + 'static) -> Field {
        self.brief = Some(Arc::new(brief));
        self
    }

    /// The values it takes, labeled.
    #[must_use]
    pub fn choices<V: Into<Value>>(mut self, choices: impl IntoIterator<Item = (V, &'static str)>) -> Field {
        self.choices = choices.into_iter().map(|(value, label)| (value.into(), label.to_string())).collect();
        self
    }

    /// A value's label among its choices.
    #[must_use]
    pub fn choice(&self, value: &Value) -> Option<&str> {
        self.choices.iter().find(|(v, _)| v == value).map(|(_, label)| label.as_str())
    }

    #[must_use]
    pub fn required(mut self) -> Field {
        self.required = true;
        self
    }

    /// Its value is not edited here.
    #[must_use]
    pub fn read_only(mut self) -> Field {
        self.read_only = true;
        self
    }

    /// Drawn by the type's inspector view instead of the host's sheet.
    #[must_use]
    pub fn custom(mut self) -> Field {
        self.custom = true;
        self
    }
}

/// A command an extension offers: its label, and the tool it runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandDef {
    pub label: String,
    pub tool: Name,
}

/// A way to make a node: its stack as it is stamped into the node, `{file}` standing for its file. A node made so holds its
/// stack whole: what becomes of the template later is no concern of its.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateDef {
    /// By this a declaration names it, and an editor offers it.
    pub name: String,
    pub label: String,
    /// The type of node it makes, qualified.
    pub node_type: String,
    /// The files it takes, by name (`*.mp4`); none, it is chosen by name alone.
    pub files: Vec<String>,
    pub stack: String,
}

impl TemplateDef {
    /// Whether a node of it holds a stored file: it cannot be made with nothing, a file having to come from somewhere.
    #[must_use]
    pub fn stored(&self) -> bool {
        let parsed: Option<Table> = self.stack.parse().ok();
        let layers = parsed.as_ref().and_then(|stack| stack.get("stack")).and_then(Value::as_array);
        self.stack.contains("{file}") || layers.is_some_and(|layers| layers.iter().any(|layer| layer.get("op").and_then(Value::as_str) == Some("file")))
    }
}

/// Makes a view of an asset.
pub type MakeView = dyn Fn(&Asset) -> Box<dyn View> + Send + Sync;

/// Makes a panel's view.
pub type MakePanel = dyn Fn() -> Box<dyn View> + Send + Sync;

/// A tool's work: its arguments to its answer.
pub type Run = dyn Fn(&dyn Project, &Value) -> Result<Value> + Send + Sync;

/// A pane of the extension's own, not of an asset: shown from the View menu.
#[derive(Clone)]
pub struct PanelDef {
    pub name: Name,
    pub label: String,
    pub icon: Option<Icon>,
    pub make: Arc<MakePanel>,
}

/// A tool agents call (the agents' server lists it), its arguments described by a JSON schema.
#[derive(Clone)]
pub struct ToolDef {
    pub name: Name,
    pub description: String,
    pub schema: String,
    pub run: Arc<Run>,
}

/// What an extension registered.
#[derive(Clone, Default)]
pub struct Parts {
    pub panels: Vec<PanelDef>,
    pub tools: Vec<ToolDef>,
    /// Services: like tools, but for other extensions to call.
    pub services: Vec<ToolDef>,
    pub ops: Vec<OpDef>,
    pub types: Vec<TypeDef>,
    pub extractors: Vec<crate::ExtractorDef>,
    /// Whether its instance may be reclaimed now; without, always.
    pub reclaimable: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    /// Viewers: what shows an output of a type in a document, by the type's name.
    pub viewers: Vec<(String, ViewDef)>,
    /// The events it declares, by name, with their labels.
    pub events: Vec<(String, String)>,
    /// What it runs where an event is signaled.
    pub hooks: Vec<HookDef>,
    /// Ways to make a node.
    pub templates: Vec<TemplateDef>,
    /// What it offers among the editor's commands.
    pub commands: Vec<CommandDef>,
    /// The memory a job of something it registered may take, where more than the host's default, by its name.
    pub memory: Vec<(Name, u64)>,
    /// Its settings, as declared, each with the value it took.
    pub settings: Vec<(Setting, Value)>,
    /// What it registered wrongly (a type not written as one): the host's problems of it.
    pub problems: Vec<String>,
}

/// Where a setting is kept, and so whom it is the same for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// In the project (`project.toml`): the same for everyone on it. What changes how assets read or are made.
    Project,
    /// The user's (the editor's settings): a preference, the same in every project. Never what assets are made of.
    User,
}

impl Scope {
    /// Its name in the stored settings the host gives (`project`, `user`).
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Scope::Project => "project",
            Scope::User => "user",
        }
    }
}

/// A setting of an extension: its key, how it reads, its default - whose type its values must have - where it is
/// kept, and the values it takes where they are few.
#[derive(Clone, Debug)]
pub struct Setting {
    pub key: String,
    pub label: Option<String>,
    /// What it does, said where it is edited.
    pub help: Option<String>,
    pub default: Value,
    pub scope: Scope,
    pub choices: Vec<(Value, String)>,
}

impl Setting {
    /// A project's setting, `key`, its default `default`.
    #[must_use]
    pub fn new(key: &str, default: impl Into<Value>) -> Setting {
        let default = default.into();
        Setting { key: key.to_string(), label: None, help: None, default, scope: Scope::Project, choices: vec![] }
    }

    #[must_use]
    pub fn label(mut self, label: &str) -> Setting {
        self.label = Some(label.to_string());
        self
    }

    #[must_use]
    pub fn help(mut self, help: &str) -> Setting {
        self.help = Some(help.to_string());
        self
    }

    /// The user's, not the project's: a preference.
    #[must_use]
    pub fn user(mut self) -> Setting {
        self.scope = Scope::User;
        self
    }

    /// The values it takes, labeled.
    #[must_use]
    pub fn choices<V: Into<Value>>(mut self, choices: impl IntoIterator<Item = (V, &'static str)>) -> Setting {
        self.choices = choices.into_iter().map(|(value, label)| (value.into(), label.to_string())).collect();
        self
    }

    /// Whether `value` is one it may take: of its default's type, and among its choices where it has them.
    #[must_use]
    pub fn accepts(&self, value: &Value) -> bool {
        let same = std::mem::discriminant(value) == std::mem::discriminant(&self.default)
            // An integer where a float is wanted reads as one: `2` for `2.0`.
            || matches!((value, &self.default), (Value::Integer(_), Value::Float(_)));
        same && (self.choices.is_empty() || self.choices.iter().any(|(v, _)| v == value))
    }
}

#[derive(Clone)]
pub struct ViewDef {
    pub name: String,
    pub make: Arc<MakeView>,
    /// In the inspector: the tab it has of its own; none, it is among the properties.
    pub tab: Option<TabDef>,
}

/// The place a view has of its own, an inspector's tab or an editor: shown for an asset with any of the fields, or with none
/// named for every asset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabDef {
    pub icon: Icon,
    pub fields: Vec<String>,
}

/// What can be done to a row of the contents, or to the pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentAction {
    pub id: String,
    pub label: String,
    pub icon: Option<Icon>,
    /// A button on the row itself, not an entry of its menu.
    pub on_row: bool,
    /// Held down: a row's visibility.
    pub on: bool,
}

impl ContentAction {
    #[must_use]
    pub fn new(id: &str, label: &str) -> ContentAction {
        ContentAction { id: id.to_string(), label: label.to_string(), icon: None, on_row: false, on: false }
    }

    #[must_use]
    pub fn icon(mut self, icon: Icon) -> ContentAction {
        self.icon = Some(icon);
        self
    }

    /// A button on the row itself.
    #[must_use]
    pub fn on_row(mut self) -> ContentAction {
        self.on_row = true;
        self
    }

    /// Held down.
    #[must_use]
    pub fn on(mut self, on: bool) -> ContentAction {
        self.on = on;
        self
    }
}

/// How a row of the contents reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ContentStyle {
    /// Selected when clicked.
    #[default]
    Item,
    /// Greyed: left out, hidden.
    Dim,
    /// Only a name for those under it: not selected.
    Heading,
}

/// A row dragged: what it is, and whether those of the same `what` go around it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentDrag {
    pub what: String,
    pub reorders: bool,
}

/// A row of what is inside an asset. `id` is what selecting it selects, an output's address where it is one; a row deeper
/// than the one before it is under that one, which folds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentRow {
    pub id: String,
    pub label: String,
    /// What it says beside its label.
    pub detail: String,
    /// What it says on hovering it.
    pub hint: String,
    pub depth: u32,
    pub style: ContentStyle,
    pub icon: Option<(Icon, Tint)>,
    /// Its part's picture, where its icon would go.
    pub picture: bool,
    /// How it starts, where it folds.
    pub open: bool,
    /// The look its asset is shown in once it is selected.
    pub look: Option<String>,
    /// An asset's path, opened by a double-click.
    pub link: Option<String>,
    /// Offers renaming it.
    pub rename: bool,
    /// What it is when dragged; none, it stays.
    pub drag: Option<ContentDrag>,
    /// What it holds, by what those are when dragged.
    pub takes: Vec<String>,
    pub actions: Vec<ContentAction>,
}

impl ContentRow {
    #[must_use]
    pub fn new(id: &str, label: &str) -> ContentRow {
        ContentRow {
            id: id.to_string(),
            label: label.to_string(),
            detail: String::new(),
            hint: String::new(),
            depth: 0,
            style: ContentStyle::Item,
            icon: None,
            picture: false,
            open: true,
            look: None,
            link: None,
            rename: false,
            drag: None,
            takes: vec![],
            actions: vec![],
        }
    }

    /// A name for the rows under it.
    #[must_use]
    pub fn heading(id: &str, label: &str) -> ContentRow {
        ContentRow { style: ContentStyle::Heading, ..ContentRow::new(id, label) }
    }

    /// Whether it only names those under it.
    #[must_use]
    pub fn is_heading(&self) -> bool {
        self.style == ContentStyle::Heading
    }

    #[must_use]
    pub fn detail(mut self, detail: &str) -> ContentRow {
        detail.clone_into(&mut self.detail);
        self
    }

    #[must_use]
    pub fn hint(mut self, hint: &str) -> ContentRow {
        hint.clone_into(&mut self.hint);
        self
    }

    #[must_use]
    pub fn depth(mut self, depth: u32) -> ContentRow {
        self.depth = depth;
        self
    }

    #[must_use]
    pub fn icon(mut self, icon: Icon, tint: Tint) -> ContentRow {
        self.icon = Some((icon, tint));
        self
    }

    /// Show its part's picture where its icon would go.
    #[must_use]
    pub fn picture(mut self) -> ContentRow {
        self.picture = true;
        self
    }

    /// Start folded.
    #[must_use]
    pub fn closed(mut self) -> ContentRow {
        self.open = false;
        self
    }

    /// Greyed: left out, hidden.
    #[must_use]
    pub fn dim(mut self, dim: bool) -> ContentRow {
        self.style = if dim { ContentStyle::Dim } else { ContentStyle::Item };
        self
    }

    /// Show its asset in this look once it is selected.
    #[must_use]
    pub fn look(mut self, look: &str) -> ContentRow {
        self.look = Some(look.to_string());
        self
    }

    /// Open an asset on a double-click.
    #[must_use]
    pub fn link(mut self, path: &str) -> ContentRow {
        self.link = Some(path.to_string());
        self
    }

    #[must_use]
    pub fn renames(mut self) -> ContentRow {
        self.rename = true;
        self
    }

    /// Dragged, as what: onto the rows that take it.
    #[must_use]
    pub fn drags(mut self, what: &str) -> ContentRow {
        self.drag = Some(ContentDrag { what: what.to_string(), reorders: false });
        self
    }

    /// Dragged, as what: onto the rows that take it, and around others of the same.
    #[must_use]
    pub fn reorders(mut self, what: &str) -> ContentRow {
        self.drag = Some(ContentDrag { what: what.to_string(), reorders: true });
        self
    }

    /// Holds what is dragged as these.
    #[must_use]
    pub fn takes(mut self, drags: &[&str]) -> ContentRow {
        self.takes = drags.iter().map(|what| (*what).to_string()).collect();
        self
    }

    #[must_use]
    pub fn action(mut self, action: ContentAction) -> ContentRow {
        self.actions.push(action);
        self
    }
}

/// What is inside an asset: its rows, what the pane offers above them, and whether its stack follows them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Contents {
    pub rows: Vec<ContentRow>,
    pub actions: Vec<ContentAction>,
    pub stack: bool,
    /// Why they could not be told: the host's to say, not an extension's.
    pub problem: Option<String>,
}

impl Default for Contents {
    fn default() -> Contents {
        Contents { rows: vec![], actions: vec![], stack: true, problem: None }
    }
}

/// Where a row dragged lands by the row it is dropped on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentPlace {
    Before,
    Inside,
    After,
}

/// What was done in the contents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContentRequest {
    /// An action, of a row, or of the pane where `row` is empty.
    Action { row: String, action: String },
    Rename { row: String, name: String },
    Moved { row: String, onto: String, place: ContentPlace },
    /// An asset, by its path, dropped on a row whose `takes` names `asset:` and its type.
    Dropped { row: String, asset: String },
}

/// What comes of it: its name in the history, what it changes in the declaration, each by its path (keys, a list's place) and
/// removed where it has no value, and what is selected after it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ContentOutcome {
    pub label: String,
    pub sets: Vec<(Vec<String>, Option<Value>)>,
    pub select: Option<String>,
}

/// A field of a part in the inspector.
#[derive(Clone, Debug, PartialEq)]
pub struct PartField {
    pub key: String,
    pub label: String,
    /// None: left out, reading as `absent`.
    pub value: Option<Value>,
    pub absent: Option<String>,
    pub choices: Vec<(Value, String)>,
    pub read_only: bool,
}

/// A part's fields, and the table of the declaration they are in.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PartSheet {
    pub within: Vec<String>,
    pub fields: Vec<PartField>,
    /// Why it could not be told: the host's to say, not an extension's.
    pub problem: Option<String>,
}

/// What is inside an asset.
pub type ListContents = dyn Fn(&Asset, &dyn crate::Project) -> Contents + Send + Sync;
/// What comes of something done in an asset's contents.
pub type ActOnContents = dyn Fn(&Asset, &dyn crate::Project, ContentRequest) -> Result<ContentOutcome> + Send + Sync;
/// A part's fields, by its fragment.
pub type SheetOfPart = dyn Fn(&Asset, &dyn crate::Project, &str) -> Option<PartSheet> + Send + Sync;

/// An operation run where an event is signaled, before what the event pulls: failing, it stops those.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookDef {
    pub event: String,
    /// The operation, qualified.
    pub op: String,
    /// The events it signals, where it succeeds.
    pub signals: Vec<String>,
}

/// A hook being registered.
pub struct HookBuilder<'a> {
    hook: &'a mut HookDef,
}

impl HookBuilder<'_> {
    /// Say an event it signals where it succeeds: what a project's graphs are planned by, so it cannot be decided as it runs.
    #[must_use]
    pub fn signals<E: crate::events::Event>(self) -> Self {
        self.hook.signals.push(E::NAME.to_string());
        self
    }
}

/// What an extension registers, as its entry point calls it. The host checks the outcome: an alias must be a built-in's,
/// a name registered once.
pub struct Registry {
    id: String,
    parts: Parts,
    /// The settings kept for it, by scope (`project`, `user`), as the host gives them.
    stored: Table,
}

impl Registry {
    /// A registry for the extension `id`, as the host makes it.
    #[must_use]
    pub fn new(id: &str) -> Registry {
        Registry::with_settings(id, Table::new())
    }

    /// A registry for the extension `id`, with the settings kept for it: by scope, each a table of values by key
    /// (`{ project = { ... }, user = { ... } }`).
    #[must_use]
    pub fn with_settings(id: &str, stored: Table) -> Registry {
        Registry { id: id.to_string(), parts: Parts::default(), stored }
    }

    /// The settings kept for it, by scope, as the host gave them.
    #[must_use]
    pub fn stored(&self) -> &Table {
        &self.stored
    }

    /// A setting: declared, and its value now - as kept in its scope, else its default (a value of another type, or
    /// none of its choices, is not taken; the host says so). An extension reads its settings as it registers; a
    /// change loads it again.
    pub fn setting(&mut self, setting: Setting) -> Value {
        let kept = self.stored.get(setting.scope.name()).and_then(Value::as_table).and_then(|t| t.get(&setting.key));
        let value = match kept {
            Some(Value::Integer(i)) if matches!(setting.default, Value::Float(_)) && setting.accepts(&Value::Integer(*i)) => {
                #[expect(clippy::cast_precision_loss, reason = "a setting's number")]
                Value::Float(*i as f64)
            }
            Some(value) if setting.accepts(value) => value.clone(),
            _ => setting.default.clone(),
        };
        self.parts.settings.push((setting, value.clone()));
        value
    }

    /// The registering extension's id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Add to a type of an extension this one depends on, by its qualified name: its own extension can so stay free of what
    /// is added. An output made by a `weak` or `optional` dependency not linked is not there, and nothing else changes.
    /// Registered by `register` on what this returns.
    pub fn extend(&mut self, name: &str) -> TypeBuilder<'_> {
        let mut builder = self.named_type("");
        builder.def.name = Name(name.to_string());
        builder.def.extends = true;
        builder
    }

    /// A panel, `<id>@<name>`, labeled in the View menu and its tab.
    pub fn panel(&mut self, name: &str, label: &str, icon: Icon, make: impl Fn() -> Box<dyn View> + Send + Sync + 'static) {
        let name = Name::of(&self.id, name);
        self.parts.panels.push(PanelDef { name, label: label.to_string(), icon: Some(icon), make: Arc::new(make) });
    }

    /// A type of the extension's, by its marker: its name and label, and what recognizes it in a stored file. Its
    /// name must be in the extension's namespace.
    pub fn value_type<T: crate::ty::Registered>(&mut self) -> Name {
        let Some(name) = Name::parse(T::NAME).filter(|n| n.extension() == self.id) else {
            self.parts.problems.push(format!("{}: not a type of {}", T::NAME, self.id));
            return Name::of(&self.id, T::NAME);
        };
        let mut builder = self.named_type(name.local()).label(T::LABEL);
        if let Some(media_type) = T::MEDIA_TYPE {
            builder = builder.media_type(media_type);
        }
        builder.def.signatures = T::signatures();
        if let Some(sniff) = T::SNIFF {
            builder = builder.sniff(sniff);
        }
        builder.register()
    }

    /// A type, `<id>@<name>`: what flows between operations, and - where it says how they show - what a node making one, or
    /// a value it is `of`, is. Registered by `register` on what this returns.
    pub fn named_type(&mut self, name: &str) -> TypeBuilder<'_> {
        let def = TypeDef {
            name: Name::of(&self.id, name),
            label: name.to_string(),
            media_type: None,
            signatures: vec![],
            sniff: None,
            alias: None,
            icon: None,
            fields: vec![],
            views: vec![],
            inspector: vec![],
            editors: vec![],
            contents_views: vec![],
            contents: None,
            content_act: None,
            part_sheet: None,
            stack: None,
            data: false,
            values: vec![],
            extends: false,
        };
        TypeBuilder { registry: self, def }
    }

    /// An extractor, `<id>@<name>`, making the native files of assets of `type` (of those declaring all of `requires`; of every one
    /// where none is named) from the discs, its sessions begun by `begin`.
    pub fn extractor(
        &mut self,
        name: &str,
        node_type: &Name,
        requires: &[&str],
        begin: impl Fn(&dyn crate::Declared) -> Result<Box<dyn crate::Extraction>> + Send + Sync + 'static,
    ) -> Name {
        let def = crate::ExtractorDef {
            name: Name::of(&self.id, name),
            node_type: node_type.clone(),
            requires: requires.iter().map(|field| (*field).to_string()).collect(),
            begin: Arc::new(begin),
        };
        let name = def.name.clone();
        self.parts.extractors.push(def);
        name
    }

    /// Whether an instance of it may be reclaimed now: the host asks before it drops a long-running instance (its
    /// views' when the last closes), and keeps one that says no - a scene its next view shows, which cannot be rebuilt
    /// cheaply. An extension that keeps nothing between calls needs none.
    pub fn reclaimable(&mut self, reclaimable: impl Fn() -> bool + Send + Sync + 'static) {
        self.parts.reclaimable = Some(Arc::new(reclaimable));
    }

    /// How much memory a job of `of` may take - a type's pictures, thumbnails, parts and views, an importer's
    /// imports and exports, an extractor's sessions, a tool's runs, a panel - where its default is too little (a
    /// renderer's scene, a movie's frames). The host enforces it for each instance by the job it runs (up to 4 GiB, a
    /// 32-bit memory's most).
    pub fn memory(&mut self, of: &Name, bytes: u64) {
        self.parts.memory.push((of.clone(), bytes));
    }

    /// An operation, `<id>@<name>`: registered by `run` on what this returns. It makes `ty::Any` until it says
    /// otherwise.
    pub fn op(&mut self, name: &str) -> OpBuilder<'_> {
        let def = OpDef {
            name: Name::of(&self.id, name),
            label: name.to_string(),
            inputs: vec![],
            output: Type::Any,
            fields: vec![],
            derives: vec![],
            inverse: None,
            parameter: None,
            priority: 0,
            provides: None,
            members: None,
            also: vec![],
            run: Arc::new(|_| Err(crate::anyhow!("it does nothing"))),
            start: None,
        };
        OpBuilder { registry: self, def, same: Vec::new(), list_of: None, out: PhantomData }
    }


    /// Declare an event of this extension, for others to wire to and for it to signal.
    pub fn event<E: crate::events::Event>(&mut self) {
        self.parts.events.push((E::NAME.to_string(), E::LABEL.to_string()));
    }

    /// Run this where an event is signaled: called with the event's name as its `event` setting, seeing the config and the project as any operation does.
    pub fn on<E: crate::events::Event>(&mut self, run: impl Fn(&mut Call<'_>) -> Result<()> + Send + Sync + 'static) -> HookBuilder<'_> {
        self.on_id(E::NAME, run)
    }

    /// `on`, for an event only known by its id: a project's own, from the extension's settings.
    pub fn on_id(&mut self, event: &str, run: impl Fn(&mut Call<'_>) -> Result<()> + Send + Sync + 'static) -> HookBuilder<'_> {
        let at = self.parts.hooks.len();
        let name = format!("on-{at}");
        let op = self.op(&name).label("Hook").output::<crate::ty::Table>().run(move |call| run(call).map(|()| crate::Table::new()));
        self.parts.hooks.push(HookDef { event: event.to_string(), op: op.to_string(), signals: Vec::new() });
        HookBuilder { hook: &mut self.parts.hooks[at] }
    }

    /// Add a hook another side of the contract described.
    pub fn add_hook(&mut self, hook: HookDef) {
        self.parts.hooks.push(hook);
    }

    /// Declare an event another side of the contract described.
    pub fn add_event(&mut self, name: &str, label: &str) {
        self.parts.events.push((name.to_string(), label.to_string()));
    }

    /// A viewer of a type: a document's tab showing any output of it that is a preview.
    pub fn viewer(&mut self, ty: &Name, name: &str, icon: Icon, make: impl Fn(&Asset) -> Box<dyn View> + Send + Sync + 'static) {
        let tab = Some(TabDef { icon, fields: Vec::new() });
        self.parts.viewers.push((ty.to_string(), ViewDef { name: name.to_string(), make: Arc::new(make), tab }));
    }

    /// A viewer as a component declared it.
    pub fn viewer_def(&mut self, ty: &str, view: ViewDef) {
        self.parts.viewers.push((ty.to_string(), view));
    }

    /// A way to make a node of a type: `stack`, stamped into it, `{file}` standing for its file.
    pub fn template(&mut self, name: &str, node_type: &str, stack: &str) -> TemplateBuilder<'_> {
        let def = TemplateDef { name: name.to_string(), label: name.to_string(), node_type: node_type.to_string(), files: vec![], stack: stack.to_string() };
        TemplateBuilder { registry: self, def: Some(def) }
    }

    /// A command among the editor's: a label, and a tool of this extension it runs with no arguments. It can be bound to a
    /// shortcut like any, and agents can run it.
    pub fn command(&mut self, label: &str, tool: &str) {
        self.parts.commands.push(CommandDef { label: label.to_string(), tool: Name::of(&self.id, tool) });
    }

    /// A tool for agents, `<id>@<name>`: what it does, its arguments as a JSON schema, its work.
    pub fn tool(
        &mut self,
        name: &str,
        description: &str,
        schema: &str,
        run: impl Fn(&dyn Project, &Value) -> Result<Value> + Send + Sync + 'static,
    ) {
        let name = Name::of(&self.id, name);
        let (description, schema) = (description.to_string(), schema.to_string());
        self.parts.tools.push(ToolDef { name, description, schema, run: Arc::new(run) });
    }

    /// A service, `<id>@<name>`, other extensions call through the host (`Project::call`) - those requiring this one:
    /// what it does, its arguments as a JSON schema, its work. It runs in an instance of this extension's own.
    pub fn service(
        &mut self,
        name: &str,
        description: &str,
        schema: &str,
        run: impl Fn(&dyn Project, &Value) -> Result<Value> + Send + Sync + 'static,
    ) -> Name {
        let name = Name::of(&self.id, name);
        let (description, schema) = (description.to_string(), schema.to_string());
        self.parts.services.push(ToolDef { name: name.clone(), description, schema, run: Arc::new(run) });
        name
    }

    /// What was registered, for the host.
    #[must_use]
    pub fn into_parts(self) -> Parts {
        self.parts
    }
}

/// A way to make a node, being registered.
pub struct TemplateBuilder<'a> {
    registry: &'a mut Registry,
    def: Option<TemplateDef>,
}

impl TemplateBuilder<'_> {
    #[must_use]
    pub fn label(mut self, label: &str) -> Self {
        if let Some(def) = &mut self.def {
            def.label = label.to_string();
        }
        self
    }

    /// The files it takes, by name (`*.mp4`).
    #[must_use]
    pub fn files(mut self, patterns: &[&str]) -> Self {
        if let Some(def) = &mut self.def {
            def.files = patterns.iter().map(|pattern| (*pattern).to_string()).collect();
        }
        self
    }
}

impl Drop for TemplateBuilder<'_> {
    fn drop(&mut self) {
        if let Some(def) = self.def.take() {
            self.registry.parts.templates.push(def);
        }
    }
}

/// What is said of a type, by what registers it or by an extension adding to it (`Registry::extend`).
pub struct TypeBuilder<'a> {
    registry: &'a mut Registry,
    def: TypeDef,
}

impl TypeBuilder<'_> {
    /// A node of it holds only what is set on it: there is an operation of its name, making those settings, of this type.
    #[must_use]
    pub fn data(mut self) -> Self {
        self.def.data = true;
        self
    }

    /// A node making a value of this type, by its qualified name, is of this type.
    #[must_use]
    pub fn of(mut self, value: &str) -> Self {
        self.def.values.push(value.to_string());
        self
    }

    /// Also read where a declaration names it bare: for a built-in's types, which declarations named before names were
    /// qualified.
    #[must_use]
    pub fn alias(mut self, alias: &str) -> Self {
        self.def.alias = Some(alias.to_string());
        self
    }

    #[must_use]
    pub fn label(mut self, label: &str) -> Self {
        self.def.label = label.to_string();
        self
    }

    #[must_use]
    pub fn icon(mut self, icon: Icon, tint: Tint) -> Self {
        self.def.icon = Some((icon, tint));
        self
    }

    #[must_use]
    pub fn field(mut self, field: Field) -> Self {
        self.def.fields.push(field);
        self
    }

    #[must_use]
    pub fn view(mut self, name: &str, make: impl Fn(&Asset) -> Box<dyn View> + Send + Sync + 'static) -> Self {
        self.def.views.push(ViewDef { name: name.to_string(), make: Arc::new(make), tab: None });
        self
    }

    /// A view drawn in the inspector, after the property sheet: what the type's custom fields hold.
    #[must_use]
    pub fn inspector(mut self, name: &str, make: impl Fn(&Asset) -> Box<dyn View> + Send + Sync + 'static) -> Self {
        self.def.inspector.push(ViewDef { name: name.to_string(), make: Arc::new(make), tab: None });
        self
    }

    /// A view with a tab of its own in the inspector, shown for an asset with any of `fields` (with none, for every asset).
    #[must_use]
    pub fn inspector_tab(
        mut self,
        name: &str,
        icon: Icon,
        fields: &[&str],
        make: impl Fn(&Asset) -> Box<dyn View> + Send + Sync + 'static,
    ) -> Self {
        let tab = TabDef { icon, fields: fields.iter().map(|field| (*field).to_string()).collect() };
        self.def.inspector.push(ViewDef { name: name.to_string(), make: Arc::new(make), tab: Some(tab) });
        self
    }

    /// An editor of its own, beside the preview: what a document can show instead of it, for an asset with any of `fields`
    /// (with none, for every asset). A stage's viewport is one.
    #[must_use]
    pub fn editor(
        mut self,
        name: &str,
        icon: Icon,
        fields: &[&str],
        make: impl Fn(&Asset) -> Box<dyn View> + Send + Sync + 'static,
    ) -> Self {
        let tab = TabDef { icon, fields: fields.iter().map(|field| (*field).to_string()).collect() };
        self.def.editors.push(ViewDef { name: name.to_string(), make: Arc::new(make), tab: Some(tab) });
        self
    }

    /// An inspector view as a component declared it.
    #[must_use]
    pub fn inspector_def(mut self, view: ViewDef) -> Self {
        self.def.inspector.push(view);
        self
    }

    /// An editor as a component declared it.
    #[must_use]
    pub fn editor_def(mut self, view: ViewDef) -> Self {
        self.def.editors.push(view);
        self
    }

    /// A view at the head of the contents pane, over its rows: for what rows cannot say. With no rows, the pane is its own.
    #[must_use]
    pub fn contents_view(
        mut self,
        name: &str,
        make: impl Fn(&Asset) -> Box<dyn View> + Send + Sync + 'static,
    ) -> Self {
        self.def.contents_views.push(ViewDef { name: name.to_string(), make: Arc::new(make), tab: None });
        self
    }

    /// A contents view as a component declared it.
    #[must_use]
    pub fn contents_view_def(mut self, view: ViewDef) -> Self {
        self.def.contents_views.push(view);
        self
    }

    /// The stack a node of the type has where its bundle states none, as `[[stack]]` tables: extraction writes it.
    #[must_use]
    pub fn stack(mut self, toml: &str) -> Self {
        self.def.stack = Some(toml.to_string());
        self
    }

    /// What is inside its assets, for the contents pane.
    #[must_use]
    pub fn contents(mut self, contents: impl Fn(&Asset, &dyn crate::Project) -> Contents + Send + Sync + 'static) -> Self {
        self.def.contents = Some(Arc::new(contents));
        self
    }

    /// What comes of what is done to its contents: a row's action, a row renamed or dragged.
    #[must_use]
    pub fn content_act(mut self, act: impl Fn(&Asset, &dyn crate::Project, ContentRequest) -> Result<ContentOutcome> + Send + Sync + 'static) -> Self {
        self.def.content_act = Some(Arc::new(act));
        self
    }

    /// Its parts' fields, where those are not simply what their tables hold.
    #[must_use]
    pub fn part_sheet(mut self, sheet: impl Fn(&Asset, &dyn crate::Project, &str) -> Option<PartSheet> + Send + Sync + 'static) -> Self {
        self.def.part_sheet = Some(Arc::new(sheet));
        self
    }

    #[must_use]
    pub fn media_type(mut self, media_type: &str) -> Self {
        self.def.media_type = Some(media_type.to_string());
        self
    }

    /// Recognized by these bytes at this offset.
    #[must_use]
    pub fn bytes(mut self, offset: usize, bytes: &[u8]) -> Self {
        self.def.signatures.push(Signature::Bytes { offset, bytes: bytes.to_vec() });
        self
    }

    /// Recognized by its file name: exact, or a suffix after `*`.
    #[must_use]
    pub fn named(mut self, name: &str) -> Self {
        self.def.signatures.push(Signature::Name(name.to_string()));
        self
    }

    /// Recognized as text: UTF-8 without a NUL byte.
    #[must_use]
    pub fn text(mut self) -> Self {
        self.def.signatures.push(Signature::Text);
        self
    }

    /// Recognized, where its signatures leave it open, by its name, its size and its first bytes.
    #[must_use]
    pub fn sniff(mut self, sniff: impl Fn(&str, u64, &[u8]) -> Option<Recognized> + Send + Sync + 'static) -> Self {
        self.def.sniff = Some(Arc::new(sniff));
        self
    }

    /// Register it; its name, which ports take it by.
    #[expect(clippy::must_use_candidate, reason = "a type no operation of its extension names needs no name")]
    pub fn register(self) -> Name {
        let name = self.def.name.clone();
        self.registry.parts.types.push(self.def);
        name
    }
}

/// An operation being registered: `Out` the type it makes (`ty::Any` until `output` says).
pub struct OpBuilder<'a, Out = crate::ty::Any> {
    registry: &'a mut Registry,
    def: OpDef,
    /// Ports and its output (`None`) that are one type, whatever it is: the constraints.
    same: Vec<Vec<Option<String>>>,
    /// Its output is a list of what this port takes.
    list_of: Option<String>,
    out: PhantomData<Out>,
}

impl<'a, Out: ValueType> OpBuilder<'a, Out> {
    #[must_use]
    pub fn label(mut self, label: &str) -> Self {
        self.def.label = label.to_string();
        self
    }

    /// A port taking one value of a type (`.input::<Indexed>("native")`).
    #[must_use]
    pub fn input<T: ValueType>(mut self, name: &str) -> Self {
        self.def.inputs.push(Port { name: name.to_string(), ty: T::ty(), many: false });
        self
    }

    /// Another output, by its name, after the first: made with `Call::make` as it runs.
    #[must_use]
    pub fn also<T: ValueType>(mut self, name: &str) -> Self {
        self.def.also.push((name.to_string(), T::ty()));
        self
    }

    /// Another output as described where types cross as text.
    #[must_use]
    pub fn also_described(mut self, name: &str, ty: &str) -> Self {
        let ty = self.parse(ty);
        self.def.also.push((name.to_string(), ty));
        self
    }

    /// A required input taking any of these types, by their qualified names.
    #[must_use]
    pub fn takes(mut self, name: &str, types: &[&str]) -> Self {
        let mut any: Vec<Type> = types.iter().map(|ty| Type::Named((*ty).to_string())).collect();
        let ty = if any.len() == 1 { any.remove(0) } else { Type::Union(any) };
        self.def.inputs.push(Port { name: name.to_string(), ty, many: false });
        self
    }

    /// A conversion: whoever asks a node making what this takes for this name has this make it, transient unless `keeps`
    /// says otherwise. What a node itself offers is no conversion: it is in its stack.
    #[must_use]
    pub fn provides(mut self, name: &str) -> Self {
        self.def.provides = Some(Provides { name: name.to_string(), retain: Retain::Transient });
        self
    }

    /// How what it provides is kept.
    #[must_use]
    pub fn keeps(mut self, retain: Retain) -> Self {
        if let Some(provides) = &mut self.def.provides {
            provides.retain = retain;
        }
        self
    }

    /// The members of the family it makes: each by a value of its parameter, with a label.
    #[must_use]
    pub fn members<P: crate::ty::Parameter>(self, members: impl Fn(&Asset) -> Vec<(P, String)> + Send + Sync + 'static) -> Self {
        self.members_described(move |asset| members(asset).into_iter().map(|(p, label)| (p.to_text(), label)).collect())
    }

    /// The members of a family as described where values cross as text.
    #[must_use]
    pub fn members_described(mut self, members: impl Fn(&Asset) -> Vec<(String, String)> + Send + Sync + 'static) -> Self {
        self.def.members = Some(Arc::new(members));
        self
    }

    /// What it offers as a component declared it.
    #[must_use]
    pub fn provides_def(mut self, provides: Provides) -> Self {
        self.def.provides = Some(provides);
        self
    }

    /// A port taking any number of values of a type, in order.
    #[must_use]
    pub fn inputs<T: ValueType>(mut self, name: &str) -> Self {
        self.def.inputs.push(Port { name: name.to_string(), ty: T::ty(), many: true });
        self
    }

    /// A port as described where types cross as text (a component's registration): its type, parsed.
    #[must_use]
    pub fn port(mut self, name: &str, ty: &str, many: bool) -> Self {
        let ty = self.parse(ty);
        self.def.inputs.push(Port { name: name.to_string(), ty, many });
        self
    }

    /// What it makes, as described where types cross as text.
    #[must_use]
    pub fn output_described(mut self, ty: &str) -> Self {
        self.def.output = self.parse(ty);
        self
    }

    fn parse(&mut self, ty: &str) -> Type {
        Type::parse(ty, &self.registry.id).unwrap_or_else(|e| {
            self.registry.parts.problems.push(format!("{}: {e}", self.def.name));
            Type::Any
        })
    }

    /// What it makes (`.output::<ty::Image>()`): what `run` returns a value of.
    #[must_use]
    pub fn output<T: ValueType>(self) -> OpBuilder<'a, T> {
        let OpBuilder { registry, mut def, same, list_of, .. } = self;
        def.output = T::ty();
        OpBuilder { registry, def, same, list_of, out: PhantomData }
    }

    /// Two ports taking `Any` take one type, whatever it is: given a texture first, the other takes a texture.
    #[must_use]
    pub fn same(self, a: &str, b: &str) -> Self {
        self.join(Some(a), Some(b))
    }

    /// What it makes is of the type a port taking `Any` is given.
    #[must_use]
    pub fn output_same_as(self, port: &str) -> Self {
        self.join(None, Some(port))
    }

    /// What it makes is a list of what a port taking `Any` is given.
    #[must_use]
    pub fn output_list_of(mut self, port: &str) -> Self {
        self.list_of = Some(port.to_string());
        self.join(Some(port), Some(port))
    }

    fn join(mut self, a: Option<&str>, b: Option<&str>) -> Self {
        let (a, b) = (a.map(str::to_string), b.map(str::to_string));
        let find = |same: &Vec<Vec<Option<String>>>, s: &Option<String>| same.iter().position(|g| g.contains(s));
        match (find(&self.same, &a), find(&self.same, &b)) {
            (Some(i), Some(j)) if i != j => {
                let moved = self.same.remove(j.max(i));
                self.same[j.min(i)].extend(moved);
            }
            (Some(_), Some(_)) => {}
            (Some(i), None) => self.same[i].push(b),
            (None, Some(j)) => self.same[j].push(a),
            (None, None) => self.same.push(if a == b { vec![a] } else { vec![a, b] }),
        }
        self
    }

    /// The constraints as the host checks them: each group one variable, its ports and output taking it.
    fn constrain(&mut self) {
        let name = self.def.name.clone();
        for (i, group) in self.same.iter().enumerate() {
            let var = Type::Var(char::from(b'A' + u8::try_from(i % 26).unwrap_or(0)).to_string());
            for slot in group {
                let ty = match slot {
                    Some(port) => self.def.inputs.iter_mut().find(|p| p.name == *port).map(|p| &mut p.ty),
                    None => Some(&mut self.def.output),
                };
                match ty {
                    Some(ty) if *ty == Type::Any => *ty = var.clone(),
                    Some(_) => self.registry.parts.problems.push(format!("{name}: a constraint relates a port of a type")),
                    None => self.registry.parts.problems.push(format!("{name}: a constraint names no port {slot:?}")),
                }
            }
            if self.list_of.as_ref().is_some_and(|p| group.contains(&Some(p.clone()))) {
                self.def.output = Type::List(Box::new(var.clone()));
            }
        }
    }

    /// A setting.
    #[must_use]
    pub fn field(mut self, field: Field) -> Self {
        self.def.fields.push(field);
        self
    }

    /// A setting of its node it derives (`size`, `cells.*.rect`): what it makes says it, in the declaration's place.
    #[must_use]
    pub fn derives(mut self, path: &str) -> Self {
        self.def.derives.push(path.to_string());
        self
    }

    /// The operation undoing it: what writes its input from what it makes, as extraction writes a source from the
    /// disc's native form.
    #[must_use]
    pub fn inverse(mut self, op: &Name) -> Self {
        self.def.inverse = Some(op.to_string());
        self
    }

    /// The operation undoing it, as described where names cross as text (qualified).
    #[must_use]
    pub fn inverse_described(mut self, op: &str) -> Self {
        self.def.inverse = Some(op.to_string());
        self
    }

    /// Which of several operations taking a source is offered first: the highest.
    #[must_use]
    pub fn priority(mut self, priority: i32) -> Self {
        self.def.priority = priority;
        self
    }

    /// Its parameter, of a type (`.parameter::<String>("look")`): what reaches it is a family, each member addressed
    /// by a value of it, which `Call::parameter` reads.
    #[must_use]
    pub fn parameter<P: crate::ty::Parameter>(mut self, name: &str) -> Self {
        self.def.parameter = Some((name.to_string(), P::KIND.to_string()));
        self
    }

    /// Its parameter as described where it crosses as text: its name and the type of value it takes.
    #[must_use]
    pub fn parameter_described(mut self, name: &str, node_type: &str) -> Self {
        self.def.parameter = Some((name.to_string(), node_type.to_string()));
        self
    }

    /// What it does - given its call, a value of what it makes - registered with it; its name, which a type's
    /// outputs and an inverse name it by.
    pub fn run(mut self, run: impl Fn(&mut Call<'_>) -> Result<Out::Value> + Send + Sync + 'static) -> Name {
        self.constrain();
        let also: Vec<String> = self.def.also.iter().map(|(name, _)| name.clone()).collect();
        self.def.run = Arc::new(move |call| {
            let first = run(call).map(Out::encode)?;
            if also.is_empty() {
                return Ok(first);
            }
            let mut made = call.made.take();
            let mut rest = Vec::with_capacity(also.len() + 1);
            rest.push(first);
            for name in &also {
                rest.push(made.remove(name).ok_or_else(|| crate::anyhow!("it made no {name}"))?);
            }
            Ok(crate::Data::List(rest))
        });
        let name = self.def.name.clone();
        self.registry.parts.ops.push(self.def);
        name
    }

    /// Register what a component runs: it makes what it makes, several outputs already a list of them.
    pub fn run_described(mut self, run: impl Fn(&mut Call<'_>) -> Result<crate::Data> + Send + Sync + 'static) -> Name {
        self.constrain();
        self.def.run = Arc::new(run);
        let name = self.def.name.clone();
        self.registry.parts.ops.push(self.def);
        name
    }

    /// Register it as a stream operation: it makes a stream of `Out`'s values, opened by starting the generator
    /// `start` makes (`Generator`); its items are encoded as `Out`'s.
    pub fn streams(
        mut self,
        start: impl Fn(&mut Call<'_>) -> Result<Box<dyn crate::Generator>> + Send + Sync + 'static,
    ) -> Name {
        self.constrain();
        self.def.output = Type::Stream(Box::new(self.def.output.clone()));
        self.def.run = Arc::new(|_| Err(crate::anyhow!("a stream is opened, not made")));
        self.def.start = Some(Arc::new(start));
        let name = self.def.name.clone();
        self.registry.parts.ops.push(self.def);
        name
    }

    /// Register it as a stream operation as `streams` does, its output's type as text (where types cross as text).
    pub fn streams_described(
        mut self,
        start: impl Fn(&mut Call<'_>) -> Result<Box<dyn crate::Generator>> + Send + Sync + 'static,
    ) -> Name {
        self.constrain();
        self.def.run = Arc::new(|_| Err(crate::anyhow!("a stream is opened, not made")));
        self.def.start = Some(Arc::new(start));
        let name = self.def.name.clone();
        self.registry.parts.ops.push(self.def);
        name
    }
}
