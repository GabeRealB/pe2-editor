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

use crate::index;

/// Where an extension runs from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    /// A component built into the host: a built-in.
    Builtin,
    /// A component, from this file.
    Component(PathBuf),
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
    /// Its component, where it runs as one: its instances, their memory.
    component: Option<Arc<component::Shared>>,
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
        }
    }
}

/// A loaded extension.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Loaded {
    pub id: String,
    pub version: String,
    pub origin: Origin,
}

/// The extensions loaded, and what they registered.
#[derive(Default)]
pub struct Extensions {
    pub loaded: Vec<Loaded>,
    /// What was refused or disabled, and why.
    pub problems: Vec<String>,
    kinds: HashMap<String, (KindDef, usize)>,
    roles: HashMap<String, RoleDef>,
    panels: Vec<PanelDef>,
    tools: Vec<ToolDef>,
    formats: Vec<FormatDef>,
    importers: HashMap<String, ImporterDef>,
    extractors: Vec<pe2_ext::ExtractorDef>,
    /// The components of the extensions loaded, by id.
    components: Vec<(String, Arc<component::Shared>)>,
    /// The formats recognized in files, by their content's hash.
    recognized: Mutex<HashMap<String, Vec<(usize, Recognized)>>>,
    /// Bare names declarations write, to the qualified names of the built-ins registering them.
    kind_aliases: HashMap<String, String>,
    role_aliases: HashMap<String, String>,
}

/// A version's major, minor and patch numbers.
fn semver(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.split('.').map(|p| p.parse::<u64>().ok());
    Some((parts.next()??, parts.next().unwrap_or(Some(0))?, parts.next().unwrap_or(Some(0))?))
}

/// Whether `version` meets a caret requirement (`^0.1`, `0.1.2`): at least it, and the same major (the same minor
/// below 1.0).
fn satisfies(version: &str, requirement: &str) -> bool {
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
        let mut out = Extensions::default();
        let mut pending: Vec<Loadable> = vec![];
        for extension in extensions {
            if pending.iter().any(|e| e.id == extension.id) {
                out.problems.push(format!("{}: another extension has this id; the second is not loaded", extension.id));
            } else {
                pending.push(extension);
            }
        }
        let ids: HashSet<String> = pending.iter().map(|e| e.id.clone()).collect();
        let mut disabled: HashSet<String> = HashSet::new();
        loop {
            let mut progress = false;
            for extension in &mut pending {
                if disabled.contains(&extension.id) || out.loaded.iter().any(|l| l.id == extension.id) {
                    continue;
                }
                let mut ready = true;
                for text in &extension.requires {
                    let (id, version) = requirement(text);
                    let blocked = if !ids.contains(id) {
                        Some(format!("it needs {text}, which is not installed"))
                    } else if disabled.contains(id) {
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
                        disabled.insert(extension.id.clone());
                        ready = false;
                        progress = true;
                        break;
                    }
                }
                if !ready || disabled.contains(&extension.id) {
                    continue;
                }
                progress = true;
                if let Err(e) = out.register(extension, builtin) {
                    out.problems.push(format!("{} is disabled: {e:#}", extension.id));
                    disabled.insert(extension.id.clone());
                }
            }
            if !progress {
                break;
            }
        }
        for extension in &pending {
            if !disabled.contains(&extension.id) && !out.loaded.iter().any(|l| l.id == extension.id) {
                out.problems.push(format!("{} is disabled: its requirements form a cycle", extension.id));
            }
        }
        out
    }

    /// Call an extension's entry point and take what it registered, checked.
    fn register(&mut self, extension: &mut Loadable, builtin: bool) -> Result<()> {
        let mut registry = Registry::new(&extension.id);
        let register = extension.register.take().context("it was registered already")?;
        register(&mut registry).context("its registration failed")?;
        let parts = registry.into_parts();
        let (kinds, roles) = (parts.kinds, parts.roles);
        let dependencies: Vec<&str> = extension.requires.iter().map(|r| requirement(r).0).collect();
        let mut seen = HashSet::new();
        let names = kinds.iter().map(|k| &k.name).chain(roles.iter().map(|r| &r.name));
        let names = names.chain(parts.panels.iter().map(|p| &p.name)).chain(parts.tools.iter().map(|t| &t.name));
        let names = names.chain(parts.formats.iter().map(|f| &f.name)).chain(parts.importers.iter().map(|i| &i.name));
        let names = names.chain(parts.extractors.iter().map(|e| &e.name));
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
        self.loaded.push(Loaded { id, version, origin });
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
        self.formats.extend(parts.formats);
        self.importers.extend(parts.importers.into_iter().map(|i| (i.name.to_string(), i)));
        self.extractors.extend(parts.extractors);
        Ok(())
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
        format!("{}@{name} {}", loaded.id, loaded.version)
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

    /// The tools the extensions registered, for agents.
    #[must_use]
    pub fn tools(&self) -> &[ToolDef] {
        &self.tools
    }

    /// Every asset of a kind, by its qualified name or a built-in's bare one.
    pub fn find<'a>(&'a self, index: &'a index::Index, kind: &str) -> impl Iterator<Item = &'a index::Asset> {
        let wanted = self.kind_named(kind).map(|(k, _)| k.name.as_str());
        index.assets.values().filter(move |a| {
            wanted.is_some_and(|w| self.kind_of(a).is_some_and(|k| k.name.as_str() == w))
                || wanted.is_none() && a.kind.as_deref() == Some(kind)
        })
    }

    /// An asset's importer, as its import settings name it.
    #[must_use]
    pub fn importer_of(&self, asset: &index::Asset) -> Option<&ImporterDef> {
        let name = asset.fields().get("import")?.get("importer")?.as_str()?;
        self.importers.get(name)
    }

    /// An importer, by its qualified name.
    #[must_use]
    pub fn importer(&self, name: &str) -> Option<&ImporterDef> {
        self.importers.get(name)
    }

    /// What each extension running as a component holds: its live instances and their memory, by id.
    #[must_use]
    pub fn memory(&self) -> Vec<(&str, component::Memory)> {
        self.components.iter().map(|(id, c)| (id.as_str(), c.memory())).collect()
    }

    /// A frame ended: each extension's readings' instance unused for long enough is reclaimed.
    pub fn frame(&self) {
        for (_, component) in &self.components {
            component.frame();
        }
    }

    /// Reclaim every readings' instance now: where there are no frames (the agents' server, between requests).
    pub fn release(&self) {
        for (_, component) in &self.components {
            component.release();
        }
    }

    /// The extractor of assets of a declared kind and role (`image`, `atlas`), where an extension registers one.
    #[must_use]
    pub fn extractor_for(&self, kind: &str, role: Option<&str>) -> Option<&pe2_ext::ExtractorDef> {
        let (Some(kind), role) = self.qualify(kind, role) else { return None };
        self.extractors.iter().find(|e| e.takes(kind, role))
    }

    /// The native files an asset's importer makes of its source, by name; none where its importer is unknown or makes
    /// nothing (a native importer).
    ///
    /// # Errors
    /// The import failed.
    pub fn import(&self, asset: &Asset) -> Option<Result<pe2_ext::Imported>> {
        let import = self.importer(asset.importer()?)?.import.as_ref()?;
        Some(import(asset))
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

    /// The importers that fit an asset's source - the file its import settings name - with how far each takes it, the
    /// highest priority first. An importer not reading any of the source's formats is not listed.
    #[must_use]
    pub fn importers_for(&self, asset: &index::Asset) -> Vec<(&ImporterDef, Support)> {
        let Some(source) = asset.fields().get("import").and_then(|i| i.get("source")).and_then(Value::as_str) else {
            return vec![];
        };
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
    use super::{Extensions, Loadable, satisfies};
    use pe2_ext::{Extension, Name, Registry, Result};

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
            "test.images: another extension has this id",
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
    fn caret_requirements() {
        assert!(satisfies("0.1.4", "^0.1"));
        assert!(satisfies("0.1.4", "0.1.2"));
        assert!(!satisfies("0.1.1", "0.1.2"));
        assert!(!satisfies("0.2.0", "^0.1"));
        assert!(satisfies("1.4.0", "^1.2"));
        assert!(!satisfies("2.0.0", "^1.2"));
    }
}
