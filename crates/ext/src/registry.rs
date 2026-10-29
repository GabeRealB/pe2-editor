//! Registration: an extension's entry point and what it registers.

use std::fmt;
use std::sync::Arc;

use crate::import::{FormatDef, ImporterDef, Recognized, Signature, Support};
use crate::{Asset, Image, Project, Result, Table, Value, View};

/// An extension: its id, its version, the extensions it needs and its entry point. `extension!` makes it.
pub struct Extension {
    pub id: &'static str,
    /// Its version, as semver: `0.1.0`.
    pub version: &'static str,
    /// The extensions it needs, by id and the versions it accepts: `pe2.images@^0.1` (a version alone is a caret
    /// requirement; an id alone takes any). It may extend only kinds these register.
    pub requires: &'static [&'static str],
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
    ($id:literal, $version:literal, $register:path $(, requires = [$($requires:literal),* $(,)?])? $(,)?) => {
        /// This extension's id.
        pub const ID: &str = $id;

        /// This extension, for the host to load.
        #[must_use]
        pub fn extension() -> $crate::Extension {
            $crate::Extension { id: $id, version: $version, requires: &[$($($requires),*)?], register: $register }
        }

        /// This extension, for the SDK's component exports.
        #[cfg(target_family = "wasm")]
        #[unsafe(no_mangle)]
        fn __pe2_extension() -> $crate::Extension {
            extension()
        }

        /// Who it is, as its component carries it in the section the host reads without running it
        /// (`pe2-extension`): its id, version and requirements as TOML.
        #[cfg(target_family = "wasm")]
        const __PE2_ABOUT: &str = concat!(
            "id = \"", $id, "\"\nversion = \"", $version, "\"\nrequires = [", $($("\"", $requires, "\", ",)*)? "]\n"
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

/// An icon of the host's set, for a kind in the tree and the inspector.
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

/// A field of a kind or a role, as the inspector shows it: its label and how its value reads.
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
    /// Drawn by its kind's inspector view, not by the host's property sheet.
    pub custom: bool,
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
    pub fn read_only(mut self) -> Field {
        self.read_only = true;
        self
    }

    /// Drawn by the kind's inspector view instead of the host's sheet.
    #[must_use]
    pub fn custom(mut self) -> Field {
        self.custom = true;
        self
    }
}

/// What an asset looks like as an image, in each of its looks: what the host shows it by wherever it draws it (the
/// inspector's header, VRAM, a container's preview, an agent's request).
pub trait Picture: Send + Sync {
    /// Whether the asset has a picture.
    fn has(&self, asset: &Asset) -> bool;
    /// Its looks, by name; empty or one where it has a single look. The looks are its declaration's `variants`: the
    /// host's tree asks only an asset declaring some, as it knows which do without parsing their declarations.
    fn looks(&self, asset: &Asset) -> Vec<String>;
    /// Its picture in a look (an index into `looks`; a look past them is its first).
    ///
    /// # Errors
    /// Its files cannot be read or decoded.
    fn draw(&self, asset: &Asset, look: usize) -> Result<Image>;
    /// A part of it as a picture, by the part's path (an image's `palettes/<name>`, as a strip of its colors); none
    /// where it has no such part.
    fn part(&self, asset: &Asset, part: &str) -> Option<Result<Image>> {
        let _ = (asset, part);
        None
    }
}

/// An asset's thumbnail, at most `side` pixels on its longer side; none where it has none. The host keeps it as an
/// artifact, under a key of the extension's version and the asset's files.
pub type Thumbnail = dyn Fn(&Asset, usize) -> Option<Result<Image>> + Send + Sync;

/// Makes a view of an asset.
pub type MakeView = dyn Fn(&Asset) -> Box<dyn View> + Send + Sync;

/// Makes a panel's view.
pub type MakePanel = dyn Fn() -> Box<dyn View> + Send + Sync;

/// What an asset is, for an agent: facts its kind adds to its description (an image's palettes, its cells).
pub type Facts = dyn Fn(&Asset) -> Table + Send + Sync;

/// A part of an asset: what a reference names by fragment (`uid://...#palettes/default`), an output of its import or
/// one its settings define.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    /// Its id within the asset, its fragment: `palettes/default`, `cells/cursor`, `variants/night`.
    pub id: String,
    pub label: String,
    /// The parts it is made of, where it is virtual - files of its own it has none, its settings link them (an
    /// image's variant: the texels through a palette); empty for a part of its own.
    pub made_of: Vec<String>,
}

/// An asset's parts, in order.
pub type ListParts = dyn Fn(&Asset) -> Vec<Part> + Send + Sync;

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
    pub kinds: Vec<KindDef>,
    pub roles: Vec<RoleDef>,
    pub panels: Vec<PanelDef>,
    pub tools: Vec<ToolDef>,
    /// Services: like tools, but for other extensions to call.
    pub services: Vec<ToolDef>,
    pub formats: Vec<FormatDef>,
    pub importers: Vec<ImporterDef>,
    pub extractors: Vec<crate::ExtractorDef>,
    /// Whether its instance may be reclaimed now; without, always.
    pub reclaimable: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    /// The memory a job of something it registered may take, where more than the host's default, by its name.
    pub memory: Vec<(Name, u64)>,
    /// Its settings, as declared, each with the value it took.
    pub settings: Vec<(Setting, Value)>,
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
}

/// A kind as registered.
#[derive(Clone)]
pub struct KindDef {
    pub name: Name,
    /// The bare name declarations wrote before names were qualified (`image`): a built-in's alone.
    pub alias: Option<String>,
    pub label: String,
    pub icon: Option<(Icon, Tint)>,
    pub fields: Vec<Field>,
    pub picture: Option<Arc<dyn Picture>>,
    pub thumbnail: Option<Arc<Thumbnail>>,
    pub views: Vec<ViewDef>,
    /// Views drawn in the inspector, after the host's property sheet: what its custom fields hold.
    pub inspector: Vec<ViewDef>,
    pub facts: Option<Arc<Facts>>,
    pub parts: Option<Arc<ListParts>>,
}

/// A role as registered, on a kind of its extension's or of one it requires.
#[derive(Clone)]
pub struct RoleDef {
    pub name: Name,
    pub kind: Name,
    pub alias: Option<String>,
    pub label: String,
    /// Fields it adds to its kind's, or shows otherwise.
    pub fields: Vec<Field>,
    /// Views it adds to its kind's.
    pub views: Vec<ViewDef>,
    /// Inspector views it adds to its kind's.
    pub inspector: Vec<ViewDef>,
}

/// What an extension registers, as its entry point calls it. The host checks the outcome: a role's kind must be its
/// extension's or a dependency's, an alias a built-in's, a name registered once.
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

    /// A kind, `<id>@<name>`: registered by `register` on what this returns.
    pub fn kind(&mut self, name: &str) -> KindBuilder<'_> {
        let def = KindDef {
            name: Name::of(&self.id, name),
            alias: None,
            label: name.to_string(),
            icon: None,
            fields: vec![],
            picture: None,
            thumbnail: None,
            views: vec![],
            inspector: vec![],
            facts: None,
            parts: None,
        };
        KindBuilder { registry: self, def }
    }

    /// A role on `kind`, `<id>@<name>`: registered by `register` on what this returns.
    pub fn role(&mut self, kind: &Name, name: &str) -> RoleBuilder<'_> {
        let def = RoleDef {
            name: Name::of(&self.id, name),
            kind: kind.clone(),
            alias: None,
            label: name.to_string(),
            fields: vec![],
            views: vec![],
            inspector: vec![],
        };
        RoleBuilder { registry: self, def }
    }

    /// A panel, `<id>@<name>`, labeled in the View menu and its tab.
    pub fn panel(&mut self, name: &str, label: &str, icon: Icon, make: impl Fn() -> Box<dyn View> + Send + Sync + 'static) {
        let name = Name::of(&self.id, name);
        self.parts.panels.push(PanelDef { name, label: label.to_string(), icon: Some(icon), make: Arc::new(make) });
    }

    /// A format, `<id>@<name>`: registered by `register` on what this returns.
    pub fn format(&mut self, name: &str) -> FormatBuilder<'_> {
        let def = FormatDef {
            name: Name::of(&self.id, name),
            label: name.to_string(),
            media_type: None,
            signatures: vec![],
            sniff: None,
        };
        FormatBuilder { registry: self, def }
    }

    /// An extractor, `<id>@<name>`, making the native files of assets of `kind` (of `roles`, qualified; of every role
    /// where none is named) from the discs, its sessions begun by `begin`.
    pub fn extractor(
        &mut self,
        name: &str,
        kind: &Name,
        roles: &[Name],
        begin: impl Fn(&dyn crate::Declared) -> Result<Box<dyn crate::Extraction>> + Send + Sync + 'static,
    ) -> Name {
        let def = crate::ExtractorDef {
            name: Name::of(&self.id, name),
            kind: kind.clone(),
            roles: roles.to_vec(),
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

    /// How much memory a job of `of` may take - a kind's pictures, thumbnails, parts and views, an importer's
    /// imports and exports, an extractor's sessions, a tool's runs, a panel - where its default is too little (a
    /// renderer's scene, a movie's frames). The host enforces it for each instance by the job it runs (up to 4 GiB, a
    /// 32-bit memory's most).
    pub fn memory(&mut self, of: &Name, bytes: u64) {
        self.parts.memory.push((of.clone(), bytes));
    }

    /// An importer, `<id>@<name>`, making assets of `kind`: registered by `register` on what this returns.
    pub fn importer(&mut self, name: &str, kind: &Name) -> ImporterBuilder<'_> {
        let def = ImporterDef {
            name: Name::of(&self.id, name),
            label: name.to_string(),
            reads: vec![],
            makes: kind.clone(),
            native: false,
            fields: vec![],
            inspector: vec![],
            check: None,
            import: None,
            derives: vec![],
            export: None,
            priority: 0,
        };
        ImporterBuilder { registry: self, def }
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

pub struct KindBuilder<'a> {
    registry: &'a mut Registry,
    def: KindDef,
}

impl KindBuilder<'_> {
    /// Also read where a declaration names it bare: for a built-in's kinds, which declarations named before names were
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
    pub fn picture(mut self, picture: impl Picture + 'static) -> Self {
        self.def.picture = Some(Arc::new(picture));
        self
    }

    #[must_use]
    pub fn thumbnail(mut self, thumbnail: impl Fn(&Asset, usize) -> Option<Result<Image>> + Send + Sync + 'static) -> Self {
        self.def.thumbnail = Some(Arc::new(thumbnail));
        self
    }

    #[must_use]
    pub fn view(mut self, name: &str, make: impl Fn(&Asset) -> Box<dyn View> + Send + Sync + 'static) -> Self {
        self.def.views.push(ViewDef { name: name.to_string(), make: Arc::new(make) });
        self
    }

    /// A view drawn in the inspector, after the property sheet: what the kind's custom fields hold.
    #[must_use]
    pub fn inspector(mut self, name: &str, make: impl Fn(&Asset) -> Box<dyn View> + Send + Sync + 'static) -> Self {
        self.def.inspector.push(ViewDef { name: name.to_string(), make: Arc::new(make) });
        self
    }

    /// What an agent is told of an asset of the kind besides its declaration.
    #[must_use]
    pub fn facts(mut self, facts: impl Fn(&Asset) -> Table + Send + Sync + 'static) -> Self {
        self.def.facts = Some(Arc::new(facts));
        self
    }

    /// An asset's parts: what references name by fragment.
    #[must_use]
    pub fn parts(mut self, parts: impl Fn(&Asset) -> Vec<Part> + Send + Sync + 'static) -> Self {
        self.def.parts = Some(Arc::new(parts));
        self
    }

    /// Register it; its name, which roles name it by.
    #[expect(clippy::must_use_candidate, reason = "a kind without roles needs no name")]
    pub fn register(self) -> Name {
        let name = self.def.name.clone();
        self.registry.parts.kinds.push(self.def);
        name
    }
}

pub struct RoleBuilder<'a> {
    registry: &'a mut Registry,
    def: RoleDef,
}

impl RoleBuilder<'_> {
    /// Also read where a declaration names it bare (a built-in's alone).
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
    pub fn field(mut self, field: Field) -> Self {
        self.def.fields.push(field);
        self
    }

    #[must_use]
    pub fn view(mut self, name: &str, make: impl Fn(&Asset) -> Box<dyn View> + Send + Sync + 'static) -> Self {
        self.def.views.push(ViewDef { name: name.to_string(), make: Arc::new(make) });
        self
    }

    /// A view drawn in the inspector, after its kind's.
    #[must_use]
    pub fn inspector(mut self, name: &str, make: impl Fn(&Asset) -> Box<dyn View> + Send + Sync + 'static) -> Self {
        self.def.inspector.push(ViewDef { name: name.to_string(), make: Arc::new(make) });
        self
    }

    /// Register it; its name.
    #[expect(clippy::must_use_candidate, reason = "a role's name is seldom needed")]
    pub fn register(self) -> Name {
        let name = self.def.name.clone();
        self.registry.parts.roles.push(self.def);
        name
    }
}

pub struct FormatBuilder<'a> {
    registry: &'a mut Registry,
    def: FormatDef,
}

impl FormatBuilder<'_> {
    #[must_use]
    pub fn label(mut self, label: &str) -> Self {
        self.def.label = label.to_string();
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

    /// Register it; its name, which importers read it by.
    #[expect(clippy::must_use_candidate, reason = "a format no importer of its extension reads needs no name")]
    pub fn register(self) -> Name {
        let name = self.def.name.clone();
        self.registry.parts.formats.push(self.def);
        name
    }
}

pub struct ImporterBuilder<'a> {
    registry: &'a mut Registry,
    def: ImporterDef,
}

impl ImporterBuilder<'_> {
    #[must_use]
    pub fn label(mut self, label: &str) -> Self {
        self.def.label = label.to_string();
        self
    }

    /// It reads this format.
    #[must_use]
    pub fn reads(mut self, format: &Name) -> Self {
        self.def.reads.push(format.to_string());
        self
    }

    /// It reads any format of this media type.
    #[must_use]
    pub fn reads_media(mut self, media_type: &str) -> Self {
        self.def.reads.push(media_type.to_string());
        self
    }

    /// It makes nothing: its outputs are its inputs, its settings how to read them.
    #[must_use]
    pub fn native(mut self) -> Self {
        self.def.native = true;
        self
    }

    /// A setting.
    #[must_use]
    pub fn field(mut self, field: Field) -> Self {
        self.def.fields.push(field);
        self
    }

    /// A view drawn with its settings in the properties pane: what its custom settings hold.
    #[must_use]
    pub fn inspector(mut self, name: &str, make: impl Fn(&Asset) -> Box<dyn View> + Send + Sync + 'static) -> Self {
        self.def.inspector.push(ViewDef { name: name.to_string(), make: Arc::new(make) });
        self
    }

    /// How far it takes a source of its formats; without a check, fully.
    #[must_use]
    pub fn check(mut self, check: impl Fn(&Asset) -> Support + Send + Sync + 'static) -> Self {
        self.def.check = Some(Arc::new(check));
        self
    }

    /// What it makes of a source: the native files, by name, and the settings it derives.
    #[must_use]
    pub fn import(mut self, import: impl Fn(&Asset) -> Result<crate::Imported> + Send + Sync + 'static) -> Self {
        self.def.import = Some(Arc::new(import));
        self
    }

    /// A kind setting it derives from the source (`size`, `cells.*.rect`).
    #[must_use]
    pub fn derives(mut self, path: &str) -> Self {
        self.def.derives.push(path.to_string());
        self
    }

    /// How it writes its source from the native form, which importing makes again.
    #[must_use]
    pub fn export(mut self, export: impl Fn(&Asset) -> Result<Vec<u8>> + Send + Sync + 'static) -> Self {
        self.def.export = Some(Arc::new(export));
        self
    }

    #[must_use]
    pub fn priority(mut self, priority: i32) -> Self {
        self.def.priority = priority;
        self
    }

    #[expect(clippy::must_use_candidate, reason = "an importer's name is seldom needed")]
    pub fn register(self) -> Name {
        let name = self.def.name.clone();
        self.registry.parts.importers.push(self.def);
        name
    }
}

