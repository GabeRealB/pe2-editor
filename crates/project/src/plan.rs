//! An extraction plan, run: what fills a generated layer with the files the game's discs hold for the nodes of the
//! layer it fills.
//!
//! A plan is `<layer>.plan.jsonl` beside the generated layer's folder - one line each, JSON: first what it is and what
//! it needs (`{"plan": 1, "requires": [...], "inputs": {<release>: {"look_in": "res://roms", "discs": [...]}}}`: the
//! extensions its operations are of, and each release's discs by name, size and sha1, looked for in a place of the
//! project, an unmanaged mount), then its operations (`{"id", "op", "in", "args", "expect"}`), each
//! taking what operations before it made by their ids, then what each bundle is given (`{"emit": <node>, "files":
//! {<name>: <id>}}`). It holds where things are on the discs, how large, their sha1s and what reads them - never their
//! bytes - so it travels with a project, stating what extraction makes of the discs.
//!
//! An operation is known by its key: a hash of what it is, its version and its settings, and the keys of what it takes,
//! the discs' own sha1s at the root - all known without the discs. Running a plan makes only the bundles' files whose
//! key changed since the last run or which are gone, each operation they need once, at once where threads are free; it
//! keeps a record in the layer (`extraction.record.jsonl`: each file written, by its key and sha1), removes the files
//! no bundle is given any more, and writes no `node.toml` - the nodes are whoever wrote the plan's. With the record, a
//! layer is checked without the discs (`verify`): every file there, made by what the plan says now, unchanged.
//!
//! The operations are the host's: a disc (`disc`, found by its size and sha1 and held to its slots), a disc's slot read
//! whole (`slot`), a stage's container (`container`), a file's chunk (`chunk`), a stream stored in a stage
//! (`stored_stream`), a movie file's clips found (`movie_clips`) and one of them read (`clip`), what is compressed
//! inflated (`inflate`) and a run of it taken (`slice`) - and any operation an extension registers, given what it takes
//! by its ports' names and its `args` as its settings, its version its extension's. What an operation makes may be held
//! to a sha1 (`expect`); one whose value is that it succeeds is a check (`{"check": <id>}`), run whenever its key
//! changes: a container assembled from what the bundles were given (`assembled`, a disc's movie file joined from its
//! movies) is one, run after everything is written, its key that of what it comes `after`.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::discs::{self, Container, OpenDisc};
use crate::extract::sha1_hex;

pub const RECORD: &str = "extraction.record.jsonl";

/// A release a plan reads: where its discs are looked for (`res://`), and each disc's image, by name, size and sha1 -
/// the sha1 deciding, the name where it is looked for first.
#[derive(Clone, Debug)]
pub struct Input {
    pub look_in: String,
    pub discs: Vec<(String, u64, String)>,
}
/// The host's operations' version: another makes everything again.
const VERSION: &str = "1";

/// An operation of a plan: what it is, what it takes (by port, the ids of what made it), its settings, and the sha1 what
/// it makes is held to.
#[derive(Clone, Debug)]
struct Op {
    id: String,
    kind: String,
    /// What it takes, as chosen (`Plan::resolve`): an operation's id at each place of each port.
    takes: BTreeMap<String, Vec<String>>,
    /// What it may take at each place of each port, in preference order: the same bytes from any of the releases that
    /// have them (`{"any": [...]}`), or one operation.
    inputs: BTreeMap<String, Vec<Vec<String>>>,
    /// What it comes after, taking nothing of it: its key is theirs too.
    after: Vec<String>,
    /// Its arguments, as JSON written as `serde_json` writes them: what its key hashes, read again where they are used.
    /// A plan's arguments are most of it, and held as values they would take several times its size.
    args: Box<str>,
    sha1: Option<String>,
}

impl Op {
    /// Its arguments.
    fn args(&self) -> Value {
        serde_json::from_str(&self.args).unwrap_or(Value::Null)
    }
}

/// What a bundle is given: its files, each by the operation making it.
#[derive(Clone, Debug)]
struct Emit {
    node: String,
    files: Vec<Given>,
}

/// A file a bundle is given, by name; or a folder of them (its name ending in `/`): every file the operation made
/// whose name starts with `prefix` (`<op>#<prefix>`), named in the folder by the rest. An optional one is given where
/// the operation made it.
#[derive(Clone, Debug)]
struct Given {
    name: String,
    /// What makes it, as chosen (`Plan::resolve`), and the folder of what it made it is, where it is one.
    at: usize,
    prefix: Option<String>,
    optional: bool,
    /// What may make it, in preference order: the same bytes of any of the releases that have them.
    candidates: Vec<(usize, Option<String>)>,
    /// Whether any of them is made of the releases run.
    possible: bool,
}

impl Given {
    fn folder(&self) -> bool {
        self.name.ends_with('/')
    }
}

/// A plan, read.
pub struct Plan {
    /// What it reads, by release.
    pub inputs: BTreeMap<String, Input>,
    /// Its releases in the order it prefers them.
    pub order: Vec<String>,
    /// The extensions its operations are theirs, by id, and the versions it takes of each (`^0.1`).
    pub requires: Vec<(String, String)>,
    ops: Vec<Op>,
    emits: Vec<Emit>,
    /// What is run for its success alone, by its place.
    checks: Vec<usize>,
    by_id: HashMap<String, usize>,
    /// The plan's text's sha1: what its record was made by.
    pub sha1: String,
}

/// What a plan is and needs, its first line: read alone where the rest is not wanted (`header`).
#[derive(Clone, Debug, Default)]
pub struct Header {
    /// What it reads, by release.
    pub inputs: BTreeMap<String, Input>,
    /// Its releases in the order it prefers them (`releases`; else by name).
    pub order: Vec<String>,
    /// The extensions its operations are theirs, by id, and the versions it takes of each (`^0.1`).
    pub requires: Vec<(String, String)>,
}

impl Header {
    fn parse(line: &str) -> Result<Header> {
        let head: Value = serde_json::from_str(line).context("its first line")?;
        anyhow::ensure!(head.get("plan").and_then(Value::as_u64) == Some(1), "not a plan this reads (`plan`: 1)");
        let mut inputs = BTreeMap::new();
        for (release, input) in head.get("inputs").and_then(Value::as_object).into_iter().flatten() {
            let look_in =
                input.get("look_in").and_then(Value::as_str).with_context(|| format!("{release}: no look_in"))?;
            let discs = input.get("discs").and_then(Value::as_array).into_iter().flatten();
            let discs = discs
                .map(|d| {
                    let text = |key: &str| d.get(key).and_then(Value::as_str).map(str::to_string);
                    let disc = (|| Some((text("name")?, d.get("size").and_then(Value::as_u64)?, text("sha1")?)))();
                    disc.with_context(|| format!("{release}: a disc said in part"))
                })
                .collect::<Result<_>>()?;
            inputs.insert(release.clone(), Input { look_in: look_in.to_string(), discs });
        }
        let requires = head
            .get("requires")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|r| Some((r.get("id")?.as_str()?.to_string(), r.get("version")?.as_str()?.to_string())))
            .collect();
        let order: Vec<String> = head.get("releases").and_then(Value::as_array).map_or_else(
            || inputs.keys().cloned().collect(),
            |r| r.iter().filter_map(Value::as_str).filter(|r| inputs.contains_key(*r)).map(str::to_string).collect(),
        );
        Ok(Header { inputs, order, requires })
    }
}

/// A plan's first line alone: what it reads and needs, without its operations.
///
/// # Errors
/// The file cannot be read, or its first line is no plan's.
pub fn header(plan: &Path) -> Result<Header> {
    use std::io::BufRead;
    let file = std::fs::File::open(plan).with_context(|| plan.display().to_string())?;
    let mut line = String::new();
    std::io::BufReader::new(file).read_line(&mut line).with_context(|| plan.display().to_string())?;
    Header::parse(&line).with_context(|| plan.display().to_string())
}

impl Plan {
    /// A plan as its file holds it.
    ///
    /// # Errors
    /// A line is no JSON, or no plan's, or takes what no line before it makes.
    pub fn read(path: &Path) -> Result<Plan> {
        let text = std::fs::read_to_string(path).with_context(|| path.display().to_string())?;
        Self::parse(&text).with_context(|| path.display().to_string())
    }

    /// What an emit line gives its bundle: each file by the operations that may make it, in preference order.
    fn given(line: &Value, at: usize, by_id: &HashMap<String, usize>) -> Result<Vec<Given>> {
        let files = line.get("files").and_then(Value::as_object).with_context(|| format!("line {at}: no files"))?;
        let optional: Vec<&str> =
            line.get("optional").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect();
        let mut given = Vec::new();
        for (name, ids) in files {
            let ids: Vec<&str> = match ids {
                Value::String(id) => vec![id.as_str()],
                Value::Array(ids) => ids.iter().filter_map(Value::as_str).collect(),
                _ => bail!("line {at}: {name} is made by nothing"),
            };
            let mut candidates = Vec::new();
            for id in ids {
                let (id, prefix) = match id.split_once('#') {
                    Some((id, prefix)) => (id, Some(prefix.to_string())),
                    None => (id, None),
                };
                let made_by = *by_id
                    .get(id)
                    .with_context(|| format!("line {at}: {name} is made by {id}, which nothing before makes"))?;
                candidates.push((made_by, prefix));
            }
            let (made_by, prefix) = candidates.first().cloned().with_context(|| format!("line {at}: {name}"))?;
            let optional = optional.contains(&name.as_str());
            given.push(Given { name: name.clone(), at: made_by, prefix, optional, candidates, possible: true });
        }
        Ok(given)
    }

    fn parse(text: &str) -> Result<Plan> {
        let mut lines = text.lines().enumerate().filter(|(_, line)| !line.trim().is_empty());
        let (_, head) = lines.next().context("an empty plan")?;
        let Header { inputs, order, requires } = Header::parse(head)?;
        let (mut ops, mut emits, mut checks, mut by_id) = (Vec::new(), Vec::new(), Vec::new(), HashMap::new());
        for (at, line) in lines {
            let at = at + 1;
            let line: Value = serde_json::from_str(line).with_context(|| format!("line {at}"))?;
            if let Some(id) = line.get("check").and_then(Value::as_str) {
                let at = *by_id.get(id).with_context(|| format!("line {at}: {id} checks nothing before it"))?;
                checks.push(at);
                continue;
            }
            if let Some(node) = line.get("emit").and_then(Value::as_str) {
                emits.push(Emit { node: node.to_string(), files: Self::given(&line, at, &by_id)? });
                continue;
            }
            let id = line.get("id").and_then(Value::as_str).with_context(|| format!("line {at}: no id"))?.to_string();
            let op = line.get("op").and_then(Value::as_str).with_context(|| format!("line {at}: no op"))?.to_string();
            let (mut takes, mut inputs) = (BTreeMap::new(), BTreeMap::new());
            // One operation, or any of several in preference order (`{"any": [...]}`).
            let alternatives = |from: &Value| -> Result<Vec<String>> {
                let ids: Vec<String> = match from {
                    Value::String(id) => vec![id.clone()],
                    Value::Object(any) => any
                        .get("any")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect(),
                    _ => vec![],
                };
                anyhow::ensure!(!ids.is_empty(), "line {at}: it takes nothing at a place");
                Ok(ids)
            };
            for (port, from) in line.get("in").and_then(Value::as_object).into_iter().flatten() {
                let places: Vec<Vec<String>> = match from {
                    Value::Array(items) => items.iter().map(alternatives).collect::<Result<_>>()?,
                    one => vec![alternatives(one)?],
                };
                for taken in places.iter().flatten() {
                    anyhow::ensure!(
                        by_id.contains_key(taken),
                        "line {at}: {port} takes {taken}, which nothing before makes"
                    );
                }
                takes.insert(port.clone(), places.iter().map(|ids| ids[0].clone()).collect());
                inputs.insert(port.clone(), places);
            }
            let after: Vec<String> = line
                .get("after")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect();
            for earlier in &after {
                anyhow::ensure!(
                    by_id.contains_key(earlier),
                    "line {at}: it comes after {earlier}, which nothing before makes"
                );
            }
            let args = serde_json::to_string(line.get("args").unwrap_or(&Value::Null))?.into_boxed_str();
            let sha1 = line.pointer("/expect/sha1").and_then(Value::as_str).map(str::to_string);
            anyhow::ensure!(!by_id.contains_key(&id), "line {at}: {id} is made twice");
            by_id.insert(id.clone(), ops.len());
            ops.push(Op { id, kind: op, takes, inputs, after, args, sha1 });
        }
        // A release's own file (`<release>/<name>`) is the file `<name>` of what makes it.
        for given in emits.iter_mut().flat_map(|emit| emit.files.iter_mut()) {
            let Some((release, name)) = given.name.split_once('/') else { continue };
            if inputs.contains_key(release) && !given.folder() {
                for (_, prefix) in given.candidates.iter_mut().filter(|(_, prefix)| prefix.is_none()) {
                    *prefix = Some(name.to_string());
                }
                given.prefix = given.candidates[0].1.clone();
            }
        }
        Ok(Plan { inputs, order, requires, ops, emits, checks, by_id, sha1: sha1_hex(text.as_bytes()) })
    }

    /// What it requires that the extensions do not have: each extension missing - not there, left off or not trusted -
    /// or of a version it does not take.
    #[must_use]
    pub fn unmet(&self, extensions: &crate::ext::Extensions) -> Vec<String> {
        let mut unmet = Vec::new();
        for (id, version) in &self.requires {
            match extensions.loaded.iter().find(|l| l.id == *id) {
                None => unmet.push(format!("{id}, which is not loaded")),
                Some(loaded) if !crate::ext::satisfies(&loaded.version, version) => {
                    unmet.push(format!("{id} {version}, where it is {}", loaded.version));
                }
                Some(_) => {}
            }
        }
        unmet
    }

    /// Each operation's key, in the plan's order: what it is, its version, its settings, what it is held to, and the
    /// keys of what it takes.
    fn keys(&self, versions: &dyn Fn(&str) -> String) -> Vec<String> {
        let mut keys: Vec<String> = Vec::with_capacity(self.ops.len());
        for op in &self.ops {
            // A place taking any of several is keyed by all of them, whichever is taken: they are the same bytes.
            let key = |ids: &Vec<String>| match ids.as_slice() {
                [one] => json!(keys[self.by_id[one]]),
                many => json!(many.iter().map(|id| &keys[self.by_id[id]]).collect::<Vec<_>>()),
            };
            let taken: BTreeMap<&String, Vec<Value>> =
                op.inputs.iter().map(|(port, places)| (port, places.iter().map(key).collect())).collect();
            let version = match op.kind.split_once('@') {
                Some((extension, _)) => versions(extension),
                None => VERSION.to_string(),
            };
            let after: Vec<&String> = op.after.iter().map(|id| &keys[self.by_id[id]]).collect();
            let what = json!({"op": op.kind, "version": version, "args": op.args(), "sha1": op.sha1, "in": taken, "after": after});
            keys.push(sha1_hex(what.to_string().as_bytes()));
        }
        keys
    }

    /// A file's key: what makes it, whichever of its candidates does - they make the same bytes.
    fn file_key(keys: &[String], given: &Given) -> String {
        match given.candidates.as_slice() {
            [(at, _)] => keys[*at].clone(),
            many => sha1_hex(json!(many.iter().map(|(at, _)| &keys[*at]).collect::<Vec<_>>()).to_string().as_bytes()),
        }
    }

    /// The releases it reads, in its order.
    #[must_use]
    pub fn releases(&self) -> Vec<String> {
        self.order.clone()
    }

    /// Choose, of what may be taken, what is made of these releases: each place's first alternative so made, each
    /// file's first candidate. Whether each operation is made of them: a disc of a release among them, anything else
    /// of what it takes.
    fn resolve(&mut self, releases: &std::collections::BTreeSet<String>) -> Vec<bool> {
        let mut possible = vec![false; self.ops.len()];
        for at in 0..self.ops.len() {
            let op = &self.ops[at];
            possible[at] = if op.kind == "disc" {
                op.args().get("release").and_then(Value::as_str).is_some_and(|r| releases.contains(r))
            } else {
                let mut ok = op.after.iter().all(|id| possible[self.by_id[id]]);
                let mut takes = BTreeMap::new();
                for (port, places) in &op.inputs {
                    let chosen = places
                        .iter()
                        .map(|ids| {
                            let made = ids.iter().find(|id| possible[self.by_id[*id]]);
                            ok &= made.is_some();
                            made.unwrap_or(&ids[0]).clone()
                        })
                        .collect();
                    takes.insert(port.clone(), chosen);
                }
                self.ops[at].takes = takes;
                ok
            };
        }
        for emit in &mut self.emits {
            for given in &mut emit.files {
                let chosen = given.candidates.iter().find(|(at, _)| possible[*at]).cloned();
                given.possible = chosen.is_some();
                if let Some((at, prefix)) = chosen {
                    given.at = at;
                    given.prefix = prefix;
                }
            }
        }
        possible
    }
}

/// Which of a plan's releases a run reads: all of them - each disc there, or the run fails by name - those whose discs
/// are all found, or those named.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Releases {
    All,
    Available,
    Only(Vec<String>),
}

/// A disc a run looks for: its release, its name, size and sha1, and where it is looked for.
type DiscSought<'a> = (&'a String, &'a (String, u64, String), Option<PathBuf>);

/// The releases a run reads, and where each of their discs is, by its sha1.
fn found_releases(
    project: &crate::Project,
    plan: &Plan,
    releases: &Releases,
) -> Result<(std::collections::BTreeSet<String>, HashMap<String, PathBuf>)> {
    let asked: Vec<&String> = match releases {
        Releases::Only(names) => {
            for name in names {
                anyhow::ensure!(plan.inputs.contains_key(name), "the plan reads no release {name}");
            }
            plan.inputs.keys().filter(|r| names.contains(r)).collect()
        }
        _ => plan.inputs.keys().collect(),
    };
    let discs: Vec<DiscSought<'_>> = asked
        .iter()
        .flat_map(|&release| {
            let look_in = project.resolve(&plan.inputs[release].look_in).ok();
            plan.inputs[release].discs.iter().map(move |disc| (release, disc, look_in.clone()))
        })
        .collect();
    let found = crate::parallel(&discs, |(_, (name, size, sha1), look_in)| {
        look_in.as_ref().and_then(|dir| discs::find_disc(dir, name, *size, sha1).ok())
    });
    let (mut available, mut paths, mut missing) = (std::collections::BTreeSet::new(), HashMap::new(), Vec::new());
    for &release in &asked {
        available.insert(release.clone());
    }
    for ((release, (name, _, sha1), look_in), path) in discs.iter().zip(found) {
        if let Some(path) = path {
            paths.insert(sha1.clone(), path);
        } else {
            available.remove(*release);
            let place =
                look_in.as_ref().map_or_else(|| plan.inputs[*release].look_in.clone(), |p| p.display().to_string());
            missing.push(format!("{release}'s {name} is not in {place}"));
        }
    }
    if *releases != Releases::Available || available.is_empty() {
        anyhow::ensure!(missing.is_empty(), "{}", missing.join("; "));
    }
    anyhow::ensure!(!available.is_empty(), "the plan reads no release");
    Ok((available, paths))
}

/// A file a run wrote, as its record keeps it; an optional one not made has no sha1. A release's own file that is
/// the same as what the releases share is not kept: the shared one is it (`shared`).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Written {
    key: String,
    sha1: String,
    size: u64,
    shared: bool,
}

impl Written {
    fn present(&self) -> bool {
        !self.sha1.is_empty()
    }
}

/// What a run did, as its record keeps it: each file written, and the key of each check that held.
#[derive(Default)]
struct Record {
    files: BTreeMap<(String, String), Written>,
    checks: BTreeMap<String, String>,
    /// The releases its files are of: none said, all the plan's.
    releases: Option<std::collections::BTreeSet<String>>,
}

fn read_record(path: &Path) -> Record {
    let Ok(text) = std::fs::read_to_string(path) else { return Record::default() };
    let mut record = Record::default();
    let head = text.lines().next().and_then(|line| serde_json::from_str::<Value>(line).ok());
    record.releases = head
        .as_ref()
        .and_then(|h| h.get("releases"))
        .and_then(Value::as_array)
        .map(|r| r.iter().filter_map(Value::as_str).map(str::to_string).collect());
    for line in text.lines().skip(1) {
        let Ok(line) = serde_json::from_str::<Value>(line) else { continue };
        let field = |name: &str| line.get(name).and_then(Value::as_str).map(str::to_string);
        if let (Some(check), Some(key)) = (field("check"), field("key")) {
            record.checks.insert(check, key);
            continue;
        }
        if let (Some(node), Some(file), Some(key), Some(sha1)) =
            (field("node"), field("file"), field("key"), field("sha1"))
        {
            let size = line.get("size").and_then(Value::as_u64).unwrap_or(0);
            let shared = line.get("shared").and_then(Value::as_bool) == Some(true);
            record.files.insert((node, file), Written { key, sha1, size, shared });
        }
    }
    record
}

fn write_record(path: &Path, plan: &Plan, record: &Record) -> Result<()> {
    let mut text = json!({"record": 1, "plan": plan.sha1, "releases": record.releases}).to_string();
    text.push('\n');
    for ((node, file), written) in &record.files {
        let mut line =
            json!({"node": node, "file": file, "key": written.key, "sha1": written.sha1, "size": written.size});
        if written.shared {
            line["shared"] = json!(true);
        }
        text.push_str(&line.to_string());
        text.push('\n');
    }
    for (check, key) in &record.checks {
        text.push_str(&json!({"check": check, "key": key}).to_string());
        text.push('\n');
    }
    crate::cache::write_atomic(path, text.as_bytes())
}

/// What an operation made.
#[derive(Clone)]
enum Made {
    Disc(Arc<OpenDisc>),
    Container(Arc<Container>),
    /// A movie file's clips: their sectors, `[start, end)`.
    Clips(Arc<Vec<(u64, u64)>>),
    Bytes(Arc<Vec<u8>>),
    /// What an extension's operation made: its files, by name.
    Files(Arc<BTreeMap<String, Arc<Vec<u8>>>>),
}

impl Made {
    /// What a bundle's file is given of it: bytes as they are, or its file of that name.
    fn file(&self, name: &str) -> Result<Arc<Vec<u8>>> {
        match self {
            Made::Bytes(bytes) => Ok(bytes.clone()),
            Made::Files(files) => files.get(name).cloned().with_context(|| format!("it made no {name}")),
            _ => bail!("it made no file"),
        }
    }

    /// What a bundle's given file or folder is of it, each file by its name in the bundle: none for an optional one not
    /// made.
    fn given(&self, given: &Given) -> Result<GivenFiles> {
        if given.folder() {
            let Made::Files(files) = self else { bail!("it made no files to give a folder") };
            let prefix = given.prefix.as_deref().unwrap_or_default();
            let found: Vec<_> = files
                .iter()
                .filter_map(|(name, bytes)| {
                    Some((format!("{}{}", given.name, name.strip_prefix(prefix)?), Some(bytes.clone())))
                })
                .collect();
            anyhow::ensure!(!found.is_empty() || given.optional, "it made nothing under {prefix}");
            return Ok(found);
        }
        let name = given.prefix.as_deref().unwrap_or(&given.name);
        match (self, given.optional) {
            (Made::Files(files), true) if !files.contains_key(name) => Ok(vec![(given.name.clone(), None)]),
            _ => Ok(vec![(given.name.clone(), Some(self.file(name)?))]),
        }
    }

    /// It as an extension's operation is given it: its bytes as one file, or its files.
    fn value(&self) -> Result<crate::graph::Value> {
        let files = match self {
            Made::Bytes(bytes) => vec![pe2_ext::File::new("stored", bytes.to_vec())],
            Made::Files(files) => files.iter().map(|(name, bytes)| pe2_ext::File::new(name, bytes.to_vec())).collect(),
            _ => bail!("it is no value an operation takes"),
        };
        Ok(crate::graph::Value::Typed(pe2_ext::Typed::new(pe2_ext::data::BYTES, files)))
    }
}

/// The files a bundle is given of what an operation made, by name: none for an optional one not made.
type GivenFiles = Vec<(String, Option<Arc<Vec<u8>>>)>;

/// The node an extension's operation runs for in a plan: none, by a name to say in what fails.
struct Planned(String);

impl crate::graph::exec::Node for Planned {
    fn path(&self) -> &str {
        &self.0
    }

    fn uid(&self) -> Option<&str> {
        None
    }

    fn declaration(&self) -> &'static str {
        ""
    }

    fn fields(&self) -> &toml::Table {
        static NONE: std::sync::OnceLock<toml::Table> = std::sync::OnceLock::new();
        NONE.get_or_init(toml::Table::new)
    }

    fn file_sha1(&self, _: &str) -> Option<String> {
        None
    }

    fn read(&self, name: &str) -> Result<Vec<u8>> {
        bail!("{}: a plan's operation reads no file of a node ({name})", self.0)
    }
}

/// What a run did.
#[derive(Debug, Default)]
pub struct Ran {
    /// The releases read.
    pub releases: Vec<String>,
    /// Files written, and kept as they were; files of releases not read, left as they are.
    pub written: usize,
    pub kept: usize,
    pub skipped: usize,
    /// Releases' own files not kept, being the same as what the releases share.
    pub shared: usize,
    /// Operations run.
    pub ran: usize,
    /// Files removed, no bundle being given them any more.
    pub removed: usize,
}

/// How a run goes, as it goes: read from another thread, which may also stop it (`stop`).
#[derive(Debug, Default)]
pub struct Progress {
    /// Operations to run, once known; run so far; files written so far.
    pub operations: std::sync::atomic::AtomicUsize,
    pub ran: std::sync::atomic::AtomicUsize,
    pub written: std::sync::atomic::AtomicUsize,
    /// Set, the run stops where it is: what it wrote is recorded, so the next run goes on from there.
    pub stop: std::sync::atomic::AtomicBool,
}

impl Progress {
    /// Operations run and to run, and files written.
    #[must_use]
    pub fn now(&self) -> (usize, usize, usize) {
        let get = |a: &std::sync::atomic::AtomicUsize| a.load(std::sync::atomic::Ordering::Relaxed);
        (get(&self.ran), get(&self.operations), get(&self.written))
    }
}

/// A disc a plan reads, looked for: where it was found, if it was.
#[derive(Clone, Debug)]
pub struct Disc {
    pub release: String,
    pub name: String,
    pub size: u64,
    /// Where it was looked for: its release's `look_in`, resolved.
    pub look_in: PathBuf,
    pub found: Option<PathBuf>,
}

/// Look for the discs a plan reads where it says, each by its size and sha1 (each image of its size hashed).
#[must_use]
pub fn discs(project: &crate::Project, header: &Header) -> Vec<Disc> {
    let mut out = Vec::new();
    for (release, input) in header.order.iter().filter_map(|r| Some((r, header.inputs.get(r)?))) {
        let look_in = project.resolve(&input.look_in).unwrap_or_else(|_| PathBuf::from(&input.look_in));
        for (name, size, sha1) in &input.discs {
            let found = discs::find_disc(&look_in, name, *size, sha1).ok();
            out.push(Disc {
                release: release.clone(),
                name: name.clone(),
                size: *size,
                look_in: look_in.clone(),
                found,
            });
        }
    }
    out
}

/// What an operation read, as `observe` saw it: its bytes' sha1 and size, or why it read nothing.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Observed {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha1: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// What `observe` notes as it goes, and where it keeps what was read.
struct Observing {
    keep: Option<PathBuf>,
    /// The operations whose bytes are kept, by kind: all where empty.
    kinds: Vec<String>,
    seen: Mutex<Vec<Observed>>,
}

impl Observing {
    fn saw(&self, op: &Op, bytes: &[u8]) -> Result<()> {
        let sha1 = sha1_hex(bytes);
        if let Some(keep) = self.keep.as_ref().filter(|_| self.kinds.is_empty() || self.kinds.contains(&op.kind)) {
            let path = keep.join(&sha1);
            if !path.exists() {
                std::fs::write(&path, bytes).with_context(|| path.display().to_string())?;
            }
        }
        let seen = Observed { id: op.id.clone(), sha1: Some(sha1), size: Some(bytes.len() as u64), error: None };
        self.seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(seen);
        Ok(())
    }

    /// An extension's operation made what it makes: what it was given held, its declarations among them.
    fn made(&self, op: &Op) {
        let seen = Observed { id: op.id.clone(), sha1: None, size: None, error: None };
        self.seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(seen);
    }

    fn failed(&self, op: &Op, why: &anyhow::Error) {
        let seen = Observed { id: op.id.clone(), sha1: None, size: None, error: Some(format!("{why:#}")) };
        self.seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(seen);
    }
}

/// Run every operation of a plan and say what each read: its bytes' sha1 and size - kept in `keep` by sha1, where
/// given, those of the operations of `kinds` (all where empty) - that an extension's made its files, or why it failed,
/// an operation failing alone (what takes it is not run). Nothing is written but what is kept:
/// what a plan would read, surveyed - another release's discs held up to one's declarations. Each release's discs are
/// looked for where `look_in` says.
///
/// # Errors
/// The plan cannot be read, or what is read cannot be kept.
pub fn observe(
    plan_file: &Path,
    look_in: &dyn Fn(&str) -> Result<PathBuf>,
    engine: &crate::graph::exec::Engine,
    extensions: &crate::ext::Extensions,
    keep: Option<&Path>,
    kinds: &[String],
) -> Result<Vec<Observed>> {
    let plan = Plan::read(plan_file)?;
    let keys = plan.keys(&|id| extensions.version_of(id));
    let count = plan.ops.len();
    let mut waiting: Vec<usize> = vec![0; count];
    let mut takers: Vec<Vec<usize>> = vec![Vec::new(); count];
    let mut uses: Vec<usize> = vec![0; count];
    for (at, op) in plan.ops.iter().enumerate() {
        for taken in taken_by(&plan, op) {
            takers[taken].push(at);
            waiting[at] += 1;
            uses[taken] += 1;
        }
    }
    if let Some(keep) = keep {
        std::fs::create_dir_all(keep).with_context(|| keep.display().to_string())?;
    }
    let observing =
        Observing { keep: keep.map(Path::to_path_buf), kinds: kinds.to_vec(), seen: Mutex::new(Vec::new()) };
    let progress = Progress::default();
    let state = RunState {
        plan: &plan,
        keys: &keys,
        inputs: plan.inputs.iter().map(|(r, i)| Ok((r.clone(), look_in(&i.look_in)?))).collect::<Result<_>>()?,
        layer: Path::new(""),
        found: HashMap::new(),
        engine,
        extensions,
        checked: vec![false; count],
        made: Mutex::new(HashMap::new()),
        waiting: waiting.into_iter().map(std::sync::atomic::AtomicUsize::new).collect(),
        uses: uses.into_iter().map(std::sync::atomic::AtomicUsize::new).collect(),
        takers,
        files_of: vec![Vec::new(); count],
        written: Mutex::new(Vec::new()),
        failed: Mutex::new(None),
        ran: std::sync::atomic::AtomicUsize::new(0),
        progress: &progress,
        observing: Some(&observing),
    };
    let ready: Vec<usize> =
        (0..count).filter(|&at| state.waiting[at].load(std::sync::atomic::Ordering::Acquire) == 0).collect();
    let ahead = |at: &usize| matches!(plan.ops[*at].kind.as_str(), "disc" | "container" | "movie_clips");
    crate::schedule::worklist(ready, ahead, |at, add| state.run(at, add));
    let mut seen = observing.seen.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner);
    seen.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(seen)
}

impl Plan {
    /// What a release's own file (`<release>/<name>`) is apart from: the file the releases share (`<name>`).
    fn plain_of<'a>(&self, file: &'a str) -> Option<&'a str> {
        let (release, name) = file.split_once('/')?;
        (self.inputs.contains_key(release) && !name.ends_with('/')).then_some(name)
    }
}

/// Where a recorded file's bytes are in the layer: its own place, or, one the same as what the releases share, the
/// shared file's.
fn on_disk(plan: &Plan, layer: &Path, node: &str, file: &str, written: &Written) -> PathBuf {
    match plan.plain_of(file).filter(|_| written.shared) {
        Some(shared) => file_path(layer, node, shared),
        None => file_path(layer, node, file),
    }
}

/// Keep no release's own file that is the same as what the releases share: each removed, recorded as the shared one.
/// How many.
fn share_alike(plan: &Plan, layer: &Path, record: &mut Record) -> usize {
    let alike: Vec<(String, String)> = record
        .files
        .iter()
        .filter(|((node, file), written)| {
            written.present()
                && !written.shared
                && plan.plain_of(file).is_some_and(|plain| {
                    let shared = record.files.get(&(node.clone(), plain.to_string()));
                    shared.is_some_and(|s| s.present() && !s.shared && s.sha1 == written.sha1)
                })
        })
        .map(|(entry, _)| entry.clone())
        .collect();
    for (node, file) in &alike {
        let path = file_path(layer, node, file);
        let _ = crate::content::remove(&path);
        if let Some(folder) = path.parent() {
            let _ = std::fs::remove_dir(folder);
        }
        if let Some(written) = record.files.get_mut(&(node.clone(), file.clone())) {
            written.shared = true;
        }
    }
    alike.len()
}

/// The path of a bundle's file in the layer.
fn file_path(layer: &Path, node: &str, file: &str) -> PathBuf {
    layer.join(format!("{node}{}", crate::index::BUNDLE_SUFFIX)).join(file)
}

/// What a run is to make: each file whose key changed, or which is not there as written - a folder's, any of its
/// files; what is as written goes into the new record as it was.
fn wanted(
    plan: &Plan,
    keys: &[String],
    layer: &Path,
    old: &Record,
    record: &mut Record,
    ran: &mut Ran,
) -> Vec<(String, Given)> {
    // What is wanted: each file whose key changed, or which is not there as written - a folder's, any of its files.
    // One of releases not read is left as it is.
    let mut wanted: Vec<(String, Given)> = Vec::new();
    for emit in &plan.emits {
        for given in &emit.files {
            let key = Plan::file_key(keys, given);
            let held: Vec<(&(String, String), &Written)> = old
                .files
                .iter()
                .filter(|((node, file), _)| {
                    *node == emit.node
                        && if given.folder() { file.starts_with(&given.name) } else { *file == given.name }
                })
                .collect();
            let as_written = |((node, file), written): &(&(String, String), &Written)| {
                let there = crate::content::len(&on_disk(plan, layer, node, file, written)).ok();
                // What a release's own file was the same as is so still.
                let alike = !written.shared
                    || plan.plain_of(file).is_some_and(|plain| {
                        old.files.get(&(node.clone(), plain.to_string())).is_some_and(|s| s.sha1 == written.sha1)
                    });
                written.key == key && alike && there == written.present().then_some(written.size)
            };
            if !given.possible {
                // Left as it is: what the last run made of it, if anything.
                ran.skipped += usize::from(!held.is_empty());
                for (entry, written) in held {
                    record.files.insert(entry.clone(), written.clone());
                }
            } else if !held.is_empty() && held.iter().all(as_written) {
                for (entry, written) in held {
                    record.files.insert(entry.clone(), written.clone());
                }
                ran.kept += 1;
            } else {
                wanted.push((emit.node.clone(), given.clone()));
            }
        }
    }
    wanted
}

/// The checks of what was written (`assembled`), each at once: they join what was written, each its own.
#[expect(clippy::too_many_arguments, reason = "what a run holds, as it ends")]
fn joined(
    plan: &Plan,
    keys: &[String],
    last: &[usize],
    layers: [&Path; 2],
    engine: &crate::graph::exec::Engine,
    extensions: &crate::ext::Extensions,
    record: &mut Record,
    ran: &mut Ran,
) -> Result<()> {
    let joined = crate::parallel(last, |&at| -> Result<usize> {
        let op = &plan.ops[at];
        let args = op.args();
        let node = args.get("node").and_then(Value::as_str).with_context(|| format!("{}: no node", op.id))?;
        let want = op.sha1.as_deref().with_context(|| format!("{}: no sha1 to hold it to", op.id))?;
        let got = crate::extract::assembled_sha1(layers, node, engine, extensions).with_context(|| op.id.clone())?;
        anyhow::ensure!(got == want, "{}: what it joins has sha1 {got}, where the plan says {want}", op.id);
        Ok(at)
    });
    for at in joined {
        let at = at?;
        record.checks.insert(plan.ops[at].id.clone(), keys[at].clone());
        ran.ran += 1;
    }
    Ok(())
}

/// Run a generated layer's plan (`Layer::plan`), filling the layer: make what changed or is gone, its discs looked for
/// where each release's input says (`look_in`, a `res://` path of the project), running extensions' operations with
/// `engine`, each kept by its extension's version.
///
/// # Errors
/// It is no generated layer of the project, its plan cannot be read, it needs what the extensions lack, a disc is not
/// there, an operation fails, or what it makes is not what the plan says.
#[expect(clippy::too_many_lines, reason = "a run, as it goes")]
pub fn run(
    project: &crate::Project,
    generated: &crate::Layer,
    engine: &crate::graph::exec::Engine,
    extensions: &crate::ext::Extensions,
    progress: &Progress,
    releases: &Releases,
) -> Result<Ran> {
    let fills = generated.fills.as_deref().with_context(|| format!("{} is no generated layer", generated.name))?;
    let nodes = &project.layer(fills)?.path;
    // What the nodes are, read as any layer's: a records layer's through its records.
    crate::records::Set::open(project)?;
    let layer = generated.path.as_path();
    let mut plan = Plan::read(&generated.plan())?;
    let unmet = plan.unmet(extensions);
    anyhow::ensure!(unmet.is_empty(), "the plan needs {}", unmet.join("; "));
    let keys = plan.keys(&|id| extensions.version_of(id));
    let (available, found) = found_releases(project, &plan, releases)?;
    let possible = plan.resolve(&available);
    let old = read_record(&layer.join(RECORD));
    let mut record = Record::default();
    let in_order = plan.releases().into_iter().filter(|r| available.contains(r)).collect();
    let mut ran = Ran { releases: in_order, ..Ran::default() };
    let wanted = wanted(&plan, &keys, layer, &old, &mut record, &mut ran);
    // Each check whose key changed; a check of what was written, after everything is.
    let mut checked = vec![false; plan.ops.len()];
    let mut last = Vec::new();
    for &at in plan.checks.iter().filter(|&&at| possible[at]) {
        let id = &plan.ops[at].id;
        if old.checks.get(id) == Some(&keys[at]) {
            record.checks.insert(id.clone(), keys[at].clone());
        } else if plan.ops[at].kind == ASSEMBLED {
            last.push(at);
        } else {
            checked[at] = true;
        }
    }
    // What they need, each once; how many of what is wanted take each; and who takes each.
    let mut needed = vec![false; plan.ops.len()];
    let mut stack: Vec<usize> = wanted.iter().map(|(_, given)| given.at).collect();
    stack.extend((0..plan.ops.len()).filter(|&at| checked[at]));
    while let Some(at) = stack.pop() {
        if !std::mem::replace(&mut needed[at], true) {
            stack.extend(plan.ops[at].takes.values().flatten().map(|id| plan.by_id[id]));
        }
    }
    let mut waiting: Vec<usize> = vec![0; plan.ops.len()];
    let mut takers: Vec<Vec<usize>> = vec![Vec::new(); plan.ops.len()];
    let mut uses: Vec<usize> = vec![0; plan.ops.len()];
    for (at, op) in plan.ops.iter().enumerate().filter(|(at, _)| needed[*at]) {
        for taken in taken_by(&plan, op) {
            if !takers[taken].contains(&at) {
                takers[taken].push(at);
                waiting[at] += 1;
                uses[taken] += 1;
            }
        }
    }
    let mut files_of: Vec<Vec<(String, Given)>> = vec![Vec::new(); plan.ops.len()];
    for (node, given) in &wanted {
        files_of[given.at].push((node.clone(), given.clone()));
        uses[given.at] += 1;
    }
    for at in (0..plan.ops.len()).filter(|&at| checked[at]) {
        uses[at] += 1;
    }
    progress.operations.store(needed.iter().filter(|n| **n).count(), std::sync::atomic::Ordering::Relaxed);
    let state = RunState {
        plan: &plan,
        keys: &keys,
        inputs: plan
            .inputs
            .iter()
            .filter(|(release, _)| available.contains(*release))
            .map(|(release, input)| Ok((release.clone(), project.resolve(&input.look_in)?)))
            .collect::<Result<_>>()?,
        found,
        layer,
        engine,
        extensions,
        checked,
        made: Mutex::new(HashMap::new()),
        waiting: waiting.into_iter().map(std::sync::atomic::AtomicUsize::new).collect(),
        uses: uses.into_iter().map(std::sync::atomic::AtomicUsize::new).collect(),
        takers,
        files_of,
        written: Mutex::new(Vec::new()),
        failed: Mutex::new(None),
        ran: std::sync::atomic::AtomicUsize::new(0),
        progress,
        observing: None,
    };
    let ready: Vec<usize> = (0..plan.ops.len())
        .filter(|&at| needed[at] && state.waiting[at].load(std::sync::atomic::Ordering::Acquire) == 0)
        .collect();
    // The discs and containers first: everything waits on them.
    let ahead = |at: &usize| matches!(plan.ops[*at].kind.as_str(), "disc" | "container" | "movie_clips");
    crate::schedule::worklist(ready, ahead, |at, add| state.run(at, add));
    let failed = state.failed.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner);
    ran.ran = state.ran.into_inner();
    for (entry, written) in state.written.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner) {
        record.files.insert(entry, written);
        ran.written += 1;
    }
    if let Some(why) = failed {
        record.releases = Some(available.iter().cloned().chain(old.releases.clone().unwrap_or_default()).collect());
        // What was written is kept as made: a run stopped or failed goes on from there next time.
        write_record(&layer.join(RECORD), &plan, &record)?;
        return Err(why);
    }
    for at in (0..plan.ops.len()).filter(|&at| state.checked[at]) {
        record.checks.insert(plan.ops[at].id.clone(), keys[at].clone());
    }
    joined(&plan, &keys, &last, [nodes, layer], engine, extensions, &mut record, &mut ran)?;
    // The releases the files are of: those read, and those of what was left as it was.
    record.releases = Some(if ran.skipped > 0 {
        let before = old.releases.clone().unwrap_or_else(|| plan.inputs.keys().cloned().collect());
        available.iter().cloned().chain(before).collect()
    } else {
        available.clone()
    });
    ran.shared = share_alike(&plan, layer, &mut record);
    ran.removed = remove_left(layer, &old, &record);
    write_record(&layer.join(RECORD), &plan, &record)?;
    Ok(ran)
}

/// Remove what no bundle is given any more: what the last run recorded and this one did not. How many went.
fn remove_left(layer: &Path, old: &Record, record: &Record) -> usize {
    let left = old.files.keys().filter(|entry| !record.files.contains_key(*entry));
    left.filter(|(node, file)| {
        let path = file_path(layer, node, file);
        crate::content::exists(&path) && crate::content::remove(&path).is_ok()
    })
    .count()
}

const ORDER: std::sync::atomic::Ordering = std::sync::atomic::Ordering::AcqRel;
/// A container assembled from what the layer's bundles were given (a disc's movie file), joined by its node's stack and
/// held to the plan's sha1: a check run once everything is written.
const ASSEMBLED: &str = "assembled";

/// What an operation takes, each once.
fn taken_by(plan: &Plan, op: &Op) -> Vec<usize> {
    let mut taken: Vec<usize> = op.takes.values().flatten().map(|id| plan.by_id[id]).collect();
    taken.sort_unstable();
    taken.dedup();
    taken
}

struct RunState<'a> {
    plan: &'a Plan,
    keys: &'a [String],
    /// Where each release's discs are looked for, and where those found are, by sha1.
    inputs: HashMap<String, PathBuf>,
    found: HashMap<String, PathBuf>,
    layer: &'a Path,
    engine: &'a crate::graph::exec::Engine,
    extensions: &'a crate::ext::Extensions,
    /// The checks to run.
    checked: Vec<bool>,
    made: Mutex<HashMap<usize, Made>>,
    /// How many of what each takes are not made yet; how many takers and files each is still to be read by.
    waiting: Vec<std::sync::atomic::AtomicUsize>,
    uses: Vec<std::sync::atomic::AtomicUsize>,
    takers: Vec<Vec<usize>>,
    files_of: Vec<Vec<(String, Given)>>,
    written: Mutex<Vec<((String, String), Written)>>,
    failed: Mutex<Option<anyhow::Error>>,
    ran: std::sync::atomic::AtomicUsize,
    progress: &'a Progress,
    /// Observing (`observe`): what each operation read is noted, an operation failing failing alone.
    observing: Option<&'a Observing>,
}

impl RunState<'_> {
    fn run(&self, at: usize, add: &dyn Fn(usize)) {
        if self.observing.is_none() && self.failed.lock().is_ok_and(|failed| failed.is_some()) {
            return;
        }
        if self.progress.stop.load(std::sync::atomic::Ordering::Relaxed) {
            self.failed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get_or_insert(anyhow::anyhow!("stopped"));
            return;
        }
        let op = &self.plan.ops[at];
        let done = (|| -> Result<()> {
            let _span = tracing::info_span!("plan op", of = %op.kind).entered();
            let made = self.make(op).with_context(|| op.id.clone())?;
            match (self.observing, &made) {
                (Some(observing), Made::Bytes(bytes)) => observing.saw(op, bytes)?,
                (Some(observing), Made::Files(_)) => observing.made(op),
                _ => {}
            }
            self.ran.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.progress.ran.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            for (node, given) in &self.files_of[at] {
                for (file, bytes) in made.given(given).with_context(|| format!("{node}: {}", given.name))? {
                    let key = Plan::file_key(self.keys, given);
                    let Some(bytes) = bytes else {
                        // An optional file not made: recorded so, so that it is not asked for again.
                        let _ = crate::content::remove(&file_path(self.layer, node, &file));
                        self.lock_written()
                            .push(((node.clone(), file), Written { key, sha1: String::new(), size: 0, shared: false }));
                        continue;
                    };
                    let path = file_path(self.layer, node, &file);
                    crate::content::write(&path, bytes.as_slice()).with_context(|| path.display().to_string())?;
                    let written = Written { key, sha1: sha1_hex(&bytes), size: bytes.len() as u64, shared: false };
                    self.lock_written().push(((node.clone(), file), written));
                    self.progress.written.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
            let own = self.files_of[at].len() + usize::from(self.checked[at]);
            if self.uses[at].fetch_sub(own, ORDER) > own {
                self.lock_made().insert(at, made);
            }
            // What took it is let go of once each of its takers has it; its takers may now be ready.
            for taken in taken_by(self.plan, op) {
                if self.uses[taken].fetch_sub(1, ORDER) == 1 {
                    self.lock_made().remove(&taken);
                }
            }
            for &taker in &self.takers[at] {
                if self.waiting[taker].fetch_sub(1, ORDER) == 1 {
                    add(taker);
                }
            }
            Ok(())
        })();
        if let Err(why) = done {
            if let Some(observing) = self.observing {
                observing.failed(op, &why);
                return;
            }
            self.failed.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get_or_insert(why);
        }
    }

    fn lock_made(&self) -> std::sync::MutexGuard<'_, HashMap<usize, Made>> {
        self.made.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn lock_written(&self) -> std::sync::MutexGuard<'_, Vec<((String, String), Written)>> {
        self.written.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// What it takes at a port: the one value made there.
    fn taken(&self, op: &Op, port: &str) -> Result<Made> {
        let id = op.takes.get(port).and_then(|ids| ids.first()).with_context(|| format!("it takes no {port}"))?;
        self.lock_made().get(&self.plan.by_id[id]).cloned().with_context(|| format!("{id} was not made"))
    }

    fn disc(&self, op: &Op) -> Result<Arc<OpenDisc>> {
        match self.taken(op, "disc")? {
            Made::Disc(disc) => Ok(disc),
            _ => bail!("its disc is no disc"),
        }
    }

    fn container(&self, op: &Op) -> Result<Arc<Container>> {
        match self.taken(op, "container")? {
            Made::Container(container) => Ok(container),
            _ => bail!("its container is no container"),
        }
    }

    /// An extension's operation: given what it takes in its ports' order, its `args` as its settings; what it makes
    /// read back as its files.
    fn extension(&self, op: &Op) -> Result<Made> {
        let def = self.engine.ops.get(&op.kind).with_context(|| format!("no extension provides {}", op.kind))?;
        let mut inputs = Vec::with_capacity(def.inputs.len());
        for port in &def.inputs {
            let ids = op.takes.get(&port.name).map_or(&[][..], Vec::as_slice);
            let values = ids.iter().map(|id| self.made_by(id)?.value()).collect::<Result<Vec<_>>>()?;
            inputs.push(values);
        }
        let settings = match &op.args() {
            Value::Null => toml::Table::new(),
            args => toml::Table::try_from(args).context("its args as settings")?,
        };
        let value = self.engine.run(&op.kind, inputs, &settings, &Planned(op.id.clone()))?;
        let crate::graph::Value::Typed(typed) = value else { bail!("it made {}, not files", value.ty()) };
        let mut files = BTreeMap::new();
        for file in &typed.files {
            let bytes = match &file.content {
                crate::graph::Content::Inline(bytes) => bytes.clone(),
                crate::graph::Content::Stored { sha1, .. } => {
                    let path = self.engine.store().context("no store")?.blob_path(sha1);
                    std::fs::read(&path).with_context(|| path.display().to_string())?
                }
            };
            files.insert(file.name.clone(), Arc::new(bytes));
        }
        Ok(Made::Files(Arc::new(files)))
    }

    /// A node's source, written from what it is given (`native`) by the operation undoing its import, and held to
    /// import back into it, files and settings both, through the node as declared (`declared`, as TOML: its stack, and
    /// its settings with those its import derives).
    fn source(&self, op: &Op) -> Result<Made> {
        let Made::Files(native) = self.taken(op, "native")? else { bail!("its native form is no files") };
        let args = op.args();
        let text = |name: &str| args.get(name).and_then(Value::as_str).with_context(|| format!("no {name}"));
        // TOML text, whose tables keep their order: what a source is written in.
        let declared = text("declared")?.to_string();
        let native = native.iter().map(|(name, bytes)| (name.clone(), bytes.to_vec())).collect();
        let sourcing = crate::extract::Sourcing {
            extensions: self.extensions,
            engine: self.engine,
            sources: std::sync::atomic::AtomicUsize::new(0),
        };
        let (_, written) = sourcing.bundle(text("node")?, text("uid")?, declared, native)?;
        Ok(Made::Files(Arc::new(written.into_iter().map(|(name, bytes)| (name, Arc::new(bytes))).collect())))
    }

    fn made_by(&self, id: &str) -> Result<Made> {
        self.lock_made().get(&self.plan.by_id[id]).cloned().with_context(|| format!("{id} was not made"))
    }

    fn clips(&self, op: &Op) -> Result<Arc<Vec<(u64, u64)>>> {
        match self.taken(op, "clips")? {
            Made::Clips(clips) => Ok(clips),
            _ => bail!("its clips are no clips"),
        }
    }

    fn make(&self, op: &Op) -> Result<Made> {
        let args = &op.args();
        let text = |name: &str| args.get(name).and_then(Value::as_str).with_context(|| format!("no {name}"));
        let number = |name: &str| args.get(name).and_then(Value::as_u64).with_context(|| format!("no {name}"));
        let folder = || args.get("folder").and_then(Value::as_u64).map(u32::try_from).transpose();
        let made = match op.kind.as_str() {
            "disc" => {
                let image = args.get("image").context("no image")?;
                let field = |name: &str| image.get(name).and_then(Value::as_str).map(str::to_string);
                let slots = args.get("slots").and_then(Value::as_array).context("no slots")?;
                let slots = slots
                    .iter()
                    .filter_map(|s| Some((s.get(0)?.as_str()?.to_string(), s.get(1)?.as_str()?.to_string())))
                    .collect();
                let layout = discs::Layout {
                    image: field("name").context("its image has no name")?,
                    size: image.get("size").and_then(Value::as_u64).context("its image has no size")?,
                    sha1: field("sha1").context("its image has no sha1")?,
                    slots,
                };
                if let Some(path) = self.found.get(&layout.sha1) {
                    return Ok(Made::Disc(Arc::new(OpenDisc::open_at(path.clone(), &op.id, &layout)?)));
                }
                let release = text("release")?;
                let look_in =
                    self.inputs.get(release).with_context(|| format!("the plan reads no release {release}"))?;
                return Ok(Made::Disc(Arc::new(OpenDisc::open(look_in, &op.id, &layout)?)));
            }
            "slot" => self.disc(op)?.slot(text("name")?)?,
            "container" => {
                let disc = self.disc(op)?;
                let stage = u32::try_from(number("stage")?)?;
                return Ok(Made::Container(Arc::new(discs::container(&disc, "", stage, text("cdf")?)?)));
            }
            "chunk" => {
                let (disc, container) = (self.disc(op)?, self.container(op)?);
                discs::chunk(
                    &disc,
                    &container,
                    folder()?,
                    u32::try_from(number("file")?)?,
                    usize::try_from(number("index")?)?,
                )?
            }
            "stored_stream" => {
                let (disc, container) = (self.disc(op)?, self.container(op)?);
                discs::stream(&disc, &container, folder()?, usize::try_from(number("index")?)?)?
            }
            "movie_clips" => {
                let disc = self.disc(op)?;
                return Ok(Made::Clips(Arc::new(movie_clips(&disc, text("name")?)?)));
            }
            "clip" => {
                let (disc, clips) = (self.disc(op)?, self.clips(op)?);
                let index = usize::try_from(number("index")?)?;
                let &(start, end) =
                    clips.get(index).with_context(|| format!("the file holds {} clips", clips.len()))?;
                let sector = formats::str::SECTOR as u64;
                disc.slot_range(text("name")?, start * sector, usize::try_from((end - start) * sector)?)?
            }
            "inflate" => {
                let given = self.taken(op, "bytes")?.file("")?;
                formats::lzss::decode_stream(&given, 0).0
            }
            "slice" => {
                let given = self.taken(op, "bytes")?.file("")?;
                let (offset, size) = (usize::try_from(number("offset")?)?, usize::try_from(number("size")?)?);
                let run =
                    given.get(offset..offset + size).with_context(|| format!("past the end, of {}", given.len()))?;
                run.to_vec()
            }
            "source" => return self.source(op),
            other if other.contains('@') => return self.extension(op),
            other => bail!("no operation {other} runs in a plan"),
        };
        if let Some(want) = &op.sha1 {
            let got = sha1_hex(&made);
            anyhow::ensure!(got == *want, "it read sha1 {got}, where the plan says {want}");
        }
        Ok(Made::Bytes(Arc::new(made)))
    }
}

/// Sectors read at once while finding a movie file's clips: about 9.5 MB.
const WINDOW: u64 = 4096;

/// A movie file's clips: their sectors' ranges, `[start, end)`. A clip starts where a video sector's frame number drops
/// below the one before it, each clip counting its frames from 1; the file is read a window at a time, being hundreds of
/// megabytes.
fn movie_clips(disc: &OpenDisc, name: &str) -> Result<Vec<(u64, u64)>> {
    use formats::str::{SECTOR, Sector};
    let sectors = disc.slot_size(name)? / SECTOR as u64;
    let (mut clips, mut begin, mut last) = (Vec::new(), 0u64, 0u32);
    let mut at = 0;
    while at < sectors {
        let count = WINDOW.min(sectors - at);
        let bytes = disc.slot_range(name, at * SECTOR as u64, usize::try_from(count)? * SECTOR)?;
        for (k, sector) in bytes.as_chunks::<SECTOR>().0.iter().enumerate() {
            if let Sector::Video(chunk, _) = Sector::raw(sector) {
                let here = at + k as u64;
                if chunk.frame < last && here > 0 {
                    clips.push((begin, here));
                    begin = here;
                }
                last = chunk.frame;
            }
        }
        at += count;
    }
    clips.push((begin, sectors));
    Ok(clips)
}

/// How a layer is, by its plan: none to fill it by; not filled; filled as the plan says now; filled of some of the
/// plan's releases only, the others' discs not read (these); or not as it says now - what it needs not there, or what
/// was made by another plan or by other extensions, or gone since.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    NoPlan,
    Empty,
    Filled,
    Partial(Vec<String>),
    Stale(Vec<String>),
}

impl Plan {
    /// As the record's files are: what makes each chosen of the releases they are of. Those releases.
    fn as_recorded(&mut self, record: &Record) -> std::collections::BTreeSet<String> {
        let releases = record.releases.clone().unwrap_or_else(|| self.inputs.keys().cloned().collect());
        self.resolve(&releases);
        releases
    }
}

/// How a layer is, by its plan and the record of its last run - known without the discs and without reading what was
/// made: each file's key against what the plan says now, and whether it is there.
///
/// # Errors
/// The plan cannot be read.
pub fn state(plan_file: &Path, layer: &Path, extensions: &crate::ext::Extensions) -> Result<State> {
    if !plan_file.is_file() {
        return Ok(State::NoPlan);
    }
    let mut plan = Plan::read(plan_file)?;
    let record = read_record(&layer.join(RECORD));
    if record.files.is_empty() && record.checks.is_empty() {
        return Ok(State::Empty);
    }
    let releases = plan.as_recorded(&record);
    let possible = plan.resolve(&releases);
    let mut stale: Vec<String> = plan.unmet(extensions).into_iter().map(|unmet| format!("it needs {unmet}")).collect();
    let keys = plan.keys(&|id| extensions.version_of(id));
    let (mut other, mut gone) = (0, 0);
    for emit in &plan.emits {
        for given in emit.files.iter().filter(|given| given.possible) {
            let key = Plan::file_key(&keys, given);
            let held: Vec<_> = record
                .files
                .iter()
                .filter(|((node, file), _)| {
                    *node == emit.node
                        && if given.folder() { file.starts_with(&given.name) } else { *file == given.name }
                })
                .collect();
            if held.is_empty() {
                gone += 1;
            }
            for ((node, file), written) in held {
                if written.key != key {
                    other += 1;
                } else if written.present() && !crate::content::exists(&on_disk(&plan, layer, node, file, written)) {
                    gone += 1;
                }
            }
        }
    }
    let checks = plan
        .checks
        .iter()
        .filter(|&&at| possible[at] && record.checks.get(&plan.ops[at].id) != Some(&keys[at]))
        .count();
    for (count, what) in [
        (other, "files made otherwise than the plan says now"),
        (gone, "files not there"),
        (checks, "checks not run as the plan says now"),
    ] {
        if count > 0 {
            stale.push(format!("{count} {what}"));
        }
    }
    let missing: Vec<String> = plan.inputs.keys().filter(|r| !releases.contains(*r)).cloned().collect();
    Ok(match (stale.is_empty(), missing.is_empty()) {
        (false, _) => State::Stale(stale),
        (true, true) => State::Filled,
        (true, false) => State::Partial(missing),
    })
}

/// What a check of a layer against its plan found: files not there, made by another key than the plan's now, changed
/// since written, or there and given to no bundle.
#[derive(Debug, Default)]
pub struct Checked {
    pub files: usize,
    /// The plan's releases its files are not of: their discs were not read.
    pub not_read: Vec<String>,
    pub missing: Vec<String>,
    pub stale: Vec<String>,
    pub changed: Vec<String>,
}

impl Checked {
    #[must_use]
    pub fn whole(&self) -> bool {
        self.missing.is_empty() && self.stale.is_empty() && self.changed.is_empty()
    }
}

/// Check a layer against its plan and record, without the discs: each bundle's file there, made by what the plan says
/// now - its extensions' operations by their `versions` - unchanged since it was written; and each check held of what
/// the plan says now.
///
/// # Errors
/// The plan cannot be read.
pub fn verify(plan_file: &Path, layer: &Path, versions: &dyn Fn(&str) -> String) -> Result<Checked> {
    let mut plan = Plan::read(plan_file)?;
    let keys = plan.keys(versions);
    let record = read_record(&layer.join(RECORD));
    let releases = plan.as_recorded(&record);
    let possible = plan.resolve(&releases);
    // Each file given, as recorded: a folder's, every file recorded in it - none being one missing.
    let mut entries: Vec<(String, String, String, Option<Written>)> = Vec::new();
    for emit in &plan.emits {
        for given in emit.files.iter().filter(|given| given.possible) {
            let key = Plan::file_key(&keys, given);
            let held: Vec<_> = record
                .files
                .iter()
                .filter(|((node, file), _)| {
                    *node == emit.node
                        && if given.folder() { file.starts_with(&given.name) } else { *file == given.name }
                })
                .map(|((node, file), written)| (node.clone(), file.clone(), key.clone(), Some(written.clone())))
                .collect();
            if held.is_empty() {
                entries.push((emit.node.clone(), given.name.clone(), key.clone(), None));
            }
            entries.extend(held);
        }
    }
    let found = crate::parallel(&entries, |(node, file, key, written)| {
        let name = format!("{node}: {file}");
        let Some(written) = written else { return Some((0, name)) };
        if written.key != *key {
            return Some((1, name));
        }
        let path = on_disk(&plan, layer, node, file, written);
        if !written.present() {
            return crate::content::exists(&path).then_some((2, name));
        }
        match crate::content::sha1(&path) {
            Err(_) => Some((0, name)),
            Ok(sha1) if sha1 != written.sha1 => Some((2, name)),
            Ok(_) => None,
        }
    });
    let mut checked = Checked { files: entries.len(), ..Checked::default() };
    for &at in plan.checks.iter().filter(|&&at| possible[at]) {
        let id = &plan.ops[at].id;
        if record.checks.get(id) != Some(&keys[at]) {
            checked.stale.push(format!("{id} (a check)"));
        }
    }
    checked.not_read = plan.inputs.keys().filter(|r| !releases.contains(*r)).cloned().collect();
    for (kind, name) in found.into_iter().flatten() {
        match kind {
            0 => checked.missing.push(name),
            1 => checked.stale.push(name),
            _ => checked.changed.push(name),
        }
    }
    Ok(checked)
}

#[cfg(test)]
mod tests {
    use super::Plan;
    use crate::extract::sha1_hex;

    #[test]
    fn a_layer_says_whether_its_plan_ran_and_what_it_needs() {
        let layer = std::env::temp_dir().join(format!("pe2-plan-state-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&layer);
        std::fs::create_dir_all(&layer).unwrap();
        let extensions = crate::ext::Extensions::load(Vec::new(), true);
        let plan_file = layer.with_extension("plan.jsonl");
        assert_eq!(super::state(&plan_file, &layer, &extensions).unwrap(), super::State::NoPlan);
        let plan = r#"{"plan":1,"inputs":{"USA":{"look_in":"res://roms","discs":[]}},"requires":[{"id":"pe2.nothing","version":"^0.1"}]}
{"id":"a","op":"disc","args":{"image":{"name":"x","size":1,"sha1":"00"},"slots":[]}}
{"emit":"n/m","files":{"native.bin":"a"}}
"#;
        std::fs::write(&plan_file, plan).unwrap();
        assert_eq!(super::state(&plan_file, &layer, &extensions).unwrap(), super::State::Empty);
        let unmet = Plan::parse(plan).unwrap().unmet(&extensions);
        assert!(unmet.len() == 1 && unmet[0].contains("pe2.nothing"), "{unmet:?}");
        let _ = std::fs::remove_dir_all(&layer);
        let _ = std::fs::remove_file(&plan_file);
    }

    #[test]
    fn what_several_releases_make_alike_is_made_of_any_of_them_and_keyed_by_all() {
        let disc = |release: &str| {
            format!(
                r#"{{"id":"disc:{release}","op":"disc","args":{{"release":"{release}","image":{{"name":"{release}","size":1,"sha1":"{release}"}},"slots":[]}}}}"#
            )
        };
        let text = [
            r#"{"plan":1,"inputs":{"USA":{"look_in":"res://roms","discs":[{"name":"USA","size":1,"sha1":"USA"}]},"EUR":{"look_in":"res://roms","discs":[{"name":"EUR","size":1,"sha1":"EUR"}]}}}"#.to_string(),
            disc("USA"),
            disc("EUR"),
            r#"{"id":"a:USA","op":"slot","in":{"disc":"disc:USA"},"args":{"name":"A"}}"#.to_string(),
            r#"{"id":"a:EUR","op":"slot","in":{"disc":"disc:EUR"},"args":{"name":"A"}}"#.to_string(),
            r#"{"id":"made","op":"inflate","in":{"bytes":{"any":["a:USA","a:EUR"]}}}"#.to_string(),
            r#"{"emit":"n","files":{"native.bin":["a:USA","a:EUR"],"inflated":"made","EUR/own.bin":"a:EUR"}}"#
                .to_string(),
        ]
        .join("\n");
        let mut plan = Plan::parse(&text).unwrap();
        let keys = plan.keys(&|_| String::new());
        let key = |plan: &Plan, name: &str| {
            Plan::file_key(&keys, plan.emits[0].files.iter().find(|g| g.name == name).unwrap())
        };
        let before = key(&plan, "native.bin");
        let only = |names: &[&str]| names.iter().map(|n| (*n).to_string()).collect::<std::collections::BTreeSet<_>>();
        // Of Europe's discs alone: Europe's read taken, the file and what takes it made of it, keyed as before.
        let possible = plan.resolve(&only(&["EUR"]));
        let made = &plan.ops[plan.by_id["made"]];
        assert_eq!(made.takes["bytes"], ["a:EUR"]);
        assert!(possible[plan.by_id["made"]] && !possible[plan.by_id["a:USA"]]);
        let native = plan.emits[0].files.iter().find(|g| g.name == "native.bin").unwrap();
        assert_eq!((native.possible, native.at), (true, plan.by_id["a:EUR"]));
        assert_eq!(key(&plan, "native.bin"), before, "which release makes it is not its key");
        // A release's own file is the file of its name in what makes it.
        let own = plan.emits[0].files.iter().find(|g| g.name == "EUR/own.bin").unwrap();
        assert_eq!(own.prefix.as_deref(), Some("own.bin"));
        // Of USA's alone: Europe's own file is of a release not read.
        plan.resolve(&only(&["USA"]));
        assert!(!plan.emits[0].files.iter().find(|g| g.name == "EUR/own.bin").unwrap().possible);
        assert_eq!(plan.ops[plan.by_id["made"]].takes["bytes"], ["a:USA"]);
        // What is recorded of USA alone, every file of it there: filled but for Europe.
        let layer = std::env::temp_dir().join(format!("pe2-plan-partial-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&layer);
        std::fs::create_dir_all(layer.join("n.node")).unwrap();
        let plan_file = layer.with_extension("plan.jsonl");
        std::fs::write(&plan_file, &text).unwrap();
        let mut record = super::Record { releases: Some(only(&["USA"])), ..super::Record::default() };
        for name in ["native.bin", "inflated"] {
            std::fs::write(layer.join("n.node").join(name), "x").unwrap();
            let written = super::Written { key: key(&plan, name), sha1: "x".into(), size: 1, shared: false };
            record.files.insert(("n".into(), name.into()), written);
        }
        let parsed = Plan::parse(&text).unwrap();
        super::write_record(&layer.join(super::RECORD), &parsed, &record).unwrap();
        let extensions = crate::ext::Extensions::load(Vec::new(), true);
        assert_eq!(super::state(&plan_file, &layer, &extensions).unwrap(), super::State::Partial(vec!["EUR".into()]));
        // A release's own file the same as what the releases share is not kept: the shared one is it.
        let mut record = super::Record::default();
        std::fs::create_dir_all(layer.join("n.node/EUR")).unwrap();
        for (name, bytes) in [("own.bin", "same"), ("EUR/own.bin", "same"), ("inflated", "x"), ("EUR/inflated", "y")] {
            std::fs::write(layer.join("n.node").join(name), bytes).unwrap();
            let written = super::Written { key: "k".into(), sha1: sha1_hex(bytes.as_bytes()), size: 4, shared: false };
            record.files.insert(("n".into(), name.into()), written);
        }
        assert_eq!(super::share_alike(&parsed, &layer, &mut record), 1);
        assert!(!layer.join("n.node/EUR/own.bin").exists() && layer.join("n.node/EUR/inflated").exists());
        let own = &record.files[&("n".to_string(), "EUR/own.bin".to_string())];
        assert!(own.shared);
        assert_eq!(super::on_disk(&parsed, &layer, "n", "EUR/own.bin", own), layer.join("n.node/own.bin"));
        let _ = std::fs::remove_dir_all(&layer);
        let _ = std::fs::remove_file(&plan_file);
    }

    #[test]
    fn a_plan_reads_its_operations_in_order_and_keys_them_by_what_they_take() {
        let text = r#"{"plan":1,"inputs":{"USA":{"look_in":"res://roms","discs":[{"name":"x","size":1,"sha1":"00"}]}}}
{"id":"a","op":"disc","args":{"image":{"name":"x","size":1,"sha1":"00"},"slots":[]}}
{"id":"b","op":"slot","in":{"disc":"a"},"args":{"name":"S"},"expect":{"sha1":"11"}}
{"emit":"n/m","files":{"native.bin":"b"}}
"#;
        let plan = Plan::parse(text).unwrap();
        assert_eq!((plan.ops.len(), plan.emits.len()), (2, 1));
        let keys = plan.keys(&|_| String::new());
        // Another disc is another key for everything reading it.
        let other = Plan::parse(&text.replace("\"00\"", "\"01\"")).unwrap().keys(&|_| String::new());
        assert_ne!(keys[1], other[1]);
        assert!(Plan::parse(&text.replace("\"in\":{\"disc\":\"a\"}", "\"in\":{\"disc\":\"z\"}")).is_err());
    }
}
