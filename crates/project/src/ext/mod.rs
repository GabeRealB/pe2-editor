//! The extension host: loads the extensions, registers what they register, and answers what a kind has - its icon,
//! fields, picture, thumbnail, views - for the editor, `pe2-cli` and the agents' server alike.
//!
//! Loading checks what the SDK cannot: one extension an id (a second is refused), each one's requirements met by the
//! version loaded (`pe2.images@^0.1`), registration in dependency order, cycles refused; an extension whose
//! requirement is missing, too old or itself disabled is disabled, and so are its dependents, each with the reason. What
//! one registers is checked as well: a role only on its own kinds or its dependencies', an alias only from a built-in,
//! a name only once. An extension is a component the host runs (`component`), or a crate linked into the host whose
//! entry point it calls - the built-ins' fallback while developing.

pub mod component;

use anyhow::{Context, Result};
use pe2_ext::{
    Asset, Extension, Field, FormatDef, Icon, Image, ImporterDef, KindDef, PanelDef, Project, Recognized, Registry, RoleDef,
    Signature, Source, Support, Table, Tint, ToolDef, Value, ViewDef,
};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::path::PathBuf;

use crate::{graph, index};

/// The kind of an asset holding an extension: the host's own, registered by none. Its bundle holds the component
/// (`extension.wasm`), which says who it is.
pub const EXTENSION_KIND: &str = "extension";

/// The file an `extension` asset holds its component in.
pub const EXTENSION_COMPONENT: &str = "extension.wasm";

/// Where an extension runs from.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Origin {
    /// A component built into the host: a built-in.
    Builtin,
    /// A component, from this file.
    Component(PathBuf),
    /// A component the project holds: an `extension` asset, by its path, and its file.
    Project { asset: String, file: PathBuf },
    /// A component in the user's own extensions folder.
    User(PathBuf),
    /// Linked into the host, its entry point called directly: a built-in, for debugging.
    Linked,
}

/// How an extension registers: its entry point, or what its component registered.
type Register = Box<dyn FnOnce(&mut Registry) -> Result<()>>;

/// An extension to load: who it is, what it requires, and how it registers.
pub struct Loadable {
    pub id: String,
    pub version: String,
    pub requires: Vec<String>,
    pub origin: Origin,
    register: Option<Register>,
    /// Its component, where it runs as one - set when it registers: its instances, their memory.
    component: Option<Arc<std::sync::OnceLock<Arc<component::Shared>>>>,
    /// Its component's size in bytes, where it is one.
    pub size: Option<usize>,
    /// The contract its component was built against (`0.2.0`); none linked in.
    pub contract: Option<String>,
    /// The settings kept for it, by scope (`{ project = { ... }, user = { ... } }`), which it reads as it registers.
    pub settings: toml::Table,
}

impl From<Extension> for Loadable {
    fn from(extension: Extension) -> Loadable {
        Loadable {
            id: extension.id.to_string(),
            version: extension.version.to_string(),
            requires: extension.requires.iter().map(ToString::to_string).collect(),
            origin: Origin::Linked,
            register: Some(Box::new(move |registry| (extension.register)(registry))),
            component: None,
            size: None,
            contract: None,
            settings: toml::Table::new(),
        }
    }
}

/// A loaded extension.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Loaded {
    pub id: String,
    pub version: String,
    pub origin: Origin,
    /// What it requires (`pe2.images@^0.1`): whose services it may call.
    pub requires: Vec<String>,
}

/// An extension present, loaded or not: who it is, what became of it, what it registered.
#[derive(Clone, Debug)]
pub struct Present {
    pub id: String,
    pub version: String,
    pub origin: Origin,
    pub requires: Vec<String>,
    pub status: Status,
    /// Its component's size in bytes, where it is one.
    pub size: Option<usize>,
    /// The contract its component was built against; none linked in, or unreadable.
    pub contract: Option<String>,
    /// How long registering it took (compiling and instantiating a component), where it was registered.
    pub load: Option<std::time::Duration>,
    /// What it registered, by what it is - kinds, roles, formats, importers, extractors, panels, tools, services -
    /// each name qualified.
    pub registers: Registers,
}

/// What an extension registered, by what it is (`kinds`, `services`, ...), each name qualified.
pub type Registers = Vec<(&'static str, Vec<String>)>;

impl Present {
    fn of(extension: &Loadable, status: Status) -> Present {
        Present {
            id: extension.id.clone(),
            version: extension.version.clone(),
            origin: extension.origin.clone(),
            requires: extension.requires.clone(),
            status,
            size: extension.size,
            contract: extension.contract.clone(),
            load: None,
            registers: vec![],
        }
    }
}

/// What became of an extension present.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Loaded,
    /// Left off by the project (`[extensions] disabled`).
    Disabled,
    /// Not loaded, and why: a requirement missing, disabled or too old, a cycle, its registration failing, another of
    /// its id from the same place.
    Refused(String),
    /// Not loaded: one of its id from a nearer place (the project before the user's folder before the built-ins) takes
    /// its place, named.
    Replaced(String),
}

impl Origin {
    /// How near a project it is: the project's own first, then the user's, then the built-ins (and a file named).
    fn nearness(&self) -> u8 {
        match self {
            Origin::Project { .. } => 0,
            Origin::User(_) => 1,
            Origin::Component(_) => 2,
            Origin::Builtin | Origin::Linked => 3,
        }
    }

    /// It runs with the host: a built-in, as a component or linked.
    #[must_use]
    pub fn builtin(&self) -> bool {
        matches!(self, Origin::Builtin | Origin::Linked)
    }

    /// Where it is from, said in a few words.
    #[must_use]
    pub fn said(&self) -> String {
        let file = |path: &std::path::Path| path.file_name().map_or_default(|n| n.to_string_lossy().into_owned());
        match self {
            Origin::Builtin => "built in".into(),
            Origin::Linked => "linked in".into(),
            Origin::Project { asset, .. } => format!("the project's {asset}"),
            Origin::User(path) => format!("your {}", file(path)),
            Origin::Component(path) => file(path),
        }
    }
}

/// The extensions loaded, and what they registered.
#[derive(Default)]
pub struct Extensions {
    pub loaded: Vec<Loaded>,
    /// Every extension present, in the order found, loaded or not.
    pub present: Vec<Present>,
    /// What was refused or disabled, and why.
    pub problems: Vec<String>,
    /// Each loaded extension's settings, by its id: as declared, with the values they took.
    pub settings: HashMap<String, Vec<(pe2_ext::Setting, toml::Value)>>,
    kinds: HashMap<String, (KindDef, usize)>,
    roles: HashMap<String, RoleDef>,
    panels: Vec<PanelDef>,
    tools: Vec<ToolDef>,
    services: HashMap<String, ToolDef>,
    formats: Vec<FormatDef>,
    importers: HashMap<String, ImporterDef>,
    extractors: Vec<pe2_ext::ExtractorDef>,
    /// The components of the extensions loaded, by id.
    components: Vec<(String, Arc<std::sync::OnceLock<Arc<component::Shared>>>)>,
    /// The formats recognized in files, by their content's hash.
    recognized: Mutex<HashMap<String, Vec<(usize, Recognized)>>>,
    /// Bare names declarations write, to the qualified names of the built-ins registering them.
    kind_aliases: HashMap<String, String>,
    role_aliases: HashMap<String, String>,
}

/// A version's major, minor and patch numbers.
pub(crate) fn semver(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.split('.').map(|p| p.parse::<u64>().ok());
    Some((parts.next()??, parts.next().unwrap_or(Some(0))?, parts.next().unwrap_or(Some(0))?))
}

/// Whether `version` meets a caret requirement (`^0.1`, `0.1.2`): at least it, and the same major (the same minor
/// below 1.0).
pub(crate) fn satisfies(version: &str, requirement: &str) -> bool {
    let (Some(have), Some(want)) = (semver(version), semver(requirement.trim_start_matches('^'))) else {
        return false;
    };
    let compatible = if want.0 == 0 { have.0 == 0 && have.1 == want.1 } else { have.0 == want.0 };
    compatible && have >= want
}

/// A requirement's id and the versions it accepts.
fn requirement(text: &str) -> (&str, Option<&str>) {
    match text.split_once('@') {
        Some((id, version)) => (id, Some(version)),
        None => (text, None),
    }
}

impl Extensions {
    /// Load `extensions`, built-ins (`builtin`) or a project's own.
    #[must_use]
    pub fn load(extensions: Vec<Loadable>, builtin: bool) -> Extensions {
        Extensions::load_without(extensions, builtin, &[])
    }

    /// Load `extensions` but those a project leaves off (`disabled`, by id) - and those requiring them.
    #[must_use]
    pub fn load_without(extensions: Vec<Loadable>, builtin: bool, disabled: &[String]) -> Extensions {
        let mut out = Extensions::default();
        // One an id: the nearest (the project's, the user's, a built-in), its place taken from the others; two from
        // the same place, the first.
        let mut extensions = extensions;
        extensions.sort_by_key(|e| e.origin.nearness());
        let mut pending: Vec<Loadable> = vec![];
        // Those taking a built-in's place, which name kinds and roles bare as it did.
        let mut standing_in: HashSet<String> = HashSet::new();
        for extension in extensions {
            if let Some(nearer) = pending.iter().find(|e| e.id == extension.id) {
                if nearer.origin.nearness() == extension.origin.nearness() {
                    let why = "another of its id from the same place is loaded instead".to_string();
                    out.problems.push(format!("{} ({}): {why}", extension.id, extension.origin.said()));
                    out.present.push(Present::of(&extension, Status::Refused(why)));
                } else {
                    if extension.origin.builtin() {
                        standing_in.insert(extension.id.clone());
                    }
                    out.present.push(Present::of(&extension, Status::Replaced(nearer.origin.said())));
                }
            } else {
                pending.push(extension);
            }
        }
        let ids: HashSet<String> = pending.iter().map(|e| e.id.clone()).collect();
        let mut off: HashSet<String> = HashSet::new();
        let mut status: HashMap<String, Status> = HashMap::new();
        for extension in pending.iter().filter(|e| disabled.contains(&e.id)) {
            off.insert(extension.id.clone());
            status.insert(extension.id.clone(), Status::Disabled);
        }
        let mut timings: HashMap<String, (std::time::Duration, Registers)> = HashMap::new();
        loop {
            let mut progress = false;
            for extension in &mut pending {
                if off.contains(&extension.id) || out.loaded.iter().any(|l| l.id == extension.id) {
                    continue;
                }
                let mut ready = true;
                for text in &extension.requires {
                    let (id, version) = requirement(text);
                    let blocked = if !ids.contains(id) {
                        Some(format!("it needs {text}, which is not installed"))
                    } else if off.contains(id) {
                        Some(format!("it needs {id}, which is disabled"))
                    } else if let Some(loaded) = out.loaded.iter().find(|l| l.id == id) {
                        version.filter(|v| !satisfies(&loaded.version, v)).map(|v| {
                            format!("it needs {id} {v}, and {} is installed", loaded.version)
                        })
                    } else {
                        ready = false;
                        None
                    };
                    if let Some(why) = blocked {
                        out.problems.push(format!("{} is disabled: {why}", extension.id));
                        status.insert(extension.id.clone(), Status::Refused(why));
                        off.insert(extension.id.clone());
                        ready = false;
                        progress = true;
                        break;
                    }
                }
                if !ready || off.contains(&extension.id) {
                    continue;
                }
                progress = true;
                let started = std::time::Instant::now();
                let builtin = builtin && (extension.origin.builtin() || standing_in.contains(&extension.id));
                match out.register(extension, builtin) {
                    Ok(registers) => {
                        timings.insert(extension.id.clone(), (started.elapsed(), registers));
                        status.insert(extension.id.clone(), Status::Loaded);
                    }
                    Err(e) => {
                        out.problems.push(format!("{} is disabled: {e:#}", extension.id));
                        status.insert(extension.id.clone(), Status::Refused(format!("{e:#}")));
                        off.insert(extension.id.clone());
                    }
                }
            }
            if !progress {
                break;
            }
        }
        for extension in &pending {
            let status = status.remove(&extension.id).unwrap_or_else(|| {
                let why = "its requirements form a cycle".to_string();
                out.problems.push(format!("{} is disabled: {why}", extension.id));
                Status::Refused(why)
            });
            let mut present = Present::of(extension, status);
            if let Some((load, registers)) = timings.remove(&extension.id) {
                present.load = Some(load);
                present.registers = registers;
            }
            out.present.push(present);
        }
        out
    }


    /// Call an extension's entry point and take what it registered, checked: what it registered, by what it is.
    fn register(&mut self, extension: &mut Loadable, builtin: bool) -> Result<Registers> {
        let mut registry = Registry::with_settings(&extension.id, extension.settings.clone());
        let register = extension.register.take().context("it was registered already")?;
        register(&mut registry).context("its registration failed")?;
        let parts = registry.into_parts();
        self.take_settings(extension, &parts.settings);
        let listed = |names: Vec<&pe2_ext::Name>| names.into_iter().map(ToString::to_string).collect::<Vec<_>>();
        let registers = vec![
            ("kinds", listed(parts.kinds.iter().map(|k| &k.name).collect())),
            ("roles", listed(parts.roles.iter().map(|r| &r.name).collect())),
            ("formats", listed(parts.formats.iter().map(|f| &f.name).collect())),
            ("importers", listed(parts.importers.iter().map(|i| &i.name).collect())),
            ("extractors", listed(parts.extractors.iter().map(|e| &e.name).collect())),
            ("panels", listed(parts.panels.iter().map(|p| &p.name).collect())),
            ("tools", listed(parts.tools.iter().map(|t| &t.name).collect())),
            ("services", listed(parts.services.iter().map(|s| &s.name).collect())),
        ];
        let (kinds, roles) = (parts.kinds, parts.roles);
        let dependencies: Vec<&str> = extension.requires.iter().map(|r| requirement(r).0).collect();
        let mut seen = HashSet::new();
        let names = kinds.iter().map(|k| &k.name).chain(roles.iter().map(|r| &r.name));
        let names = names.chain(parts.panels.iter().map(|p| &p.name)).chain(parts.tools.iter().map(|t| &t.name));
        let names = names.chain(parts.formats.iter().map(|f| &f.name)).chain(parts.importers.iter().map(|i| &i.name));
        let names = names.chain(parts.extractors.iter().map(|e| &e.name)).chain(parts.services.iter().map(|s| &s.name));
        for name in names {
            anyhow::ensure!(name.extension() == extension.id, "{name} is not in its namespace");
            anyhow::ensure!(seen.insert(name.clone()), "it registers {name} twice");
        }
        for role in &roles {
            let owner = role.kind.extension();
            let reachable = owner == extension.id && kinds.iter().any(|k| k.name == role.kind)
                || dependencies.contains(&owner) && self.kinds.contains_key(role.kind.as_str());
            anyhow::ensure!(reachable, "{} is on {}, which neither it nor what it requires registers", role.name, role.kind);
        }
        let reachable = |owner: &str, known: bool| owner == extension.id && known || dependencies.contains(&owner);
        for importer in &parts.importers {
            let own = kinds.iter().any(|k| k.name == importer.makes);
            anyhow::ensure!(
                reachable(importer.makes.extension(), own) && (own || self.kinds.contains_key(importer.makes.as_str())),
                "{} makes {}, which neither it nor what it requires registers",
                importer.name,
                importer.makes
            );
            for read in importer.reads.iter().filter_map(|r| pe2_ext::Name::parse(r)) {
                let own = parts.formats.iter().any(|f| f.name == read);
                anyhow::ensure!(
                    reachable(read.extension(), own) && (own || self.formats.iter().any(|f| f.name == read)),
                    "{} reads {read}, which neither it nor what it requires registers",
                    importer.name
                );
            }
        }
        for extractor in &parts.extractors {
            let own = kinds.iter().any(|k| k.name == extractor.kind);
            anyhow::ensure!(
                reachable(extractor.kind.extension(), own) && (own || self.kinds.contains_key(extractor.kind.as_str())),
                "{} extracts {}, which neither it nor what it requires registers",
                extractor.name,
                extractor.kind
            );
            for role in &extractor.roles {
                let known = roles.iter().any(|r| r.name == *role && r.kind == extractor.kind)
                    || self.roles.get(role.as_str()).is_some_and(|r| r.kind == extractor.kind);
                anyhow::ensure!(known, "{} extracts {role}, which is no role of {}", extractor.name, extractor.kind);
            }
        }
        let aliases = kinds.iter().filter_map(|k| k.alias.as_ref()).chain(roles.iter().filter_map(|r| r.alias.as_ref()));
        for alias in aliases {
            anyhow::ensure!(builtin, "only a built-in extension names a kind or role bare ({alias})");
        }
        let index = self.loaded.len();
        if let Some(component) = &extension.component {
            self.components.push((extension.id.clone(), component.clone()));
        }
        let (id, version, origin) = (extension.id.clone(), extension.version.clone(), extension.origin.clone());
        let requires = extension.requires.clone();
        self.loaded.push(Loaded { id, version, origin, requires });
        for kind in kinds {
            if let Some(alias) = &kind.alias {
                self.kind_aliases.insert(alias.clone(), kind.name.to_string());
            }
            self.kinds.insert(kind.name.to_string(), (kind, index));
        }
        for role in roles {
            if let Some(alias) = &role.alias {
                self.role_aliases.insert(alias.clone(), role.name.to_string());
            }
            self.roles.insert(role.name.to_string(), role);
        }
        self.panels.extend(parts.panels);
        self.tools.extend(parts.tools);
        self.services.extend(parts.services.into_iter().map(|s| (s.name.to_string(), s)));
        self.formats.extend(parts.formats);
        self.importers.extend(parts.importers.into_iter().map(|i| (i.name.to_string(), i)));
        self.extractors.extend(parts.extractors);
        Ok(registers)
    }

    /// An extension's settings as it declared them, with the values they took; what is kept for it that it could not
    /// take is a problem.
    fn take_settings(&mut self, extension: &Loadable, declared: &[(pe2_ext::Setting, toml::Value)]) {
        for (scope, kept) in extension.settings.iter().filter_map(|(scope, t)| Some((scope, t.as_table()?))) {
            for (key, value) in kept {
                let id = &extension.id;
                match declared.iter().find(|(s, _)| s.key == *key && s.scope.name() == scope) {
                    None => self.problems.push(format!("{id}: the {scope} setting {key} is none it has")),
                    Some((s, _)) if !s.accepts(value) => {
                        self.problems.push(format!("{id}: the {scope} setting {key} cannot be {value}; it is {}", s.default));
                    }
                    Some(_) => {}
                }
            }
        }
        self.settings.insert(extension.id.clone(), declared.to_vec());
    }

    /// An extension's version as what it makes is kept by: its version, and the project's settings it took where any
    /// is not its default - another value of one makes other assets, so other keys.
    #[must_use]
    pub fn version_of(&self, id: &str) -> String {
        let Some(loaded) = self.loaded.iter().find(|l| l.id == id) else { return String::new() };
        let changed: Vec<String> = self
            .settings
            .get(id)
            .into_iter()
            .flatten()
            .filter(|(s, value)| s.scope == pe2_ext::Scope::Project && *value != s.default)
            .map(|(s, value)| format!("{}={value}", s.key))
            .collect();
        if changed.is_empty() { loaded.version.clone() } else { format!("{} {}", loaded.version, changed.join(" ")) }
    }

    /// The kind a declaration names, qualified or by a built-in's bare alias.
    fn kind_named(&self, declared: &str) -> Option<&(KindDef, usize)> {
        self.kinds.get(self.kind_aliases.get(declared).map_or(declared, String::as_str))
    }

    /// A declared kind and role, qualified as extensions see them (`image`, `atlas` to `pe2.images@image`,
    /// `pe2.images@atlas`); none for what no extension registers.
    #[must_use]
    pub fn qualify(&self, kind: &str, role: Option<&str>) -> (Option<&str>, Option<&str>) {
        let Some((kind, _)) = self.kind_named(kind) else { return (None, None) };
        let role = role
            .and_then(|r| self.roles.get(self.role_aliases.get(r).map_or(r, String::as_str)))
            .filter(|r| r.kind == kind.name)
            .map(|r| r.name.as_str());
        (Some(kind.name.as_str()), role)
    }

    /// An asset's kind, where an extension registers it.
    #[must_use]
    pub fn kind_of(&self, asset: &index::Asset) -> Option<&KindDef> {
        self.kind_named(asset.kind.as_deref()?).map(|(kind, _)| kind)
    }

    /// An asset's role, where one is registered on its kind.
    #[must_use]
    pub fn role_of(&self, asset: &index::Asset) -> Option<&RoleDef> {
        let kind = self.kind_of(asset)?;
        let declared = asset.role.as_deref()?;
        let role = self.roles.get(self.role_aliases.get(declared).map_or(declared, String::as_str))?;
        (role.kind == kind.name).then_some(role)
    }

    /// An asset as its extensions see it: lent, with its kind and role qualified.
    #[must_use]
    pub fn lend<'a>(&'a self, asset: &'a index::Asset) -> Asset<'a> {
        let kind = self.kind_of(asset).map(|k| k.name.as_str());
        let role = self.role_of(asset).map(|r| r.name.as_str());
        Asset::new(asset, kind, role)
    }

    /// A kind's icon and its tint, by the name a declaration writes.
    #[must_use]
    pub fn icon(&self, declared: &str) -> Option<(Icon, Tint)> {
        self.kind_named(declared)?.0.icon
    }

    /// Whether an asset has a picture.
    #[must_use]
    pub fn has_picture(&self, asset: &index::Asset) -> bool {
        self.kind_of(asset).and_then(|k| k.picture.as_ref()).is_some_and(|p| p.has(&self.lend(asset)))
    }

    /// An asset's looks, by name.
    #[must_use]
    pub fn looks(&self, asset: &index::Asset) -> Vec<String> {
        self.kind_of(asset).and_then(|k| k.picture.as_ref()).map_or_default(|p| p.looks(&self.lend(asset)))
    }

    /// An asset's picture in one of its looks; none when it has none.
    ///
    /// # Errors
    /// Its extension could not draw it.
    pub fn picture(&self, asset: &index::Asset, look: usize) -> Option<Result<Image>> {
        let picture = self.kind_of(asset)?.picture.as_ref()?;
        let lent = self.lend(asset);
        picture.has(&lent).then(|| picture.draw(&lent, look))
    }

    /// Whether an asset's kind makes thumbnails.
    #[must_use]
    pub fn has_thumbnail(&self, asset: &index::Asset) -> bool {
        self.kind_of(asset).is_some_and(|k| k.thumbnail.is_some())
    }

    /// An asset's thumbnail, at most `side` pixels on its longer side; none when its kind makes none, or it has none.
    #[must_use]
    pub fn thumbnail(&self, asset: &index::Asset, side: usize) -> Option<Result<Image>> {
        (self.kind_of(asset)?.thumbnail.as_ref()?)(&self.lend(asset), side)
    }

    /// An artifact's generator: its extension's id and version, and its name.
    fn generator(&self, index: usize, name: &str) -> String {
        let loaded = &self.loaded[index];
        format!("{}@{name} {}", loaded.id, self.version_of(&loaded.id))
    }

    /// The artifact name an asset's thumbnail is kept under, before it is made: its extension's.
    #[must_use]
    pub fn thumbnail_name(&self, asset: &index::Asset) -> Option<String> {
        let (_, index) = self.kind_named(asset.kind.as_deref()?)?;
        Some(format!("{}@thumbnail.png", self.loaded[*index].id))
    }

    /// The generator an asset's thumbnail is kept under as an artifact: `pe2.images@thumbnail 0.1.0`, so that a new
    /// version of its extension makes it again.
    #[must_use]
    pub fn thumbnail_generator(&self, asset: &index::Asset) -> Option<String> {
        let (_, index) = self.kind_named(asset.kind.as_deref()?)?;
        Some(self.generator(*index, "thumbnail"))
    }

    /// The fields an asset's kind and role describe, the role's first.
    #[must_use]
    pub fn fields(&self, asset: &index::Asset) -> Vec<&Field> {
        let role = self.role_of(asset).map_or(&[][..], |r| r.fields.as_slice());
        let kind = self.kind_of(asset).map_or(&[][..], |k| k.fields.as_slice());
        role.iter().chain(kind).collect()
    }

    /// The inspector views of an asset: its kind's, then its role's.
    #[must_use]
    pub fn inspector(&self, asset: &index::Asset) -> Vec<&ViewDef> {
        let kind = self.kind_of(asset).map_or(&[][..], |k| k.inspector.as_slice());
        let role = self.role_of(asset).map_or(&[][..], |r| r.inspector.as_slice());
        kind.iter().chain(role).collect()
    }

    /// A part of an asset's picture (`palettes/<name>`); none where its kind draws no such part.
    #[must_use]
    pub fn part(&self, asset: &index::Asset, part: &str) -> Option<Result<Image>> {
        self.kind_of(asset)?.picture.as_ref()?.part(&self.lend(asset), part)
    }

    /// An asset's parts, as its kind lists them.
    #[must_use]
    pub fn parts(&self, asset: &index::Asset) -> Vec<pe2_ext::Part> {
        self.kind_of(asset).and_then(|k| k.parts.as_ref()).map_or_default(|parts| parts(&self.lend(asset)))
    }

    /// Whether an asset's kind lists its parts.
    #[must_use]
    pub fn lists_parts(&self, asset: &index::Asset) -> bool {
        self.kind_of(asset).is_some_and(|k| k.parts.is_some())
    }

    /// What an agent is told of an asset besides its declaration, as its kind says.
    #[must_use]
    pub fn facts(&self, asset: &index::Asset) -> Table {
        self.kind_of(asset).and_then(|k| k.facts.as_ref()).map_or_default(|facts| facts(&self.lend(asset)))
    }

    /// The panels the extensions registered.
    #[must_use]
    pub fn panels(&self) -> &[PanelDef] {
        &self.panels
    }

    /// The panel registered under a name.
    #[must_use]
    pub fn panel(&self, name: &str) -> Option<&PanelDef> {
        self.panels.iter().find(|p| p.name.as_str() == name)
    }

    /// A service an extension offers, by its qualified name.
    #[must_use]
    pub fn service(&self, name: &str) -> Option<&ToolDef> {
        self.services.get(name)
    }

    /// Whether the extension `caller` may call a service: its own, or one of an extension it requires.
    #[must_use]
    pub fn may_call(&self, caller: &str, service: &str) -> bool {
        let Some(owner) = self.services.get(service).map(|s| s.name.extension()) else { return false };
        owner == caller
            || self.loaded.iter().find(|l| l.id == caller).is_some_and(|l| l.requires.iter().any(|r| requirement(r).0 == owner))
    }

    /// The tools the extensions registered, for agents.
    #[must_use]
    pub fn tools(&self) -> &[ToolDef] {
        &self.tools
    }

    /// Every asset of a kind, by its qualified name or a built-in's bare one.
    pub fn find<'a>(&'a self, index: &'a index::Index, kind: &str) -> impl Iterator<Item = &'a index::Asset> {
        let wanted = self.kind_named(kind).map(|(k, _)| k.name.as_str());
        index.assets.values().map(|a| &**a).filter(move |a| {
            wanted.is_some_and(|w| self.kind_of(a).is_some_and(|k| k.name.as_str() == w))
                || wanted.is_none() && a.kind.as_deref() == Some(kind)
        })
    }

    /// An asset's importer: the operation its stack takes its source through.
    #[must_use]
    pub fn importer_of(&self, asset: &index::Asset) -> Option<&ImporterDef> {
        self.importers.get(graph::import_of(asset.fields())?.op)
    }

    /// An importer, by its qualified name.
    #[must_use]
    pub fn importer(&self, name: &str) -> Option<&ImporterDef> {
        self.importers.get(name)
    }

    /// The operations stacks are made of: the built-ins, and each importer as one taking its source and making its
    /// asset's native form - a native importer's being its source as it is. An importer reads its asset as it always
    /// has, its source and settings given as `[import]` (`graph::with_import`); it is versioned by its extension.
    #[must_use]
    pub fn operations(&self) -> graph::Ops {
        let mut ops = graph::Ops::default();
        for def in self.importers.values() {
            let name = def.name.to_string();
            let version = self.version_of(def.name.extension());
            let op = graph::OpDef::new(&name, &def.label, &version).input(graph::Port::one("source", graph::Type::File));
            let op = match def.import.clone() {
                None => op.makes(graph::Type::Native, |call| {
                    let graph::Value::File(file) = call.take(0)?.take() else { anyhow::bail!("its source is not a file") };
                    Ok(graph::Value::Native(pe2_ext::Imported { files: vec![(file.name, file.bytes)], ..Default::default() }))
                }),
                Some(import) => {
                    let kind = def.makes.to_string();
                    // The roles of its kind by every name a declaration may give them.
                    let mut roles: HashMap<String, String> = self
                        .roles
                        .iter()
                        .filter(|(_, r)| r.kind == def.makes)
                        .map(|(q, _)| (q.clone(), q.clone()))
                        .collect();
                    let aliases: Vec<(String, String)> =
                        self.role_aliases.iter().filter(|(_, q)| roles.contains_key(*q)).map(|(a, q)| (a.clone(), q.clone())).collect();
                    roles.extend(aliases);
                    op.makes(graph::Type::Native, move |call| {
                        let graph::Value::File(source) = call.take(0)?.take() else { anyhow::bail!("its source is not a file") };
                        let recorder = call.recorder();
                        recorder.declaration();
                        let node = recorder.node();
                        let fields = graph::with_import(
                            node.fields(),
                            &graph::Import { source: &source.name, op: &name, settings: call.settings().clone() },
                        );
                        let role = node.fields().get("role").and_then(toml::Value::as_str).and_then(|r| roles.get(r));
                        let staged = Staged { recorder, fields, source };
                        import(&Asset::new(&staged, Some(&kind), role.map(String::as_str))).map(graph::Value::Native)
                    })
                }
            };
            ops.add(op);
        }
        ops
    }

    /// What each extension running as a component holds: its live instances and their memory, by id.
    #[must_use]
    pub fn memory(&self) -> Vec<(&str, component::Memory)> {
        self.components.iter().filter_map(|(id, c)| Some((id.as_str(), c.get()?.memory()))).collect()
    }

    /// A frame ended: each extension's readings' instance unused for long enough is reclaimed.
    pub fn frame(&self) {
        for component in self.components.iter().filter_map(|(_, c)| c.get()) {
            component.frame();
        }
    }

    /// Reclaim every readings' instance now: where there are no frames (the agents' server, between requests).
    pub fn release(&self) {
        for component in self.components.iter().filter_map(|(_, c)| c.get()) {
            component.release();
        }
    }

    /// The extractor of assets of a declared kind and role (`image`, `atlas`), where an extension registers one.
    #[must_use]
    pub fn extractor_for(&self, kind: &str, role: Option<&str>) -> Option<&pe2_ext::ExtractorDef> {
        let (Some(kind), role) = self.qualify(kind, role) else { return None };
        self.extractors.iter().find(|e| e.takes(kind, role))
    }

    /// The source an importer reads, written from an asset's native files; none where the importer writes none.
    ///
    /// # Errors
    /// The export failed.
    pub fn export(&self, importer: &str, asset: &Asset) -> Option<Result<Vec<u8>>> {
        let export = self.importer(importer)?.export.as_ref()?;
        Some(export(asset))
    }

    /// The formats a file is, by signatures and sniffs: from its first bytes, read once a content.
    #[must_use]
    pub fn recognize(&self, file: &index::BundleFile) -> Vec<(&FormatDef, Recognized)> {
        let known = self.recognized.lock().ok().and_then(|r| r.get(&file.sha1).cloned());
        let found = known.unwrap_or_else(|| {
            let found = self.recognize_now(file);
            if let Ok(mut recognized) = self.recognized.lock() {
                recognized.insert(file.sha1.clone(), found.clone());
            }
            found
        });
        found.into_iter().map(|(i, r)| (&self.formats[i], r)).collect()
    }

    fn recognize_now(&self, file: &index::BundleFile) -> Vec<(usize, Recognized)> {
        use std::io::Read;
        const HEAD: u64 = 4096;
        let mut head = Vec::new();
        if let Ok(f) = std::fs::File::open(&file.path) {
            let _ = f.take(HEAD).read_to_end(&mut head);
        }
        let text = match std::str::from_utf8(&head) {
            Ok(t) => !t.contains('\0'),
            // A cut through a character at the end of what was read: what is before it decides.
            Err(e) => e.error_len().is_none() && !head[..e.valid_up_to()].contains(&0),
        };
        let named = |pattern: &str| match pattern.strip_prefix('*') {
            Some(suffix) => file.name.ends_with(suffix),
            None => file.name == pattern,
        };
        let mut out = vec![];
        for (i, format) in self.formats.iter().enumerate() {
            let signed = format.signatures.iter().any(|signature| match signature {
                Signature::Bytes { offset, bytes } => head.get(*offset..offset + bytes.len()) == Some(bytes.as_slice()),
                Signature::Name(pattern) => named(pattern),
                Signature::Text => text,
            });
            let recognized = if signed {
                Some(Recognized::default())
            } else {
                format.sniff.as_ref().and_then(|sniff| sniff(&file.name, file.size, &head))
            };
            out.extend(recognized.map(|r| (i, r)));
        }
        out
    }

    /// The importers that fit an asset's source - the stored file its stack takes - with how far each takes it, the
    /// highest priority first. An importer not reading any of the source's formats is not listed.
    #[must_use]
    pub fn importers_for(&self, asset: &index::Asset) -> Vec<(&ImporterDef, Support)> {
        let Some(source) = graph::import_of(asset.fields()).map(|i| i.source) else { return vec![] };
        let Some(file) = asset.files.iter().find(|f| f.name == source) else { return vec![] };
        let formats = self.recognize(file);
        let read = |r: &str| formats.iter().any(|(f, _)| f.name.as_str() == r || f.media_type.as_deref() == Some(r));
        let lent = self.lend(asset);
        let mut out: Vec<(&ImporterDef, Support)> = self
            .importers
            .values()
            .filter(|i| i.reads.iter().any(|r| read(r)))
            .map(|i| (i, i.check.as_ref().map_or(Support::Full, |check| check(&lent))))
            .collect();
        out.sort_by(|(a, _), (b, _)| b.priority.cmp(&a.priority).then_with(|| a.name.cmp(&b.name)));
        out
    }

    /// The views of an asset: its kind's, then its role's.
    #[must_use]
    pub fn views(&self, asset: &index::Asset) -> Vec<&ViewDef> {
        let kind = self.kind_of(asset).map_or(&[][..], |k| k.views.as_slice());
        let role = self.role_of(asset).map_or(&[][..], |r| r.views.as_slice());
        kind.iter().chain(role).collect()
    }
}

/// The project as extensions ask about it: the index, through the registry, with what the user selected and the looks
/// chosen (the editor's; none for a tool).
pub struct Lens<'a> {
    pub index: &'a index::Index,
    pub extensions: &'a Extensions,
    pub selection: Option<&'a str>,
    pub looks: Option<&'a HashMap<String, usize>>,
}

impl Project for Lens<'_> {
    fn asset(&self, path: &str) -> Option<Asset<'_>> {
        self.index.assets.get(path).map(|a| self.extensions.lend(a))
    }

    fn resolve(&self, reference: &Value) -> Option<Asset<'_>> {
        let (uid, path) = index::reference(reference)?;
        self.index.resolve(&uid, path.as_deref().unwrap_or_default()).map(|a| self.extensions.lend(a))
    }

    fn find(&self, kind: &str) -> Vec<Asset<'_>> {
        self.extensions.find(self.index, kind).map(|a| self.extensions.lend(a)).collect()
    }

    fn selection(&self) -> Option<Asset<'_>> {
        self.asset(self.selection?)
    }

    fn look(&self, path: &str) -> usize {
        self.looks.and_then(|l| l.get(path).copied()).unwrap_or(0)
    }

    fn picture(&self, path: &str, look: usize) -> Option<Result<Image>> {
        self.extensions.picture(self.index.assets.get(path)?, look)
    }

    fn part(&self, path: &str, part: &str) -> Option<Result<Image>> {
        self.extensions.part(self.index.assets.get(path)?, part)
    }

    fn parts(&self, path: &str) -> Vec<pe2_ext::Part> {
        self.index.assets.get(path).map_or_default(|a| self.extensions.parts(a))
    }

    fn facts(&self, path: &str) -> toml::Table {
        self.index.assets.get(path).map_or_default(|a| self.extensions.facts(a))
    }

    fn call(&self, service: &str, arguments: &Value) -> Result<Value> {
        let def = self.extensions.service(service).with_context(|| format!("no service {service}"))?;
        (def.run)(self, arguments).with_context(|| service.to_string())
    }

    fn may_call(&self, caller: &str, service: &str) -> bool {
        self.extensions.may_call(caller, service)
    }
}

/// A node as an operation lends it to an importer: its fields with `[import]`, its source as the operation was given
/// it, and its other stored files, each recorded as read.
struct Staged<'a> {
    recorder: graph::exec::Recorder<'a>,
    fields: toml::Table,
    source: graph::File,
}

impl Source for Staged<'_> {
    fn path(&self) -> &str {
        self.recorder.node().path()
    }

    fn uid(&self) -> Option<&str> {
        self.recorder.node().uid()
    }

    fn fields(&self) -> &toml::Table {
        &self.fields
    }

    fn declaration(&self) -> &str {
        self.recorder.node().declaration()
    }

    fn has_file(&self, name: &str) -> bool {
        name == self.source.name || self.recorder.node().file_sha1(name).is_some()
    }

    fn read(&self, name: &str) -> Result<Vec<u8>> {
        if name == self.source.name {
            return Ok(self.source.bytes.clone());
        }
        self.recorder.file(name);
        self.recorder.node().read(name)
    }
}

impl Source for index::Asset {
    fn path(&self) -> &str {
        &self.path
    }

    fn uid(&self) -> Option<&str> {
        self.uid.as_deref()
    }

    fn fields(&self) -> &toml::Table {
        self.settings()
    }

    fn declaration(&self) -> &str {
        self.settings_text()
    }

    fn has_file(&self, name: &str) -> bool {
        self.files.iter().any(|f| f.name == name)
    }

    fn read(&self, name: &str) -> Result<Vec<u8>> {
        let file = self.files.iter().find(|f| f.name == name).with_context(|| format!("{}: no file {name}", self.path))?;
        std::fs::read(&file.path).with_context(|| file.path.display().to_string())
    }

    fn size(&self, name: &str) -> Option<u64> {
        Some(self.files.iter().find(|f| f.name == name)?.size)
    }

    fn read_range(&self, name: &str, offset: u64, len: usize) -> Result<Vec<u8>> {
        use std::io::{Read, Seek, SeekFrom};
        let file = self.files.iter().find(|f| f.name == name).with_context(|| format!("{}: no file {name}", self.path))?;
        let mut opened = std::fs::File::open(&file.path).with_context(|| file.path.display().to_string())?;
        opened.seek(SeekFrom::Start(offset))?;
        let mut out = Vec::with_capacity(len.min(usize::try_from(file.size.saturating_sub(offset)).unwrap_or(len)));
        opened.take(len as u64).read_to_end(&mut out)?;
        Ok(out)
    }
}

/// An image as PNG.
///
/// # Errors
/// It cannot be encoded.
pub fn png(image: &Image) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let (width, height) = (u32::try_from(image.width)?, u32::try_from(image.height)?);
    let mut encoder = png::Encoder::new(&mut out, width.max(1), height.max(1));
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    if image.rgba.is_empty() {
        writer.write_image_data(&[0; 4])?;
    } else {
        writer.write_image_data(&image.rgba)?;
    }
    writer.finish()?;
    Ok(out)
}

/// An image from PNG, as `png` writes it.
///
/// # Errors
/// It is not an 8-bit RGBA PNG.
pub fn from_png(bytes: &[u8]) -> Result<Image> {
    let mut reader = png::Decoder::new(std::io::Cursor::new(bytes)).read_info()?;
    let mut rgba = vec![0; reader.output_buffer_size().unwrap_or(0)];
    let info = reader.next_frame(&mut rgba)?;
    anyhow::ensure!(info.color_type == png::ColorType::Rgba && info.bit_depth == png::BitDepth::Eight, "not 8-bit RGBA");
    rgba.truncate(info.buffer_size());
    Ok(Image { width: info.width as usize, height: info.height as usize, rgba })
}

#[cfg(test)]
#[expect(clippy::unnecessary_wraps, reason = "entry points may fail; these do not")]
mod tests {
    use super::{Extensions, Loadable, Origin, Status, satisfies};
    use pe2_ext::{Extension, Name, Registry, Result, Setting};

    fn images(r: &mut Registry) -> Result<()> {
        let image = r.kind("image").alias("image").register();
        r.role(&image, "atlas").alias("atlas").register();
        Ok(())
    }

    fn fonts(r: &mut Registry) -> Result<()> {
        r.role(&Name::parse("test.images@image").unwrap(), "font").register();
        Ok(())
    }

    fn stray(r: &mut Registry) -> Result<()> {
        r.role(&Name::parse("test.images@image").unwrap(), "font").register();
        Ok(())
    }

    fn nothing(_: &mut Registry) -> Result<()> {
        Ok(())
    }

    fn extension(
        id: &'static str,
        version: &'static str,
        requires: &'static [&'static str],
        register: fn(&mut Registry) -> Result<()>,
    ) -> Loadable {
        Extension { id, version, requires, register }.into()
    }

    #[test]
    fn loads_in_dependency_order_and_disables_what_cannot_load() {
        let loaded = Extensions::load(
            vec![
                // Before what it needs: loaded after it.
                extension("test.fonts", "0.1.0", &["test.images@^0.2"], fonts),
                extension("test.images", "0.2.3", &[], images),
                extension("test.images", "9.0.0", &[], nothing),
                extension("test.old", "0.1.0", &["test.images@^0.3"], nothing),
                extension("test.missing", "0.1.0", &["test.nowhere"], nothing),
                extension("test.chained", "0.1.0", &["test.missing"], nothing),
                extension("test.stray", "0.1.0", &[], stray),
                extension("test.a", "0.1.0", &["test.b"], nothing),
                extension("test.b", "0.1.0", &["test.a"], nothing),
            ],
            true,
        );
        let ids: Vec<&str> = loaded.loaded.iter().map(|l| l.id.as_str()).collect();
        assert_eq!(ids, ["test.images", "test.fonts"]);
        let problems = loaded.problems.join("\n");
        for expected in [
            "test.images (linked in): another of its id from the same place is loaded instead",
            "test.old is disabled: it needs test.images ^0.3, and 0.2.3 is installed",
            "test.missing is disabled: it needs test.nowhere, which is not installed",
            "test.chained is disabled: it needs test.missing, which is disabled",
            "test.stray is disabled: test.stray@font is on test.images@image",
            "test.a is disabled: its requirements form a cycle",
        ] {
            assert!(problems.contains(expected), "{expected}\n---\n{problems}");
        }
        assert_eq!(loaded.kind_aliases["image"], "test.images@image");
        assert!(loaded.roles.contains_key("test.fonts@font"));
    }

    #[test]
    fn only_builtins_alias() {
        let loaded = Extensions::load(vec![extension("mod.images", "0.1.0", &[], images)], false);
        assert_eq!(loaded.loaded, []);
        assert!(loaded.problems[0].contains("only a built-in extension names a kind or role bare"), "{:?}", loaded.problems);
    }

    #[test]
    fn the_nearest_of_an_id_takes_its_place() {
        let from = |origin: Origin| {
            let mut images = extension("test.images", "0.1.0", &[], images);
            images.origin = origin;
            images
        };
        let user = Origin::User("images.wasm".into());
        let project = Origin::Project { asset: "extensions/images".into(), file: "extension.wasm".into() };
        // The built-in listed first, the project's last: nearness decides, not order.
        let loaded = Extensions::load(vec![from(Origin::Linked), from(user.clone()), from(project.clone())], true);
        assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
        assert_eq!(loaded.loaded.len(), 1);
        assert_eq!(loaded.loaded[0].origin, project);
        // Standing in for a built-in, it names kinds bare as the built-in did.
        assert_eq!(loaded.kind_aliases["image"], "test.images@image");
        let replaced: Vec<&Origin> =
            loaded.present.iter().filter(|p| matches!(p.status, Status::Replaced(_))).map(|p| &p.origin).collect();
        assert_eq!(replaced, [&user, &Origin::Linked]);
        // Two from one place: the second is refused, a problem.
        let mut other = from(user.clone());
        other.origin = Origin::User("copy.wasm".into());
        let loaded = Extensions::load(vec![from(user), other], false);
        assert!(loaded.problems.iter().any(|p| p.contains("another of its id from the same place")), "{:?}", loaded.problems);
        // Taking no built-in's place, it may not name kinds bare.
        assert!(loaded.problems.iter().any(|p| p.contains("only a built-in extension names a kind or role bare")));
    }

    #[test]
    fn settings_are_read_as_kept_and_what_is_not_taken_is_said() {
        fn tuned(r: &mut Registry) -> Result<()> {
            let _ = r.setting(Setting::new("depth", 4).choices([(4, "4-bit"), (8, "8-bit")]));
            let _ = r.setting(Setting::new("scale", 1.0));
            let _ = r.setting(Setting::new("note", "").label("A note"));
            let _ = r.setting(Setting::new("grid", true).user());
            Ok(())
        }
        let mut tuned_ext = extension("test.tuned", "0.1.0", &[], tuned);
        tuned_ext.settings = toml::from_str(
            r#"
            project = { depth = 8, scale = 2, note = 3, bogus = 1 }
            user = { grid = "no" }
            "#,
        )
        .unwrap();
        let loaded = Extensions::load(vec![tuned_ext], false);
        let value = |key: &str| loaded.settings["test.tuned"].iter().find(|(s, _)| s.key == key).map(|(_, v)| v.clone());
        assert_eq!(value("depth"), Some(toml::Value::Integer(8)));
        // An integer where a float is wanted reads as one.
        assert_eq!(value("scale"), Some(toml::Value::Float(2.0)));
        // A value of another type is not taken: the default stands, and it is said.
        assert_eq!(value("note"), Some(toml::Value::String(String::new())));
        assert_eq!(value("grid"), Some(toml::Value::Boolean(true)));
        let problems = loaded.problems.join("\n");
        for expected in [
            "test.tuned: the project setting bogus is none it has",
            "test.tuned: the project setting note cannot be 3",
            "test.tuned: the user setting grid cannot be \"no\"",
        ] {
            assert!(problems.contains(expected), "{expected}\n---\n{problems}");
        }
        // What it makes is kept by its project settings too, the user's never.
        assert_eq!(loaded.version_of("test.tuned"), "0.1.0 depth=8 scale=2.0");
        let plain = Extensions::load(vec![extension("test.tuned", "0.1.0", &[], tuned)], false);
        assert_eq!(plain.version_of("test.tuned"), "0.1.0");
    }

    #[test]
    fn caret_requirements() {
        assert!(satisfies("0.1.4", "^0.1"));
        assert!(satisfies("0.1.4", "0.1.2"));
        assert!(!satisfies("0.1.1", "0.1.2"));
        assert!(!satisfies("0.2.0", "^0.1"));
        assert!(satisfies("1.4.0", "^1.2"));
        assert!(!satisfies("2.0.0", "^1.2"));
    }
}

#[cfg(test)]
#[expect(clippy::unnecessary_wraps, reason = "entry points may fail; these do not")]
mod calling {
    use super::{Extensions, Lens, Loadable};
    use crate::Project as Opened;
    use crate::cache::Cache;
    use crate::index::Index;
    use crate::lock::Lock;
    use pe2_ext::{Asset, Extension, Image, Picture, Project, Registry, Result, Table, Value, anyhow};

    /// A picture of one red pixel.
    struct Red;

    impl Picture for Red {
        fn has(&self, _: &Asset) -> bool {
            true
        }

        fn looks(&self, _: &Asset) -> Vec<String> {
            vec![]
        }

        fn draw(&self, _: &Asset, _: usize) -> Result<Image> {
            Ok(Image { width: 1, height: 1, rgba: vec![255, 0, 0, 255] })
        }
    }

    fn paint(r: &mut Registry) -> Result<()> {
        r.kind("canvas").alias("canvas").picture(Red).register();
        r.service("red", "The color red", "{}", |_, _| Ok(Value::String("red".into())));
        Ok(())
    }

    /// Asks the painter, which it requires, for its service and a picture.
    fn scene(r: &mut Registry) -> Result<()> {
        r.service("frame", "A frame", "{}", |project: &dyn Project, _| {
            let color = project.call("test.paint@red", &Value::Table(Table::new()))?;
            let picture = project.picture("a", 0).ok_or_else(|| anyhow!("no picture"))??;
            Ok(Value::String(format!("{} {}x{}", color.as_str().unwrap_or_default(), picture.width, picture.height)))
        });
        Ok(())
    }

    fn stray(_: &mut Registry) -> Result<()> {
        Ok(())
    }

    #[test]
    fn extensions_reach_one_another_through_the_host() {
        let root = std::env::temp_dir().join(format!("pe2-calling-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (path, text) in [("project.toml", "[[layer]]\nname = \"base\"\n"), (".layers/base/a.asset/asset.toml", "kind = \"canvas\"\n")] {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        let extension = |id: &'static str, requires: &'static [&'static str], register: fn(&mut Registry) -> Result<()>| {
            Loadable::from(Extension { id, version: "0.1.0", requires, register })
        };
        let extensions = Extensions::load(
            vec![
                extension("test.paint", &[], paint),
                extension("test.scene", &["test.paint@^0.1"], scene),
                extension("test.stray", &[], stray),
            ],
            true,
        );
        assert!(extensions.problems.is_empty(), "{:?}", extensions.problems);
        let project = Opened::load(&root).unwrap();
        let lock = Lock::try_acquire(&root, "a test").unwrap();
        let index = Index::open(&project, &Cache::open(&project, &lock).unwrap()).unwrap();
        let lens = Lens { index: &index, extensions: &extensions, selection: None, looks: None };
        let answer = lens.call("test.scene@frame", &Value::Table(Table::new())).unwrap();
        assert_eq!(answer.as_str(), Some("red 1x1"));
        assert!(lens.call("test.nowhere@service", &Value::Table(Table::new())).is_err());
        // A component may call its own services and those of what it requires, no other's.
        assert!(lens.may_call("test.scene", "test.paint@red"));
        assert!(lens.may_call("test.paint", "test.paint@red"));
        assert!(!lens.may_call("test.stray", "test.paint@red"));
        assert!(!lens.may_call("test.scene", "test.paint@nothing"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_project_leaves_extensions_off_with_those_requiring_them() {
        let extension = |id: &'static str, requires: &'static [&'static str], register: fn(&mut Registry) -> Result<()>| {
            Loadable::from(Extension { id, version: "0.1.0", requires, register })
        };
        let loaded = Extensions::load_without(
            vec![extension("test.paint", &[], paint), extension("test.scene", &["test.paint@^0.1"], scene), extension("test.stray", &[], stray)],
            true,
            &["test.paint".to_string()],
        );
        let status = |id: &str| loaded.present.iter().find(|p| p.id == id).map(|p| p.status.clone());
        assert_eq!(status("test.paint"), Some(super::Status::Disabled));
        assert!(matches!(status("test.scene"), Some(super::Status::Refused(why)) if why.contains("disabled")));
        assert_eq!(status("test.stray"), Some(super::Status::Loaded));
        assert_eq!(loaded.loaded.iter().map(|l| l.id.as_str()).collect::<Vec<_>>(), ["test.stray"]);
        let all = Extensions::load(vec![extension("test.paint", &[], paint)], true);
        let painter = &all.present[0];
        assert!(painter.load.is_some());
        assert_eq!(painter.registers.iter().find(|(what, _)| *what == "services").map(|(_, n)| n.clone()), Some(vec!["test.paint@red".to_string()]));
    }
}
