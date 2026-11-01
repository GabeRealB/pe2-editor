//! The extension host: loads the extensions, registers what they register, and answers what a type has - its icon,
//! fields, picture, thumbnail, views - for the editor, `pe2-cli` and the agents' server alike.
//!
//! Loading checks what the SDK cannot: one extension an id (a second is refused), each one's requirements met by the
//! version loaded (`pe2.images@^0.1`), registration in dependency order, cycles refused; an extension whose
//! requirement is missing, too old or itself disabled is disabled, and so are its dependents, each with the reason. What
//! one registers is checked as well: an alias only from a built-in,
//! a name only once. An extension is a component the host runs (`component`), or a crate linked into the host whose
//! entry point it calls - the built-ins' fallback while developing.

pub mod unsandboxed;
pub mod component;

use anyhow::{Context, Result};
use pe2_ext::{
    Asset, Extension, Field, Icon, Image, TypeDef, PanelDef, Project, Recognized, Registry, Signature,
    Source, Table, Tint, ToolDef, Value, ViewDef,
};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};
use std::path::PathBuf;

use crate::{graph, index};

/// The type of an asset holding an extension: the host's own, registered by none. Its bundle holds the component
/// (`extension.wasm`), which says who it is.
pub const EXTENSION_TYPE: &str = "extension";

/// What a stack of a shape makes.
struct Shape {
    /// The registered types of what it holds, what it comes to first.
    types: Vec<String>,
    /// Its own outputs, then the conversions there are of what it makes.
    outputs: Vec<pe2_ext::OutputDef>,
    /// The operations it holds or has provided, each once.
    ops: Vec<String>,
}

/// How an asset can be looked at: an output that is a preview, and what shows it - none, the host itself.
pub struct Preview<'a> {
    pub output: String,
    pub ty: pe2_ext::Type,
    pub viewer: Option<&'a ViewDef>,
    /// Asked for by its stack, rather than by what it is.
    pub tapped: bool,
}

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
    /// What it registered, by what it is - types, formats, importers, extractors, panels, tools, services -
    /// each name qualified.
    pub registers: Registers,
    /// For an unsandboxed extension, its hash: what enabling it records.
    pub trust: Option<String>,
}

/// What an extension registered, by what it is (`types`, `services`, ...), each name qualified.
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
    /// Found where extensions are kept, and not taken in by the project (`[[extension]]`).
    Available,
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
    /// The config being built.
    config: std::sync::RwLock<Option<pe2_ext::data::BuildConfig>>,
    /// The assets it leaves out.
    disabled: std::sync::RwLock<std::collections::BTreeSet<String>>,
    /// The events extensions declare, by name, with their labels.
    pub events: Vec<(String, String)>,
    /// What extensions run where events are signaled.
    pub hooks: Vec<pe2_ext::HookDef>,
    /// What files replace assets of types, with what.
    /// Ways to make a node, by name: what a declaration names, and what an editor offers.
    templates: std::collections::BTreeMap<String, pe2_ext::TemplateDef>,
    /// What extensions offer among the editor's commands.
    commands: Vec<pe2_ext::CommandDef>,
    pub loaded: Vec<Loaded>,
    /// Every extension present, in the order found, loaded or not.
    pub present: Vec<Present>,
    /// What the project requires and does not have, by id, and why.
    pub unmet: Vec<(String, String)>,
    /// What is no longer what the project took in, by id.
    pub unlocked: Vec<String>,
    /// What was refused or disabled, and why.
    pub problems: Vec<String>,
    /// Each loaded extension's settings, by its id: as declared, with the values they took.
    pub settings: HashMap<String, Vec<(pe2_ext::Setting, toml::Value)>>,
    types: HashMap<String, (TypeDef, usize)>,
    /// What shows a value of a type in a document.
    viewers: Vec<(pe2_ext::Type, ViewDef)>,
    /// The type a node making a value is, by the value's type.
    presented: HashMap<String, String>,
    /// What nodes are, by the shape of their stacks: read off the plan, once for all nodes alike.
    shapes: RwLock<HashMap<Arc<str>, Arc<Shape>>>,
    panels: Vec<PanelDef>,
    tools: Vec<ToolDef>,
    services: HashMap<String, ToolDef>,
    /// The types a stored file may be recognized as: those registered with a recognizer.
    recognizers: Vec<TypeDef>,
    ops: HashMap<String, pe2_ext::OpDef>,
    /// The operations as the work graph runs them, made once loaded.
    graph_ops: std::sync::OnceLock<graph::Ops>,
    /// The store outputs asked for in passing read and keep: the open project's, as these outlive it (`extensions::loaded`).
    store: std::sync::RwLock<Option<graph::store::Store>>,
    extractors: Vec<pe2_ext::ExtractorDef>,
    /// The components of the extensions loaded, by id.
    components: Vec<(String, Arc<std::sync::OnceLock<Arc<component::Shared>>>)>,
    /// The formats recognized in files, by their content's hash.
    recognized: Mutex<HashMap<String, Vec<(usize, Recognized)>>>,
    /// Bare names declarations write, to the qualified names of the built-ins registering them.
    type_aliases: HashMap<String, String>,
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
#[must_use]
pub fn satisfies(version: &str, requirement: &str) -> bool {
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
        // Those taking a built-in's place, which name types bare as it did.
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
            ("operations", listed(parts.ops.iter().map(|o| &o.name).collect())),
            ("types", listed(parts.types.iter().filter(|t| !t.extends).map(|t| &t.name).collect())),
            ("extractors", listed(parts.extractors.iter().map(|e| &e.name).collect())),
            ("panels", listed(parts.panels.iter().map(|p| &p.name).collect())),
            ("tools", listed(parts.tools.iter().map(|t| &t.name).collect())),
            ("services", listed(parts.services.iter().map(|s| &s.name).collect())),
        ];
        let (extended, own_types): (Vec<TypeDef>, Vec<TypeDef>) = parts.types.into_iter().partition(|k| k.extends);
        // What it requires, a service also by its provider.
        let wanted = extension.reaches.iter().map(|r| requirement(r).0);
        let behind: Vec<String> = wanted.clone().filter_map(|r| self.providers.get(r).cloned()).collect();
        let dependencies: Vec<&str> = wanted.chain(behind.iter().map(String::as_str)).collect();
        let mut seen = HashSet::new();
        let names = own_types.iter().map(|k| &k.name);
        let names = names.chain(parts.panels.iter().map(|p| &p.name)).chain(parts.tools.iter().map(|t| &t.name));
        let names = names.chain(parts.ops.iter().map(|o| &o.name));
        let names = names.chain(parts.extractors.iter().map(|e| &e.name)).chain(parts.services.iter().map(|s| &s.name));
        for name in names {
            anyhow::ensure!(name.extension() == extension.id, "{name} is not in its namespace");
            anyhow::ensure!(seen.insert(name.clone()), "it registers {name} twice");
        }
        let reachable = |owner: &str, known: bool| owner == extension.id && known || dependencies.contains(&owner);
        self.check_graph(&extension.id, &dependencies, (&parts.ops, &own_types, &parts.problems))?;
        for extractor in &parts.extractors {
            let own = own_types.iter().any(|k| k.name == extractor.node_type);
            anyhow::ensure!(
                reachable(extractor.node_type.extension(), own) && (own || self.types.contains_key(extractor.node_type.as_str())),
                "{} extracts {}, which neither it nor what it requires registers",
                extractor.name,
                extractor.node_type
            );
        }
        let aliases = own_types.iter().filter_map(|k| k.alias.as_ref());
        for alias in aliases {
            anyhow::ensure!(builtin, "only a built-in extension names a type bare ({alias})");
        }
        let extended = self.extended(extension, &dependencies, extended)?;
        let index = self.loaded.len();
        if let Some(component) = &extension.component {
            self.components.push((extension.id.clone(), component.clone()));
        }
        let (id, version, origin) = (extension.id.clone(), extension.version.clone(), extension.origin.clone());
        let reaches = extension.reaches.clone();
        self.loaded.push(Loaded { id, version, build: extension.build.clone(), origin, reaches });
        self.recognizers.extend(own_types.iter().filter(|t| t.recognizable()).cloned());
        for node_type in own_types {
            if let Some(alias) = &node_type.alias {
                self.type_aliases.insert(alias.clone(), node_type.name.to_string());
            }
            let written = node_type.stack.clone().or_else(|| node_type.data.then(|| format!("[[stack]]\nop = \"{}\"\n", node_type.name)));
            if let Some(stack) = written {
                self.templates.insert(node_type.name.local().to_string(), pe2_ext::TemplateDef {
                    name: node_type.name.local().to_string(),
                    label: node_type.label.clone(),
                    node_type: node_type.name.to_string(),
                    files: node_type.takes.clone(),
                    stack,
                    offered: !node_type.extracted,
                });
            }
            if node_type.shows_nodes() {
                self.presented.insert(node_type.name.to_string(), node_type.name.to_string());
            }
            self.presented.extend(node_type.values.iter().map(|value| (value.clone(), node_type.name.to_string())));
            self.types.insert(node_type.name.to_string(), (node_type, index));
        }
        for (ty, viewer) in parts.viewers {
            self.viewers.push((pe2_ext::Type::parse(&ty, &extension.id)?, viewer));
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
        // Its operations, by their names and the services it provides: what requires one names them so.
        let offered: Vec<&str> = extension.provides.iter().map(|p| requirement(p).0).collect();
        for op in parts.ops {
            for service in &offered {
                self.ops.insert(format!("{service}@{}", op.name.local()), op.clone());
            }
            self.ops.insert(op.name.to_string(), op);
        }
        self.add_to_types(extended);
        self.add_declared(parts.events, parts.hooks, parts.templates, parts.commands);
        self.extractors.extend(parts.extractors);
        Ok(registers)
    }

    fn add_declared(&mut self, events: Vec<(String, String)>, hooks: Vec<pe2_ext::HookDef>, templates: Vec<pe2_ext::TemplateDef>, commands: Vec<pe2_ext::CommandDef>) {
        self.events.extend(events);
        self.hooks.extend(hooks);
        self.templates.extend(templates.into_iter().map(|template| (template.name.clone(), template)));
        self.commands.extend(commands);
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

    /// What an extension adds to types of what it depends on, checked. What it adds to what it can do without and does not
    /// have is not there.
    fn extended(&self, extension: &Loadable, dependencies: &[&str], extended: Vec<TypeDef>) -> Result<Vec<TypeDef>> {
        let id = extension.id.as_str();
        let absent = |owner: &str| {
            !dependencies.contains(&owner) && extension.weak.iter().chain(&extension.optional).any(|d| requirement(d).0 == owner)
        };
        let mut kept = Vec::new();
        for added in extended {
            let node_type = added.name.to_string();
            let owner = node_type.split_once('@').map_or("", |(owner, _)| owner);
            if absent(owner) {
                continue;
            }
            anyhow::ensure!(dependencies.contains(&owner), "{id} adds to {node_type}, which it does not require");
            anyhow::ensure!(self.types.contains_key(&node_type), "{id}: no type {node_type}");
            kept.push(added);
        }
        Ok(kept)
    }

    /// What is added to types, checked: a type keeps its stack, and what says what is inside it, where it has them.
    fn add_to_types(&mut self, extended: Vec<TypeDef>) {
        for added in extended {
            let Some((def, _)) = self.types.get_mut(&added.name.to_string()) else { continue };
            def.values.extend(added.values.iter().cloned());
            def.fields.extend(added.fields);
            def.views.extend(added.views);
            def.inspector.extend(added.inspector);
            def.editors.extend(added.editors);
            def.contents_views.extend(added.contents_views);
            def.contents = def.contents.take().or(added.contents);
            def.content_act = def.content_act.take().or(added.content_act);
            def.part_sheet = def.part_sheet.take().or(added.part_sheet);
            // What it shows, an addition may be the first to say.
            let name = def.name.to_string();
            let shown = def.shows_nodes().then(|| name.clone());
            self.presented.extend(shown.into_iter().map(|own| (own.clone(), own)));
            self.presented.extend(added.values.iter().map(|value| (value.clone(), name.clone())));
            if let Some(stack) = added.stack {
                let (name, label, node_type) = (def.name.local().to_string(), def.label.clone(), def.name.to_string());
                self.templates.entry(name.clone()).or_insert(pe2_ext::TemplateDef { name, label, node_type, files: Vec::new(), stack, offered: true });
            }
        }
    }

    /// What an extension registers for the work graph, checked: its registration's own problems; that what its
    /// operations take and make, and what its types' outputs are made by, is its own or what it requires registers.
    fn check_graph(
        &self,
        id: &str,
        dependencies: &[&str],
        (ops, types, problems): (&[pe2_ext::OpDef], &[pe2_ext::TypeDef], &[String]),
    ) -> Result<()> {
        let reachable = |owner: &str, known: bool| owner == id && known || dependencies.contains(&owner);
        anyhow::ensure!(problems.is_empty(), "{}", problems.join("; "));
        // What its operations take and make, and what its types' outputs are made by: its own, or what it requires.
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

    /// The type a declaration names, qualified or by a built-in's bare alias.
    fn node_type_named(&self, declared: &str) -> Option<&(TypeDef, usize)> {
        self.types.get(self.type_aliases.get(declared).map_or(declared, String::as_str))
    }

    /// A declared type, qualified as extensions see it (`image` to `pe2.images@image`); none for what no extension
    /// registers.
    #[must_use]
    pub fn qualify(&self, node_type: &str) -> Option<&str> {
        self.node_type_named(node_type).map(|(node_type, _)| node_type.name.as_str())
    }

    /// What a stack of a shape makes: read off its plan, once for all stacks of that shape.
    fn shape_of(&self, shape: &Arc<str>) -> Arc<Shape> {
        if let Some(known) = self.shapes.read().unwrap_or_else(std::sync::PoisonError::into_inner).get(shape) {
            return known.clone();
        }
        let declared: Table = shape.parse().unwrap_or_default();
        let ops = self.operations();
        let (plan, _) = graph::Plan::of(&declared, &ops, &[]);
        let outputs = self.provided(&plan, &ops);
        let mut held: Vec<String> = plan.steps.iter().map(|step| step.op.clone()).chain(outputs.iter().map(|o| o.op.clone())).collect();
        let mut seen = HashSet::new();
        held.retain(|op| seen.insert(op.clone()));
        let found = Arc::new(Shape { types: self.types_in(&plan), outputs, ops: held });
        self.shapes.write().unwrap_or_else(std::sync::PoisonError::into_inner).insert(shape.clone(), found.clone());
        found
    }

    /// The registered types of what a plan holds, what it comes to first.
    fn types_in(&self, plan: &graph::Plan) -> Vec<String> {
        let mut found: Vec<String> = Vec::new();
        for step in plan.result.into_iter().chain((0..plan.steps.len()).rev()) {
            let mut names = Vec::new();
            named_types(&plan.steps[step].ty, &mut names);
            for name in names.iter().filter_map(|name| self.presented.get(name)) {
                if !found.contains(name) {
                    found.push(name.clone());
                }
            }
        }
        found
    }

    /// The outputs a plan has: its own, then what a reader may ask of what it makes - each conversion taking one of its
    /// result, its outputs and what is set on it, or what another makes; of two by one name, the first by priority.
    fn provided(&self, plan: &graph::Plan, ops: &graph::Ops) -> Vec<pe2_ext::OutputDef> {
        let of_stack = |name: String, step: usize, retain, preview, each| {
            let op = plan.steps[step].op.clone();
            let members = self.ops.get(&op).and_then(|def| def.members.clone());
            pe2_ext::OutputDef { name, ty: plan.steps[step].ty.clone(), op, retain, members, preview, each, in_stack: true, from: None, step: Some(step) }
        };
        let mut offered: Vec<pe2_ext::OutputDef> =
            plan.outputs.iter().map(|o| of_stack(o.name.clone(), o.from, o.retain, o.preview, None)).collect();
        offered.extend(plan.each.iter().map(|(table, step, retain)| of_stack(table.clone(), *step, *retain, false, Some(table.clone()))));
        let mut makers: Vec<&pe2_ext::OpDef> = self.ops.values().filter(|def| def.provides.is_some()).collect();
        makers.sort_by_key(|def| (-def.priority, def.name.to_string()));
        makers.dedup_by_key(|def| def.name.clone());
        let data = (0..plan.steps.len()).filter(|step| ops.get(&plan.steps[*step].op).is_some_and(|def| def.data));
        let tapped = plan.outputs.iter().map(|output| output.from);
        let mut steps: Vec<usize> = plan.result.into_iter().chain(data).chain(tapped).collect();
        let mut seen = HashSet::new();
        steps.retain(|step| seen.insert(*step));
        let mut pending: std::collections::VecDeque<(Option<usize>, Option<String>, pe2_ext::Type)> =
            steps.iter().map(|step| (Some(*step), None, plan.steps[*step].ty.clone())).collect();
        while let Some((step, from, ty)) = pending.pop_front() {
            for def in &makers {
                let Some(provides) = def.provides.as_ref().filter(|provides| !offered.iter().any(|o| o.name == provides.name)) else { continue };
                if !def.inputs.first().is_some_and(|port| specific(&port.ty) && port.ty.unify(&ty, &mut Vec::new())) {
                    continue;
                }
                offered.push(pe2_ext::OutputDef {
                    name: provides.name.clone(),
                    op: def.name.to_string(),
                    ty: def.output.clone(),
                    retain: provides.retain,
                    members: def.members.clone(),
                    preview: false,
                    each: None,
                    in_stack: false,
                    from: from.clone(),
                    step,
                });
                pending.push_back((None, Some(provides.name.clone()), def.output.clone()));
            }
        }
        // What it says of itself comes from what holds that: what is set on it, where that is its own step.
        let settings = steps.iter().find(|step| ops.get(&plan.steps[**step].op).is_some_and(|def| def.data)).or(steps.first());
        if let Some(step) = settings.filter(|_| !offered.iter().any(|o| o.name == graph::SETTINGS)) {
            let name = graph::SETTINGS.to_string();
            offered.push(pe2_ext::OutputDef {
                op: name.clone(),
                name,
                ty: pe2_ext::Type::Table,
                retain: pe2_ext::Retain::Transient,
                members: None,
                preview: false,
                each: None,
                in_stack: false,
                from: None,
                step: Some(*step),
            });
        }
        offered
    }

    /// What an asset is: the types of what its stack makes, first what it comes to.
    #[must_use]
    pub fn node_types_of(&self, asset: &index::Asset) -> Vec<&TypeDef> {
        let shape = self.shape_of(&asset.shape);
        shape.types.iter().filter_map(|name| self.types.get(name)).map(|(node_type, _)| node_type).collect()
    }

    /// What an asset is mainly.
    #[must_use]
    pub fn node_type_of(&self, asset: &index::Asset) -> Option<&TypeDef> {
        self.node_types_of(asset).first().copied()
    }

    /// The name of what an asset is, qualified. An extension in a project is no output of anything: it is what it holds.
    #[must_use]
    pub fn type_name(&self, asset: &index::Asset) -> Option<&str> {
        let held = || asset.files.iter().any(|file| file.name == EXTENSION_COMPONENT).then_some(EXTENSION_TYPE);
        self.node_type_of(asset).map(|node_type| node_type.name.as_str()).or_else(held)
    }

    /// The name of what an asset is, without what registered it.
    #[must_use]
    pub fn type_label(&self, asset: &index::Asset) -> Option<&str> {
        self.type_name(asset).and_then(|name| name.rsplit('@').next())
    }

    /// What its types give an asset, in their order: what each lists, by `list`.
    fn each_type<'a, T>(&'a self, asset: &index::Asset, list: impl Fn(&'a TypeDef) -> &'a [T]) -> Vec<&'a T> {
        self.node_types_of(asset).into_iter().flat_map(|node_type| list(node_type).iter()).collect()
    }

    /// The outputs what an asset makes has provided.
    fn outputs_of(&self, asset: &index::Asset) -> Vec<pe2_ext::OutputDef> {
        self.shape_of(&asset.shape).outputs.clone()
    }

    /// An asset as its extensions see it: lent, with its type qualified.
    #[must_use]
    pub fn lend<'a>(&'a self, asset: &'a index::Asset) -> Asset<'a> {
        let node_type = self.node_type_of(asset).map(|k| k.name.as_str());
        Asset::new(asset, node_type)
    }

    /// A type's icon and its tint, by the name a declaration writes.
    #[must_use]
    pub fn icon(&self, declared: &str) -> Option<(Icon, Tint)> {
        self.node_type_named(declared)?.0.icon
    }

    /// The stack a file makes of an asset: the template of what it is that takes the file, stamped with it.
    ///
    /// # Errors
    /// It does, by an operation no extension provides.
    #[must_use]
    pub fn stack_for(&self, asset: &index::Asset, file: &str) -> Option<std::result::Result<String, String>> {
        let name = self.type_name(asset)?;
        let template = self.templates_for(file).into_iter().find(|template| template.node_type == name)?;
        Some(self.stamp(template, file))
    }

    /// The templates that take a file, by its name.
    #[must_use]
    pub fn templates_for(&self, file: &str) -> Vec<&pe2_ext::TemplateDef> {
        let lower = file.to_lowercase();
        self.templates.values().filter(|template| template.files.iter().any(|pattern| name_matches(pattern, &lower))).collect()
    }

    /// Every template, by its name.
    pub fn templates(&self) -> impl Iterator<Item = &pe2_ext::TemplateDef> {
        self.templates.values()
    }

    /// A template, by its name.
    #[must_use]
    pub fn template(&self, name: &str) -> Option<&pe2_ext::TemplateDef> {
        self.templates.get(name)
    }

    /// A template's stack as it goes into a node, made with this file.
    ///
    /// # Errors
    /// It does not read, or it takes an operation no extension provides.
    pub fn stamp(&self, template: &pe2_ext::TemplateDef, file: &str) -> std::result::Result<String, String> {
        let stack = template.stack.replace("{file}", file);
        let table: Table = stack.parse().map_err(|e| format!("{}: {e}", template.name))?;
        let ops = self.operations();
        let name_of = |t: &toml::Value| t.get("op").and_then(toml::Value::as_str).map(str::to_string);
        let layers = table.get("stack").and_then(toml::Value::as_array).map_or(&[][..], Vec::as_slice);
        let inner = layers.iter().filter_map(|l| l.get("ops")?.as_table()).flat_map(|o| o.values());
        let missing = layers.iter().chain(inner).filter_map(name_of).find(|name| ![graph::GRAPH, graph::OUTPUT, graph::PREVIEW].contains(&name.as_str()) && ops.get(name).is_none());
        missing.map_or(Ok(stack), |name| Err(format!("It takes {name}, which no extension provides")))
    }

    /// What may be asked of an asset beyond its stack, as its plan lowers it: the conversions of what it makes, then the
    /// host's thumbnail (the first picture it has, made small, kept).
    #[must_use]
    pub fn tail(&self, asset: &index::Asset) -> Vec<graph::Tail> {
        let outputs = self.outputs_of(asset);
        let conversions: Vec<&pe2_ext::OutputDef> = outputs.iter().filter(|o| !o.in_stack).collect();
        let from = |o: &pe2_ext::OutputDef| o.from.as_ref().and_then(|name| conversions.iter().position(|o| o.name == *name));
        let mut tail: Vec<graph::Tail> = conversions
            .iter()
            .map(|o| graph::Tail {
                name: o.name.clone(),
                fixed: false,
                op: o.op.clone(),
                retain: o.retain,
                from: from(o),
                step: o.step,
                settings: Table::new(),
                preview: false,
            })
            .collect();
        if let Some(pictured) = outputs.iter().find(|o| self.makes_pictures(o)) {
            let mut settings = Table::new();
            settings.insert("side".into(), Value::Integer(THUMBNAIL_SIDE));
            let (name, op) = (graph::THUMBNAIL.to_string(), graph::THUMBNAIL.to_string());
            // One picture: of its first look, which `thumbnail` supplies when it is asked for.
            let from = conversions.iter().position(|o| o.name == pictured.name);
            let step = pictured.step.filter(|_| pictured.in_stack);
            tail.push(graph::Tail { name, fixed: true, op, retain: pe2_ext::Retain::Cache, from, step, preview: false, settings });
        }
        tail
    }

    /// Whether a type's output is a picture: its operation makes an image.
    fn makes_pictures(&self, output: &pe2_ext::OutputDef) -> bool {
        self.ops.get(&output.op).is_some_and(|op| op.output == pe2_ext::Type::Image)
    }

    /// An asset's plan: its stack, then its type's outputs; and what is wrong with it.
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
        let world = Graph { index, extensions: self, written: false };
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
        let world = Graph { index, extensions: self, written: false };
        let reach = graph::exec::Reach { engine, world: &world };
        engine.output_in(Some(reach), &plan, asset, address, None, read)
    }

    /// Whether an output is not what it would be made as now.
    #[must_use]
    pub fn output_is_stale(&self, engine: &graph::exec::Engine, index: &index::Index, asset: &index::Asset, plan: &graph::Plan, output: usize) -> bool {
        let world = Graph { index, extensions: self, written: false };
        engine.kept_in(Some(graph::exec::Reach { engine, world: &world }), plan, asset, output, None).is_none()
    }

    /// Run an extension's hook, its node the project as a whole.
    ///
    /// # Errors
    /// No extension provides it, or it failed.
    pub fn run_hook(&self, engine: &graph::exec::Engine, index: &index::Index, op: &str, event: &str) -> Result<()> {
        let world = Graph { index, extensions: self, written: false };
        let reach = graph::exec::Reach { engine, world: &world };
        let mut settings = Table::new();
        settings.insert("event".into(), event.into());
        engine.run_in(Some(reach), op, Vec::new(), &settings, &graph::exec::Nowhere::default()).map(|_| ())
    }

    /// An output of an asset that is a table, made or found kept.
    ///
    /// # Errors
    /// It has no such output, it could not be made, or it is no table.
    pub fn table(&self, asset: &index::Asset, index: &index::Index, address: &str) -> Result<Table> {
        let given = self.output(&self.engine(), index, asset, address, true)?;
        match given.value.as_deref() {
            Some(graph::Value::Table(table)) => Ok(table.clone()),
            _ => anyhow::bail!("{}: {address} is no table", asset.path),
        }
    }

    /// An engine for what is asked now, in passing: the extensions' operations, reading what the project's store
    /// keeps (`keep_in`), keeping what it is asked to.
    #[must_use]
    pub fn engine(&self) -> graph::exec::Engine {
        let mut engine = graph::exec::Engine::new(self.operations(), self.store.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone());
        engine.config.clone_from(&self.config.read().unwrap_or_else(std::sync::PoisonError::into_inner));
        engine
    }

    /// The store outputs are kept in, for what is asked in passing: that of the project opened, and of the next one.
    pub fn keep_in(&self, store: graph::store::Store) {
        *self.store.write().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(store);
    }

    /// The config what is made from now on is made for, with the assets left out of it.
    pub fn set_config(&self, config: Option<pe2_ext::data::BuildConfig>, disabled: &[String]) {
        *self.config.write().unwrap_or_else(std::sync::PoisonError::into_inner) = config;
        *self.disabled.write().unwrap_or_else(std::sync::PoisonError::into_inner) = disabled.iter().cloned().collect();
    }

    /// Whether the config being built leaves an asset out.
    #[must_use]
    pub fn is_disabled(&self, path: &str) -> bool {
        self.disabled.read().unwrap_or_else(std::sync::PoisonError::into_inner).contains(path)
    }

    /// Whether a type's output is a family: its operation has a parameter.
    fn family(&self, output: &pe2_ext::OutputDef) -> bool {
        self.ops.get(&output.op).is_some_and(|op| op.parameter.is_some())
    }

    /// The output of an asset's type it is shown by - the first that is a picture - with its members (one, unnamed,
    /// where it is no family).
    fn pictured(&self, asset: &index::Asset) -> Option<(pe2_ext::OutputDef, Vec<(String, String)>)> {
        let outputs = self.outputs_of(asset);
        let output = outputs.iter().find(|o| self.makes_pictures(o))?;
        let members = match &output.members {
            Some(members) => members(&self.lend(asset)),
            None if !self.family(output) => vec![(String::new(), output.name.clone())],
            None => Vec::new(),
        };
        Some((output.clone(), members))
    }

    /// How an asset can be looked at: each output of it that is a preview, once for each viewer of what it is. The host's
    /// own - a picture, a stream - shows it with no viewer of an extension's.
    #[must_use]
    pub fn previews(&self, asset: &index::Asset) -> Vec<Preview<'_>> {
        let outputs = self.outputs_of(asset);
        let mut found: Vec<Preview<'_>> = Vec::new();
        for output in outputs.iter().filter(|output| output.preview) {
            let viewers: Vec<Option<&ViewDef>> = self.viewers.iter().filter(|(takes, _)| takes.unify(&output.ty, &mut Vec::new())).map(|(_, viewer)| Some(viewer)).collect();
            for viewer in if viewers.is_empty() { vec![None] } else { viewers } {
                // What is shown already, by what shows it, gets a tab of its own.
                let tapped = found.iter().any(|known| known.viewer.map(|v| &v.name) == viewer.map(|v| &v.name) && known.ty.is_stream() == output.ty.is_stream());
                let family = self.ops.get(&output.op).is_some_and(|def| def.parameter.is_some());
                // Of a family, what it is first shown as: the first member it lists.
                let listed = output.members.as_ref().filter(|_| tapped && family).map(|members| members(&self.lend(asset)));
                // A family listing nothing yet has nothing to show. One with no list is told what to show when asked.
                if listed.as_ref().is_some_and(Vec::is_empty) {
                    continue;
                }
                let first = listed.and_then(|members| members.into_iter().next());
                let address = first.map_or_else(|| output.name.clone(), |(member, _)| format!("{}/{member}", output.name));
                found.push(Preview { output: address, ty: output.ty.clone(), viewer, tapped });
            }
        }
        found
    }

    /// An output of an asset that is one file, whole, made from the settings of what it reads as they are written, not
    /// as last applied: what a viewer shows of what is being edited follows the edit.
    ///
    /// # Errors
    /// It has no such output, it is no file, or it could not be made.
    pub fn bytes(&self, index: &index::Index, asset: &index::Asset, address: &str) -> Result<Vec<u8>> {
        let asset = asset.written();
        let (plan, _) = self.plan(asset);
        let world = Graph { index, extensions: self, written: true };
        let engine = self.engine();
        let reach = graph::exec::Reach { engine: &engine, world: &world };
        let given = engine.output_in(Some(reach), &plan, asset, address, None, true)?;
        let Some(graph::Value::Typed(typed)) = given.value.as_deref() else { anyhow::bail!("{}: {address} is no file", asset.path) };
        let file = typed.files.first().with_context(|| format!("{}: {address} has no file", asset.path))?;
        match &file.content {
            pe2_ext::Content::Inline(bytes) => Ok(bytes.clone()),
            // Made here, it is in the store; only what is stored with its node is found there.
            pe2_ext::Content::Stored { sha1, .. } => match engine.store() {
                Some(store) if store.blob_path(sha1).exists() => Ok(std::fs::read(store.blob_path(sha1))?),
                _ => graph::exec::Node::read_content(asset, sha1),
            },
        }
    }

    /// The first of an asset's type's outputs that is a stream, by name: what plays.
    #[must_use]
    pub fn stream_output(&self, asset: &index::Asset) -> Option<String> {
        let outputs = self.outputs_of(asset);
        let stream = outputs.iter().find(|o| self.ops.get(&o.op).is_some_and(|op| op.output.is_stream()));
        stream.map(|stream| stream.name.clone())
    }

    /// The output of an asset's type saying what is marked on its pictures, by name.
    #[must_use]
    pub fn marks_output(&self, asset: &index::Asset) -> Option<String> {
        let outputs = self.outputs_of(asset);
        let marks = outputs.iter().find(|o| self.ops.get(&o.op).is_some_and(|op| op.output == pe2_ext::Type::Marks))?;
        Some(marks.name.clone())
    }

    /// Whether an asset's type shows its nodes by a picture: it outputs one.
    #[must_use]
    pub fn shows_pictures(&self, asset: &index::Asset) -> bool {
        self.outputs_of(asset).iter().any(|o| self.makes_pictures(o))
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

    /// Where a look is among an asset's looks: its first, for no look or one it does not have.
    #[must_use]
    pub fn look_at(&self, asset: &index::Asset, look: Option<&str>) -> usize {
        look.and_then(|look| self.looks(asset).iter().position(|known| known == look)).unwrap_or(0)
    }

    /// The address of an asset's picture in one of its looks (its first, past them).
    #[must_use]
    pub fn picture_address(&self, asset: &index::Asset, look: usize) -> Option<String> {
        let (output, members) = self.pictured(asset)?;
        let (key, _) = members.get(look).or_else(|| members.first())?;
        Some(if self.family(&output) { format!("{}/{key}", output.name) } else { output.name.clone() })
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

    /// Whether an asset may have a thumbnail: its type shows its nodes by a picture. (Whether this one has one is known
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
        let first = members.first().filter(|_| self.family(&output)).map(|(key, _)| key.as_str());
        let (plan, _) = self.plan(asset);
        engine.output_with(&plan, asset, graph::THUMBNAIL, first, true)
    }

    /// The fields an asset has: what is set on the operations of its stack and those it has provided, each once.
    #[must_use]
    pub fn fields(&self, asset: &index::Asset) -> Vec<&Field> {
        let shape = self.shape_of(&asset.shape);
        let of_ops = shape.ops.iter().filter_map(|op| self.ops.get(op)).flat_map(|def| def.fields.iter());
        let mut seen = HashSet::new();
        self.each_type(asset, |k| k.fields.as_slice()).into_iter().chain(of_ops).filter(|field| seen.insert(field.key.as_str())).collect()
    }

    /// The inspector views of an asset: its type's.
    #[must_use]
    pub fn inspector(&self, asset: &index::Asset) -> Vec<&ViewDef> {
        self.each_type(asset, |k| k.inspector.as_slice()).into_iter().collect()
    }

    /// An asset's inspector views with a tab of their own, by their place among its inspector views: those it has what brings.
    #[must_use]
    pub fn inspector_tabs(&self, asset: &index::Asset) -> Vec<(usize, &ViewDef)> {
        self.inspector(asset).into_iter().enumerate().filter(|(_, view)| view.tab.is_some() && brought(view, asset)).collect()
    }

    /// The editors its type gives an asset, of those it has what brings.
    #[must_use]
    pub fn editors(&self, asset: &index::Asset) -> Vec<&ViewDef> {
        self.each_type(asset, |k| k.editors.as_slice()).into_iter().filter(|view| brought(view, asset)).collect()
    }

    /// What gives an asset's document a tab of its own, of an extension's: its editors, and the viewers of its previews.
    #[must_use]
    pub fn tabs(&self, asset: &index::Asset) -> Vec<&ViewDef> {
        let viewers = self.previews(asset).into_iter().filter_map(|preview| preview.viewer);
        self.editors(asset).into_iter().chain(viewers).collect()
    }

    /// An output of an asset that is a picture, as last applied.
    ///
    /// # Errors
    /// It has no such output, or it is no picture, or it could not be made.
    pub fn output_picture(&self, index: &index::Index, asset: &index::Asset, address: &str) -> Result<Image> {
        let given = self.output(&self.engine(), index, asset, address, true)?;
        match given.value.as_deref() {
            Some(graph::Value::Image(image)) => Ok(image.clone()),
            _ => anyhow::bail!("{}: {address} is no picture", asset.path),
        }
    }

    /// The name of every editor any type gives, and of every viewer.
    #[must_use]
    pub fn editor_names(&self) -> std::collections::BTreeSet<&str> {
        let editors = self.types.values().flat_map(|(node_type, _)| &node_type.editors);
        editors.chain(self.viewers.iter().map(|(_, view)| view)).map(|view| view.name.as_str()).collect()
    }

    /// An output of an asset that is a picture, made now: `palettes/default`, `cells/skin@night` (that member through
    /// another palette, which is no member to list); none where it has no such output.
    #[must_use]
    pub fn output_image(&self, asset: &index::Asset, address: &str) -> Option<Result<Image>> {
        let (name, member) = address.split_once('/').map_or((address, None), |(name, member)| (name, Some(member)));
        let outputs = self.outputs_of(asset);
        let output = outputs.iter().find(|o| o.name == name && self.makes_pictures(o))?;
        let there = match (&output.members, member) {
            (Some(members), Some(member)) => members(&self.lend(asset)).iter().any(|(key, _)| key == listed(member)),
            (None, None) => true,
            _ => false,
        };
        there.then(|| self.image(asset, address))
    }

    /// What of an asset a reference can name, each by its address and what it reads as: its outputs, and the members of
    /// those that are families.
    #[must_use]
    pub fn addresses(&self, asset: &index::Asset) -> Vec<(String, String)> {
        let lent = self.lend(asset);
        let outputs = self.outputs_of(asset);
        let members = |name: &str| {
            let outputs = outputs.iter();
            if let Some(family) = outputs.clone().find(|o| o.name == name) {
                return Some(family.members.as_ref()?(&lent));
            }
            // One for a key of a table lists its members under that key.
            let family = outputs.filter(|o| o.each.as_ref().is_some_and(|table| asset.settings().get(table).is_some_and(|table| table.get(name).is_some())));
            let listed = family.filter_map(|o| Some(o.members.as_ref()?(&lent))).flatten();
            let under = |(address, label): (String, String)| Some((address.strip_prefix(name)?.strip_prefix('/')?.to_string(), label));
            Some(listed.filter_map(under).collect())
        };
        let (plan, _) = self.plan(asset);
        let addresses = plan.outputs.iter().flat_map(|output| {
            if output.parameter.is_none() {
                return vec![(output.name.clone(), output.name.clone())];
            }
            let listed = members(&output.name).unwrap_or_default();
            listed.into_iter().map(|(key, label)| (format!("{}/{key}", output.name), label)).collect()
        });
        addresses.collect()
    }

    /// What is inside an asset, as its type says.
    #[must_use]
    pub fn contents(&self, index: &index::Index, asset: &index::Asset, chosen: &Chosen) -> pe2_ext::Contents {
        let contents = self.node_types_of(asset).into_iter().find_map(|k| k.contents.as_ref());
        contents.map_or_default(|contents| contents(&self.lend(asset), &Lens { index, extensions: self, selection: None, chosen: Some(chosen), revision: 0 }))
    }

    /// What comes of something done in an asset's contents.
    ///
    /// # Errors
    /// Why it cannot be, as its type says.
    pub fn content_act(&self, index: &index::Index, asset: &index::Asset, chosen: &Chosen, request: pe2_ext::ContentRequest) -> Result<pe2_ext::ContentOutcome> {
        let act = self.node_types_of(asset).into_iter().find_map(|k| k.content_act.as_ref()).context("its type does nothing with its contents")?;
        act(&self.lend(asset), &Lens { index, extensions: self, selection: None, chosen: Some(chosen), revision: 0 }, request)
    }

    /// A part's fields, where its type says what they are.
    #[must_use]
    pub fn part_sheet(&self, index: &index::Index, asset: &index::Asset, chosen: &Chosen, part: &str) -> Option<pe2_ext::PartSheet> {
        self.node_types_of(asset).into_iter().find_map(|k| k.part_sheet.as_ref())?(&self.lend(asset), &Lens { index, extensions: self, selection: None, chosen: Some(chosen), revision: 0 }, part)
    }

    /// The views a type gives the head of the contents pane.
    #[must_use]
    pub fn contents_views(&self, asset: &index::Asset) -> Vec<&ViewDef> {
        self.each_type(asset, |k| k.contents_views.as_slice()).into_iter().collect()
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

    /// The tests extensions registered, by name, in order.
    #[must_use]
    pub fn tests(&self) -> Vec<String> {
        let mut tests: Vec<String> = self.services.values().filter(|s| s.name.local().starts_with("test.")).map(|s| s.name.to_string()).collect();
        tests.sort();
        tests.dedup();
        tests
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

    /// Every asset of a type, by its qualified name or a built-in's bare one.
    pub fn find<'a>(&'a self, index: &'a index::Index, node_type: &str) -> impl Iterator<Item = &'a index::Asset> {
        let wanted = self.node_type_named(node_type).map(|(k, _)| k.name.as_str());
        index.assets.values().map(|a| &**a).filter(move |a| wanted.is_some_and(|w| self.node_types_of(a).iter().any(|k| k.name.as_str() == w)))
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
        // What the host itself shows, and what extensions' viewers take.
        let host = [pe2_ext::Type::Image, pe2_ext::Type::Stream(Box::new(pe2_ext::Type::Any))];
        ops.viewers = Arc::new(host.into_iter().chain(self.viewers.iter().map(|(ty, _)| ty.clone())).collect());
        // What a node holding only data is: what is set on it, of its type.
        for (name, (node_type, _)) in self.types.iter().filter(|(_, (node_type, _))| node_type.data) {
            let mut op = graph::OpDef::new(name, &node_type.label, "1");
            op.data = true;
            op.required = node_type.fields.iter().filter(|field| field.required).map(|field| field.key.clone()).collect();
            let ty = name.clone();
            ops.add(op.makes(pe2_ext::Type::Named(name.clone()), move |call| {
                Ok(graph::Value::Typed(pe2_ext::Typed { ty: ty.clone(), settings: call.settings().clone(), ..pe2_ext::Typed::default() }))
            }));
        }
        for (name, def) in &self.ops {
            let version = self.version_of(def.name.extension());
            let mut op = graph::OpDef::new(name, &def.label, &version);
            op.inputs.clone_from(&def.inputs);
            op.parameter = def.parameter.as_ref().map(|(name, _)| name.clone());
            op.priority = def.priority;
            op.also.clone_from(&def.also);
            op.required = def.fields.iter().filter(|field| field.required).map(|field| field.key.clone()).collect();
            let run = def.run.clone();
            if let (Some(start), pe2_ext::Type::Stream(item)) = (def.start.clone(), &def.output) {
                ops.add(op.streams((**item).clone(), move |call| start_stream(call, &*start)));
                continue;
            }
            ops.add(op.makes(def.output.clone(), move |call| {
                let inputs = call.take_inputs();
                let hosted = Hosted::new(call);
                let carried = pe2_ext::Call::carried_by(&inputs);
                let mut lent = pe2_ext::Call { inputs, settings: call.settings(), made: std::cell::RefCell::default(), carried, parameter_text: call.parameter(), host: &hosted };
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

    /// The extractor of an asset of a declared type (`image`) declaring what `declares` says, where an extension registers one.
    #[must_use]
    pub fn extractor_for(&self, node_type: &str, declares: impl Fn(&str) -> bool) -> Option<&pe2_ext::ExtractorDef> {
        let node_type = self.qualify(node_type)?;
        self.extractors.iter().find(|e| e.takes(node_type, &declares))
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
        pe2_ext::Type::Named(_) | pe2_ext::Type::Stream(_) => true,
        pe2_ext::Type::Union(any) => any.iter().any(specific),
        pe2_ext::Type::File(of) | pe2_ext::Type::List(of) => specific(of),
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

/// A member of a family, as it is listed: `skin` of `skin@night`.
#[must_use]
pub fn listed(member: &str) -> &str {
    member.split_once('@').map_or(member, |(listed, _)| listed)
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

    /// The views of an asset: its type's.
    #[must_use]
    pub fn views(&self, asset: &index::Asset) -> Vec<&ViewDef> {
        self.each_type(asset, |k| k.views.as_slice()).into_iter().collect()
    }
}

/// The family an asset's looks are: the output it is shown by.
pub const PICTURE: &str = "picture";

/// By (asset, family of its outputs), the member chosen: what is being looked at, never written.
pub type Chosen = HashMap<(String, String), String>;

/// The project as extensions ask about it: the index, through the registry, with what the user selected and the members
/// chosen (the editor's; none for a tool).
pub struct Lens<'a> {
    pub index: &'a index::Index,
    pub extensions: &'a Extensions,
    pub selection: Option<&'a str>,
    /// The members of families assets are being looked at in.
    pub chosen: Option<&'a Chosen>,
    /// Which taking in of changes this is as of.
    pub revision: u64,
}

impl Project for Lens<'_> {
    fn asset(&self, path: &str) -> Option<Asset<'_>> {
        self.index.assets.get(path).map(|a| self.extensions.lend(a))
    }

    fn resolve(&self, reference: &Value) -> Option<Asset<'_>> {
        let (uid, path) = index::reference(reference)?;
        self.index.resolve(&uid, path.as_deref().unwrap_or_default()).map(|a| self.extensions.lend(a))
    }

    fn find(&self, node_type: &str) -> Vec<Asset<'_>> {
        self.extensions.find(self.index, node_type).map(|a| self.extensions.lend(a)).collect()
    }

    fn selection(&self) -> Option<Asset<'_>> {
        self.asset(self.selection?)
    }

    fn chosen(&self, path: &str, family: &str) -> Option<String> {
        self.chosen?.get(&(path.to_string(), family.to_string())).cloned()
    }

    fn picture(&self, path: &str, look: Option<&str>) -> Option<Result<Image>> {
        let asset = self.index.assets.get(path)?;
        self.extensions.picture(asset, self.extensions.look_at(asset, look))
    }

    fn image(&self, path: &str, address: &str) -> Option<Result<Image>> {
        self.extensions.output_image(self.index.assets.get(path)?, address)
    }

    fn revision(&self) -> u64 {
        self.revision
    }

    fn target(&self) -> Option<String> {
        self.extensions.config.read().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref()?.target.clone()
    }

    fn table(&self, path: &str, address: &str) -> Option<Result<Table>> {
        let asset = self.index.assets.get(path)?;
        Some(self.extensions.table(asset, self.index, address))
    }

    fn bytes(&self, path: &str, address: &str) -> Option<Result<Vec<u8>>> {
        let asset = self.index.assets.get(path)?;
        Some(self.extensions.bytes(self.index, asset, address))
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
    /// The nodes as they are written, not as last applied.
    pub written: bool,
}

impl graph::exec::World for Graph<'_> {
    fn named(&self, reference: &toml::Value) -> Option<pe2_ext::NodeNamed> {
        let (node, _) = self.node(reference)?;
        let asset = self.index.assets.get(node.path())?;
        let types = self.extensions.node_types_of(asset).iter().map(|node_type| node_type.name.to_string()).collect();
        Some(pe2_ext::NodeNamed { path: node.path().to_string(), uid: node.uid().map(str::to_string), types })
    }

    fn node(&self, reference: &toml::Value) -> Option<(&dyn graph::exec::Node, graph::Plan)> {
        let asset: &index::Asset = if let Some(version) = reference.as_str().filter(|r| r.starts_with(index::BELOW)) {
            self.index.version(version)?
        } else if let Some(path) = reference.as_str().filter(|r| !r.starts_with("uid://")) {
            self.index.assets.get(path)?
        } else {
            let (uid, path) = index::reference(reference)?;
            self.index.resolve(&uid, path.as_deref().unwrap_or_default())?
        };
        let asset = if self.written { asset.written() } else { asset };
        let (plan, _) = self.extensions.plan(asset);
        Some((asset as &dyn graph::exec::Node, plan))
    }

    fn entries(&self, folder: &str) -> Vec<String> {
        let within = format!("{}/", folder.trim_end_matches('/'));
        let files = self.index.entries.range(within.clone()..).take_while(|(path, _)| path.starts_with(&within));
        files.filter(|(_, entry)| entry.kind == index::Kind::File).map(|(path, _)| path.clone()).collect()
    }

    fn place(&self, path: &str) -> Option<std::path::PathBuf> {
        std::fs::canonicalize(&self.index.entries.get(path)?.disk).ok()
    }

    fn entry(&self, path: &str) -> Option<std::path::PathBuf> {
        self.index.entries.get(path).filter(|entry| entry.kind == index::Kind::File).map(|entry| entry.disk.clone())
    }

    fn assets_of(&self, node_type: &str) -> Vec<(String, String)> {
        let found = self.extensions.find(self.index, node_type).filter(|asset| !self.extensions.is_disabled(&asset.path));
        found.map(|asset| (asset.path.clone(), asset.settings_text().to_string())).collect()
    }
}

/// What an extension's operation is lent of its node: its path and type, its declared settings (recorded as
/// read when it is lent), and contents read by their hashes.
struct Hosted<'a> {
    call: &'a graph::exec::Call<'a>,
}

impl<'a> Hosted<'a> {
    fn new(call: &'a graph::exec::Call<'a>) -> Hosted<'a> {
        Hosted { call }
    }
}

/// Start an extension's stream operation: its generator given what the call is, then kept with what it reads by - its
/// node's settings, and the contents its inputs held, where they are - for the stream's life past the call.
fn start_stream(
    call: &mut graph::exec::Call<'_>,
    start: &pe2_ext::StartStream,
) -> Result<Box<dyn graph::stream::Producer>> {
    let inputs = call.take_inputs();
    let mut stored = Vec::new();
    for value in inputs.iter().flatten() {
        graph::exec::stored_contents(value, &mut stored);
    }
    let contents: HashMap<String, std::path::PathBuf> =
        stored.into_iter().filter_map(|sha1| Some((sha1.clone(), call.content_path(&sha1)?))).collect();
    let hosted = Hosted::new(call);
    let lent_carried = pe2_ext::Call::carried_by(&inputs);
    let mut lent = pe2_ext::Call { inputs, settings: call.settings(), made: std::cell::RefCell::default(), carried: lent_carried.clone(), parameter_text: call.parameter(), host: &hosted };
    let generator = start(&mut lent)?;
    let node = call.recorder().node();
    let host = StreamHost {
        path: node.path().to_string(),
        contents,
    };
    let (settings, parameter) = (call.settings().clone(), call.parameter().map(str::to_string));
    Ok(Box::new(Generating { generator, host, settings, carried: lent_carried, parameter }))
}

/// What a stream's generator is lent between its calls: its node's path, type and settings, and the contents its
/// inputs held, read where they are.
struct StreamHost {
    path: String,
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

    fn node_type(&self) -> Option<&str> {
        None
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
    carried: Table,
    parameter: Option<String>,
}

impl graph::stream::Producer for Generating {
    fn header(&self) -> Table {
        self.generator.header()
    }

    fn next(&mut self, count: usize) -> Result<Vec<graph::Value>> {
        let call = pe2_ext::Call { inputs: vec![], settings: &self.settings, made: std::cell::RefCell::default(), carried: self.carried.clone(), parameter_text: self.parameter.as_deref(), host: &self.host };
        self.generator.next(&call, count)
    }

    fn seek(&mut self, position: u64) -> Result<bool> {
        let call = pe2_ext::Call { inputs: vec![], settings: &self.settings, made: std::cell::RefCell::default(), carried: self.carried.clone(), parameter_text: self.parameter.as_deref(), host: &self.host };
        self.generator.seek(&call, position)
    }
}

impl pe2_ext::CallHost for Hosted<'_> {
    fn path(&self) -> &str {
        self.call.recorder().node().path()
    }

    fn node_type(&self) -> Option<&str> {
        None
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

    fn entries(&self, folder: &str) -> Result<Vec<String>> {
        self.call.entries(folder)
    }

    fn read_entry(&self, path: &str) -> Result<Vec<u8>> {
        self.call.read_entry(path)
    }

    fn place(&self, path: &str) -> Option<String> {
        self.call.place(path)
    }

    fn node_named(&self, reference: &str) -> Option<pe2_ext::NodeNamed> {
        self.call.node_named(&toml::Value::String(reference.to_string()))
    }

    fn output_of(&self, path: &str, address: &str) -> Result<graph::Value> {
        Ok((*self.call.output_of(&toml::Value::String(path.to_string()), address)?).clone())
    }

    fn build_config(&self) -> Option<pe2_ext::data::BuildConfig> {
        self.call.config()
    }

    fn assets_of(&self, node_type: &str) -> Result<Vec<(String, String)>> {
        self.call.assets_of(node_type)
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
    use pe2_ext::{Extension, Registry, Result, Setting};

    fn images(r: &mut Registry) -> Result<()> {
        r.named_type("image").alias("image").register();
        Ok(())
    }

    fn fonts(r: &mut Registry) -> Result<()> {
        r.named_type("font").register();
        Ok(())
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
    fn a_dependency_holds_as_its_type_says() {
        fn compiler(r: &mut Registry) -> Result<()> {
            let _ = r.op("compile").output::<pe2_ext::ty::Image>().run(|_| Ok(pe2_ext::Image { width: 1, height: 1, rgba: vec![0; 4] }));
            Ok(())
        }
        fn movie(r: &mut Registry) -> Result<()> {
            let _ = r.named_type("movie").register();
            Ok(())
        }
        fn editing(r: &mut Registry) -> Result<()> {
            let _ = r.extend("test.movies@movie").of("test.editing@built").register();
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
        // Adding to a type of what it can do without, which is not there: what it adds is not there, and nothing else
        // changes. Not declared, it is an error.
        let movies = || Loadable::from(of("test.movies", &[], &[], &[], movie));
        let plain = load(vec![of("test.editing", &[], &["test.movies"], &[], editing).into()]);
        assert_eq!(plain.loaded.len(), 1, "{:?}", plain.problems);
        let added = load(vec![movies(), of("test.editing", &["test.movies"], &[], &[], editing).into()]);
        assert!(added.types["test.movies@movie"].0.values.contains(&"test.editing@built".to_string()));
        let undeclared = load(vec![movies(), of("test.editing", &[], &[], &[], editing).into()]);
        assert_eq!(undeclared.loaded.len(), 1);
    }

    #[test]
    fn a_service_is_met_by_what_provides_it_and_an_extension_adds_to_a_type() {
        fn compiler(r: &mut Registry) -> Result<()> {
            let _ = r.op("compile").output::<pe2_ext::ty::Image>().run(|_| Ok(pe2_ext::Image { width: 1, height: 1, rgba: vec![0; 4] }));
            Ok(())
        }
        fn nothing(_: &mut Registry) -> Result<()> {
            Ok(())
        }
        fn movie(r: &mut Registry) -> Result<()> {
            let _ = r.named_type("movie").register();
            Ok(())
        }
        fn editing(r: &mut Registry) -> Result<()> {
            let _ = r.extend("test.movies@movie").of("test.editing@built").register();
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
        assert!(extensions.types["test.movies@movie"].0.values.iter().any(|value| value == "test.editing@built"));
        // Two providers: the first, the other said.
        let both = load(vec![
            extension("test.gcc", &[], &["c-compiler@1.0.0"], compiler),
            extension("test.clang", &[], &["c-compiler@1.0.0"], compiler),
        ]);
        assert!(both.problems.iter().any(|p| p.contains("both provide c-compiler")), "{:?}", both.problems);
        // Adding to a type it does not require is refused.
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
            "test.a is disabled: its requirements form a cycle",
        ] {
            assert!(problems.contains(expected), "{expected}\n---\n{problems}");
        }
        assert_eq!(loaded.type_aliases["image"], "test.images@image");
        assert!(loaded.types.contains_key("test.fonts@font"));
    }

    #[test]
    fn only_builtins_alias() {
        let loaded = Extensions::load(vec![extension("mod.images", "0.1.0", &[], images)], false);
        assert_eq!(loaded.loaded, []);
        assert!(loaded.problems[0].contains("only a built-in extension names a type bare"), "{:?}", loaded.problems);
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
        // Standing in for a built-in, it names types bare as the built-in did.
        assert_eq!(loaded.type_aliases["image"], "test.images@image");
        let replaced: Vec<&Origin> =
            loaded.present.iter().filter(|p| matches!(p.status, Status::Replaced(_))).map(|p| &p.origin).collect();
        assert_eq!(replaced, [&user, &Origin::Linked]);
        // Two from one place: the second is refused, a problem.
        let mut other = from(user.clone());
        other.origin = Origin::User("copy.wasm".into());
        let loaded = Extensions::load(vec![from(user), other], false);
        assert!(loaded.problems.iter().any(|p| p.contains("another of its id from the same place")), "{:?}", loaded.problems);
        // Taking no built-in's place, it may not name types bare.
        assert!(loaded.problems.iter().any(|p| p.contains("only a built-in extension names a type bare")));
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
    use pe2_ext::{Extension, Image, Project, Registry, Result, Table, Value, anyhow, ty};

    fn paint(r: &mut Registry) -> Result<()> {
        // A picture of one red pixel, whatever the canvas is.
        let _ = r
            .op("draw")
            .takes("canvas", &["test.paint@canvas"])
            .output::<ty::Image>()
            .provides("picture")
            .run(|_| Ok(Image { width: 1, height: 1, rgba: vec![255, 0, 0, 255] }));
        r.named_type("canvas").alias("canvas").data().register();
        r.service("red", "The color red", "{}", |_, _| Ok(Value::String("red".into())));
        Ok(())
    }

    /// Asks the painter, which it requires, for its service and a picture.
    fn scene(r: &mut Registry) -> Result<()> {
        r.service("frame", "A frame", "{}", |project: &dyn Project, _| {
            let color = project.call("test.paint@red", &Value::Table(Table::new()))?;
            let picture = project.picture("a", None).ok_or_else(|| anyhow!("no picture"))??;
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
        let canvas = "[[stack]]\nop = \"test.paint@canvas\"\n";
        for (path, text) in [("project.toml", "[[layer]]\nname = \"base\"\n"), (".layers/base/a.node/node.toml", canvas)] {
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
        let lens = Lens { index: &index, extensions: &extensions, selection: None, revision: 0, chosen: None };
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
