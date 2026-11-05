//! What a package compiles from, in two steps around a target's first pass. `gather` reads text, as what it finds decides what a
//! preprocessor is given: every `#include`, whichever branch of a conditional it is in, and the options read, which `#if` needs. What is
//! written and not found is left for the compiler to say. `resolve` reads what the first pass left, the live code with its other macros
//! expanded: the slots and values a package really uses, each made only where it is read, an error being a macro that fires at the use.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fmt::Write as _;

use pe2_ext::tokens::{self, Args};
use pe2_ext::ty::{Registered, SourceTree};
use pe2_ext::{Call, File, Result, Table, Typed, Value, anyhow, bail};

use crate::types::{Shape, Tree, Types};
use crate::{scan, tokenizers};

/// The package holding what every unit includes first, and what the assembler reads.
const CORE: &str = "core";
/// What `pe2.h`, of `core`, includes: the values of the slots of the package being built.
pub const VALUES: &str = "pe2_values.h";
/// What it includes in both passes: the options and the build config.
pub const OPTIONS: &str = "pe2_options.h";
/// The folders of the tree its includes are found in, in order, one a line: `quote <dir>` for what it includes in quotes, `angle <dir>`
/// for what in angle brackets - what a target puts on its include path.
pub const INCLUDES: &str = "includes";
/// What a gathered package links besides its own objects, a line each: `dependency <path>` and `bound <path>`.
pub const LINKED: &str = "linked";
/// What declares a package's slots and options, and how the editor shows them: all of it in one of the package's units, whose order is
/// the order the editor lists them in.
const DECLARING: [&str; 5] = ["PE2_BIND", "PE2_BIND_ARG", "PE2_BIND_SELF", "PE2_OPTION_DEF", "PE2_EDITOR_LABEL"];
/// What a gathered tree is of: the package's name, which names its objects.
pub const NAME: &str = "package";
/// Where a package's tree holds what it can include of others, each under its name: what it includes in angle brackets.
const DEP: &str = "dep";
/// What the build makes for the package being built, which core's `pe2.h` includes in quotes.
const MADE: [&str; 3] = [VALUES, OPTIONS, "pe2_server.h"];
/// Where a first pass leaves what it made of each unit.
const FIRST: &str = "first/";
/// The most a line of a macro takes, as a preprocessor may limit one, so a long value is a chain of macros.
const LINE: usize = 32_000;

/// A package of the build graph: the graph has one, by name.
pub struct Package {
    pub path: String,
    /// What it declares, as its target reads it (`pe2_ext::targeted`) ...
    fields: Table,
    /// ... and as written, every target's part with it.
    declared: Table,
    /// Its files, by their paths, as the stack resolves them.
    files: BTreeSet<String>,
    /// What it depends on, by name, as its declaration says for its target.
    dependencies: Vec<String>,
}

impl Package {
    /// Whether it has anything of its own to build.
    pub fn compiles(&self) -> bool {
        VISIBILITIES.iter().any(|visibility| !declared(&self.fields, "sources", visibility).is_empty())
    }

    /// Whether it gives what links it objects: public sources compiled, or public objects as they are.
    fn makes_objects(&self) -> bool {
        ["sources", "objects"].iter().any(|key| !declared(&self.fields, key, PUBLIC).is_empty())
    }

    /// Whether its target makes it an image, which others link against by the symbols it publishes: it has a link script, or,
    /// built elsewhere, publishes symbols (`symbols`).
    fn makes_image(&self) -> bool {
        self.fields.contains_key("link_script") || self.fields.contains_key("symbols")
    }

    /// The folders its own quoted includes are found in, within it: its public one (`headers.public`), then its private ones
    /// (`headers.private`).
    pub fn header_folders(&self) -> Vec<&str> {
        let headers = self.fields.get("headers");
        let public = headers.and_then(|headers| headers.get(PUBLIC)).and_then(Value::as_str);
        public.into_iter().chain(declared(&self.fields, "headers", PRIVATE)).collect()
    }

    /// The folder of what others may include of it, from the project's root, if it has one: one, which its name stands for.
    pub fn public_folder(&self) -> Option<String> {
        let headers = self.fields.get("headers");
        headers.and_then(|headers| headers.get(PUBLIC)).and_then(Value::as_str).map(|folder| format!("{}/{folder}", self.path))
    }

    /// Whether a file is one of its.
    fn owns(&self, file: &str) -> bool {
        file.strip_prefix(self.path.as_str()).is_some_and(|inside| inside.starts_with('/'))
    }
}

/// What a package's files are for, which its declaration says of each kind: `private`, its own build's; `public`, its own and, as
/// far as an image, what links it.
const PRIVATE: &str = "private";
const PUBLIC: &str = "public";
const VISIBILITIES: [&str; 2] = [PRIVATE, PUBLIC];

/// The names a package declares of a kind, of one visibility (`sources.public`).
fn declared<'a>(fields: &'a Table, key: &str, visibility: &str) -> Vec<&'a str> {
    fields.get(key).and_then(|kind| kind.get(visibility)).and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect()
}

/// Whether a package declares its kinds as both builds read them: each by visibility, its public headers one folder.
fn shaped(path: &str, fields: &Table) -> Result<()> {
    for key in ["headers", "sources", "objects"] {
        let Some(kind) = fields.get(key) else { continue };
        let by_visibility = kind.as_table().is_some_and(|kind| kind.keys().all(|visibility| VISIBILITIES.contains(&visibility.as_str())));
        if !by_visibility {
            bail!("{path}: {key} is declared by visibility: {key}.{PRIVATE}, {key}.{PUBLIC}");
        }
    }
    if fields.get("headers").and_then(|headers| headers.get(PUBLIC)).is_some_and(|public| public.as_str().is_none()) {
        bail!("{path}: headers.public is one folder, which its name stands for");
    }
    Ok(())
}

/// A file to take in: where it is in the project, where it goes in the tree, and whose it is.
struct Taken {
    entry: String,
    place: String,
    owner: String,
}

/// The tree being gathered.
struct Gathering<'a> {
    call: &'a Call<'a>,
    packages: HashMap<String, Package>,
    /// The package gathered.
    own: String,
    tree: BTreeMap<String, Vec<u8>>,
    /// The packages whose target files it takes: its own, and the images it links against.
    reached: BTreeSet<String>,
    pending: Vec<Taken>,
    options: Options,
    declaring: Declaring,
}

/// Every package's name, by its path: what others include it by (`<engine/fs.h>`) and its objects are named by - its folder's name,
/// after the name of the nearest package whose folder holds it. A package nests in packages, not in folders: `packages/a/aya` and
/// `packages/b/aya` are both `aya`, and `packages/aya/akropolis` is `aya/akropolis`. (`declarations.py`'s `Declarations.names`.)
fn package_names(paths: &[String]) -> HashMap<String, String> {
    let mut sorted: Vec<&String> = paths.iter().collect();
    sorted.sort();
    let mut names: HashMap<String, String> = HashMap::new();
    for path in sorted {
        let outer = paths.iter().filter(|other| path.strip_prefix(other.as_str()).is_some_and(|rest| rest.starts_with('/'))).max_by_key(|other| other.len());
        let last = name_of(path);
        let name = match outer.and_then(|outer| names.get(outer)) {
            Some(outer) => format!("{outer}/{last}"),
            None => last.to_string(),
        };
        names.insert(path.clone(), name);
    }
    names
}

/// Where a package declares its slots and options: one of its units, the first that does, which any other declaring breaks
/// (`gen_package_values.py`'s `gathered`) - never a header, which whatever includes it reads.
struct Declaring {
    file: Option<String>,
    /// The package's units, by their paths, and its name.
    units: BTreeSet<String>,
    own: String,
}

impl Declaring {
    fn of(own: &str, units: &[String]) -> Declaring {
        Declaring { file: None, units: units.iter().cloned().collect(), own: own.to_string() }
    }

    fn check(&mut self, found: &scan::Use, file: &str) -> Result<()> {
        if !DECLARING.contains(&found.name) {
            return Ok(());
        }
        let at = format!("{file}:{}", found.line);
        if !self.units.contains(file) {
            bail!("{at}: {} in no unit of {}: a package declares its slots and options in one of its units, never in a header", found.name, self.own);
        }
        match &self.file {
            Some(first) if first != file => {
                bail!("{at}: {} outside {first}: a package's slots and options are declared in one file, in the order the editor lists them", found.name)
            }
            _ => self.file = Some(file.to_string()),
        }
        Ok(())
    }
}

/// The name of the package at a path, in the graph.
fn name_at(packages: &HashMap<String, Package>, path: &str) -> Result<String> {
    packages.iter().find(|(_, package)| package.path == path).map(|(name, _)| name.clone()).ok_or_else(|| anyhow!("{path} is no package of the graph"))
}

/// The package an include in angle brackets names, and the path within its public folder: the longest name it starts with.
fn spelled<'a>(packages: &HashMap<String, Package>, spelled: &'a str) -> Option<(String, &'a str)> {
    let name = packages.keys().filter(|name| spelled.strip_prefix(name.as_str()).is_some_and(|rest| rest.starts_with('/'))).max_by_key(|name| name.len())?;
    Some((name.clone(), &spelled[name.len() + 1..]))
}

/// The package a file is of: the one whose folder holds it nearest.
fn owner_of(packages: &HashMap<String, Package>, file: &str) -> Option<String> {
    packages.iter().filter(|(_, package)| package.owns(file)).max_by_key(|(_, package)| package.path.len()).map(|(name, _)| name.clone())
}

/// The names of everything a package depends on, however far.
fn closure(packages: &HashMap<String, Package>, name: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut pending: Vec<String> = packages.get(name).map_or_else(Vec::new, |package| package.dependencies.clone());
    while let Some(next) = pending.pop() {
        if found.insert(next.clone()) {
            pending.extend(packages.get(&next).map_or_else(Vec::new, |package| package.dependencies.clone()));
        }
    }
    found
}

/// What a package's image links: the libraries whose objects it takes, and the images it links against. Everything it depends on,
/// and theirs, as far as an image: an image is linked against by its symbols, and what it depends on is its own, so a library reached
/// through it is not taken again (Psy-Q's members are the executable's).
fn linked(packages: &HashMap<String, Package>, own: &str) -> (Vec<String>, Vec<String>) {
    let (mut libraries, mut images, mut covered) = (vec![], vec![], BTreeSet::new());
    let mut pending: VecDeque<String> = packages.get(own).map_or_else(VecDeque::new, |package| package.dependencies.iter().cloned().collect());
    while let Some(name) = pending.pop_front() {
        if name == own || libraries.contains(&name) || images.contains(&name) {
            continue;
        }
        let Some(package) = packages.get(&name) else { continue };
        if package.makes_image() {
            covered.extend(closure(packages, &name));
            images.push(name);
            continue;
        }
        pending.extend(package.dependencies.iter().cloned());
        libraries.push(name);
    }
    let objects = libraries.into_iter().filter(|name| !covered.contains(name) && packages.get(name).is_some_and(Package::makes_objects)).collect();
    (objects, images)
}

/// The file an include in a file of `owner` names, by the rules both builds hold every package's files to (`declarations.py`'s
/// `Package.resolved`): in quotes, a file of the same package - of the same public folder, from a public header - or one the build
/// makes (`pe2_options.h`); in angle brackets, a public file of a package its declaration names. With whose it is. A file not there is
/// the compiler's to say: it knows which branch of a conditional is taken.
fn resolved(packages: &HashMap<String, Package>, owner: &str, from: &str, written: &str, quoted: bool, at: &str) -> Result<Option<(String, String)>> {
    let package = packages.get(owner).ok_or_else(|| anyhow!("{at}: {owner} is no package"))?;
    if quoted {
        if MADE.contains(&written) {
            return Ok(None);
        }
        let public = package.public_folder();
        let in_public = public.as_ref().is_some_and(|public| from.starts_with(&format!("{public}/")));
        let mut folders = vec![folder_of(from).to_string()];
        if !in_public {
            folders.extend(package.header_folders().into_iter().map(|folder| format!("{}/{folder}", package.path)));
        }
        let within = if in_public { public.unwrap_or_default() } else { package.path.clone() };
        for folder in folders {
            let entry = normal(&format!("{folder}/{written}"));
            if packages.values().any(|any| any.files.contains(&entry)) {
                if !entry.starts_with(&format!("{within}/")) {
                    bail!("{at}: \"{written}\" is not {} of {owner}", if in_public { "a public file" } else { "a file" });
                }
                return Ok(Some((entry, owner.to_string())));
            }
        }
        bail!("{at}: \"{written}\" is no file of {owner}; another package's is included in angle brackets");
    }
    let (name, inside) = spelled(packages, written).ok_or_else(|| anyhow!("{at}: <{written}> names no package"))?;
    if !package.dependencies.contains(&name) {
        bail!("{at}: <{written}> is {name}'s, which {owner} does not depend on");
    }
    let entry = packages.get(&name).and_then(Package::public_folder).map(|public| format!("{public}/{inside}"));
    Ok(entry.filter(|entry| packages.get(&name).is_some_and(|dependency| dependency.files.contains(entry))).map(|entry| (entry, name)))
}

/// Where a file goes in a package's tree: its own at `self/<path>`, a dependency's public file at `dep/<name>/<path>`, where the
/// dependency's name is what includes it in angle brackets.
fn place_of(packages: &HashMap<String, Package>, own: &str, owner: &str, entry: &str) -> String {
    let Some(package) = packages.get(owner) else { return entry.to_string() };
    if owner == own {
        return format!("self/{}", entry.strip_prefix(&format!("{}/", package.path)).unwrap_or(entry));
    }
    let public = package.public_folder().unwrap_or_default();
    format!("{DEP}/{owner}/{}", entry.strip_prefix(&format!("{public}/")).unwrap_or(entry))
}

pub fn gather(call: &mut Call) -> Result<Typed> {
    let call: &Call = call;
    let path = call.host.path().to_string();
    let packages = packages(call)?;
    let name = name_at(&packages, &path)?;
    let own = &packages[&name];
    let (own_path, own_files) = (own.path.clone(), own.files.clone());
    let (objects, images) = linked(&packages, &name);
    let mut gathering = Gathering { call, packages, own: name.clone(), tree: BTreeMap::new(), reached: BTreeSet::new(), pending: vec![], options: Options::new(&target_of(call).unwrap_or_default()), declaring: Declaring::of(&name, &[]) };
    // A library may have nothing to compile: no units; sources declared and matching nothing are a mistake.
    let fields = own_fields(call);
    let units = if gathering.packages[&name].compiles() { selected(&fields, &own_path, &own_files)? } else { vec![] };
    gathering.declaring = Declaring::of(&name, &units.iter().map(|(unit, _)| unit.clone()).collect::<Vec<_>>());
    let within = format!("{own_path}/");
    // Each unit by its visibility and place: what the target compiles, and whether what it makes goes to what links the package.
    let mut listed = String::new();
    for (unit, visibility) in &units {
        let place = format!("self/{}", unit.strip_prefix(&within).unwrap_or(unit));
        let _ = writeln!(listed, "{visibility} {place}");
        gathering.pending.push(Taken { entry: unit.clone(), place, owner: name.clone() });
    }
    gathering.reached.insert(name.clone());
    gathering.reached.extend(images.iter().cloned());
    gathering.follow()?;
    let header = gathering.options.header(call)?;
    gathering.tree.insert(OPTIONS.to_string(), header.into_bytes());
    gathering.take_target_files(&name)?;
    // Where its includes are found: in quotes, its own folders; in angle brackets, its dependencies, each under its name.
    let own = &gathering.packages[&name];
    let mut dirs: Vec<String> = own.header_folders().into_iter().map(|folder| format!("quote self/{folder}")).collect();
    dirs.push(format!("angle {DEP}"));
    // What its image links besides its own: the objects of the libraries it takes, and what the images it depends on publish.
    let mut linked = String::new();
    for name in &objects {
        let _ = writeln!(linked, "dependency {}", gathering.packages[name].path);
    }
    for name in images.iter().filter(|name| gathering.packages[*name].fields.contains_key("link_script")) {
        let _ = writeln!(linked, "bound {}", gathering.packages[name].path);
    }
    let mut files: Vec<File> = vec![
        File::new("units", listed.into_bytes()),
        File::new(INCLUDES, dirs.join("\n").into_bytes()),
        File::new(LINKED, linked.into_bytes()),
        File::new(NAME, name.into_bytes()),
    ];
    let notes = gathering.options.notes(call);
    files.extend(gathering.tree.into_iter().map(|(place, bytes)| File::new(&place, bytes)));
    // For a first pass stopped by one of these in `#if`, whose preprocessor can only say it divided by zero.
    let unset = gathering.options.unset(&own_fields(call));
    let settings = Table::from_iter([("unset".to_string(), Value::Array(unset.into_iter().map(Value::String).collect()))]);
    Ok(Typed { notes, settings: pe2_ext::merged(&call.given(), &settings), ..Typed::new(SourceTree::NAME, files) })
}

fn name_of(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The target the config builds for, which a package is read as (`pe2_ext::targeted`); none outside a build.
fn target_of(call: &Call) -> Option<String> {
    call.config().and_then(|config| config.target)
}

/// What this package's node declares, as its target reads it.
fn own_fields(call: &Call) -> Table {
    pe2_ext::targeted(&call.given(), target_of(call).as_deref())
}

/// The build graph's one table: every `package` asset by its name, with its files and what it depends on.
pub fn packages(call: &Call) -> Result<HashMap<String, Package>> {
    let target = target_of(call);
    let mut read = vec![];
    for (path, declaration) in call.assets_of("package")? {
        let declared: Table = declaration.parse().map_err(|e| anyhow!("{path}: {e}"))?;
        let fields = pe2_ext::targeted(&declared, target.as_deref());
        shaped(&path, &fields)?;
        let files = call.entries(&path)?.into_iter().collect();
        read.push((path, fields, declared, files));
    }
    let names = package_names(&read.iter().map(|(path, ..)| path.clone()).collect::<Vec<_>>());
    let mut packages = HashMap::new();
    for (path, fields, declared, files) in read {
        let mut dependencies = vec![];
        for reference in fields.get("dependencies").and_then(Value::as_array).into_iter().flatten() {
            let target = pe2_ext::target_of(reference).ok_or_else(|| anyhow!("{path}: a dependency names nothing"))?;
            let node = call.node_named(target).ok_or_else(|| anyhow!("{path}: its dependency {target} names nothing"))?;
            dependencies.push(names.get(&node.path).cloned().ok_or_else(|| anyhow!("{path}: {} is no package", node.path))?);
        }
        let name = names[&path].clone();
        let package = Package { path: path.clone(), fields, declared, files, dependencies };
        if let Some(other) = packages.insert(name.clone(), package) {
            bail!("{} and {path} are both named {name}, in the same package or none", other.path);
        }
    }
    // Two packages' public files at one place: one with a public folder inside one that has one.
    for name in packages.iter().filter(|(_, package)| package.public_folder().is_some()).map(|(name, _)| name) {
        if let Some(outer) = packages.iter().find(|(outer, package)| package.public_folder().is_some() && name.starts_with(&format!("{outer}/"))) {
            bail!("the package {name}: its public files and {}'s are both <{name}/...>", outer.0);
        }
    }
    Ok(packages)
}

/// The files `sources` names, of every visibility, each with its visibility: a path or a glob of `*` within a step, a leading `!`
/// leaving out what is matched.
fn selected(fields: &Table, path: &str, files: &BTreeSet<String>) -> Result<Vec<(String, &'static str)>> {
    let kept: Vec<(String, &'static str)> =
        VISIBILITIES.iter().flat_map(|visibility| listed(fields, "sources", visibility, path, files).into_iter().map(move |file| (file, *visibility))).collect();
    if kept.is_empty() {
        bail!("{path}: its sources name no file");
    }
    Ok(kept)
}

/// The files a package declares of a kind and visibility (`sources.private`, `objects.public`): globs from its folder, a leading
/// `!` leaving out what is matched.
fn listed(fields: &Table, key: &str, visibility: &str, path: &str, files: &BTreeSet<String>) -> Vec<String> {
    let patterns = declared(fields, key, visibility);
    let within = format!("{path}/");
    let matched = |file: &str, wanted: bool| {
        patterns.iter().filter(|p| p.starts_with('!') != wanted).any(|p| matches(p.trim_start_matches('!'), file))
    };
    let kept: Vec<String> = files
        .iter()
        .filter(|file| file.strip_prefix(&within).is_some_and(|inside| matched(inside, true) && !matched(inside, false)))
        .cloned()
        .collect();
    kept
}

/// A path against a pattern, step by step.
fn matches(pattern: &str, path: &str) -> bool {
    let (pattern, path): (Vec<&str>, Vec<&str>) = (pattern.split('/').collect(), path.split('/').collect());
    pattern.len() == path.len() && pattern.iter().zip(&path).all(|(p, name)| step(p, name))
}

fn step(pattern: &str, name: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == name,
        Some((head, tail)) => name
            .strip_prefix(head)
            .is_some_and(|rest| (0..=rest.len()).any(|skip| rest.is_char_boundary(skip) && step(tail, &rest[skip..]))),
    }
}

/// A path with its `.` and `..` steps gone.
fn normal(path: &str) -> String {
    let mut steps: Vec<&str> = vec![];
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => drop(steps.pop()),
            other => steps.push(other),
        }
    }
    steps.join("/")
}

fn folder_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(folder, _)| folder)
}

/// The file a line includes, and whether it is quoted, so looked for beside it first: `#include "path"` or `#include <path>`.
fn included(line: &str) -> Option<(&str, bool)> {
    let rest = line.trim_start().strip_prefix('#')?.trim_start().strip_prefix("include")?.trim_start();
    if let Some(quoted) = rest.strip_prefix('"') {
        return quoted.split_once('"').map(|(file, _)| (file, true));
    }
    rest.strip_prefix('<')?.split_once('>').map(|(file, _)| (file, false))
}

/// What the assembler is told to read: `.include "name"`, in assembly or in the strings of C.
fn assembled(text: &str) -> Vec<&str> {
    let mut names = vec![];
    for (at, _) in text.match_indices(".include") {
        let rest = text[at + ".include".len()..].trim_start().trim_start_matches('\\');
        if let Some(name) = rest.strip_prefix('"').and_then(|rest| rest.split(['"', '\\']).next()) {
            names.push(name);
        }
    }
    names
}

fn is_source(file: &str) -> bool {
    std::path::Path::new(file).extension().and_then(std::ffi::OsStr::to_str).is_some_and(|extension| ["c", "h", "inc"].contains(&extension))
}

/// What a package's files say, read as text: what it compiles, its slots, each with its type, the addresses read and the args given, and
/// its options. What it is set to is its asset's, as written: a view shows that.
pub fn reflection(call: &mut Call) -> Result<Table> {
    let call: &Call = call;
    let path = call.host.path().to_string();
    let packages = packages(call)?;
    let name = name_at(&packages, &path)?;
    let own = &packages[&name];
    let units: Vec<String> = if own.compiles() { selected(&own.fields, &path, &own.files)?.into_iter().map(|(unit, _)| unit).collect() } else { vec![] };
    let mut walk = Walk {
        call,
        packages: &packages,
        seen: BTreeSet::new(),
        label: String::new(),
        order: 0,
        slots: Slots::default(),
        options: Options::new(&target_of(call).unwrap_or_default()),
        declaring: Declaring::of(&name, &units),
    };
    // As a compile reads them, then whatever only dependents read.
    for file in units.iter().chain(own.files.iter().filter(|file| is_source(file))) {
        walk.label.clear();
        walk.file(file)?;
    }
    let Walk { slots, options, .. } = walk;
    let within = format!("{path}/");
    let fields = &own_fields(call);
    let sources = if own.compiles() { selected(fields, &path, &own.files)? } else { vec![] };
    let sources = sources.iter().map(|(file, _)| Value::String(file.strip_prefix(&within).unwrap_or(file).to_string())).collect();
    let mut table = Table::new();
    table.insert("sources".into(), Value::Array(sources));
    table.insert("slots".into(), Value::Array(slots.described(&packages)));
    table.insert("options".into(), Value::Array(options.described()));
    Ok(table)
}

/// A package's files read as a compile reads them, each once: its units in order, following what they include of it, so that what they
/// declare comes in the order a reader of them meets it. A label holds to the end of its file, what a file includes starting under it.
struct Walk<'a> {
    call: &'a Call<'a>,
    packages: &'a HashMap<String, Package>,
    seen: BTreeSet<String>,
    label: String,
    order: i64,
    slots: Slots,
    options: Options,
    declaring: Declaring,
}

impl Walk<'_> {
    fn file(&mut self, file: &str) -> Result<()> {
        if !self.seen.insert(file.to_string()) {
            return Ok(());
        }
        let bytes = self.call.read_entry(file)?;
        let text = String::from_utf8_lossy(&bytes);
        let entered = self.label.clone();
        let mut includes = text.lines().enumerate().filter_map(|(at, line)| Some((at + 1, included(line)?))).peekable();
        for found in scan::uses(&text) {
            while let Some((line, (name, quoted))) = includes.next_if(|(line, _)| *line < found.line) {
                self.include(file, line, name, quoted)?;
            }
            self.declaring.check(&found, file)?;
            self.order += 1;
            self.options.read(&found, &format!("{file}:{}", found.line), &text);
            if let ("PE2_EDITOR_LABEL", [label]) = (found.name, found.args.as_slice()) {
                self.label = label_in(label).unwrap_or_default();
            }
            self.slots.read(&found, &self.label, self.order);
            self.options.place(&found, &self.label, self.order);
        }
        for (line, (name, quoted)) in includes {
            self.include(file, line, name, quoted)?;
        }
        self.label = entered;
        Ok(())
    }

    /// A file included, as the build finds it (`resolved`).
    fn include(&mut self, from: &str, line: usize, written: &str, quoted: bool) -> Result<()> {
        let owner = owner_of(self.packages, from).ok_or_else(|| anyhow!("{from} is no package's"))?;
        match resolved(self.packages, &owner, from, written, quoted, &format!("{from}:{line}"))? {
            Some((entry, _)) => self.file(&entry),
            None => Ok(()),
        }
    }
}

/// The text of a label: a string literal.
fn label_in(argument: &str) -> Option<String> {
    let inner = argument.strip_prefix('"')?.strip_suffix('"')?;
    Some(inner.replace("\\\"", "\"").replace("\\\\", "\\"))
}

/// What a source says of its slots.
#[derive(Default)]
struct Slots {
    /// Each by its name: its type, whether public, the addresses read and the args given.
    held: BTreeMap<String, Slot>,
}

#[derive(Default)]
struct Slot {
    ty: Option<String>,
    /// What it is beside its type, after it: `self`, filled where its package's declaration does not by the package built;
    /// `hidden`, kept out of the editor.
    attributes: BTreeSet<String>,
    label: String,
    /// Where it was first declared, among all a package's slots and options: a later declaration of it moves nothing.
    order: i64,
    reads: BTreeSet<Vec<String>>,
    args: BTreeMap<(Vec<String>, String), Vec<String>>,
}

impl Slots {
    fn read(&mut self, found: &scan::Use, label: &str, order: i64) {
        let Some(name) = found.args.first().filter(|name| !name.is_empty()) else { return };
        let rest: Vec<String> = found.args[1..].iter().map(ToString::to_string).collect();
        match found.name {
            "PE2_BIND" => {
                let slot = self.held.entry((*name).to_string()).or_default();
                if slot.ty.is_none() {
                    (slot.ty, slot.label, slot.order) = (rest.first().cloned(), label.to_string(), order);
                }
                slot.attributes.extend(rest.iter().skip(1).filter_map(|word| word.strip_prefix('_')).map(str::to_string));
            }
            // What `pe2.h` expands it to, as a source reads before a first pass has: a file, and the package built.
            "PE2_BIND_SELF" => {
                let file = self.held.entry((*name).to_string()).or_default();
                if file.ty.is_none() {
                    (file.ty, file.label, file.order) = (Some("file".to_string()), label.to_string(), order);
                }
                let own = self.held.entry(SELF.to_string()).or_default();
                own.ty.get_or_insert_with(|| "package".to_string());
                own.attributes.extend(["self".to_string(), HIDDEN.to_string()]);
            }
            "PE2_VALUE" => {
                self.held.entry((*name).to_string()).or_default().reads.insert(rest);
            }
            "PE2_BIND_ARG" => {
                if let Some(at) = rest.iter().position(|word| word.starts_with('_')) {
                    let (path, given) = rest.split_at(at);
                    let values = given[1..].to_vec();
                    self.held.entry((*name).to_string()).or_default().args.insert((path.to_vec(), given[0][1..].to_string()), values);
                }
            }
            _ => {}
        }
    }

    fn described(self, packages: &HashMap<String, Package>) -> Vec<Value> {
        let list = self.held.into_iter().map(|(name, slot)| {
            let mut out = Table::new();
            out.insert("label".into(), slot.label.clone().into());
            out.insert("order".into(), slot.order.into());
            out.insert("type".into(), slot.ty.clone().unwrap_or_default().into());
            out.insert(HIDDEN.into(), slot.attributes.contains(HIDDEN).into());
            let reads = slot.reads.iter().map(|read| Value::String(read.join("/")));
            out.insert("reads".into(), Value::Array(reads.collect()));
            let args = slot.args.iter().map(|((path, arg), values)| {
                let mut given = Table::new();
                given.insert("path".into(), path.join("/").into());
                given.insert("arg".into(), arg.clone().into());
                given.insert("values".into(), Value::Array(values.iter().map(|value| Value::String(value.clone())).collect()));
                Value::Table(given)
            });
            out.insert("args".into(), Value::Array(args.collect()));
            if let Some(package) = packages.get(&name).filter(|_| slot.ty.as_deref() == Some("package")) {
                out.insert("package".into(), package.path.clone().into());
            }
            out.insert("name".into(), name.into());
            Value::Table(out)
        });
        list.collect()
    }
}

/// An option a source defines.
struct Option_ {
    ty: String,
    /// An enum's members; for the other types, the default written after the type, if any.
    members: Vec<String>,
    at: String,
    label: String,
    order: i64,
}

/// What sources say of options and the build config.
pub struct Options {
    types: Types,
    defined: BTreeMap<String, Option_>,
    /// Each read, with where.
    read: Vec<(String, Option<String>, String)>,
    /// Each `PE2_OPTION_AT`, its arguments with where.
    at_read: Vec<(Vec<String>, String)>,
    /// Each `PE2_OPTION_COUNT`, the option it reads with where.
    count_read: Vec<(String, String)>,
    config: Vec<(String, Option<String>, String)>,
    wrong: Vec<String>,
}

const CONFIG_KEYS: [&str; 3] = ["name", "release", "target"];
/// What an option with no value is defined by, followed by its name.
const UNSET: &str = "pe2_unset_";

impl Options {
    fn new(target: &str) -> Options {
        Options { types: Types::new(target), defined: BTreeMap::new(), read: vec![], at_read: vec![], count_read: vec![], config: vec![], wrong: vec![] }
    }

    fn read(&mut self, found: &scan::Use, at: &str, text: &str) {
        let args = &found.args;
        match (found.name, args.as_slice()) {
            ("PE2_TYPE", [name]) => self.types.register(name, text, found.offset, at),
            ("PE2_OPTION_AT", [_, _, ..]) => self.at_read.push((args.iter().map(ToString::to_string).collect(), at.to_string())),
            ("PE2_OPTION_COUNT", [name]) => self.count_read.push(((*name).to_string(), at.to_string())),
            ("PE2_EDITOR_LABEL", [label]) if label_in(label).is_none() => self.wrong.push(format!("{at}: a label is a string literal: {label}")),
            ("PE2_OPTION_DEF", [name, ty, members @ ..]) => {
                let members: Vec<String> = members.iter().map(ToString::to_string).collect();
                let def = Option_ { ty: (*ty).to_string(), members, at: at.to_string(), label: String::new(), order: 0 };
                if *ty != "enum" && def.members.len() > 1 {
                    self.wrong.push(format!("{at}: {name}: an option has one default"));
                }
                // A package has one of each, wherever it is read.
                if let Some(known) = self.defined.get(*name) {
                    self.wrong.push(format!("{at}: {name} is defined already, at {}", known.at));
                    return;
                }
                self.defined.insert((*name).to_string(), def);
            }
            ("PE2_OPTION", [name]) => self.read.push(((*name).to_string(), None, at.to_string())),
            ("PE2_OPTION_IS", [name, member]) => self.read.push(((*name).to_string(), Some((*member).to_string()), at.to_string())),
            ("PE2_CONFIG", [key]) => self.config.push(((*key).to_string(), None, at.to_string())),
            ("PE2_CONFIG_IS", [key, value]) => self.config.push(((*key).to_string(), Some((*value).to_string()), at.to_string())),
            _ => {}
        }
    }

    /// Why what was read cannot be: the first thing wrong.
    fn check(&self, fields: &Table) -> Result<()> {
        if let Some(wrong) = self.wrong.first() {
            bail!("{wrong}");
        }
        self.types.check()?;
        for (name, def) in &self.defined {
            let shape = shape_of(def);
            if !BUILT_IN.contains(&def.ty.as_str()) && !shape.as_ref().is_some_and(|shape| self.types.knows(shape)) {
                bail!(
                    "{}: {name}: there is no option type {}; an option is int, bool, string, enum, an integer primitive or a registered type, or a fixed array of either",
                    def.at,
                    def.ty
                );
            }
        }
        for (name, at) in &self.count_read {
            let shape = self.defined.get(name).and_then(shape_of);
            if !shape.is_some_and(|shape| shape.dynamic || shape.count.is_some()) {
                bail!("{at}: PE2_OPTION_COUNT reads an array option, and {name} is none");
            }
        }
        for (args, at) in &self.at_read {
            let name = &args[0];
            let Some(def) = self.defined.get(name).filter(|def| shape_of(def).is_some()) else {
                bail!("{at}: PE2_OPTION_AT reads an option of a registered type or an array, and {name} is none");
            };
            if let Some(value) = shaped_value(name, def, fields, &self.types)? {
                let path = &args[1..];
                if !value.leaves().iter().any(|(leaf, _)| leaf.as_slice() == path) {
                    bail!("{at}: {name} has no integer at {}", path.join(", "));
                }
            }
        }
        for (name, member, at) in &self.read {
            let Some(def) = self.defined.get(name) else { bail!("{at}: {name} has no PE2_OPTION_DEF") };
            if member.is_some() && def.ty != "enum" {
                bail!("{at}: {name} is {}, and only an enum has members", def.ty);
            }
            if let Some(member) = member.as_ref().filter(|member| !def.members.contains(member)) {
                bail!("{at}: {name} has no member {member}; it has {}", def.members.join(", "));
            }
        }
        if let Some((key, _, at)) = self.config.iter().find(|(key, ..)| !CONFIG_KEYS.contains(&key.as_str())) {
            bail!("{at}: a config has no {key}; it has {}", CONFIG_KEYS.join(", "));
        }
        // One not set is no problem until something evaluates it; one set wrongly is one already.
        for (name, def) in &self.defined {
            value_of(name, def, fields, &self.types)?;
        }
        Ok(())
    }

    /// The options this package has no value for.
    fn unset(&self, fields: &Table) -> Vec<String> {
        let unset = self.defined.iter().filter(|(name, def)| matches!(value_of(name, def, fields, &self.types), Ok(None)));
        unset.map(|(name, _)| name.clone()).collect()
    }

    /// Where the editor shows an option just defined.
    fn place(&mut self, found: &scan::Use, label: &str, order: i64) {
        let name = found.args.first().filter(|_| found.name == "PE2_OPTION_DEF");
        if let Some(def) = name.and_then(|name| self.defined.get_mut(*name)).filter(|def| def.order == 0) {
            (def.label, def.order) = (label.to_string(), order);
        }
    }

    /// What may be a mistake and may be another target's code: a value no config of the project has.
    fn notes(&self, call: &Call) -> Vec<String> {
        let known = call.config().map_or_else(Vec::new, |config| config.known);
        let tested = self.config.iter().filter_map(|(key, value, at)| Some((key, value.as_ref()?, at)));
        let unknown = tested.filter(|(key, value, _)| !known.contains(&format!("{key}={value}")));
        unknown.map(|(key, value, at)| format!("{at}: no config of the project has {key} {value}")).collect()
    }

    /// What `pe2.h` includes in both passes.
    fn header(&self, call: &Call) -> Result<String> {
        let fields = &own_fields(call);
        self.check(fields)?;
        let mut header = String::new();
        let mut unwritten = String::new();
        for (name, def) in &self.defined {
            let value = value_of(name, def, fields, &self.types)?;
            // With no value it divides by zero wherever `#if` evaluates it, and only there, as a skipped branch is not; and in code it is
            // an error wherever it is expanded. Nothing has to find out which are read: what is not expanded says nothing.
            let unset = format!("(0/{UNSET}{name})");
            if value.is_none() {
                unwritten.push_str(&failing(&format!("{UNSET}{name}"), &format!("{name} has no value on this package")));
            }
            let _ = writeln!(header, "#define pe2_option_{name} {}", value.as_deref().unwrap_or(&unset));
            if let Some(tree) = shaped_value(name, def, fields, &self.types)? {
                if let Tree::List(items) = &tree {
                    let _ = writeln!(header, "#define pe2_option_{name}_count {}", items.len());
                }
                header.push_str(&tree.leaf_macros(name));
            }
            if def.ty == "enum" {
                for (at, member) in def.members.iter().enumerate() {
                    let is = value.as_ref().map_or_else(|| unset.clone(), |value| i32::from(*value == at.to_string()).to_string());
                    let _ = writeln!(header, "#define pe2_option_{name}_is_{member} {is}");
                }
            }
        }
        let config = call.config();
        let values = [
            ("name", config.as_ref().map(|config| config.name.clone())),
            ("release", config.as_ref().and_then(|config| config.release.clone())),
            ("target", config.as_ref().and_then(|config| config.target.clone())),
        ];
        for (key, value) in values {
            if let Some(value) = value {
                let _ = writeln!(header, "#define pe2_config_{key} \"{value}\"\n#define pe2_config_{key}_is_{value} 1");
            }
        }
        // Past the first pass: there, an undefined name in `#if` is what divides by zero.
        let _ = write!(header, "#ifndef PE2_FIRST_PASS\n{unwritten}#endif\n");
        Ok(header)
    }

    fn described(self) -> Vec<Value> {
        let read: BTreeSet<&String> = self.read.iter().map(|(name, ..)| name).collect();
        let list = self.defined.iter().map(|(name, def)| {
            let mut out = Table::new();
            out.insert("label".into(), def.label.clone().into());
            out.insert("order".into(), def.order.into());
            out.insert("read".into(), read.contains(name).into());
            out.insert("type".into(), def.ty.clone().into());
            let members = if def.ty == "enum" { def.members.clone() } else { Vec::new() };
            out.insert("members".into(), Value::Array(members.into_iter().map(Value::String).collect()));
            if let Some(shape) = shape_of(def).filter(|shape| self.types.knows(shape)) {
                out.insert("shape".into(), self.types.describe(&shape));
                if let Ok(Some(default)) = shaped_value(name, def, &Table::new(), &self.types) {
                    out.insert("default".into(), default.value());
                }
            } else if let Ok(Some(default)) = default_of(name, def, &self.types) {
                out.insert("default".into(), Value::String(default));
            }
            out.insert("name".into(), name.clone().into());
            Value::Table(out)
        });
        list.collect()
    }
}

/// The option types that are no primitive and no registered type.
const BUILT_IN: [&str; 4] = ["int", "bool", "string", "enum"];

/// An option's shape when it is of a primitive or a registered type, or a fixed array of one.
fn shape_of(def: &Option_) -> Option<Shape> {
    if BUILT_IN.contains(&def.ty.as_str()) { None } else { crate::types::spelled(&def.ty) }
}

/// A typed option's value: its default merged with what its package sets; none where it has no default and is not set, or is no typed
/// option.
fn shaped_value(name: &str, def: &Option_, fields: &Table, types: &Types) -> Result<Option<Tree>> {
    let Some(shape) = shape_of(def).filter(|shape| types.knows(shape)) else { return Ok(None) };
    let default = def.members.first().map(|written| types.parsed(&shape, written, &format!("{}: {name}", def.at))).transpose()?;
    let Some(given) = fields.get("option").and_then(|options| options.get(name)) else { return Ok(default) };
    types.merged(&shape, default.as_ref(), given, &format!("{}: {name}, as this package sets it", def.at), "").map(Some)
}

/// An option's value for this package, as C reads it: what its package sets, else the default its source gives; none where it has
/// neither.
fn value_of(name: &str, def: &Option_, fields: &Table, types: &Types) -> Result<Option<String>> {
    if shape_of(def).is_some() {
        return Ok(shaped_value(name, def, fields, types)?.map(|tree| tree.initializer()));
    }
    let at = &def.at;
    let Some(value) = fields.get("option").and_then(|options| options.get(name)) else { return default_of(name, def, types) };
    let wrong = |what: &str| anyhow!("{at}: {name} is {what}, and this package gives it {value}");
    let read = match def.ty.as_str() {
        "int" => value.as_integer().map(|number| number.to_string()).ok_or_else(|| wrong("an int")),
        "bool" => value.as_bool().map(|flag| i32::from(flag).to_string()).ok_or_else(|| wrong("a bool")),
        "string" => value.as_str().map(|text| format!("{text:?}")).ok_or_else(|| wrong("a string")),
        _ => {
            let at = value.as_str().and_then(|member| def.members.iter().position(|known| known == member));
            at.map(|at| at.to_string()).ok_or_else(|| wrong("one of its members"))
        }
    };
    read.map(Some)
}

/// The default a source gives an option after its type, as C reads it: an integer (`900`, `0x38`, `-1`), `true` or `false`, or a
/// string literal.
fn default_of(name: &str, def: &Option_, types: &Types) -> Result<Option<String>> {
    if shape_of(def).is_some() {
        return Ok(shaped_value(name, def, &Table::new(), types)?.map(|tree| tree.initializer()));
    }
    let (Some(written), false) = (def.members.first(), def.ty == "enum") else { return Ok(None) };
    let wrong = || anyhow!("{}: {name} is {}, and its default is {written}", def.at, def.ty);
    let read = match def.ty.as_str() {
        "int" => integer(written).map(|number| number.to_string()).ok_or_else(wrong),
        "bool" => match written.as_str() {
            "true" | "1" => Ok("1".to_string()),
            "false" | "0" => Ok("0".to_string()),
            _ => Err(wrong()),
        },
        _ => (written.len() >= 2 && written.starts_with('"') && written.ends_with('"')).then(|| written.clone()).ok_or_else(wrong),
    };
    read.map(Some)
}

/// An integer as C writes one, in decimal or hexadecimal, signed.
fn integer(text: &str) -> Option<i64> {
    let (negative, digits) = text.strip_prefix('-').map_or((false, text), |rest| (true, rest));
    let magnitude = match digits.strip_prefix("0x").or_else(|| digits.strip_prefix("0X")) {
        Some(hex) => i64::from_str_radix(hex, 16).ok()?,
        None => digits.parse().ok()?,
    };
    Some(if negative { -magnitude } else { magnitude })
}

/// A use of a value, read from what a first pass left.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Read {
    binding: String,
    path: Vec<String>,
    derived: Option<String>,
}

impl Read {
    fn of(args: &[&str]) -> Option<Read> {
        let (binding, rest) = args.split_first()?;
        let mut path: Vec<String> = rest.iter().map(ToString::to_string).collect();
        let derived = path.last().filter(|word| word.starts_with('_')).map(|word| word[1..].to_string());
        if derived.is_some() {
            path.pop();
        }
        Some(Read { binding: (*binding).to_string(), path, derived })
    }

    /// The macro it reads, as `PE2_VALUE` pastes it.
    fn name(&self) -> String {
        let mut name = format!("pe2_value_{}", self.binding);
        for word in &self.path {
            name.push('_');
            name.push_str(word);
        }
        if let Some(derived) = &self.derived {
            let _ = write!(name, "__{derived}");
        }
        name
    }
}

/// A macro firing an error where it is used: stopping at the preprocessor where that can, and on what cannot, leaving what the compiler
/// stops on.
fn failing(name: &str, why: &str) -> String {
    let why = why.replace('\\', "\\\\").replace('"', "\\\"");
    format!("#define {name} _Pragma(\"GCC error \\\"{why}\\\"\") pe2_error_{name}\n")
}

/// A macro of any length: long ones a chain of macros, cut between tokens.
fn defined(name: &str, tokens: &str) -> String {
    if tokens.len() <= LINE {
        return format!("#define {name} {tokens}\n");
    }
    let (mut out, mut pieces, mut rest) = (String::new(), vec![], tokens);
    while !rest.is_empty() {
        let cut = if rest.len() <= LINE { rest.len() } else { rest[..LINE].rfind(' ').map_or(LINE, |at| at + 1) };
        let piece = format!("{name}_part{}", pieces.len());
        let _ = writeln!(out, "#define {piece} {}", &rest[..cut]);
        pieces.push(piece);
        rest = &rest[cut..];
    }
    let _ = writeln!(out, "#define {name} {}", pieces.join(" "));
    out
}

/// What a first pass left, with each binding's macros written for the compile: `pe2_values.h`.
pub fn resolve(call: &mut Call) -> Result<Typed> {
    let tree = call.take::<SourceTree>(0)?;
    let call: &Call = call;
    let (first, mut kept): (Vec<File>, Vec<File>) = tree.files.into_iter().partition(|file| file.name.starts_with(FIRST));
    let mut slots = Slots::default();
    let (mut reads, mut queries) = (BTreeSet::new(), BTreeSet::new());
    for file in &first {
        let bytes = call.read(file)?;
        let text = String::from_utf8_lossy(&bytes);
        for found in scan::uses(&text) {
            slots.read(&found, "", 0);
            match found.name {
                "PE2_VALUE" => reads.extend(Read::of(&found.args)),
                "PE2_QUERY" => queries.extend(Query::of(&found.args)),
                _ => {}
            }
        }
    }
    let mut notes = tree.notes;
    let (header, failed) = values(call, &slots, &reads, &queries);
    notes.extend(failed);
    kept.push(File::new(VALUES, header.into_bytes()));
    Ok(Typed { notes, settings: call.given(), ..Typed::new(SourceTree::NAME, kept) })
}

/// What a binding holds: the reference filling it.
fn values(call: &Call, slots: &Slots, reads: &BTreeSet<Read>, queries: &BTreeSet<Query>) -> (String, Vec<String>) {
    let fields = &own_fields(call);
    let mut header = String::new();
    let mut failed = Vec::new();
    for (name, slot) in &slots.held {
        let ty = slot.ty.as_deref().unwrap_or_default();
        let wrong = if ty == "package" { None } else { slot_problem(call, fields, slots, name, ty) };
        failed.extend(wrong.clone());
        header.push_str(&wrong.map_or_else(|| format!("#define pe2_bind_{name}\n"), |why| failing(&format!("pe2_bind_{name}"), &why)));
    }
    let mut written: BTreeMap<String, &Read> = BTreeMap::new();
    for read in reads {
        let name = read.name();
        if let Some(other) = written.insert(name.clone(), read) {
            header.push_str(&failing(&name, &format!("{other:?} and {read:?} are one name")));
            continue;
        }
        header.push_str(&match tokens(call, fields, slots, read) {
            Ok(tokens) => defined(&name, &tokens),
            Err(why) => {
                let why = said(&read.binding, &format!("{why:#}"));
                let written = failing(&name, &why);
                failed.push(why);
                written
            }
        });
    }
    for query in queries {
        header.push_str(&match answer(call, fields, slots, query) {
            Ok(answer) => defined(&query.name(), answer),
            Err(why) => {
                let why = said(&query.binding, &format!("{why:#}"));
                let written = failing(&query.name(), &why);
                failed.push(why);
                written
            }
        });
    }
    (header, failed)
}

/// A question the build answers of what a slot holds: `PE2_QUERY(file, contains, self)`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Query {
    binding: String,
    key: String,
    args: Vec<String>,
}

/// The keys a query asks by.
const QUERIES: [&str; 1] = ["contains"];

impl Query {
    fn of(args: &[&str]) -> Option<Query> {
        let [binding, key, rest @ ..] = args else { return None };
        Some(Query { binding: (*binding).to_string(), key: (*key).to_string(), args: rest.iter().map(ToString::to_string).collect() })
    }

    /// The macro it reads, as `PE2_QUERY` pastes it.
    fn name(&self) -> String {
        let mut name = format!("pe2_query_{}_{}", self.binding, self.key);
        for arg in &self.args {
            name.push('_');
            name.push_str(arg);
        }
        name
    }
}

/// What the build says to a query, 1 or 0: `contains`, whether what a slot holds names, anywhere in it, what another holds.
fn answer(call: &Call, fields: &Table, slots: &Slots, query: &Query) -> Result<&'static str> {
    if !QUERIES.contains(&query.key.as_str()) {
        bail!("there is no query {}; there is {}", query.key, QUERIES.join(", "));
    }
    let [other] = query.args.as_slice() else { bail!("{} takes a slot of the package", query.key) };
    if !slots.held.contains_key(other) {
        bail!("{} takes a slot of the package, and {other} is none", query.key);
    }
    let filled = |name: &str| bound(call, fields, slots, name).ok_or_else(|| anyhow!("nothing fills {name} on this package"));
    let wanted = holder(call, &filled(other)?)?;
    let held = call.table_of(&holder(call, &filled(&query.binding)?)?, "settings")?;
    Ok(if names(call, &Value::Table(held), &wanted) { "1" } else { "0" })
}

/// What a slot comes to as a holder of others, by its path: a file of a group is the file's asset.
fn holder(call: &Call, bound: &Bound) -> Result<String> {
    let Reached::Node { node, reads_as, args } = walked(call, bound, &[])? else { bail!("it holds a value") };
    let path = |reference: &str| call.node_named(reference).map(|named| named.path).ok_or_else(|| anyhow!("{reference} names nothing"));
    match args.get("file").and_then(|file| file.first()).filter(|_| reads_as.as_deref() == Some("file")) {
        Some(file) => {
            let group = call.table_of(&node, "settings")?;
            let files = group.get("files").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
            let entry = files.iter().find(|entry| entry.get("name").and_then(Value::as_str) == Some(file.as_str()));
            path(entry.and_then(|entry| entry.get("asset")).and_then(pe2_ext::target_of).ok_or_else(|| anyhow!("{node} has no file {file}"))?)
        }
        None => path(&node),
    }
}

/// Whether a value names, anywhere in it, the node at a path.
fn names(call: &Call, value: &Value, wanted: &str) -> bool {
    if pe2_ext::target_of(value).and_then(|target| call.node_named(target)).is_some_and(|named| named.path == wanted) {
        return true;
    }
    match value {
        Value::Table(table) => table.values().any(|value| names(call, value, wanted)),
        Value::Array(values) => values.iter().any(|value| names(call, value, wanted)),
        _ => false,
    }
}

/// What is wrong with a read, as the source's author is told it: what a tokenizer said, without where in the graph it was said.
fn said(binding: &str, why: &str) -> String {
    let told = why.rsplit("pe2.packages: ").next().unwrap_or(why);
    match told.strip_prefix("it needs ") {
        Some(arg) => format!("{binding} needs PE2_BIND_ARG({binding}, _{arg}, <value>)"),
        None => format!("{binding}: {told}"),
    }
}

/// Why a slot cannot be filled as it declares.
fn slot_problem(call: &Call, fields: &Table, slots: &Slots, name: &str, ty: &str) -> Option<String> {
    let Some(bound) = bound(call, fields, slots, name) else { return Some(format!("{name}: nothing fills it on this package")) };
    let own = match walked(call, &bound, &[]) {
        Ok(Reached::Node { reads_as: Some(reads_as), .. }) => reads_as,
        Ok(Reached::Node { node, .. }) => {
            let types = call.node_named(&node).map_or_else(Vec::new, |named| named.types);
            if types.iter().any(|known| known.rsplit('@').next() == Some(ty)) {
                return None;
            }
            types.first().and_then(|known| known.rsplit('@').next()).unwrap_or("nothing").to_string()
        }
        Ok(Reached::Value(_)) => "a value".to_string(),
        Err(why) => return Some(format!("{name}: {why:#}")),
    };
    (own != ty).then(|| format!("{name} is declared {ty}, and what fills it is {own}"))
}

/// What a read comes to.
fn tokens(call: &Call, fields: &Table, slots: &Slots, read: &Read) -> Result<String> {
    let slot = slots.held.get(&read.binding).filter(|slot| slot.ty.is_some()).ok_or_else(|| anyhow!("it has no PE2_BIND"))?;
    let bound = bound(call, fields, slots, &read.binding).ok_or_else(|| anyhow!("nothing fills it on this package"))?;
    let (target, led) = match walked(call, &bound, &read.path)? {
        Reached::Node { node, args, .. } => (node, args),
        Reached::Value(_) if read.derived.is_some() => bail!("{} is a value, and nothing is derived from it", read.path.join("/")),
        Reached::Value(value) => return Ok(tokenizers::printed(&value)),
    };
    let mut args = Args::default();
    for ((path, arg), values) in &slot.args {
        if *path == read.path {
            args.0.insert(arg.clone(), values.clone());
        }
    }
    args.0.extend(led);
    let output = tokenizer_of(call, &target)?;
    let address = pe2_ext::tokens::address(read.derived.as_deref(), &args).replacen(tokens::OUTPUT, &output, 1);
    pe2_ext::tokens::text_of(call, &call.output_of(&target, &address)?)
}

/// The slot `PE2_BIND_SELF` fills with the package built.
const SELF: &str = "self";
/// What a slot kept out of the editor says of itself.
const HIDDEN: &str = "hidden";

/// What fills a slot: a node, and the output of it a reference names.
struct Bound {
    node: String,
    output: Option<String>,
}

/// What its package's declaration fills a slot with; else, for a slot filled by default with the package built, the package.
fn bound(call: &Call, fields: &Table, slots: &Slots, name: &str) -> Option<Bound> {
    let Some(reference) = fields.get("bind").and_then(|bind| bind.get(name)) else {
        let own = slots.held.get(name).is_some_and(|slot| slot.attributes.contains("self"));
        return own.then(|| Bound { node: call.host.path().to_string(), output: None });
    };
    Some(Bound { node: pe2_ext::target_of(reference)?.to_string(), output: pe2_ext::output_of(reference).map(str::to_string) })
}

/// Where a sub-path ends: a node, as what it is read here where that is not what it is, with the args it is then read by; or a
/// plain value.
enum Reached {
    Node { node: String, reads_as: Option<String>, args: BTreeMap<String, Vec<String>> },
    Value(Value),
}

impl Reached {
    fn leading_on(table: &Table, node: String) -> Reached {
        let held = table.get(tokens::ARGS).and_then(Value::as_table).into_iter().flatten();
        let args = held.filter_map(|(name, value)| Some((name.clone(), vec![value.as_str()?.to_string()])));
        Reached::Node { node, reads_as: table.get(tokens::AS).and_then(Value::as_str).map(str::to_string), args: args.collect() }
    }
}

/// A sub-path followed from what a reference names: each step an output, a family's member first, leading to another node or
/// ending at a plain value.
fn walked(call: &Call, from: &Bound, path: &[String]) -> Result<Reached> {
    let (mut reached, mut at) = (Reached::Node { node: from.node.clone(), reads_as: None, args: BTreeMap::new() }, 0);
    if let Some(output) = &from.output {
        let table = call.table_of(&from.node, output).map_err(|_| anyhow!("{} has no output {output}", from.node))?;
        let next = table.get(tokens::NODE).and_then(Value::as_str).ok_or_else(|| anyhow!("nothing goes on from {output}"))?;
        reached = Reached::leading_on(&table, next.to_string());
    }
    while at < path.len() {
        let Reached::Node { node, .. } = &reached else { bail!("nothing goes on from {}", path[..at].join("/")) };
        let member = path.get(at + 1).map(|member| format!("{}/{member}", path[at]));
        let told = member.and_then(|address| Some((call.table_of(node, &address).ok()?, 2)));
        let (table, used) = match told {
            Some(told) => told,
            None => (call.table_of(node, &path[at]).map_err(|_| anyhow!("{} has no output {}", path[..at].join("/"), path[at]))?, 1),
        };
        at += used;
        if let Some(next) = table.get(tokens::NODE).and_then(Value::as_str) {
            reached = Reached::leading_on(&table, next.to_string());
        } else if let Some(value) = table.get(tokens::VALUE).filter(|_| at == path.len()) {
            return Ok(Reached::Value(value.clone()));
        } else {
            bail!("nothing goes on from {}", path[..at].join("/"));
        }
    }
    Ok(reached)
}

/// The output making a node's tokens: the target's own, where it has one.
pub fn tokenizer_of(call: &Call, target: &str) -> Result<String> {
    let active = call.config().and_then(|config| config.target);
    if let Some(active) = active {
        let own = format!("{}.{active}", tokens::OUTPUT);
        if call.table_of(target, &format!("{}.{active}", tokens::DECLARED)).is_ok() {
            return Ok(own);
        }
    }
    call.table_of(target, tokens::DECLARED).map(|_| tokens::OUTPUT.to_string()).map_err(|_| anyhow!("it has no tokenizer"))
}

impl Gathering<'_> {
    /// Take in what is pending and everything it reaches.
    fn follow(&mut self) -> Result<()> {
        while let Some(taken) = self.pending.pop() {
            if self.tree.contains_key(&taken.place) {
                continue;
            }
            let bytes = self.call.read_entry(&taken.entry)?;
            let text = String::from_utf8_lossy(&bytes).into_owned();
            self.tree.insert(taken.place.clone(), bytes);
            for found in scan::uses(&text) {
                self.declaring.check(&found, &taken.entry)?;
                self.options.read(&found, &format!("{}:{}", taken.entry, found.line), &text);
            }
            for (at, line) in text.lines().enumerate() {
                let Some((written, quoted)) = included(line) else { continue };
                let reached = resolved(&self.packages, &taken.owner, &taken.entry, written, quoted, &format!("{}:{}", taken.entry, at + 1))?;
                if let Some((entry, owner)) = reached {
                    let place = place_of(&self.packages, &self.own, &owner, &entry);
                    self.pending.push(Taken { entry, place, owner });
                }
            }
            // What the assembler reads is core's, where its public files are.
            for name in assembled(&text) {
                let entry = self.packages.get(CORE).and_then(Package::public_folder).map(|public| format!("{public}/{name}"));
                if let Some(entry) = entry.filter(|entry| self.packages[CORE].files.contains(entry)) {
                    let place = place_of(&self.packages, &self.own, CORE, &entry);
                    self.pending.push(Taken { entry, place, owner: CORE.to_string() });
                }
            }
        }
        Ok(())
    }

    /// What the packages reached name under `target.<target>`: each target takes what it knows - a file by its key, and
    /// the package's own `objects`, as the target reads them, under `objects/<visibility>/`.
    fn take_target_files(&mut self, own: &str) -> Result<()> {
        if let Some(package) = self.packages.get(own) {
            let within = format!("{}/", package.path);
            let target = target_of(self.call).unwrap_or_default();
            for visibility in VISIBILITIES {
                for entry in listed(&package.fields, "objects", visibility, &package.path, &package.files) {
                    let place = format!("target/{target}/{own}/objects/{visibility}/{}", entry.strip_prefix(&within).unwrap_or(&entry));
                    let bytes = self.call.read_entry(&entry)?;
                    self.tree.insert(place, bytes);
                }
            }
        }
        for name in self.reached.clone() {
            let Some(package) = self.packages.get(&name) else { continue };
            let Some(targets) = package.declared.get("target").and_then(Value::as_table) else { continue };
            let mut files = vec![];
            for (target, values) in targets {
                for (key, value) in values.as_table().into_iter().flatten() {
                    let entry = value.as_str().map(|file| format!("{}/{file}", package.path)).filter(|entry| package.files.contains(entry));
                    if let Some(entry) = entry {
                        files.push((format!("target/{target}/{name}/{key}/{}", name_of(&entry)), entry));
                    }
                }
            }
            for (place, entry) in files {
                let bytes = self.call.read_entry(&entry)?;
                self.tree.insert(place, bytes);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Option_, Types, value_of};

    #[test]
    fn a_string_option_is_a_string_to_the_compiler() {
        let def = Option_ { ty: "string".to_string(), members: Vec::new(), at: "here".to_string(), label: String::new(), order: 0 };
        let fields: toml::Table = "option.title = 'a \"name\"'".parse().unwrap();
        assert_eq!(value_of("title", &def, &fields, &Types::new("psx")).unwrap().as_deref(), Some("\"a \\\"name\\\"\""));
        assert_eq!(value_of("other", &def, &fields, &Types::new("psx")).unwrap(), None);
        let wrong: toml::Table = "option.title = 3".parse().unwrap();
        assert!(value_of("title", &def, &wrong, &Types::new("psx")).is_err());
    }

    #[test]
    fn a_default_is_what_a_package_does_not_set() {
        let def = |ty: &str, default: &str| Option_ { ty: ty.to_string(), members: vec![default.to_string()], at: "here".to_string(), label: String::new(), order: 0 };
        let unset = toml::Table::new();
        assert_eq!(value_of("ticks", &def("int", "900"), &unset, &Types::new("psx")).unwrap().as_deref(), Some("900"));
        assert_eq!(value_of("y", &def("int", "0x38"), &unset, &Types::new("psx")).unwrap().as_deref(), Some("56"));
        assert_eq!(value_of("n", &def("int", "-1"), &unset, &Types::new("psx")).unwrap().as_deref(), Some("-1"));
        assert_eq!(value_of("on", &def("bool", "true"), &unset, &Types::new("psx")).unwrap().as_deref(), Some("1"));
        assert_eq!(value_of("s", &def("string", "\"a\""), &unset, &Types::new("psx")).unwrap().as_deref(), Some("\"a\""));
        let set: toml::Table = "option.ticks = 600".parse().unwrap();
        assert_eq!(value_of("ticks", &def("int", "900"), &set, &Types::new("psx")).unwrap().as_deref(), Some("600"));
        assert!(value_of("ticks", &def("int", "fast"), &unset, &Types::new("psx")).is_err());
        assert!(value_of("s", &def("string", "a"), &unset, &Types::new("psx")).is_err());
        let members = Option_ { ty: "enum".to_string(), members: vec!["matte".into(), "gloss".into()], at: "here".into(), label: String::new(), order: 0 };
        assert_eq!(value_of("finish", &members, &unset, &Types::new("psx")).unwrap(), None);
    }

    use super::label_in;

    #[test]
    fn a_label_is_a_string_literal() {
        assert_eq!(label_in("\"Weapon stats, by level\"").as_deref(), Some("Weapon stats, by level"));
        assert_eq!(label_in("\"\"").as_deref(), Some(""));
        assert_eq!(label_in("\"say \\\"hi\\\"\"").as_deref(), Some("say \"hi\""));
        assert_eq!(label_in("Face"), None);
    }
}
