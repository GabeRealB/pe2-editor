//! Registration: an extension's entry point and what it registers.

use std::fmt;
use std::sync::Arc;

use crate::{Asset, Image, Result, Value, View};

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
/// id, and `extension()`, which the host loads it by.
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
    };
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
}

impl Field {
    #[must_use]
    pub fn new(key: &str) -> Field {
        Field { key: key.to_string(), label: None, format: None, brief: None, choices: vec![], read_only: false }
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
}

/// An asset's thumbnail, at most `side` pixels on its longer side; none where it has none. The host keeps it as an
/// artifact, under a key of the extension's version and the asset's files.
pub type Thumbnail = dyn Fn(&Asset, usize) -> Option<Result<Image>> + Send + Sync;

/// Makes a view of an asset.
pub type MakeView = dyn Fn(&Asset) -> Box<dyn View> + Send + Sync;

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
}

/// What an extension registers, as its entry point calls it. The host checks the outcome: a role's kind must be its
/// extension's or a dependency's, an alias a built-in's, a name registered once.
pub struct Registry {
    id: String,
    kinds: Vec<KindDef>,
    roles: Vec<RoleDef>,
}

impl Registry {
    /// A registry for the extension `id`, as the host makes it.
    #[must_use]
    pub fn new(id: &str) -> Registry {
        Registry { id: id.to_string(), kinds: vec![], roles: vec![] }
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
        };
        RoleBuilder { registry: self, def }
    }

    /// What was registered, for the host.
    #[must_use]
    pub fn into_parts(self) -> (Vec<KindDef>, Vec<RoleDef>) {
        (self.kinds, self.roles)
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

    /// Register it; its name, which roles name it by.
    #[expect(clippy::must_use_candidate, reason = "a kind without roles needs no name")]
    pub fn register(self) -> Name {
        let name = self.def.name.clone();
        self.registry.kinds.push(self.def);
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

    /// Register it; its name.
    #[expect(clippy::must_use_candidate, reason = "a role's name is seldom needed")]
    pub fn register(self) -> Name {
        let name = self.def.name.clone();
        self.registry.roles.push(self.def);
        name
    }
}
