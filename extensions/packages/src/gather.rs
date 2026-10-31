//! What a package compiles from: its sources, and what they reach, read as text. A request has to be read before a preprocessor runs, as it
//! decides what the preprocessor is given, so this reads every `#include`, whichever branch of a conditional it is in. What is written and
//! not found is left for the compiler to say.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;

use pe2_ext::ty::{Registered, SourceTree};
use pe2_ext::{Call, File, Result, Table, Typed, Value, anyhow, bail};

/// The package holding what every unit includes first, and what the assembler reads.
const CORE: &str = "core";
/// What every unit includes first, beside `core`'s public files.
const MACROS: &str = "pe2.h";
/// What `MACROS` includes: the values of the slots of the package being built.
const VALUES: &str = "pe2_values.h";

/// A package of the build graph: the graph has one, by name.
pub struct Package {
    pub path: String,
    fields: Table,
    /// Its files, by their paths, as the stack resolves them.
    files: BTreeSet<String>,
}

impl Package {
    pub fn public_folders(&self) -> Vec<&str> {
        strings(&self.fields, "public")
    }

    /// A public file by its path within what it makes public.
    fn public(&self, inside: &str) -> Option<String> {
        let folders = strings(&self.fields, "public");
        folders.into_iter().map(|folder| format!("{}/{folder}/{inside}", self.path)).find(|file| self.files.contains(file))
    }
}

fn strings<'a>(fields: &'a Table, key: &str) -> Vec<&'a str> {
    fields.get(key).and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect()
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
    tree: BTreeMap<String, Vec<u8>>,
    reached: BTreeSet<String>,
    pending: Vec<Taken>,
    /// Names asked for that no package answers: the slots left open.
    open: BTreeSet<String>,
    /// The slots what was reached declares (`PE2_BIND`).
    bound: BTreeSet<String>,
}

pub fn gather(call: &mut Call) -> Result<Typed> {
    let call: &Call = call;
    let path = call.host.path().to_string();
    let name = name_of(&path).to_string();
    let mut gathering = Gathering { call, packages: packages(call)?, tree: BTreeMap::new(), reached: BTreeSet::new(), pending: vec![], bound: BTreeSet::new(), open: BTreeSet::new() };
    let own = gathering.packages.get(&name).ok_or_else(|| anyhow!("{path} is no package of the graph"))?;
    let (own_path, own_files) = (own.path.clone(), own.files.clone());
    let units = selected(call.host.fields(), &own_path, &own_files)?;
    let within = format!("{own_path}/");
    let mut listed = String::new();
    for unit in &units {
        let place = format!("self/{}", unit.strip_prefix(&within).unwrap_or(unit));
        listed.push_str(&place);
        listed.push('\n');
        gathering.pending.push(Taken { entry: unit.clone(), place, owner: name.clone() });
    }
    gathering.reached.insert(name);
    gathering.take_macros()?;
    gathering.follow()?;
    gathering.take_values()?;
    if !gathering.open.is_empty() {
        let open: Vec<&str> = gathering.open.iter().map(String::as_str).collect();
        bail!("{path} asks for {}, which no package answers", open.join(", "));
    }
    gathering.take_target_files()?;
    let mut files: Vec<File> = vec![File::new("units", listed.into_bytes())];
    files.extend(gathering.tree.into_iter().map(|(place, bytes)| File::new(&place, bytes)));
    Ok(Typed::new(SourceTree::NAME, files))
}

fn name_of(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The build graph's one table: every `package` asset by its name, with its files.
pub fn packages(call: &Call) -> Result<HashMap<String, Package>> {
    let mut packages = HashMap::new();
    for (path, declaration) in call.assets_of("package")? {
        let fields: Table = declaration.parse().map_err(|e| anyhow!("{path}: {e}"))?;
        let files = call.entries(&path)?.into_iter().collect();
        let name = name_of(&path).to_string();
        let package = Package { path: path.clone(), fields, files };
        if let Some(other) = packages.insert(name.clone(), package) {
            bail!("two packages are named {name}: {}, {path}", other.path);
        }
    }
    Ok(packages)
}

/// The files `sources` names, in order: a path or a glob of `*` within a step, a leading `!` leaving out what is matched.
fn selected(fields: &Table, path: &str, files: &BTreeSet<String>) -> Result<Vec<String>> {
    let patterns = strings(fields, "sources");
    let within = format!("{path}/");
    let matched = |file: &str, wanted: bool| {
        patterns.iter().filter(|p| p.starts_with('!') != wanted).any(|p| matches(p.trim_start_matches('!'), file))
    };
    let kept: Vec<String> = files
        .iter()
        .filter(|file| file.strip_prefix(&within).is_some_and(|inside| matched(inside, true) && !matched(inside, false)))
        .cloned()
        .collect();
    if kept.is_empty() {
        bail!("{path}: its sources name no file");
    }
    Ok(kept)
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

/// What a line of source asks for.
enum Asked<'a> {
    /// `#include PE2_PACKAGE(name, path)`
    Package(&'a str, &'a str),
    /// `#include "path"`
    Beside(&'a str),
}

fn asked(line: &str) -> Option<Asked<'_>> {
    let rest = line.trim_start().strip_prefix('#')?.trim_start().strip_prefix("include")?.trim_start();
    if let Some(args) = rest.strip_prefix("PE2_PACKAGE(") {
        let (name, path) = args.split_once(')')?.0.split_once(',')?;
        return Some(Asked::Package(name.trim(), path.trim()));
    }
    Some(Asked::Beside(rest.strip_prefix('"')?.split_once('"')?.0))
}

/// The slot a line declares: `PE2_BIND(name, type)`.
fn declared_slot(line: &str) -> Option<&str> {
    let (name, _) = line.trim_start().strip_prefix("PE2_BIND(")?.split_once(',')?;
    Some(name.trim())
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

/// What a package's files say, read as text: what it compiles, and what it asks for by name, each slot public where a file its dependents
/// read names it, which then reaches them too, and private otherwise. A slot is answered by the graph's package of that name.
pub fn reflection(call: &mut Call) -> Result<Table> {
    let call: &Call = call;
    let path = call.host.path().to_string();
    let name = name_of(&path).to_string();
    let packages = packages(call)?;
    let own = packages.get(&name).ok_or_else(|| anyhow!("{path} is no package of the graph"))?;
    let public: Vec<String> = strings(&own.fields, "public").iter().map(|folder| format!("{path}/{folder}/")).collect();
    let mut slots: BTreeMap<String, (bool, i64)> = BTreeMap::new();
    for file in own.files.iter().filter(|file| is_source(file)) {
        let in_public = public.iter().any(|folder| file.starts_with(folder.as_str()));
        let bytes = call.read_entry(file)?;
        for line in String::from_utf8_lossy(&bytes).lines() {
            if let Some(Asked::Package(asked, _)) = asked(line).filter(|asked| !matches!(asked, Asked::Package(name_asked, _) if *name_asked == name)) {
                let slot = slots.entry(asked.to_string()).or_insert((false, 0));
                slot.0 |= in_public;
                slot.1 += 1;
            }
        }
    }
    let within = format!("{path}/");
    let fields = call.host.fields();
    let sources = if strings(fields, "sources").is_empty() { vec![] } else { selected(fields, &path, &own.files)? };
    let sources = sources.iter().map(|file| Value::String(file.strip_prefix(&within).unwrap_or(file).to_string())).collect();
    let slots = slots.into_iter().map(|(asked, (in_public, uses))| {
        let mut slot = Table::new();
        slot.insert("name".into(), asked.clone().into());
        slot.insert("public".into(), in_public.into());
        slot.insert("uses".into(), uses.into());
        if let Some(package) = packages.get(&asked) {
            slot.insert("package".into(), package.path.clone().into());
        }
        Value::Table(slot)
    });
    let mut table = Table::new();
    table.insert("sources".into(), Value::Array(sources));
    table.insert("slots".into(), Value::Array(slots.collect()));
    Ok(table)
}

impl Gathering<'_> {
    fn take_macros(&mut self) -> Result<()> {
        let core = self.packages.get(CORE).ok_or_else(|| anyhow!("the graph has no package {CORE}"))?;
        let entry = format!("{}/{MACROS}", core.path);
        let bytes = self.call.read_entry(&entry)?;
        self.tree.insert(MACROS.to_string(), bytes);
        Ok(())
    }

    /// What the slots declared hold, each a macro of what `PE2_VALUE` reads: a table's every key, as tokens. It is a
    /// file of its own, each package having its own.
    fn take_values(&mut self) -> Result<()> {
        let bindings = self.call.host.fields().get("bind").cloned();
        let mut header = String::new();
        for name in std::mem::take(&mut self.bound) {
            let Some(held) = bindings.as_ref().and_then(|bindings| bindings.get(&name)) else {
                self.open.insert(name);
                continue;
            };
            let target = match held {
                Value::Table(table) => table.get("uid").and_then(Value::as_str),
                _ => held.as_str().map(|text| text.split_once('#').map_or(text, |(uid, _)| uid)),
            };
            let target = target.ok_or_else(|| anyhow!("{name} is bound to no reference"))?;
            let output = pe2_ext::output_of(held).ok_or_else(|| anyhow!("{name} is bound to no output of {target}"))?;
            for (key, value) in self.call.table_of(target, output)? {
                let tokens = value.as_str().map_or_else(|| value.to_string(), str::to_string);
                let _ = writeln!(header, "#define pe2_value_{name}_{key} {tokens}");
            }
        }
        self.tree.insert(VALUES.to_string(), header.into_bytes());
        Ok(())
    }

    /// Take in what is pending and everything it reaches.
    fn follow(&mut self) -> Result<()> {
        while let Some(taken) = self.pending.pop() {
            if self.tree.contains_key(&taken.place) {
                continue;
            }
            let bytes = self.call.read_entry(&taken.entry)?;
            let text = String::from_utf8_lossy(&bytes).into_owned();
            self.tree.insert(taken.place.clone(), bytes);
            for line in text.lines() {
                if let Some(name) = declared_slot(line) {
                    self.bound.insert(name.to_string());
                }
                match asked(line) {
                    Some(Asked::Package(name, inside)) => self.take_public(name, inside),
                    Some(Asked::Beside(file)) => self.take_beside(&taken, file),
                    None => {}
                }
            }
            for name in assembled(&text) {
                self.take_public(CORE, name);
            }
        }
        Ok(())
    }

    fn take_public(&mut self, name: &str, inside: &str) {
        let Some(package) = self.packages.get(name) else {
            self.open.insert(name.to_string());
            return;
        };
        // A file not there is the compiler's to say: it knows which branch of a conditional is taken.
        let Some(entry) = package.public(inside) else { return };
        self.reached.insert(name.to_string());
        self.pending.push(Taken { entry, place: format!("pkg/{name}/{inside}"), owner: name.to_string() });
    }

    /// A file named beside another: of the same package, kept beside it in the tree.
    fn take_beside(&mut self, from: &Taken, file: &str) {
        let entry = normal(&format!("{}/{file}", folder_of(&from.entry)));
        if self.packages.get(&from.owner).is_some_and(|package| package.files.contains(&entry)) {
            let place = normal(&format!("{}/{file}", folder_of(&from.place)));
            self.pending.push(Taken { entry, place, owner: from.owner.clone() });
        }
    }

    /// What the packages reached name under `target.<target>`: each target takes what it knows.
    fn take_target_files(&mut self) -> Result<()> {
        for name in self.reached.clone() {
            let Some(package) = self.packages.get(&name) else { continue };
            let Some(targets) = package.fields.get("target").and_then(Value::as_table) else { continue };
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
