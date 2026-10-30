//! The extension host: loads the extensions, registers what they register, and answers what a kind has - its icon,
//! fields, picture, thumbnail, views - for the editor, `pe2-cli` and the agents' server alike.
//!
//! Loading checks what the SDK cannot: one extension an id (a second is refused), each one's requirements met by the
//! version loaded (`pe2.images@^0.1`), registration in dependency order, cycles refused; an extension whose
//! requirement is missing, too old or itself disabled is disabled, and so are its dependents, each with the reason. What
//! one registers is checked as well: a role only on its own kinds or its dependencies', an alias only from a built-in,
//! a name only once. An extension is a component the host runs (`component`), or a crate linked into the host whose
//! entry point it calls - the built-ins' fallback while developing.

pub mod unsandboxed;
pub mod component;

use anyhow::{Context, Result};
use pe2_ext::{
    Asset, Extension, Field, TypeDef, Icon, Image, KindDef, PanelDef, Project, Recognized, Registry, RoleDef, Signature,
    Source, Table, Tint, ToolDef, Value, ViewDef,
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
    /// A component in the extensions folder beside the editor.
    Install(PathBuf),
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
    /// What it uses where a compatible one is enabled, and is turned off with once linked.
    pub weak: Vec<String>,
    /// What it uses where a compatible one is enabled.
    pub optional: Vec<String>,
    /// The services it offers (`c-compiler@1.0.0`).
    pub provides: Vec<String>,
    pub origin: Origin,
    register: Option<Register>,
    /// Its component, where it runs as one - set when it registers: its instances, their memory.
    component: Option<Arc<std::sync::OnceLock<Arc<component::Shared>>>>,
    /// Its component's size in bytes, where it is one.
    pub size: Option<usize>,
    /// The contract its component was built against (`0.1.0`); none linked in.
    pub contract: Option<String>,
    /// Its component's hash, which what it makes is kept by: a rebuilt extension makes it again, its version the
    /// same. None linked in.
    pub build: Option<String>,
    /// The settings kept for it, by scope (`{ project = { ... }, user = { ... } }`), which it reads as it registers.
    pub settings: toml::Table,
    /// For an unsandboxed extension, its hash: what the user enabled.
    pub trust: Option<String>,
    /// What it depends on and has, once it is to register.
    reaches: Vec<String>,
}

impl From<Extension> for Loadable {
    fn from(extension: Extension) -> Loadable {
        Loadable {
            id: extension.id.to_string(),
            version: extension.version.to_string(),
            requires: extension.requires.iter().map(ToString::to_string).collect(),
            weak: extension.weak.iter().map(ToString::to_string).collect(),
            optional: extension.optional.iter().map(ToString::to_string).collect(),
            provides: extension.provides.iter().map(ToString::to_string).collect(),
            origin: Origin::Linked,
            register: Some(Box::new(move |registry| (extension.register)(registry))),
            component: None,
            size: None,
            contract: None,
            build: None,
            settings: toml::Table::new(),
            trust: None,
            reaches: Vec::new(),
        }
    }
}

/// A loaded extension.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Loaded {
    pub id: String,
    pub version: String,
    /// Its component's hash; none linked in.
    pub build: Option<String>,
    pub origin: Origin,
    /// What it depends on and has (`pe2.images@^0.1`): whose services it may call.
    pub reaches: Vec<String>,
}

/// An extension present, loaded or not: who it is, what became of it, what it registered.
#[derive(Clone, Debug)]
pub struct Present {
    pub id: String,
    pub version: String,
    pub origin: Origin,
    pub requires: Vec<String>,
    /// What it uses where a compatible one is enabled, and is turned off with once linked.
    pub weak: Vec<String>,
    /// What it uses where a compatible one is enabled.
    pub optional: Vec<String>,
    /// The services it offers.
    pub provides: Vec<String>,
    /// What it depends on and was linked to as it loaded, as written.
    pub linked: Vec<String>,
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
    /// For an unsandboxed extension, its hash: what enabling it records.
    pub trust: Option<String>,
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
            weak: extension.weak.clone(),
            optional: extension.optional.clone(),
            provides: extension.provides.clone(),
            linked: vec![],
            status,
            size: extension.size,
            contract: extension.contract.clone(),
            load: None,
            registers: vec![],
            trust: extension.trust.clone(),
        }
    }
}

/// How an extension depends on another. Either a compatible one is enabled, and it is linked, or it is not, whatever the
/// reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Dependency {
    /// Not linked, the extension is refused.
    Required,
    /// Not linked, the extension runs without it. Linked, turning it off turns the extension off too.
    Weak,
    /// Not linked, the extension runs without it. Linked, turning it off leaves the extension on.
    Optional,
}

/// Who provides each service, and at what version.
type Provided = HashMap<String, (String, String)>;

/// How a dependency stands for an extension about to load.
enum Fit {
    /// It is there, and fits.
    Has,
    /// No compatible one is enabled, and the extension does without.
    Without,
    /// It is yet to load.
    Waiting,
    /// The extension cannot load: why.
    Refused(String),
}

/// Dependencies, each as written, and how.
fn dependencies<'a>(requires: &'a [String], weak: &'a [String], optional: &'a [String]) -> impl Iterator<Item = (&'a String, Dependency)> {
    let lists = [(requires, Dependency::Required), (weak, Dependency::Weak), (optional, Dependency::Optional)];
    lists.into_iter().flat_map(|(texts, dependency)| texts.iter().map(move |text| (text, dependency)))
}

impl Present {
    /// What it depends on: each as written, and how.
    pub fn dependencies(&self) -> impl Iterator<Item = (&String, Dependency)> {
        dependencies(&self.requires, &self.weak, &self.optional)
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
    /// An unsandboxed extension the user has not enabled, or - `changed` - that is no longer what they enabled: nothing
    /// of it was run.
    NotEnabled { changed: bool },
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
            Origin::Component(_) | Origin::Install(_) => 2,
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
        // One in a folder goes by the folder.
        let file = |path: &std::path::Path| {
            let named = if path.file_name().is_some_and(|n| n == EXTENSION_COMPONENT) { path.parent().unwrap_or(path) } else { path };
            named.file_name().map_or_default(|n| n.to_string_lossy().into_owned())
        };
        match self {
            Origin::Builtin => "built in".into(),
            Origin::Linked => "linked in".into(),
            Origin::Project { asset, .. } => format!("the project's {asset}"),
            Origin::User(path) => format!("your {}", file(path)),
            Origin::Install(path) => format!("{}, beside the editor", file(path)),
            Origin::Component(path) => file(path),
        }
    }
}

/// The extensions loaded, and what they registered.
#[derive(Default)]
pub struct Extensions {
    /// What files replace assets of kinds, with what.
    recipes: Vec<pe2_ext::Recipe>,
    /// What extensions offer among the editor's commands.
    commands: Vec<pe2_ext::CommandDef>,
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
    /// The types a stored file may be recognized as: those registered with a recognizer.
    recognizers: Vec<TypeDef>,
    ops: HashMap<String, pe2_ext::OpDef>,
    types: HashMap<String, pe2_ext::TypeDef>,
    /// The operations as the work graph runs them, made once loaded.
    graph_ops: std::sync::OnceLock<graph::Ops>,
    /// The store outputs asked for in passing read and keep, once the project is open.
    store: std::sync::OnceLock<graph::store::Store>,
    extractors: Vec<pe2_ext::ExtractorDef>,
    /// The components of the extensions loaded, by id.
    components: Vec<(String, Arc<std::sync::OnceLock<Arc<component::Shared>>>)>,
    /// The formats recognized in files, by their content's hash.
    recognized: Mutex<HashMap<String, Vec<(usize, Recognized)>>>,
    /// Bare names declarations write, to the qualified names of the built-ins registering them.
    kind_aliases: HashMap<String, String>,
    role_aliases: HashMap<String, String>,
    /// Who provides each service: its provider's id.
    providers: HashMap<String, String>,
    /// The streams open, by the key of the output each is: one is shared by its readers while what it read holds.
    streams: Mutex<HashMap<String, (std::sync::Weak<graph::stream::Stream>, graph::exec::Reads)>>,
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

    /// Load `extensions` but those a project leaves off (`disabled`, by id) - and those needing them.
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
        let provided = out.note_providers(&pending);
        let mut off: HashSet<String> = HashSet::new();
        let mut status: HashMap<String, Status> = HashMap::new();
        for extension in pending.iter().filter(|e| disabled.contains(&e.id)) {
            off.insert(extension.id.clone());
            status.insert(extension.id.clone(), Status::Disabled);
        }
        let mut timings: HashMap<String, (std::time::Duration, Registers)> = HashMap::new();
        // Nothing left to do but what waits on another: what waits only on those it can do without goes on without them.
        let mut impatient = false;
        loop {
            let mut progress = false;
            for extension in &mut pending {
                if off.contains(&extension.id) || out.loaded.iter().any(|l| l.id == extension.id) {
                    continue;
                }
                let mut ready = true;
                let mut reaches = Vec::new();
                for (text, dependency) in dependencies(&extension.requires, &extension.weak, &extension.optional) {
                    match out.fit(text, dependency, (&provided, &ids, &off)) {
                        Fit::Has => reaches.push(text.clone()),
                        Fit::Without => {}
                        Fit::Waiting => ready = ready && impatient && dependency != Dependency::Required,
                        Fit::Refused(why) => {
                            out.problems.push(format!("{} is disabled: {why}", extension.id));
                            status.insert(extension.id.clone(), Status::Refused(why));
                            off.insert(extension.id.clone());
                            ready = false;
                            progress = true;
                            break;
                        }
                    }
                }
                if !ready || off.contains(&extension.id) {
                    continue;
                }
                extension.reaches = reaches;
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
            if !progress && impatient {
                break;
            }
            impatient = !progress;
        }
        for extension in &pending {
            let status = status.remove(&extension.id).unwrap_or_else(|| {
                let why = "its requirements form a cycle".to_string();
                out.problems.push(format!("{} is disabled: {why}", extension.id));
                Status::Refused(why)
            });
            let mut present = Present::of(extension, status);
            present.linked.clone_from(&extension.reaches);
            if let Some((load, registers)) = timings.remove(&extension.id) {
                present.load = Some(load);
                present.registers = registers;
            }
            out.present.push(present);
        }
        out
    }


    /// How a dependency of an extension about to load stands, given who provides what, the ids there are and those off.
    fn fit(
        &self,
        text: &str,
        dependency: Dependency,
        (provided, ids, off): (&Provided, &HashSet<String>, &HashSet<String>),
    ) -> Fit {
        let (named, version) = requirement(text);
        let (id, service) = provided.get(named).map_or((named, None), |(id, v)| (id.as_str(), Some(v.as_str())));
        let none = |why: String| if dependency == Dependency::Required { Fit::Refused(why) } else { Fit::Without };
        if !ids.contains(id) {
            return none(format!("it needs {text}, which is not installed"));
        }
        if off.contains(id) {
            return none(format!("it needs {id}, which is disabled"));
        }
        let Some(loaded) = self.loaded.iter().find(|l| l.id == id) else { return Fit::Waiting };
        let installed = service.unwrap_or(&loaded.version);
        match version.filter(|v| !satisfies(installed, v)) {
            Some(version) => none(format!("it needs {named} {version}, and {installed} is installed")),
            None => Fit::Has,
        }
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
            ("operations", listed(parts.ops.iter().map(|o| &o.name).collect())),
            ("types", listed(parts.types.iter().map(|t| &t.name).collect())),
            ("extractors", listed(parts.extractors.iter().map(|e| &e.name).collect())),
            ("panels", listed(parts.panels.iter().map(|p| &p.name).collect())),
            ("tools", listed(parts.tools.iter().map(|t| &t.name).collect())),
            ("services", listed(parts.services.iter().map(|s| &s.name).collect())),
        ];
        let (kinds, roles) = (parts.kinds, parts.roles);
        // What it requires, a service also by its provider.
        let wanted = extension.reaches.iter().map(|r| requirement(r).0);
        let behind: Vec<String> = wanted.clone().filter_map(|r| self.providers.get(r).cloned()).collect();
        let dependencies: Vec<&str> = wanted.chain(behind.iter().map(String::as_str)).collect();
        let mut seen = HashSet::new();
        let names = kinds.iter().map(|k| &k.name).chain(roles.iter().map(|r| &r.name));
        let names = names.chain(parts.panels.iter().map(|p| &p.name)).chain(parts.tools.iter().map(|t| &t.name));
        let names = names.chain(parts.ops.iter().map(|o| &o.name));
        let names = names.chain(parts.types.iter().map(|t| &t.name));
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
        self.check_graph(&extension.id, &dependencies, (&parts.ops, &parts.types, &parts.problems), &kinds)?;
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
        let additions = self.additions(extension, &dependencies, parts.additions)?;
        self.check_stacks(&extension.id, &dependencies, &parts.stacks)?;
        let index = self.loaded.len();
        if let Some(component) = &extension.component {
            self.components.push((extension.id.clone(), component.clone()));
        }
        let (id, version, origin) = (extension.id.clone(), extension.version.clone(), extension.origin.clone());
        let reaches = extension.reaches.clone();
        self.loaded.push(Loaded { id, version, build: extension.build.clone(), origin, reaches });
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
        // Its services, by their names and the services it provides, as its operations are: what requires one names
        // them so.
        let provided: Vec<&str> = extension.provides.iter().map(|p| requirement(p).0).collect();
        for service in parts.services {
            for name in &provided {
                self.services.insert(format!("{name}@{}", service.name.local()), service.clone());
            }
            self.services.insert(service.name.to_string(), service);
        }
        self.recognizers.extend(parts.types.iter().filter(|t| t.recognizable()).cloned());
        // Its operations, by their names and the services it provides: what requires one names them so.
        let offered: Vec<&str> = extension.provides.iter().map(|p| requirement(p).0).collect();
        for op in parts.ops {
            for service in &offered {
                self.ops.insert(format!("{service}@{}", op.name.local()), op.clone());
            }
            self.ops.insert(op.name.to_string(), op);
        }
        self.add_to_kinds(additions, parts.stacks);
        self.recipes.extend(parts.recipes);
        self.commands.extend(parts.commands);
        self.types.extend(parts.types.into_iter().map(|t| (t.name.to_string(), t)));
        self.extractors.extend(parts.extractors);
        Ok(registers)
    }

    /// Who provides each service, and at what version: the first of several, the others said.
    fn note_providers(&mut self, pending: &[Loadable]) -> Provided {
        let mut provided = Provided::new();
        for extension in pending {
            for text in &extension.provides {
                let (service, version) = requirement(text);
                if let Some((first, _)) = provided.get(service) {
                    self.problems.push(format!("{} and {first} both provide {service}: {first} is used", extension.id));
                } else {
                    provided.insert(service.to_string(), (extension.id.clone(), version.unwrap_or("0.0.0").to_string()));
                }
            }
        }
        self.providers = provided.iter().map(|(service, (id, _))| (service.clone(), id.clone())).collect();
        provided
    }

    /// The outputs an extension adds to kinds of what it depends on, checked. One through what it can do without and
    /// does not have is not there.
    fn additions(
        &self,
        extension: &Loadable,
        dependencies: &[&str],
        additions: Vec<(String, pe2_ext::OutputDef)>,
    ) -> Result<Vec<(String, pe2_ext::OutputDef)>> {
        let id = extension.id.as_str();
        let absent = |owner: &str| {
            !dependencies.contains(&owner) && extension.weak.iter().chain(&extension.optional).any(|d| requirement(d).0 == owner)
        };
        let mut added = Vec::new();
        for (kind, output) in additions {
            let owner = kind.split_once('@').map_or("", |(owner, _)| owner);
            let made_by = output.op.split_once('@').map_or("", |(owner, _)| owner);
            if absent(owner) || absent(made_by) {
                continue;
            }
            anyhow::ensure!(dependencies.contains(&owner), "{id} adds to {kind}, which it does not require");
            anyhow::ensure!(
                made_by == id || dependencies.contains(&made_by),
                "{id}'s output {} on {kind} is made by {}, which neither it nor what it requires registers",
                output.name,
                output.op
            );
            anyhow::ensure!(self.kinds.contains_key(&kind), "{id}: no kind {kind}");
            added.push((kind, output));
        }
        Ok(added)
    }

    /// The stacks an extension gives kinds, checked: of what it depends on.
    fn check_stacks(&self, id: &str, dependencies: &[&str], stacks: &[(String, String)]) -> Result<()> {
        for (kind, _) in stacks {
            let owner = kind.split_once('@').map_or("", |(owner, _)| owner);
            anyhow::ensure!(dependencies.contains(&owner) && self.kinds.contains_key(kind), "{id} gives {kind} a stack, which it does not require");
        }
        Ok(())
    }

    /// Outputs and stacks added to kinds, checked; a kind keeps the stack it has.
    fn add_to_kinds(&mut self, additions: Vec<(String, pe2_ext::OutputDef)>, stacks: Vec<(String, String)>) {
        for (kind, output) in additions {
            if let Some((def, _)) = self.kinds.get_mut(&kind) {
                def.outputs.push(output);
            }
        }
        for (kind, stack) in stacks {
            if let Some((def, _)) = self.kinds.get_mut(&kind) {
                def.stack.get_or_insert(stack);
            }
        }
    }

    /// What an extension registers for the work graph, checked: its registration's own problems; that what its
    /// operations take and make, and what its kinds' outputs are made by, is its own or what it requires registers.
    fn check_graph(
        &self,
        id: &str,
        dependencies: &[&str],
        (ops, types, problems): (&[pe2_ext::OpDef], &[pe2_ext::TypeDef], &[String]),
        kinds: &[KindDef],
    ) -> Result<()> {
        let reachable = |owner: &str, known: bool| owner == id && known || dependencies.contains(&owner);
        anyhow::ensure!(problems.is_empty(), "{}", problems.join("; "));
        // What its operations take and make, and what its kinds' outputs are made by: its own, or what it requires.
        for op in ops {
            let mut named = Vec::new();
            for ty in op.inputs.iter().map(|p| &p.ty).chain([&op.output]) {
                named_types(ty, &mut named);
            }
            // A media type is no extension's: any may claim a file answers to it.
            for ty in named.into_iter().filter(|t| !t.contains('/')) {
                let owner = ty.split_once('@').map_or("", |(owner, _)| owner);
                let own = types.iter().any(|t| t.name.to_string() == ty);
                anyhow::ensure!(
                    reachable(owner, own) && (own || self.types.contains_key(&ty)),
                    "{} takes or makes {ty}, which neither it nor what it requires registers",
                    op.name
                );
            }
        }
        for (kind, output) in kinds.iter().flat_map(|k| k.outputs.iter().map(move |o| (k, o))) {
            let owner = output.op.split_once('@').map_or("", |(owner, _)| owner);
            let own = ops.iter().any(|o| o.name.to_string() == output.op);
            anyhow::ensure!(
                reachable(owner, own) && (own || self.ops.contains_key(&output.op)),
                "{}'s output {} is made by {}, which neither it nor what it requires registers",
                kind.name,
                output.name,
                output.op
            );
        }
        Ok(())
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

    /// An extension's version as what it makes is kept by: its version and its build (its component's hash), and the
    /// project's settings it took where any is not its default - another value of one makes other assets, so other
    /// keys.
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
        // Its build: a component rebuilt is another extension to what it made, its version the same.
        let version = loaded.build.as_ref().map_or_else(|| loaded.version.clone(), |b| format!("{} {b}", loaded.version));
        if changed.is_empty() { version } else { format!("{version} {}", changed.join(" ")) }
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

    /// The outputs an asset's kind adds after its stack, as its plan lowers them - then the host's thumbnail (the
    /// first that is a picture, made small, kept).
    #[must_use]
    /// The stack a file makes of an asset, by what the extensions say of its kind and the file's name, its file named in it.
    /// None where nothing does.
    ///
    /// # Errors
    /// One does, by an operation no extension provides.
    pub fn recipe_for(&self, asset: &index::Asset, file: &str) -> Option<std::result::Result<String, String>> {
        let kind = self.kind_of(asset)?;
        let lower = file.to_lowercase();
        let recipe = self.recipes.iter().find(|r| r.kind == kind.name.to_string() && name_matches(&r.pattern, &lower))?;
        let stack = recipe.stack.replace("{file}", file);
        let table: Table = match stack.parse() {
            Ok(table) => table,
            Err(e) => return Some(Err(format!("{}'s recipe does not read: {e}", recipe.kind))),
        };
        let ops = self.operations();
        let name_of = |t: &toml::Value| t.get("op").and_then(toml::Value::as_str).map(str::to_string);
        let layers = table.get("stack").and_then(toml::Value::as_array).map_or(&[][..], Vec::as_slice);
        let inner = layers.iter().filter_map(|l| l.get("ops")?.as_table()).flat_map(|o| o.values());
        let missing = layers.iter().chain(inner).filter_map(name_of).find(|name| name != graph::GRAPH && ops.get(name).is_none());
        Some(missing.map_or(Ok(stack), |name| Err(format!("It takes {name}, which no extension provides"))))
    }

    pub fn tail(&self, asset: &index::Asset) -> Vec<graph::Tail> {
        let Some(kind) = self.kind_of(asset) else { return Vec::new() };
        let from = |o: &pe2_ext::OutputDef| o.from.as_ref().and_then(|name| kind.outputs.iter().position(|o| o.name == *name));
        let mut tail: Vec<graph::Tail> = kind
            .outputs
            .iter()
            .map(|o| graph::Tail {
                name: o.name.clone(),
                fixed: false,
                op: o.op.clone(),
                retain: o.retain,
                from: from(o),
                settings: Table::new(),
            })
            .collect();
        if let Some(pictured) = kind.outputs.iter().position(|o| self.makes_pictures(o)) {
            let mut settings = Table::new();
            settings.insert("side".into(), Value::Integer(THUMBNAIL_SIDE));
            let (name, op) = (graph::THUMBNAIL.to_string(), graph::THUMBNAIL.to_string());
            // One picture: of its first look, which `thumbnail` supplies when it is asked for.
            tail.push(graph::Tail { name, fixed: true, op, retain: pe2_ext::Retain::Cache, from: Some(pictured), settings });
        }
        tail
    }

    /// Whether a kind's output is a picture: its operation makes an image.
    fn makes_pictures(&self, output: &pe2_ext::OutputDef) -> bool {
        self.ops.get(&output.op).is_some_and(|op| op.output == pe2_ext::Type::Image)
    }

    /// An asset's plan: its stack, then its kind's outputs; and what is wrong with it.
    #[must_use]
    pub fn plan(&self, asset: &index::Asset) -> (graph::Plan, Vec<graph::Problem>) {
        // Its stack as last applied.
        let fields = graph::exec::Node::fields(asset);
        graph::Plan::of(fields, &self.operations(), &self.tail(asset))
    }

    /// An asset's plan as its declaration is written, applied or not: what its stack would do, for whoever edits it.
    #[must_use]
    pub fn plan_written(&self, asset: &index::Asset) -> (graph::Plan, Vec<graph::Problem>) {
        graph::Plan::of(asset.fields(), &self.operations(), &self.tail(asset))
    }

    /// The operations a stack's layer may be that take what comes down to it, each by the name it goes by - a service's,
    /// where it provides one - and its label, in the labels' order. One taking several inputs belongs in a graph layer.
    #[must_use]
    pub fn layers_taking(&self, above: &pe2_ext::Type) -> Vec<(String, String)> {
        let fits = |def: &pe2_ext::OpDef| matches!(def.inputs.as_slice(), [port] if port.ty.unify(above, &mut Vec::new()));
        let by_service = |def: &pe2_ext::OpDef| self.ops.iter().any(|(name, other)| other.name == def.name && *name != def.name.to_string());
        let named = self.ops.iter().filter(|(name, def)| fits(def) && (**name != def.name.to_string() || !by_service(def)));
        let mut layers: Vec<(String, String)> = named.map(|(name, def)| (name.clone(), def.label.clone())).collect();
        layers.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        layers
    }

    /// The operations a graph layer may hold, each by the name it goes by and its label, in the labels' order.
    #[must_use]
    pub fn graph_operations(&self) -> Vec<(String, String)> {
        let by_service = |name: &str| self.ops.iter().any(|(other, def)| def.name.to_string() == name && other != name);
        let operations = self.operations();
        let shown = operations.iter().filter(|(name, _)| name.as_str() != graph::GRAPH && !by_service(name));
        let mut listed: Vec<(String, String)> = shown.map(|(name, def)| (name.clone(), def.label.clone())).collect();
        listed.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        listed
    }

    /// The assets whose outputs read those of these, through others or not: each referring to one whose stack reads
    /// the outputs of what it refers to (a `references` layer). These not among them, unless they read each other.
    #[must_use]
    pub fn dependents(&self, index: &index::Index, paths: &std::collections::BTreeSet<String>) -> std::collections::BTreeSet<String> {
        let mut found = std::collections::BTreeSet::new();
        let mut next: Vec<&str> = paths.iter().map(String::as_str).collect();
        while let Some(path) = next.pop() {
            let referrers = index.referenced_by.get(path).into_iter().flatten().map(|r| r.from.as_str());
            for from in referrers {
                if found.contains(from) {
                    continue;
                }
                let Some(asset) = index.assets.get(from) else { continue };
                let (plan, _) = self.plan(asset);
                if plan.steps.iter().any(|s| s.op == graph::REFERENCES) {
                    found.insert(from.to_string());
                    next.push(from);
                }
            }
        }
        found
    }

    /// A stream output of an asset of `index`, by its address, open: shared with whatever reads it already, while what
    /// it read holds; else opened now.
    ///
    /// # Errors
    /// It has no such output, it is no stream, or it could not be opened.
    pub fn stream(
        &self,
        engine: &graph::exec::Engine,
        index: &index::Index,
        asset: &index::Asset,
        address: &str,
    ) -> Result<Arc<graph::stream::Stream>> {
        let (plan, _) = self.plan(asset);
        let world = Graph { index, extensions: self };
        let reach = graph::exec::Reach { engine, world: &world };
        let (output, parameter) = plan.output(address).with_context(|| format!("{} has no output `{address}`", asset.path))?;
        let key = engine.key(&plan, asset, output, parameter.as_deref());
        let mut streams = self.streams.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        streams.retain(|_, (stream, _)| stream.strong_count() > 0);
        if let Some((stream, reads)) = streams.get(&key)
            && let Some(stream) = stream.upgrade()
            && reads.hold(asset, Some(reach))
        {
            return Ok(stream);
        }
        drop(streams);
        let (producer, reads, key) = engine.open_stream(Some(reach), &plan, asset, address)?;
        let stream = graph::stream::Stream::open(producer);
        self.streams.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(key, (Arc::downgrade(&stream), reads));
        Ok(stream)
    }

    /// An output of an asset of `index`, by its address (`native`, `picture/night`), through `engine`: what it reads
    /// of other nodes made in the index.
    ///
    /// # Errors
    /// It has no such output, or it could not be made.
    pub fn output(
        &self,
        engine: &graph::exec::Engine,
        index: &index::Index,
        asset: &index::Asset,
        address: &str,
        read: bool,
    ) -> Result<graph::exec::Given> {
        let (plan, _) = self.plan(asset);
        let world = Graph { index, extensions: self };
        let reach = graph::exec::Reach { engine, world: &world };
        engine.output_in(Some(reach), &plan, asset, address, None, read)
    }

    /// An engine for what is asked now, in passing: the extensions' operations, reading what the project's store
    /// keeps (`keep_in`), keeping what it is asked to.
    #[must_use]
    pub fn engine(&self) -> graph::exec::Engine {
        graph::exec::Engine::new(self.operations(), self.store.get().cloned())
    }

    /// The store outputs are kept in, for what is asked in passing: the project's, once it is open.
    pub fn keep_in(&self, store: graph::store::Store) {
        let _ = self.store.set(store);
    }

    /// Whether a kind's output is a family: its operation has a parameter.
    fn family(&self, output: &pe2_ext::OutputDef) -> bool {
        self.ops.get(&output.op).is_some_and(|op| op.parameter.is_some())
    }

    /// The output of an asset's kind it is shown by - the first that is a picture - with its members (one, unnamed,
    /// where it is no family).
    fn pictured(&self, asset: &index::Asset) -> Option<(&pe2_ext::OutputDef, Vec<(String, String)>)> {
        let output = self.kind_of(asset)?.outputs.iter().find(|o| self.makes_pictures(o))?;
        let members = match &output.members {
            Some(members) => members(&self.lend(asset)),
            None if !self.family(output) => vec![(String::new(), output.name.clone())],
            None => Vec::new(),
        };
        Some((output, members))
    }

    /// The first of an asset's kind's outputs that is a stream, by name: what plays.
    #[must_use]
    pub fn stream_output(&self, asset: &index::Asset) -> Option<String> {
        let kind = self.kind_of(asset)?;
        let stream = kind.outputs.iter().find(|o| self.ops.get(&o.op).is_some_and(|op| op.output.is_stream()))?;
        Some(stream.name.clone())
    }

    /// The output of an asset's kind saying what is marked on its pictures, by name.
    #[must_use]
    pub fn marks_output(&self, asset: &index::Asset) -> Option<String> {
        let kind = self.kind_of(asset)?;
        let marks = kind.outputs.iter().find(|o| self.ops.get(&o.op).is_some_and(|op| op.output == pe2_ext::Type::Marks))?;
        Some(marks.name.clone())
    }

    /// Whether an asset's kind shows its nodes by a picture: it outputs one.
    #[must_use]
    pub fn shows_pictures(&self, asset: &index::Asset) -> bool {
        self.kind_of(asset).is_some_and(|k| k.outputs.iter().any(|o| self.makes_pictures(o)))
    }

    /// Whether an asset has a picture.
    #[must_use]
    pub fn has_picture(&self, asset: &index::Asset) -> bool {
        self.pictured(asset).is_some_and(|(_, members)| !members.is_empty())
    }

    /// An asset's looks - the members of the picture it is shown by - by name.
    #[must_use]
    pub fn looks(&self, asset: &index::Asset) -> Vec<String> {
        self.pictured(asset).map_or_default(|(_, members)| members.into_iter().map(|(_, label)| label).collect())
    }

    /// The address of an asset's picture in one of its looks (its first, past them).
    #[must_use]
    pub fn picture_address(&self, asset: &index::Asset, look: usize) -> Option<String> {
        let (output, members) = self.pictured(asset)?;
        let (key, _) = members.get(look).or_else(|| members.first())?;
        Some(if self.family(output) { format!("{}/{key}", output.name) } else { output.name.clone() })
    }

    /// An asset's picture in one of its looks, made now; none when it has none.
    ///
    /// # Errors
    /// It could not be made.
    pub fn picture(&self, asset: &index::Asset, look: usize) -> Option<Result<Image>> {
        let address = self.picture_address(asset, look)?;
        Some(self.image(asset, &address))
    }

    /// An output that is an image, made now (of its node alone).
    fn image(&self, asset: &index::Asset, address: &str) -> Result<Image> {
        let (plan, _) = self.plan(asset);
        let given = self.engine().output(&plan, asset, address, true)?;
        match given.value.as_deref() {
            Some(graph::Value::Image(image)) => Ok(image.clone()),
            _ => anyhow::bail!("{address} is no picture"),
        }
    }

    /// Whether an asset may have a thumbnail: its kind shows its nodes by a picture. (Whether this one has one is known
    /// once it is asked for: `thumbnail`.)
    #[must_use]
    pub fn has_thumbnail(&self, asset: &index::Asset) -> bool {
        self.shows_pictures(asset)
    }

    /// An asset's thumbnail, through `engine`: its picture in its first look, made small, kept.
    ///
    /// # Errors
    /// It has no picture, or it could not be made.
    pub fn thumbnail(&self, engine: &graph::exec::Engine, asset: &index::Asset) -> Result<graph::exec::Given> {
        let (output, members) = self.pictured(asset).context("it has no picture")?;
        let first = members.first().filter(|_| self.family(output)).map(|(key, _)| key.as_str());
        let (plan, _) = self.plan(asset);
        engine.output_with(&plan, asset, graph::THUMBNAIL, first, true)
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

    /// An asset's inspector views with a tab of their own, by their place among its inspector views: those it has what brings.
    #[must_use]
    pub fn inspector_tabs(&self, asset: &index::Asset) -> Vec<(usize, &ViewDef)> {
        self.inspector(asset).into_iter().enumerate().filter(|(_, view)| view.tab.is_some() && brought(view, asset)).collect()
    }

    /// The editors its kind and role give an asset, of those it has what brings.
    #[must_use]
    pub fn editors(&self, asset: &index::Asset) -> Vec<&ViewDef> {
        let kind = self.kind_of(asset).map_or(&[][..], |k| k.editors.as_slice());
        let role = self.role_of(asset).map_or(&[][..], |r| r.editors.as_slice());
        kind.iter().chain(role).filter(|view| brought(view, asset)).collect()
    }

    /// The name of every editor any kind or role gives.
    #[must_use]
    pub fn editor_names(&self) -> std::collections::BTreeSet<&str> {
        let kinds = self.kinds.values().flat_map(|(kind, _)| &kind.editors);
        let roles = self.roles.values().flat_map(|role| &role.editors);
        kinds.chain(roles).map(|view| view.name.as_str()).collect()
    }

    /// A part of an asset as a picture (`palettes/<name>`): the member of a family of its kind's pictures keyed by it;
    /// none where none is.
    #[must_use]
    pub fn part(&self, asset: &index::Asset, part: &str) -> Option<Result<Image>> {
        let lent = self.lend(asset);
        let output = self.kind_of(asset)?.outputs.iter().find(|o| {
            self.makes_pictures(o) && o.members.as_ref().is_some_and(|m| m(&lent).iter().any(|(key, _)| key == part))
        })?;
        Some(self.image(asset, &format!("{}/{part}", output.name)))
    }

    /// An asset's parts, as its kind lists them.
    #[must_use]
    pub fn parts(&self, asset: &index::Asset) -> Vec<pe2_ext::Part> {
        self.kind_of(asset).and_then(|k| k.parts.as_ref()).map_or_default(|parts| parts(&self.lend(asset)))
    }

    /// What is inside an asset: what its kind says, else its parts, under a heading for each group of them.
    #[must_use]
    pub fn contents(&self, asset: &index::Asset) -> pe2_ext::Contents {
        if let Some(contents) = self.kind_of(asset).and_then(|k| k.contents.as_ref()) {
            return contents(&self.lend(asset));
        }
        pe2_ext::Contents { rows: rows_of_parts(&self.parts(asset)), ..pe2_ext::Contents::default() }
    }

    /// What comes of something done in an asset's contents.
    ///
    /// # Errors
    /// Why it cannot be, as its kind says.
    pub fn content_act(&self, asset: &index::Asset, request: pe2_ext::ContentRequest) -> Result<pe2_ext::ContentOutcome> {
        let act = self.kind_of(asset).and_then(|k| k.content_act.as_ref()).context("its kind does nothing with its contents")?;
        act(&self.lend(asset), request)
    }

    /// A part's fields, where its kind says what they are.
    #[must_use]
    pub fn part_sheet(&self, asset: &index::Asset, part: &str) -> Option<pe2_ext::PartSheet> {
        self.kind_of(asset)?.part_sheet.as_ref()?(&self.lend(asset), part)
    }

    /// The views a kind and a role give the head of the contents pane.
    #[must_use]
    pub fn contents_views(&self, asset: &index::Asset) -> Vec<&ViewDef> {
        let kind = self.kind_of(asset).map_or(&[][..], |k| k.contents_views.as_slice());
        let role = self.role_of(asset).map_or(&[][..], |r| r.contents_views.as_slice());
        kind.iter().chain(role).collect()
    }

    /// Whether an asset's kind lists its parts.
    #[must_use]
    pub fn lists_parts(&self, asset: &index::Asset) -> bool {
        self.kind_of(asset).is_some_and(|k| k.parts.is_some())
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
            || self.loaded.iter().find(|l| l.id == caller).is_some_and(|l| {
                l.reaches.iter().map(|r| requirement(r).0).any(|r| r == owner || self.providers.get(r).is_some_and(|p| p == owner))
            })
    }

    /// The tools the extensions registered, for agents.
    #[must_use]
    pub fn tools(&self) -> &[ToolDef] {
        &self.tools
    }

    #[must_use]
    pub fn commands(&self) -> &[pe2_ext::CommandDef] {
        &self.commands
    }

    /// Every asset of a kind, by its qualified name or a built-in's bare one.
    pub fn find<'a>(&'a self, index: &'a index::Index, kind: &str) -> impl Iterator<Item = &'a index::Asset> {
        let wanted = self.kind_named(kind).map(|(k, _)| k.name.as_str());
        index.assets.values().map(|a| &**a).filter(move |a| {
            wanted.is_some_and(|w| self.kind_of(a).is_some_and(|k| k.name.as_str() == w))
                || wanted.is_none() && a.kind.as_deref() == Some(kind)
        })
    }

    /// The stack a kind's nodes have where their bundles state none (`[[stack]]` tables), by the name a declaration
    /// writes it by.
    #[must_use]
    pub fn kind_stack(&self, declared: &str) -> Option<&str> {
        self.kind_named(declared)?.0.stack.as_deref()
    }

    /// An asset's import: the operation its stack takes its source through.
    #[must_use]
    pub fn import_op(&self, asset: &index::Asset) -> Option<&pe2_ext::OpDef> {
        self.ops.get(graph::import_of(asset.fields())?.op)
    }

    /// An operation, by its qualified name.
    #[must_use]
    pub fn op(&self, name: &str) -> Option<&pe2_ext::OpDef> {
        self.ops.get(name)
    }

    /// The operations stacks are made of: the host's (a stored file - typed by the formats it is recognized as -
    /// constants, outputs, the thumbnail) and the extensions', each lent its node as it runs.
    #[must_use]
    pub fn operations(&self) -> graph::Ops {
        self.graph_ops.get_or_init(|| self.make_operations()).clone()
    }

    fn make_operations(&self) -> graph::Ops {
        let mut ops = graph::Ops::default();
        let recognizers = Arc::new(self.recognizers.clone());
        let any_file = pe2_ext::Type::File(Box::new(pe2_ext::Type::Any));
        let by_name = recognizers.clone();
        let named = move |settings: &toml::Table| named_type(&by_name, settings.get("file")?.as_str()?);
        let stored = graph::OpDef::new(graph::FILE, "Stored file", "1").output_of(named).makes(any_file, move |call| {
            let name = call.setting_str("file")?.to_string();
            let node = call.recorder().node();
            let head = node.read_head(&name, HEAD);
            let size = node.file_size(&name).unwrap_or_default();
            let known = recognize_head(&recognizers, &name, size, &head);
            call.stored(&name, type_names(known.iter().map(|(i, _)| &recognizers[*i]))).map(graph::Value::Typed)
        });
        ops.add(stored);
        // Declared kinds and roles to the qualified names an operation is told.
        let mut qualified: HashMap<String, String> = self.kinds.keys().map(|k| (k.clone(), k.clone())).collect();
        qualified.extend(self.kind_aliases.iter().map(|(a, q)| (a.clone(), q.clone())));
        qualified.extend(self.roles.keys().map(|r| (r.clone(), r.clone())));
        qualified.extend(self.role_aliases.iter().map(|(a, q)| (a.clone(), q.clone())));
        let qualified = Arc::new(qualified);
        for (name, def) in &self.ops {
            let version = self.version_of(def.name.extension());
            let mut op = graph::OpDef::new(name, &def.label, &version);
            op.inputs.clone_from(&def.inputs);
            op.parameter = def.parameter.as_ref().map(|(name, _)| name.clone());
            let (run, qualified) = (def.run.clone(), qualified.clone());
            if let (Some(start), pe2_ext::Type::Stream(item)) = (def.start.clone(), &def.output) {
                ops.add(op.streams((**item).clone(), move |call| start_stream(call, &qualified, &*start)));
                continue;
            }
            ops.add(op.makes(def.output.clone(), move |call| {
                let inputs = call.take_inputs();
                let hosted = Hosted::new(call, &qualified);
                let mut lent = pe2_ext::Call { inputs, settings: call.settings(), parameter_text: call.parameter(), host: &hosted };
                run(&mut lent)
            }));
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

    /// The formats a file is, by signatures and sniffs: from its first bytes, read once a content.
    #[must_use]
    pub fn recognize(&self, file: &index::BundleFile) -> Vec<(&TypeDef, Recognized)> {
        let known = self.recognized.lock().ok().and_then(|r| r.get(&file.sha1).cloned());
        let found = known.unwrap_or_else(|| {
            let found = self.recognize_now(file);
            if let Ok(mut recognized) = self.recognized.lock() {
                recognized.insert(file.sha1.clone(), found.clone());
            }
            found
        });
        found.into_iter().map(|(i, r)| (&self.recognizers[i], r)).collect()
    }

    fn recognize_now(&self, file: &index::BundleFile) -> Vec<(usize, Recognized)> {
        use std::io::Read;
        let mut head = Vec::new();
        if let Ok(f) = std::fs::File::open(&file.path) {
            let _ = f.take(HEAD as u64).read_to_end(&mut head);
        }
        recognize_head(&self.recognizers, &file.name, file.size, &head)
    }
}

/// The types a type names, within unions, lists and files.
fn named_types(ty: &pe2_ext::Type, out: &mut Vec<String>) {
    match ty {
        pe2_ext::Type::Named(name) => out.push(name.clone()),
        pe2_ext::Type::Union(any) => any.iter().for_each(|t| named_types(t, out)),
        pe2_ext::Type::List(of) | pe2_ext::Type::File(of) => named_types(of, out),
        _ => {}
    }
}

/// Whether a port's type names types: not anything, nor bytes, nor a variable.
fn specific(ty: &pe2_ext::Type) -> bool {
    match ty {
        pe2_ext::Type::Named(_) => true,
        pe2_ext::Type::Union(any) => any.iter().any(specific),
        pe2_ext::Type::File(of) => specific(of),
        _ => false,
    }
}

/// How much of a file its types are recognized by: its first bytes.
const HEAD: usize = 4096;

/// The thumbnail's longer side, in pixels: twice the tree's row, for scaled displays.
pub const THUMBNAIL_SIDE: i64 = 32;

/// The names a file recognized as these types answers to: each type's, then its media type.
fn type_names<'a>(types: impl Iterator<Item = &'a TypeDef>) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for ty in types {
        for name in std::iter::once(ty.name.to_string()).chain(ty.media_type.clone()) {
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    names
}

/// The types a file of a name and size is recognized as, by its first bytes: each by its index among `types`.
/// Whether an asset has a field that brings a view: any where none is named.
fn brought(view: &ViewDef, asset: &index::Asset) -> bool {
    let settings = asset.settings();
    view.tab.as_ref().is_none_or(|tab| tab.fields.is_empty() || tab.fields.iter().any(|field| settings.contains_key(field)))
}

/// Parts as rows: a group's (`cells/` of `cells/cursor`) under its heading, those of no group as they are.
fn rows_of_parts(parts: &[pe2_ext::Part]) -> Vec<pe2_ext::ContentRow> {
    let mut rows = Vec::new();
    let mut group = "";
    for part in parts {
        let within = part.id.split_once('/').map(|(within, _)| within);
        if let Some(within) = within
            && within != group
        {
            group = within;
            let mut letters = within.chars();
            let label: String = letters.next().map_or_else(String::new, |first| first.to_uppercase().chain(letters).collect());
            rows.push(pe2_ext::ContentRow::heading(within, &label));
        }
        rows.push(pe2_ext::ContentRow::new(&part.id, &part.label).depth(u32::from(within.is_some())));
    }
    rows
}

/// Whether a file's name is a pattern's: `*.png`, or the name itself.
fn name_matches(pattern: &str, name: &str) -> bool {
    match pattern.strip_prefix('*') {
        Some(suffix) => name.ends_with(suffix),
        None => name == pattern,
    }
}

/// What a stored file is by its name alone, as a stack's shape tells it: a file of the types named so. Other signatures
/// need its bytes, so a name nothing claims says nothing - and a name is a hint, the bytes told when it runs.
fn named_type(types: &[TypeDef], name: &str) -> Option<pe2_ext::Type> {
    use pe2_ext::{Signature, Type};
    let claims = |pattern: &str| pattern.strip_prefix('*').map_or(name == pattern, |suffix| name.ends_with(suffix));
    let claimed = types.iter().filter(|t| t.signatures.iter().any(|s| matches!(s, Signature::Name(p) if claims(p))));
    let mut answers: Vec<Type> = type_names(claimed).into_iter().map(Type::Named).collect();
    let of = match answers.len() {
        0 => return None,
        1 => answers.remove(0),
        _ => Type::Union(answers),
    };
    Some(Type::File(Box::new(of)))
}

fn recognize_head(types: &[TypeDef], name: &str, size: u64, head: &[u8]) -> Vec<(usize, Recognized)> {
    let text = match std::str::from_utf8(head) {
        Ok(t) => !t.contains('\0'),
        // A cut through a character at the end of what was read: what is before it decides.
        Err(e) => e.error_len().is_none() && !head[..e.valid_up_to()].contains(&0),
    };
    let named = |pattern: &str| name_matches(pattern, name);
    let mut out = vec![];
    for (i, format) in types.iter().enumerate() {
        let signed = format.signatures.iter().any(|signature| match signature {
            Signature::Bytes { offset, bytes } => head.get(*offset..offset + bytes.len()) == Some(bytes.as_slice()),
            Signature::Name(pattern) => named(pattern),
            Signature::Text => text,
        });
        let recognized = if signed { Some(Recognized::default()) } else { format.sniff.as_ref().and_then(|sniff| sniff(name, size, head)) };
        out.extend(recognized.map(|r| (i, r)));
    }
    out
}

impl Extensions {
    /// The operations that take an asset's source - the stored file its stack takes - by the types it is recognized
    /// as: those whose first port names one of them (not those taking anything), the highest priority first.
    #[must_use]
    pub fn ops_for(&self, asset: &index::Asset) -> Vec<&pe2_ext::OpDef> {
        use pe2_ext::Type;
        let Some(source) = graph::import_of(asset.fields()).map(|i| i.source) else { return vec![] };
        let Some(file) = asset.files.iter().find(|f| f.name == source && f.made_by.is_none()) else { return vec![] };
        let names = type_names(self.recognize(file).into_iter().map(|(t, _)| t));
        let given = Type::Union(names.into_iter().map(Type::Named).collect());
        let mut out: Vec<&pe2_ext::OpDef> = self
            .ops
            .values()
            .filter(|op| op.inputs.first().is_some_and(|p| specific(&p.ty) && p.ty.unify(&given, &mut Vec::new())))
            .collect();
        out.sort_by(|a, b| b.priority.cmp(&a.priority).then_with(|| a.name.cmp(&b.name)));
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

    fn call(&self, service: &str, arguments: &Value) -> Result<Value> {
        let def = self.extensions.service(service).with_context(|| format!("no service {service}"))?;
        (def.run)(self, arguments).with_context(|| service.to_string())
    }

    fn may_call(&self, caller: &str, service: &str) -> bool {
        self.extensions.may_call(caller, service)
    }
}

/// The project's nodes as the work graph reaches them: by reference in the index, each with its plan.
pub struct Graph<'a> {
    pub index: &'a index::Index,
    pub extensions: &'a Extensions,
}

impl graph::exec::World for Graph<'_> {
    fn node(&self, reference: &toml::Value) -> Option<(&dyn graph::exec::Node, graph::Plan)> {
        let asset: &index::Asset = if let Some(version) = reference.as_str().filter(|r| r.starts_with(index::BELOW)) {
            self.index.version(version)?
        } else if let Some(path) = reference.as_str().filter(|r| !r.starts_with("uid://")) {
            self.index.assets.get(path)?
        } else {
            let (uid, path) = index::reference(reference)?;
            self.index.resolve(&uid, path.as_deref().unwrap_or_default())?
        };
        let (plan, _) = self.extensions.plan(asset);
        Some((asset as &dyn graph::exec::Node, plan))
    }
}

/// What an extension's operation is lent of its node: its path, kind and role, its declared settings (recorded as
/// read when it is lent), and contents read by their hashes.
struct Hosted<'a> {
    call: &'a graph::exec::Call<'a>,
    kind: Option<&'a str>,
    role: Option<&'a str>,
}

impl<'a> Hosted<'a> {
    fn new(call: &'a graph::exec::Call<'a>, qualified: &'a HashMap<String, String>) -> Hosted<'a> {
        let recorder = call.recorder();
        recorder.declaration();
        let fields = recorder.node().fields();
        let named = |key: &str| fields.get(key).and_then(Value::as_str).and_then(|n| qualified.get(n)).map(String::as_str);
        Hosted { call, kind: named("kind"), role: named("role") }
    }
}

/// Start an extension's stream operation: its generator given what the call is, then kept with what it reads by - its
/// node's settings, and the contents its inputs held, where they are - for the stream's life past the call.
fn start_stream(
    call: &mut graph::exec::Call<'_>,
    qualified: &HashMap<String, String>,
    start: &pe2_ext::StartStream,
) -> Result<Box<dyn graph::stream::Producer>> {
    let inputs = call.take_inputs();
    let mut stored = Vec::new();
    for value in inputs.iter().flatten() {
        graph::exec::stored_contents(value, &mut stored);
    }
    let contents: HashMap<String, std::path::PathBuf> =
        stored.into_iter().filter_map(|sha1| Some((sha1.clone(), call.content_path(&sha1)?))).collect();
    let hosted = Hosted::new(call, qualified);
    let mut lent = pe2_ext::Call { inputs, settings: call.settings(), parameter_text: call.parameter(), host: &hosted };
    let generator = start(&mut lent)?;
    let node = call.recorder().node();
    let host = StreamHost {
        path: node.path().to_string(),
        kind: hosted.kind.map(str::to_string),
        role: hosted.role.map(str::to_string),
        fields: node.fields().clone(),
        contents,
    };
    let (settings, parameter) = (call.settings().clone(), call.parameter().map(str::to_string));
    Ok(Box::new(Generating { generator, host, settings, parameter }))
}

/// What a stream's generator is lent between its calls: its node's path, kind, role and settings, and the contents its
/// inputs held, read where they are.
struct StreamHost {
    path: String,
    kind: Option<String>,
    role: Option<String>,
    fields: Table,
    contents: HashMap<String, std::path::PathBuf>,
}

impl StreamHost {
    fn path_of(&self, sha1: &str) -> Result<&std::path::Path> {
        self.contents.get(sha1).map(std::path::PathBuf::as_path).with_context(|| format!("{}: no content {sha1}", self.path))
    }
}

impl pe2_ext::CallHost for StreamHost {
    fn path(&self) -> &str {
        &self.path
    }

    fn path_of(&self, content: &pe2_ext::Content) -> Option<String> {
        match content {
            pe2_ext::Content::Inline(_) => None,
            pe2_ext::Content::Stored { sha1, .. } => StreamHost::path_of(self, sha1).ok().map(|p| p.display().to_string()),
        }
    }

    fn kind(&self) -> Option<&str> {
        self.kind.as_deref()
    }

    fn role(&self) -> Option<&str> {
        self.role.as_deref()
    }

    fn fields(&self) -> &Table {
        &self.fields
    }

    fn read(&self, content: &pe2_ext::Content) -> Result<Vec<u8>> {
        match content {
            pe2_ext::Content::Inline(bytes) => Ok(bytes.clone()),
            pe2_ext::Content::Stored { sha1, .. } => {
                let path = self.path_of(sha1)?;
                std::fs::read(path).with_context(|| path.display().to_string())
            }
        }
    }

    fn read_range(&self, content: &pe2_ext::Content, offset: u64, len: usize) -> Result<Vec<u8>> {
        use std::io::{Read, Seek, SeekFrom};
        match content {
            pe2_ext::Content::Inline(bytes) => {
                let start = usize::try_from(offset).unwrap_or(usize::MAX).min(bytes.len());
                Ok(bytes[start..start.saturating_add(len).min(bytes.len())].to_vec())
            }
            pe2_ext::Content::Stored { sha1, .. } => {
                let path = self.path_of(sha1)?;
                let mut file = std::fs::File::open(path).with_context(|| path.display().to_string())?;
                file.seek(SeekFrom::Start(offset))?;
                let mut out = Vec::new();
                file.take(len as u64).read_to_end(&mut out)?;
                Ok(out)
            }
        }
    }
}

/// An extension's generator as the host drives it: each call given its node as `StreamHost` lends it.
struct Generating {
    generator: Box<dyn pe2_ext::Generator>,
    host: StreamHost,
    settings: Table,
    parameter: Option<String>,
}

impl graph::stream::Producer for Generating {
    fn header(&self) -> Table {
        self.generator.header()
    }

    fn next(&mut self, count: usize) -> Result<Vec<graph::Value>> {
        let call = pe2_ext::Call { inputs: vec![], settings: &self.settings, parameter_text: self.parameter.as_deref(), host: &self.host };
        self.generator.next(&call, count)
    }

    fn seek(&mut self, position: u64) -> Result<bool> {
        let call = pe2_ext::Call { inputs: vec![], settings: &self.settings, parameter_text: self.parameter.as_deref(), host: &self.host };
        self.generator.seek(&call, position)
    }
}

impl pe2_ext::CallHost for Hosted<'_> {
    fn path(&self) -> &str {
        self.call.recorder().node().path()
    }

    fn kind(&self) -> Option<&str> {
        self.kind
    }

    fn role(&self) -> Option<&str> {
        self.role
    }

    fn fields(&self) -> &Table {
        self.call.recorder().node().fields()
    }

    fn read(&self, content: &pe2_ext::Content) -> Result<Vec<u8>> {
        self.call.read(content)
    }

    fn read_range(&self, content: &pe2_ext::Content, offset: u64, len: usize) -> Result<Vec<u8>> {
        self.call.read_range(content, offset, len)
    }

    fn writer(&self, name: &str) -> Result<Box<dyn pe2_ext::FileWriter>> {
        Ok(Box::new(self.call.writer(name)))
    }

    fn temp(&self) -> Result<String> {
        Ok(self.call.temp()?.display().to_string())
    }

    fn keep(&self, path: &str, name: &str) -> Result<pe2_ext::File> {
        self.call.keep(std::path::Path::new(path), name)
    }

    fn path_of(&self, content: &pe2_ext::Content) -> Option<String> {
        match content {
            pe2_ext::Content::Inline(_) => None,
            pe2_ext::Content::Stored { sha1, .. } => self.call.content_path(sha1).map(|p| p.display().to_string()),
        }
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

    #[test]
    fn parts_make_rows_under_a_heading_for_each_group() {
        let part = |id: &str, label: &str| pe2_ext::Part { id: id.to_string(), label: label.to_string(), made_of: vec![] };
        let rows = super::rows_of_parts(&[part("texels", "Texels"), part("cells/a", "a"), part("cells/b", "b"), part("palettes/p", "p")]);
        let read: Vec<(&str, bool, u32)> = rows.iter().map(|r| (r.id.as_str(), r.is_heading(), r.depth)).collect();
        let expected = [("texels", false, 0), ("cells", true, 0), ("cells/a", false, 1), ("cells/b", false, 1), ("palettes", true, 0), ("palettes/p", false, 1)];
        assert_eq!(read, expected);
    }

    #[test]
    fn a_command_of_an_extension_is_offered_by_the_tool_it_runs() {
        fn offering(r: &mut Registry) -> Result<()> {
            r.command("Say hello", "hello");
            Ok(())
        }
        let extensions = Extensions::load(vec![extension("test.greeter", "0.1.0", &[], offering)], false);
        let commands = extensions.commands();
        assert_eq!(commands.len(), 1);
        assert_eq!((commands[0].label.as_str(), commands[0].tool.to_string().as_str()), ("Say hello", "test.greeter@hello"));
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
        Extension { id, version, requires, weak: &[], optional: &[], provides: &[], platforms: &[], unsandboxed: false, register }.into()
    }

    #[test]
    fn a_stack_is_checked_from_its_shape() {
        struct Clip;
        impl pe2_ext::ty::Registered for Clip {
            const NAME: &str = "test.shape@clip";
            const LABEL: &str = "A clip";
            fn signatures() -> Vec<pe2_ext::Signature> {
                vec![pe2_ext::Signature::Name("*.clip".into())]
            }
        }
        impl pe2_ext::ty::Files for Clip {}
        struct Other;
        impl pe2_ext::ty::Registered for Other {
            const NAME: &str = "test.shape@other";
            const LABEL: &str = "Another";
        }
        impl pe2_ext::ty::Files for Other {}
        fn register(r: &mut Registry) -> Result<()> {
            let _ = r.value_type::<Clip>();
            let _ = r.value_type::<Other>();
            let _ = r.op("takes").input::<Clip>("clip").output::<pe2_ext::ty::Table>().run(|_| Ok(toml::Table::new()));
            let _ = r.op("wants").input::<Other>("other").output::<pe2_ext::ty::Table>().run(|_| Ok(toml::Table::new()));
            Ok(())
        }
        let extensions = Extensions::load(vec![extension("test.shape", "0.1.0", &[], register)], false);
        assert_eq!(extensions.problems, Vec::<String>::new());
        let problems = |text: &str| {
            let fields: toml::Table = text.parse().unwrap();
            crate::graph::Plan::of(&fields, &extensions.operations(), &[]).1.into_iter().map(|p| p.what).collect::<Vec<_>>()
        };
        let stack = |file: &str, op: &str| format!("[[stack]]\nop = \"file\"\nfile = \"{file}\"\n[[stack]]\nop = \"test.shape@{op}\"\n");
        // Fits, and does not: told by the file's name and the operation's port, nothing run.
        assert_eq!(problems(&stack("a.clip", "takes")), Vec::<String>::new());
        let wrong = problems(&stack("a.clip", "wants"));
        assert_eq!(wrong.len(), 1, "{wrong:?}");
        assert!(wrong[0].contains("test.shape@clip"), "{wrong:?}");
        // A name nothing claims says nothing: told when it runs.
        assert_eq!(problems(&stack("a.bin", "wants")), Vec::<String>::new());
        // What comes down to each layer, and so what may be added there.
        let fields: toml::Table = stack("a.clip", "takes").parse().unwrap();
        let (plan, _) = crate::graph::Plan::of(&fields, &extensions.operations(), &[]);
        assert_eq!(plan.above.len(), 3);
        assert_eq!(plan.above[0], None);
        assert_eq!(plan.above[2], Some(pe2_ext::Type::Table));
        let offered = extensions.layers_taking(plan.above[1].as_ref().unwrap());
        assert_eq!(offered.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(), ["test.shape@takes"]);
        assert_eq!(extensions.layers_taking(&pe2_ext::Type::Table), Vec::<(String, String)>::new());
        // What the version below makes, as it is declared.
        let below = |ty: &str| format!("[[stack]]\nop = \"below\"\ntype = \"{ty}\"\n[[stack]]\nop = \"test.shape@takes\"\n");
        assert_eq!(problems(&below("test.shape@clip")), Vec::<String>::new());
        assert_eq!(problems(&below("test.shape@other")).len(), 1);
    }

    #[test]
    fn a_dependency_holds_as_its_kind_says() {
        fn compiler(r: &mut Registry) -> Result<()> {
            let _ = r.op("compile").output::<pe2_ext::ty::Image>().run(|_| Ok(pe2_ext::Image { width: 1, height: 1, rgba: vec![0; 4] }));
            Ok(())
        }
        fn movie(r: &mut Registry) -> Result<()> {
            let _ = r.kind("movie").register();
            Ok(())
        }
        fn editing(r: &mut Registry) -> Result<()> {
            r.add_output("test.movies@movie", "built", "c-compiler@compile", None, pe2_ext::Retain::Transient);
            Ok(())
        }
        let of = |id, requires, weak, optional, register| Extension {
            id,
            version: "0.1.0",
            requires,
            weak,
            optional,
            provides: &[],
            platforms: &[],
            unsandboxed: false,
            register,
        };
        let load = |extensions: Vec<Loadable>| Extensions::load(extensions, false);
        let user = |weak, optional| Loadable::from(of("test.user", &[], weak, optional, nothing));
        let gcc = || Loadable::from(Extension { provides: &["c-compiler@1.2.0"], ..of("test.gcc", &[], &[], &[], compiler) });
        let loaded = |extensions: &Extensions| extensions.loaded.iter().any(|l| l.id == "test.user");
        // No compatible one enabled, for each reason there is: weak and optional run without it, nothing said.
        let broken = || Loadable::from(of("test.broken", &["test.nothing"], &[], &[], nothing));
        let off = |user| Extensions::load_without(vec![user, gcc()], false, &["test.gcc".to_string()]);
        for (weak, optional) in [(&["test.gcc"] as &'static [&'static str], &[] as &'static [&'static str]), (&[], &["test.gcc"])] {
            assert!(loaded(&load(vec![user(weak, optional)])));
            assert!(loaded(&off(user(weak, optional))));
        }
        for (weak, optional) in [(&["c-compiler@^2"] as &'static [&'static str], &[] as &'static [&'static str]), (&[], &["c-compiler@^2"])] {
            let without = load(vec![user(weak, optional), gcc()]);
            assert!(loaded(&without));
            assert_eq!(without.problems, Vec::<String>::new());
        }
        for (weak, optional) in [(&["test.broken"] as &'static [&'static str], &[] as &'static [&'static str]), (&[], &["test.broken"])] {
            assert!(loaded(&load(vec![user(weak, optional), broken()])));
        }
        assert!(!loaded(&off(of("test.user", &["test.gcc"], &[], &[], nothing).into())));
        // A compatible one enabled: it comes first and is linked, its services the extension's to call.
        for extensions in [load(vec![user(&["c-compiler@^1"], &[]), gcc()]), load(vec![user(&[], &["c-compiler@^1"]), gcc()])] {
            assert_eq!(extensions.loaded[0].id, "test.gcc");
            assert_eq!(extensions.loaded[1].reaches, ["c-compiler@^1"]);
            assert!(extensions.present.iter().any(|p| p.id == "test.user" && p.linked == ["c-compiler@^1"]));
        }
        // None enabled, nothing is linked.
        assert!(load(vec![user(&["test.gcc"], &[])]).present.iter().all(|p| p.linked.is_empty()));
        // Each optional to the other: neither is left waiting. A requirement going round still is.
        let round = |id, requires, optional| Loadable::from(of(id, requires, &[], optional, nothing));
        let both = load(vec![round("test.a", &[], &["test.b"]), round("test.b", &[], &["test.a"])]);
        assert_eq!(both.loaded.len(), 2, "{:?}", both.problems);
        let both = load(vec![round("test.a", &["test.b"], &[]), round("test.b", &["test.a"], &[])]);
        assert_eq!(both.loaded, vec![]);
        // What makes an output, weak and absent: the output is not there, and nothing else changes. Not declared, it is
        // an error.
        let movies = || Loadable::from(of("test.movies", &[], &[], &[], movie));
        let plain = load(vec![movies(), of("test.editing", &["test.movies"], &["c-compiler@^1"], &[], editing).into()]);
        assert_eq!(plain.loaded.len(), 2, "{:?}", plain.problems);
        assert_eq!(plain.kinds["test.movies@movie"].0.outputs.len(), 0);
        let added = load(vec![movies(), gcc(), of("test.editing", &["test.movies"], &["c-compiler@^1"], &[], editing).into()]);
        assert_eq!(added.kinds["test.movies@movie"].0.outputs.len(), 1);
        let undeclared = load(vec![movies(), of("test.editing", &["test.movies"], &[], &[], editing).into()]);
        assert_eq!(undeclared.loaded.len(), 1);
    }

    #[test]
    fn a_service_is_met_by_what_provides_it_and_an_extension_adds_to_a_kind() {
        fn compiler(r: &mut Registry) -> Result<()> {
            let _ = r.op("compile").output::<pe2_ext::ty::Image>().run(|_| Ok(pe2_ext::Image { width: 1, height: 1, rgba: vec![0; 4] }));
            Ok(())
        }
        fn nothing(_: &mut Registry) -> Result<()> {
            Ok(())
        }
        fn movie(r: &mut Registry) -> Result<()> {
            let _ = r.kind("movie").register();
            Ok(())
        }
        fn editing(r: &mut Registry) -> Result<()> {
            r.add_output("test.movies@movie", "built", "c-compiler@compile", None, pe2_ext::Retain::Transient);
            Ok(())
        }
        let load = |extensions: Vec<Extension>| {
            let loadable = extensions.into_iter().map(Loadable::from).collect();
            Extensions::load(loadable, false)
        };
        let extension = |id, requires, provides, register| Extension { id, version: "0.1.0", requires, weak: &[], optional: &[], provides, platforms: &[], unsandboxed: false, register };
        let extensions = load(vec![
            extension("test.gcc", &[], &["c-compiler@1.2.0"], compiler),
            extension("test.movies", &[], &[], movie),
            extension("test.user", &["c-compiler@^1", "test.movies"], &[], editing),
            extension("test.newer", &["c-compiler@^2"], &[], nothing),
            extension("test.other", &["fortran-compiler@^1"], &[], nothing),
        ]);
        let status = |id: &str| extensions.present.iter().find(|p| p.id == id).map(|p| p.status.clone()).unwrap();
        assert_eq!(status("test.user"), Status::Loaded);
        assert!(matches!(status("test.newer"), Status::Refused(why) if why.contains("c-compiler ^2, and 1.2.0")), "{:?}", status("test.newer"));
        assert!(matches!(status("test.other"), Status::Refused(why) if why.contains("not installed")));
        // Its operations, by the service: what uses it does not name what provides it.
        assert!(extensions.operations().get("c-compiler@compile").is_some());
        assert!(extensions.kinds["test.movies@movie"].0.outputs.iter().any(|o| o.name == "built"));
        // Two providers: the first, the other said.
        let both = load(vec![
            extension("test.gcc", &[], &["c-compiler@1.0.0"], compiler),
            extension("test.clang", &[], &["c-compiler@1.0.0"], compiler),
        ]);
        assert!(both.problems.iter().any(|p| p.contains("both provide c-compiler")), "{:?}", both.problems);
        // Adding to a kind it does not require is refused.
        let stray = load(vec![extension("test.movies", &[], &[], movie), extension("test.stray", &["c-compiler"], &[], editing)]);
        assert!(stray.problems.iter().any(|p| p.contains("test.stray is disabled")), "{:?}", stray.problems);
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
    use pe2_ext::{Extension, Image, Project, Registry, Result, Retain, Table, Value, anyhow, ty};

    fn paint(r: &mut Registry) -> Result<()> {
        // A picture of one red pixel, whatever the canvas is.
        let draw = r
            .op("draw")
            .input::<ty::Any>("canvas")
            .output::<ty::Image>()
            .run(|_| Ok(Image { width: 1, height: 1, rgba: vec![255, 0, 0, 255] }));
        r.kind("canvas").alias("canvas").output("picture", &draw, Retain::Transient).register();
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
        let canvas = "kind = \"canvas\"\n\n[[stack]]\nop = \"constant\"\nvalue = {}\n";
        for (path, text) in [("project.toml", "[[layer]]\nname = \"base\"\n"), (".layers/base/a.asset/asset.toml", canvas)] {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        let extension = |id: &'static str, requires: &'static [&'static str], register: fn(&mut Registry) -> Result<()>| {
            Loadable::from(Extension { id, version: "0.1.0", requires, weak: &[], optional: &[], provides: &[], platforms: &[], unsandboxed: false, register })
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
            Loadable::from(Extension { id, version: "0.1.0", requires, weak: &[], optional: &[], provides: &[], platforms: &[], unsandboxed: false, register })
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
